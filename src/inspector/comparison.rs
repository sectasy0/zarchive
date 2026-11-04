use super::*;
use crate::comparison::{ComparisonItem, ComparisonReport, DifferenceKind};

pub(crate) struct ComparisonDialog {
    pub(super) path_a: Option<PathBuf>,
    pub(super) path_b: Option<PathBuf>,
    pub(super) report: Option<Arc<ComparisonReport>>,
    pub(super) error: Option<String>,
    pub(super) task: Option<u64>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) revision: u64,
    pub(super) view: Option<gpui::Entity<ComparisonPane>>,
}

impl Default for ComparisonDialog {
    fn default() -> Self {
        Self {
            path_a: None,
            path_b: None,
            report: None,
            error: None,
            task: None,
            cancel: Arc::new(AtomicBool::new(false)),
            revision: 0,
            view: None,
        }
    }
}

impl ComparisonDialog {
    pub(crate) fn is_busy(&self) -> bool {
        self.task.is_some()
    }

    fn reset(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.path_a = None;
        self.path_b = None;
        self.report = None;
        self.error = None;
        self.task = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.view = None;
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn close(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.task = None;
        self.view = None;
        self.revision = self.revision.wrapping_add(1);
    }
}

impl ArchiveManager {
    pub(crate) fn open_comparison_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.comparison_dialog.reset();
        self.show_modal(crate::Modal::CompareArchives, window, cx);
    }

    pub(crate) fn choose_comparison_archive_a(&mut self, cx: &mut Context<Self>) {
        self.choose_comparison_archive(true, cx);
    }

    pub(crate) fn choose_comparison_archive_b(&mut self, cx: &mut Context<Self>) {
        self.choose_comparison_archive(false, cx);
    }

    fn choose_comparison_archive(&mut self, archive_a: bool, cx: &mut Context<Self>) {
        let label = if archive_a { "Archive A" } else { "Archive B" };
        let mut picker = rfd::FileDialog::new().set_title(self.language.text(label));
        if let Some(directory) = self.current_directory.as_deref() {
            picker = picker.set_directory(directory);
        }
        let Some(path) = picker.pick_file() else {
            return;
        };
        if !crate::archive::is_supported_archive(&path) {
            self.comparison_dialog.error = Some(
                self.language
                    .text("Both files must be supported archives")
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        if archive_a {
            self.comparison_dialog.path_a = Some(path);
        } else {
            self.comparison_dialog.path_b = Some(path);
        }
        self.comparison_dialog.report = None;
        self.comparison_dialog.error = None;
        self.comparison_dialog.view = None;
        cx.notify();
    }

    pub(crate) fn start_archive_comparison(&mut self, cx: &mut Context<Self>) {
        let (Some(path_a), Some(path_b)) = (
            self.comparison_dialog.path_a.clone(),
            self.comparison_dialog.path_b.clone(),
        ) else {
            self.comparison_dialog.error = Some(
                self.language
                    .text("Select both archives to compare")
                    .to_owned(),
            );
            cx.notify();
            return;
        };
        if path_a == path_b {
            self.comparison_dialog.error = Some(
                self.language
                    .text("Choose two different archives")
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        self.comparison_dialog.cancel.store(true, Ordering::Relaxed);
        self.comparison_dialog.cancel = Arc::new(AtomicBool::new(false));
        self.comparison_dialog.report = None;
        self.comparison_dialog.error = None;
        self.comparison_dialog.view = None;
        let cancel = self.comparison_dialog.cancel.clone();
        let control = crate::task_queue::TaskControl::with_cancel(cancel.clone())
            .for_paths([path_a.clone(), path_b.clone()]);
        self.comparison_dialog.revision = self.comparison_dialog.revision.wrapping_add(1);
        let revision = self.comparison_dialog.revision;
        self.comparison_dialog.task = Some(self.background_queue.enqueue(
            "Comparing archives...",
            control,
            move |control, reporter| async move {
                reporter.phase("Comparing archives...");
                reporter.set(0, 1);
                let report = crate::comparison::compare_archives(&path_a, &path_b, &cancel)?;
                if !control.checkpoint() {
                    anyhow::bail!("comparison cancelled");
                }
                reporter.advance(1);
                Ok(report)
            },
            move |this, result, _, cx| {
                if this.comparison_dialog.revision != revision
                    || this.modal != Some(crate::Modal::CompareArchives)
                {
                    return;
                }
                this.comparison_dialog.task = None;
                match result {
                    Ok(report) => this.comparison_dialog.report = Some(Arc::new(report)),
                    Err(crate::task_queue::TaskFailure::Cancelled) => {}
                    Err(crate::task_queue::TaskFailure::Failed(error)) => {
                        this.comparison_dialog.error = Some(format!(
                            "{}: {error:#}",
                            this.language.text("Could not compare archives")
                        ));
                    }
                }
                cx.notify();
            },
            cx,
        ));
        cx.notify();
    }

    fn export_comparison_report(&mut self, cx: &mut Context<Self>) {
        let Some(report) = self.comparison_dialog.report.clone() else {
            return;
        };
        let language = self.language;
        let mut dialog = rfd::FileDialog::new()
            .set_title(language.text("Save comparison report"))
            .set_file_name("zarchive-comparison-report.json")
            .add_filter("JSON", &["json"]);
        if let Some(parent) = Path::new(&report.archive_a).parent() {
            dialog = dialog.set_directory(parent);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        if Path::new(&report.archive_a) == path || Path::new(&report.archive_b) == path {
            self.comparison_dialog.error = Some(
                language
                    .text("Report path must differ from archive")
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        self.background_queue.enqueue(
            "Exporting report...",
            crate::task_queue::TaskControl::default().for_paths([path.clone()]),
            move |_, _| async move {
                std::fs::write(path, serde_json::to_vec_pretty(&report.json())?)?;
                Ok(())
            },
            move |this, result, _, cx| {
                match result {
                    Ok(()) => {
                        this.status_message = Some(this.language.text("Report saved").to_owned())
                    }
                    Err(crate::task_queue::TaskFailure::Cancelled) => {}
                    Err(crate::task_queue::TaskFailure::Failed(error)) => {
                        this.comparison_dialog.error = Some(format!(
                            "{}: {error:#}",
                            this.language.text("Could not save report")
                        ));
                    }
                }
                cx.notify();
            },
            cx,
        );
    }
}

fn comparison_summary(
    report: &ComparisonReport,
    language: crate::i18n::Language,
    theme: Theme,
) -> impl IntoElement {
    let status = if report.complete {
        "Comparison complete"
    } else {
        "Comparison incomplete"
    };
    let changed = report
        .items
        .iter()
        .filter(|item| item.kind != DifferenceKind::Common)
        .count();
    let only_a = report
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                DifferenceKind::OnlyInA
                    | DifferenceKind::DirectoryOnlyInA
                    | DifferenceKind::EmptyDirectoryOnlyInA
            )
        })
        .count();
    let only_b = report
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                DifferenceKind::OnlyInB
                    | DifferenceKind::DirectoryOnlyInB
                    | DifferenceKind::EmptyDirectoryOnlyInB
            )
        })
        .count();
    let moved = report
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                DifferenceKind::Moved | DifferenceKind::Renamed | DifferenceKind::MovedAndRenamed
            )
        })
        .count();
    let content_changed = report
        .items
        .iter()
        .filter(|item| item.kind == DifferenceKind::ContentChanged)
        .count();
    let metadata_changed = report
        .items
        .iter()
        .filter(|item| item.kind == DifferenceKind::MetadataChanged)
        .count();
    let foreground = if report.complete {
        if theme.window < 0x808080 {
            0x86efac
        } else {
            0x166534
        }
    } else if theme.window < 0x808080 {
        0xfcd34d
    } else {
        0x854d0e
    };
    div()
        .id("archive-comparison-summary")
        .debug_selector(|| "archive-comparison-summary".to_owned())
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .px_1()
        .py_1()
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_h(px(24.0))
                .child(
                    div()
                        .w(px(7.0))
                        .h(px(7.0))
                        .rounded_full()
                        .bg(rgb(foreground)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(foreground))
                        .child(language.text(status)),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(theme.muted_text))
                        .child(format!(
                            "{changed} {}",
                            language.text("Differences").to_lowercase()
                        )),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(comparison_metric(
                    "Only in archive A",
                    only_a,
                    language,
                    theme,
                ))
                .child(comparison_metric(
                    "Only in archive B",
                    only_b,
                    language,
                    theme,
                ))
                .child(comparison_metric(
                    "Moved or renamed",
                    moved,
                    language,
                    theme,
                ))
                .child(comparison_metric(
                    "Content changed",
                    content_changed,
                    language,
                    theme,
                ))
                .child(comparison_metric(
                    "Metadata changed",
                    metadata_changed,
                    language,
                    theme,
                )),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .text_xs()
                .text_color(rgb(theme.muted_text))
                .child(format!(
                    "{}: {}",
                    language.text("Integrity A"),
                    language.text(integrity_label(report.integrity_a))
                ))
                .child(format!(
                    "{}: {}",
                    language.text("Integrity B"),
                    language.text(integrity_label(report.integrity_b))
                ))
                .child(format!(
                    "{}: {}",
                    language.text("Nested archives analyzed"),
                    report.nested_archives_analyzed
                )),
        )
        .when(!report.errors.is_empty(), |summary| {
            summary.child(
                div()
                    .text_xs()
                    .text_color(rgb(foreground))
                    .child(language.text("Some files could not be compared")),
            )
        })
}

fn comparison_metric(
    label: &'static str,
    value: usize,
    language: crate::i18n::Language,
    theme: Theme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .min_w(px(112.0))
        .flex_1()
        .gap_1()
        .px_2()
        .py_1()
        .border_l_1()
        .border_color(rgb(theme.subtle_border))
        .child(
            div()
                .text_base()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(value.to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme.muted_text))
                .child(language.text(label)),
        )
}

fn integrity_label(integrity: Integrity) -> &'static str {
    match integrity {
        Integrity::NotTested => "Not tested",
        Integrity::Passed => "Integrity passed",
        Integrity::Failed => "Integrity check failed",
        Integrity::Incomplete => "Inspection incomplete",
        Integrity::PasswordRequired => "Password required",
    }
}

pub(crate) fn comparison_dialog_content(
    manager: &mut ArchiveManager,
    theme: Theme,
    window: &Window,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    let path_a = manager
        .comparison_dialog
        .path_a
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| language.text("No archive selected").to_owned());
    let path_b = manager
        .comparison_dialog
        .path_b
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| language.text("No archive selected").to_owned());
    let busy = manager.comparison_dialog.is_busy();
    let report = manager.comparison_dialog.report.clone();
    let error = manager.comparison_dialog.error.clone();
    let path_a_label = language.text("Archive A");
    let path_b_label = language.text("Archive B");
    let mut content = div()
        .id("comparison-dialog-content")
        .flex()
        .flex_col()
        .gap_3()
        .w_full()
        .max_h((window.viewport_size().height - px(180.0)).max(px(280.0)));
    for (field, label, path, selector, choose_a) in [
        (
            "comparison-archive-a",
            path_a_label,
            path_a,
            "comparison-select-a",
            true,
        ),
        (
            "comparison-archive-b",
            path_b_label,
            path_b,
            "comparison-select-b",
            false,
        ),
    ] {
        let action = Button::new(selector)
            .custom(
                gpui::component::button::ButtonCustomVariant::new(cx)
                    .color(rgb(theme.surface).into())
                    .foreground(rgb(theme.soft_accent()).into())
                    .hover(rgb(theme.hover).into())
                    .active(rgb(theme.selection).into()),
            )
            .w_full()
            .h(px(32.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(theme.border))
            .px_2()
            .xsmall()
            .accessibility_label(label)
            .tooltip(language.text("Choose archive"))
            .disabled(busy)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_xs()
                            .truncate()
                            .text_color(rgb(theme.muted_text))
                            .child(path),
                    )
                    .child(crate::app_icon("archive", 14.0, theme.soft_accent())),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if choose_a {
                    this.choose_comparison_archive_a(cx);
                } else {
                    this.choose_comparison_archive_b(cx);
                }
            }));
        content = content.child(
            div()
                .id(field)
                .debug_selector(|| field.to_owned())
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.muted_text))
                        .child(label),
                )
                .child(action),
        );
    }
    if let Some(error) = error {
        content = content.child(
            div()
                .text_xs()
                .text_color(rgb(if theme.window < 0x808080 {
                    0xfca5a5
                } else {
                    0xb91c1c
                }))
                .child(error),
        );
    }
    if busy {
        content = content.child(findings_skeleton(theme));
    } else if let Some(report) = report {
        let pane = manager.comparison_dialog.view.get_or_insert_with(|| {
            cx.new(|_| ComparisonPane {
                report: report.clone(),
                scroll: gpui::UniformListScrollHandle::new(),
                filter: ComparisonFilter::All,
                language,
                theme,
            })
        });
        pane.update(cx, |pane, cx| {
            let changed = !Arc::ptr_eq(&pane.report, &report)
                || pane.language != language
                || pane.theme.surface != theme.surface;
            if !Arc::ptr_eq(&pane.report, &report) {
                pane.report = report.clone();
                pane.scroll = gpui::UniformListScrollHandle::new();
                pane.filter = ComparisonFilter::All;
            }
            pane.language = language;
            pane.theme = theme;
            if changed {
                cx.notify();
            }
        });
        content = content
            .child(comparison_summary(&report, language, theme))
            .child(
                div()
                    .h((window.viewport_size().height - px(380.0))
                        .min(px(420.0))
                        .max(px(160.0)))
                    .flex()
                    .flex_col()
                    .min_h(px(0.0))
                    .child(pane.clone()),
            );
    }
    content
}

pub(crate) fn comparison_footer(
    manager: &ArchiveManager,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    let ready = manager
        .comparison_dialog
        .path_a
        .as_ref()
        .zip(manager.comparison_dialog.path_b.as_ref())
        .is_some_and(|(path_a, path_b)| path_a != path_b);
    let label = if manager.comparison_dialog.report.is_some() {
        "Run comparison again"
    } else {
        "Start comparison"
    };
    let report = manager.comparison_dialog.report.clone();
    div()
        .id("comparison-actions")
        .debug_selector(|| "comparison-actions".to_owned())
        .flex()
        .items_center()
        .gap_2()
        .when_some(report, |actions, report| {
            actions.child(
                div()
                    .id("comparison-report-actions")
                    .debug_selector(|| "comparison-report-actions".to_owned())
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("copy-comparison-report")
                            .debug_selector(|| "copy-comparison-report".to_owned())
                            .ghost()
                            .small()
                            .icon(gpui::assets::IconName::Copy)
                            .tooltip(language.text("Copy report"))
                            .accessibility_label(language.text("Copy report"))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if let Ok(text) = serde_json::to_string_pretty(&report.json()) {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                                }
                            })),
                    )
                    .child(
                        Button::new("save-comparison-report")
                            .debug_selector(|| "save-comparison-report".to_owned())
                            .ghost()
                            .small()
                            .icon(gpui::assets::IconName::Save)
                            .tooltip(language.text("Save report"))
                            .accessibility_label(language.text("Save report"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.export_comparison_report(cx);
                            })),
                    ),
            )
        })
        .child(div().flex_1())
        .child(
            Button::new("comparison-start")
                .debug_selector(|| "comparison-start".to_owned())
                .primary()
                .small()
                .label(language.text(label))
                .disabled(!ready || manager.comparison_dialog.is_busy())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_archive_comparison(cx);
                })),
        )
}

/// Renders the appropriate empty message before or after an audit.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ComparisonFilter {
    All,
    Differences,
    Content,
    Metadata,
}

pub(super) struct ComparisonPane {
    report: Arc<ComparisonReport>,
    scroll: gpui::UniformListScrollHandle,
    filter: ComparisonFilter,
    language: crate::i18n::Language,
    theme: Theme,
}

impl gpui::Render for ComparisonPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let report = self.report.clone();
        let filter = self.filter;
        let language = self.language;
        let theme = self.theme;
        let visible_indices: Vec<_> = report
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| match filter {
                ComparisonFilter::All => true,
                ComparisonFilter::Differences => item.kind != DifferenceKind::Common,
                ComparisonFilter::Content => item.content.is_some(),
                ComparisonFilter::Metadata => !item.metadata.is_empty(),
            })
            .map(|(index, _)| index)
            .collect();
        let mut filters = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .pb_2()
            .children(
                [
                    (ComparisonFilter::All, "All"),
                    (ComparisonFilter::Differences, "Differences"),
                    (ComparisonFilter::Content, "Content"),
                    (ComparisonFilter::Metadata, "Metadata"),
                ]
                .into_iter()
                .map(|(value, label)| {
                    Button::new(label)
                        .ghost()
                        .xsmall()
                        .selected(filter == value)
                        .label(language.text(label))
                        .on_click(cx.listener(move |pane, _, _, cx| {
                            pane.filter = value;
                            pane.scroll = gpui::UniformListScrollHandle::new();
                            cx.notify();
                        }))
                }),
            );
        if visible_indices.is_empty() {
            filters = filters.child(
                div()
                    .text_xs()
                    .text_color(rgb(theme.muted_text))
                    .py_2()
                    .child(language.text("No comparison results")),
            );
        }
        div()
            .id("archive-comparison-list")
            .debug_selector(|| "archive-comparison-list".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .overflow_hidden()
            .child(filters)
            .child(
                gpui::uniform_list(
                    "archive-comparison-items",
                    visible_indices.len(),
                    cx.processor(move |_, range: std::ops::Range<usize>, _, _| {
                        range
                            .filter_map(|index| {
                                visible_indices
                                    .get(index)
                                    .and_then(|item_index| {
                                        report
                                            .items
                                            .get(*item_index)
                                            .map(|item| (*item_index, item))
                                    })
                                    .map(|(item_index, item)| {
                                        comparison_row(item, item_index, language, theme)
                                    })
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.scroll)
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0)),
            )
            .child(
                gpui::component::scroll::Scrollbar::vertical(&self.scroll)
                    .id("archive-comparison-scrollbar")
                    .mode(gpui::component::scroll::ScrollbarMode::Always),
            )
    }
}

fn comparison_row(
    item: &ComparisonItem,
    index: usize,
    language: crate::i18n::Language,
    theme: Theme,
) -> gpui::Stateful<gpui::Div> {
    let path_a = item.path_a.as_deref().unwrap_or("-");
    let path_b = item.path_b.as_deref().unwrap_or("-");
    let mut details = Vec::new();
    if let Some(content) = &item.content {
        if let Some(equal) = content.semantic_equal {
            details.push(
                language
                    .text(if equal {
                        "Semantically equal"
                    } else {
                        "Semantically different"
                    })
                    .to_owned(),
            );
        } else if let Some(equal) = content.binary_equal {
            details.push(
                language
                    .text(if equal {
                        "Byte-identical"
                    } else {
                        "Binary content differs"
                    })
                    .to_owned(),
            );
        }
        if let Some(offset) = content.first_difference {
            details.push(format!(
                "{}: {offset}",
                language.text("First different byte")
            ));
        }
        if content.sha256_a.is_some() || content.sha256_b.is_some() {
            details.push(format!(
                "{}: {} / {}",
                language.text("SHA-256 hashes"),
                hash_prefix(content.sha256_a.as_deref()),
                hash_prefix(content.sha256_b.as_deref())
            ));
        }
        if content.sha512_a.is_some() || content.sha512_b.is_some() {
            details.push(format!(
                "{}: {} / {}",
                language.text("SHA-512 hashes"),
                hash_prefix(content.sha512_a.as_deref()),
                hash_prefix(content.sha512_b.as_deref())
            ));
        }
        if content.blake3_a.is_some() || content.blake3_b.is_some() {
            details.push(format!(
                "{}: {} / {}",
                language.text("BLAKE3 hashes"),
                hash_prefix(content.blake3_a.as_deref()),
                hash_prefix(content.blake3_b.as_deref())
            ));
        }
        if let Some(diff) = &content.text_diff {
            let preview = diff.lines().take(2).collect::<Vec<_>>().join(" ");
            if !preview.is_empty() {
                details.push(format!("{}: {preview}", language.text("Text diff preview")));
            }
        }
        if content.text_diff_truncated {
            details.push(language.text("Text diff shortened").to_owned());
        }
        if let Some(diff) = &content.semantic_diff {
            let preview = diff.lines().take(2).collect::<Vec<_>>().join(" ");
            if !preview.is_empty() {
                details.push(format!(
                    "{}: {preview}",
                    language.text("Semantic diff preview")
                ));
            }
        }
        if content.semantic_diff_truncated {
            details.push(language.text("Semantic diff shortened").to_owned());
        }
        if content.semantic_limit_reached {
            details.push(language.text("Semantic analysis limit reached").to_owned());
        }
        if content.text_diff_limit_reached {
            details.push(language.text("Text diff limit reached").to_owned());
        }
        if let Some(error) = &content.semantic_error {
            details.push(format!(
                "{}: {error}",
                language.text("Semantic parser error")
            ));
        }
        for (label, value) in [
            ("Image metadata A", content.image_metadata_a.as_deref()),
            ("Image metadata B", content.image_metadata_b.as_deref()),
            ("Comparison error A", content.error_a.as_deref()),
            ("Comparison error B", content.error_b.as_deref()),
        ] {
            if let Some(value) = value {
                details.push(format!("{}: {value}", language.text(label)));
            }
        }
    }
    let metadata_details: Vec<_> = item
        .metadata
        .iter()
        .map(|change| {
            format!(
                "{}: {} → {}",
                language.text(change.key),
                change.value_a.as_deref().unwrap_or("-"),
                change.value_b.as_deref().unwrap_or("-"),
            )
        })
        .collect();
    let metadata_count = metadata_details.len();
    details.splice(0..0, metadata_details.into_iter().take(2));
    let detail_count = details.len();
    let visible_details: Vec<_> = details.into_iter().take(4).collect();
    let more_details = detail_count
        .saturating_sub(visible_details.len())
        .saturating_add(metadata_count.saturating_sub(2));
    let detail_chips = div()
        .flex()
        .flex_wrap()
        .gap_1()
        .max_h(px(38.0))
        .overflow_hidden()
        .children(visible_details.into_iter().map(|detail| {
            div()
                .max_w(px(300.0))
                .h(px(18.0))
                .px_1()
                .rounded_sm()
                .bg(rgb(theme.sidebar))
                .text_xs()
                .text_color(rgb(theme.muted_text))
                .truncate()
                .child(detail)
        }))
        .when(more_details > 0, |chips| {
            chips.child(
                div()
                    .h(px(18.0))
                    .px_1()
                    .rounded_sm()
                    .bg(rgb(theme.selection))
                    .text_xs()
                    .text_color(rgb(theme.muted_text))
                    .child(format!("+{more_details} {}", language.text("More details"))),
            )
        });
    let scope = (!item.scope.is_empty()).then(|| item.scope.clone());
    let tone = if matches!(
        item.kind,
        DifferenceKind::OnlyInA
            | DifferenceKind::OnlyInB
            | DifferenceKind::ContentChanged
            | DifferenceKind::EntryTypeChanged
    ) {
        if theme.window < 0x808080 {
            0xfcd34d
        } else {
            0x854d0e
        }
    } else {
        theme.soft_accent()
    };
    div()
        .id(("archive-comparison-item", index))
        .debug_selector(move || format!("archive-comparison-item-{index}"))
        .flex()
        .flex_col()
        .justify_center()
        .gap_2()
        .w_full()
        .h(px(132.0))
        .flex_shrink_0()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(rgb(theme.selection))
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(tone))
                        .child(language.text(item.kind.label())),
                )
                .child(div().flex_1())
                .when_some(scope, |header, scope| {
                    header.child(
                        div()
                            .max_w(px(360.0))
                            .text_xs()
                            .text_color(rgb(theme.muted_text))
                            .truncate()
                            .child(language.text_data(&scope)),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .gap_3()
                .child(comparison_path_cell("Archive A", path_a, language, theme))
                .child(comparison_path_cell("Archive B", path_b, language, theme)),
        )
        .when(detail_count > 0, |row| row.child(detail_chips))
}

fn comparison_path_cell(
    label: &'static str,
    path: &str,
    language: crate::i18n::Language,
    theme: Theme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.0))
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme.muted_text))
                .child(language.text(label)),
        )
        .child(
            div()
                .min_w(px(0.0))
                .text_sm()
                .truncate()
                .child(path.to_owned()),
        )
}

fn hash_prefix(hash: Option<&str>) -> &str {
    hash.map(|hash| &hash[..hash.len().min(12)]).unwrap_or("-")
}
