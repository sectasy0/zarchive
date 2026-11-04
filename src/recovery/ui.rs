use super::*;
use crate::{ArchiveManager, Theme};
use gpui::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
};
use gpui::{App, Context as UiContext, IntoElement, Window, div, prelude::*, px, rgb};

pub(crate) struct State {
    pub(super) source: Option<PathBuf>,
    pub password: crate::secret::SecretInput,
    pub(super) salvage: bool,
    pub(super) parity: bool,
    pub(super) result: Option<Arc<Recovery>>,
    pub(super) task: Option<u64>,
    pub(super) revision: u64,
    pub(super) exporting: bool,
    pub(super) error: Option<String>,
    pub(super) results: gpui::ListState,
    pub(super) results_layout: Option<(crate::i18n::Language, gpui::Pixels)>,
}

impl State {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            source: None,
            password: crate::secret::SecretInput::new(cx),
            salvage: false,
            parity: false,
            result: None,
            task: None,
            revision: 0,
            exporting: false,
            error: None,
            results: gpui::ListState::new(0, gpui::ListAlignment::Top, px(128.0)),
            results_layout: None,
        }
    }
}

impl ArchiveManager {
    pub(crate) fn clear_recovery(&mut self, window: &mut Window, cx: &mut UiContext<Self>) {
        self.recovery.revision = self.recovery.revision.wrapping_add(1);
        if let Some(task) = self.recovery.task.take() {
            crate::task_queue::cancel(self, task, window, cx);
        }
        self.recovery.password.clear();
        self.recovery.result = None;
        self.recovery.results.reset(0);
        self.recovery.error = None;
        self.recovery.exporting = false;
    }

    pub(crate) fn open_recovery(&mut self, window: &mut Window, cx: &mut UiContext<Self>) {
        let target = self.inspection_target();
        self.clear_recovery(window, cx);
        self.recovery.source = target.as_ref().map(|(path, _)| path.clone());
        if self.recovery.source.as_ref().is_some_and(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("par2"))
        }) {
            self.recovery.parity = true;
        }
        if let Some(password) = target.and_then(|(_, password)| password) {
            self.recovery.password.text.push_str(password.as_str());
        }
        self.show_modal(crate::Modal::Recovery, window, cx);
    }

    pub(super) fn choose_recovery_source(&mut self, window: &mut Window, cx: &mut UiContext<Self>) {
        if self.recovery.task.is_some() {
            return;
        }
        if let Some(path) = rfd::FileDialog::new()
            .set_title(self.language.text("Select archive to recover"))
            .pick_file()
        {
            self.clear_recovery(window, cx);
            self.recovery.parity = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("par2"));
            self.recovery.source = Some(path);
            cx.notify();
        }
    }

    pub(crate) fn start_recovery(&mut self, cx: &mut UiContext<Self>) {
        if self.recovery.task.is_some() {
            return;
        }
        let Some(source) = self.recovery.source.clone() else {
            return;
        };
        self.recovery.result = None;
        self.recovery.error = None;
        self.recovery.revision = self.recovery.revision.wrapping_add(1);
        let revision = self.recovery.revision;
        let salvage = self.recovery.salvage;
        let parity = self.recovery.parity;
        let password =
            (!self.recovery.password.text.is_empty()).then(|| self.recovery.password.text.clone());
        let mut resources = vec![source.clone()];
        if crate::parity::supported(&source)
            || crate::parity::is_split_zip(&source)
            || crate::volumes::numbered_base(&source).is_some()
            || crate::archive::ResolvedFormat::from_path(&source)
                == Some(crate::archive::ResolvedFormat::Rar)
        {
            resources.push(
                source
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf(),
            );
        }
        let task = self.background_queue.enqueue(
            "Recovering archive...",
            crate::task_queue::TaskControl::default().for_paths(resources),
            move |control, reporter| async move {
                reporter.phase("Recovering archive...");
                reporter.set(0, 1);
                if !control.checkpoint() {
                    anyhow::bail!("cancelled");
                }
                let result = recover_with_parity(
                    &source,
                    password.as_ref().map(|password| password.as_str()),
                    salvage,
                    parity,
                )?;
                if !control.checkpoint() {
                    anyhow::bail!("cancelled");
                }
                reporter.set(1, 1);
                Ok(Arc::new(result))
            },
            move |this, result, _, cx| {
                if this.recovery.revision != revision {
                    return;
                }
                this.recovery.task = None;
                match result {
                    Ok(result) => {
                        this.recovery.results.reset(result.row_count());
                        this.recovery.result = Some(result);
                    }
                    Err(crate::task_queue::TaskFailure::Failed(error)) => {
                        this.recovery.error = Some(format!("{error:#}"))
                    }
                    Err(crate::task_queue::TaskFailure::Cancelled) => {}
                }
                cx.notify();
            },
            cx,
        );
        self.recovery.task = Some(task);
        cx.notify();
    }

    pub(super) fn save_recovered_archive(&mut self, cx: &mut UiContext<Self>) {
        if self.recovery.task.is_some() {
            return;
        }
        let Some(result) = self.recovery.result.clone() else {
            return;
        };
        let Some(format) = result.original_format() else {
            return;
        };
        let extension = format.extension();
        let Some(destination) = rfd::FileDialog::new()
            .set_title(self.language.text("Export"))
            .set_file_name(format!("recovered.{extension}"))
            .add_filter(format.label(), &[extension])
            .save_file()
        else {
            return;
        };
        if !destination.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(&format!(".{extension}"))
        }) {
            self.recovery.error =
                Some("The file extension must match the selected format.".to_owned());
            cx.notify();
            return;
        }
        self.export_recovered_archive(result, destination, cx);
    }

    pub(super) fn export_recovered_archive(
        &mut self,
        result: Arc<Recovery>,
        destination: PathBuf,
        cx: &mut UiContext<Self>,
    ) {
        self.recovery.error = None;
        self.recovery.exporting = true;
        let revision = self.recovery.revision;
        let source = result.source.clone();
        let format = result.original_format();
        let task = self.background_queue.enqueue(
            "Exporting recovered archive...",
            crate::task_queue::TaskControl::default().for_paths([source, destination.clone()]),
            move |control, reporter| async move {
                reporter.phase("Exporting recovered archive...");
                if !control.checkpoint() {
                    anyhow::bail!("cancelled");
                }
                result.export_format(
                    &destination,
                    anyhow::Context::context(format, "The original archive format is unavailable")?,
                )?;
                Ok(destination)
            },
            move |this, result, _, cx| {
                if this.recovery.revision != revision {
                    return;
                }
                this.recovery.task = None;
                this.recovery.exporting = false;
                match result {
                    Ok(path) => {
                        this.status_message = Some(format!(
                            "{} {}",
                            this.language.text("Recovered archive saved"),
                            path.display()
                        ))
                    }
                    Err(crate::task_queue::TaskFailure::Failed(error)) => {
                        this.recovery.error = Some(format!("{error:#}"))
                    }
                    Err(crate::task_queue::TaskFailure::Cancelled) => {}
                }
                cx.notify();
            },
            cx,
        );
        self.recovery.task = Some(task);
        cx.notify();
    }
}

pub(crate) fn view(
    manager: &mut ArchiveManager,
    theme: Theme,
    window: &Window,
    cx: &mut UiContext<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    let layout = (language, window.viewport_size().width.min(px(800.0)));
    if manager.recovery.results_layout != Some(layout) {
        let count = manager
            .recovery
            .result
            .as_ref()
            .map_or(0, |result| result.row_count());
        manager.recovery.results.reset(count);
        manager.recovery.results_layout = Some(layout);
    }
    let state = &manager.recovery;
    let busy = state.task.is_some();
    div()
        .id("recovery-content")
        .debug_selector(|| "recovery-content".to_owned())
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_sm()
                        .whitespace_normal()
                        .child(
                            state
                                .source
                                .as_ref()
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|| {
                                    language.text("Select archive to recover").to_owned()
                                }),
                        ),
                )
                .child(
                    Button::new("choose-recovery-source")
                        .ghost()
                        .small()
                        .icon(gpui::assets::IconName::FolderOpen)
                        .label(language.text("Choose archive..."))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.choose_recovery_source(window, cx)
                        })),
                ),
        )
        .child(language.text("Password"))
        .child(crate::secret::field(
            "recovery-password",
            &state.password,
            crate::secret::SecretField::Recovery,
            !busy,
            language,
            cx,
        ))
        .child(
            Checkbox::new("recovery-salvage")
                .debug_selector(|| "recovery-salvage".to_owned())
                .label(language.text("Salvage Mode"))
                .checked(state.salvage)
                .disabled(busy)
                .on_change(cx.listener(|this, checked, _, cx| {
                    this.recovery.salvage = *checked;
                    cx.notify();
                })),
        )
        .when(
            state
                .source
                .as_deref()
                .is_some_and(crate::parity::supported),
            |this| {
                this.child(
                    Checkbox::new("recovery-parity")
                        .debug_selector(|| "recovery-parity".to_owned())
                        .label(language.text("Reconstruct using parity data"))
                        .checked(state.parity)
                        .disabled(busy)
                        .on_change(cx.listener(|this, checked, _, cx| {
                            this.recovery.parity = *checked;
                            cx.notify();
                        })),
                )
            },
        )
        .when(busy, |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(rgb(theme.muted_text))
                    .child(language.text(if state.exporting {
                        "Saving recovered archive..."
                    } else {
                        "Recovering archive..."
                    })),
            )
        })
        .when_some(state.error.as_ref(), |this, error| {
            this.child(div().text_xs().child(language.error_message(error)))
        })
        .when_some(state.result.clone(), |this, report| {
            let recovered = report
                .entries
                .iter()
                .filter(|entry| entry.status == Status::Recovered)
                .count();
            let partial = report
                .entries
                .iter()
                .filter(|entry| entry.status == Status::Partial)
                .count();
            let unavailable = report
                .entries
                .iter()
                .filter(|entry| entry.status == Status::NotRecovered)
                .count();
            let unverified = report
                .entries
                .iter()
                .filter(|entry| matches!(entry.status, Status::Unverified | Status::Candidate))
                .count();
            let count = report.row_count();
            let notes = report.notes.clone();
            let limited = report.limited;
            this.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .text_xs()
                    .child(format!("{}: {recovered}", language.text("Recovered")))
                    .child(format!(
                        "{}: {partial}",
                        language.text("Partially recovered")
                    ))
                    .child(format!(
                        "{}: {unverified}",
                        language.text("Recovered (unverified)")
                    ))
                    .child(format!("{}: {unavailable}", language.text("Not recovered"))),
            )
            .children(
                notes
                    .into_iter()
                    .map(|note| div().text_xs().child(language.text(note))),
            )
            .when(limited, |this| {
                this.child(
                    div()
                        .text_xs()
                        .child(language.text("Recovery safety limit reached")),
                )
            })
            .when(count == 0, |this| {
                this.child(language.text("No recoverable files were found"))
            })
            .when(count > 0, |this| {
                this.child(
                    div()
                        .id("recovery-results")
                        .debug_selector(|| "recovery-results".to_owned())
                        .w_full()
                        .h(px(320.0))
                        .child(
                            gpui::list(state.results.clone(), move |index, _, _| {
                                if let Some(entry) = report.entries.get(index) {
                                    div()
                                        .id(("recovery-result", index))
                                        .debug_selector(move || format!("recovery-result-{index}"))
                                        .w_full()
                                        .min_w(px(0.0))
                                        .py_3()
                                        .border_b_1()
                                        .border_color(rgb(theme.subtle_border))
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .whitespace_normal()
                                        .child(
                                            div()
                                                .id(("recovery-result-name", index))
                                                .debug_selector(move || {
                                                    format!("recovery-result-name-{index}")
                                                })
                                                .w_full()
                                                .min_w(px(0.0))
                                                .text_sm()
                                                .child(entry.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_3()
                                                .text_xs()
                                                .child(
                                                    div()
                                                        .id(("recovery-result-status", index))
                                                        .debug_selector(move || {
                                                            format!(
                                                                "recovery-result-status-{index}"
                                                            )
                                                        })
                                                        .min_w(px(0.0))
                                                        .child(language.text(entry.status.label())),
                                                )
                                                .child(crate::format_size(entry.bytes)),
                                        )
                                        .when_some(entry.issue, |this, issue| {
                                            this.child(
                                                div()
                                                    .id(("recovery-result-issue", index))
                                                    .debug_selector(move || {
                                                        format!("recovery-result-issue-{index}")
                                                    })
                                                    .w_full()
                                                    .min_w(px(0.0))
                                                    .text_sm()
                                                    .text_color(rgb(theme.muted_text))
                                                    .child(language.text(issue)),
                                            )
                                        })
                                        .children(
                                            report
                                                .diagnostics
                                                .iter()
                                                .filter(|diagnostic| {
                                                    diagnostic.entry == Some(index)
                                                })
                                                .map(|diagnostic| {
                                                    diagnostic_view(
                                                        diagnostic,
                                                        Some(index),
                                                        language,
                                                        theme,
                                                    )
                                                }),
                                        )
                                        .into_any_element()
                                } else if index < report.entries.len() + report.blocks.len() {
                                    let block = &report.blocks[index - report.entries.len()];
                                    div()
                                        .id(("recovery-result", index))
                                        .debug_selector(move || format!("recovery-result-{index}"))
                                        .w_full()
                                        .min_w(px(0.0))
                                        .py_3()
                                        .border_b_1()
                                        .border_color(rgb(theme.subtle_border))
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .whitespace_normal()
                                        .child(
                                            div()
                                                .w_full()
                                                .text_sm()
                                                .child(language.text(block.issue)),
                                        )
                                        .child(div().text_xs().child(format!(
                                            "{}: {} + {}",
                                            language.text("Suspect data range"),
                                            block.offset,
                                            block.length
                                        )))
                                        .into_any_element()
                                } else {
                                    let diagnostic = report
                                        .diagnostics
                                        .iter()
                                        .filter(|diagnostic| diagnostic.entry.is_none())
                                        .nth(index - report.entries.len() - report.blocks.len())
                                        .expect("recovery diagnostic index");
                                    div()
                                        .id(("recovery-diagnostic", index))
                                        .debug_selector(move || {
                                            format!("recovery-diagnostic-{index}")
                                        })
                                        .w_full()
                                        .py_3()
                                        .border_b_1()
                                        .border_color(rgb(theme.subtle_border))
                                        .child(diagnostic_view(diagnostic, None, language, theme))
                                        .into_any_element()
                                }
                            })
                            .w_full()
                            .h_full(),
                        ),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme.muted_text))
                    .child(language.text("Recovered archives are not encrypted")),
            )
        })
}

fn diagnostic_view(
    diagnostic: &Diagnostic,
    entry: Option<usize>,
    language: crate::i18n::Language,
    theme: Theme,
) -> impl IntoElement {
    div()
        .id(("recovery-cause", entry.unwrap_or(MAX_FILES + 1)))
        .debug_selector(move || {
            format!(
                "recovery-cause-{}",
                entry.map_or_else(|| "archive".to_owned(), |index| index.to_string())
            )
        })
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w(px(0.0))
        .whitespace_normal()
        .text_xs()
        .child(div().child(format!(
            "{}: {}",
            language.text("Reason"),
            language.text(diagnostic.reason.label())
        )))
        .when_some(diagnostic.expected_size, |this, size| {
            this.child(format!(
                "{}: {}",
                language.text("Expected size"),
                crate::format_size(size)
            ))
        })
        .when_some(diagnostic.actual_size, |this, size| {
            this.child(format!(
                "{}: {}",
                language.text("Recovered bytes"),
                crate::format_size(size)
            ))
        })
        .when_some(diagnostic.expected_crc, |this, crc| {
            this.child(format!("{}: {crc:08x}", language.text("Expected CRC32")))
        })
        .when_some(
            diagnostic
                .actual_crc
                .filter(|_| diagnostic.expected_crc.is_some()),
            |this, crc| this.child(format!("{}: {crc:08x}", language.text("Actual CRC32"))),
        )
        .when_some(diagnostic.offset, |this, offset| {
            this.child(format!(
                "{}: {offset} + {}",
                language.text("Suspect data range"),
                diagnostic.length.unwrap_or(0)
            ))
        })
        .child(div().text_color(rgb(theme.muted_text)).child(format!(
            "{}: {}",
            language.text("Suggestion"),
            language.text(diagnostic.reason.action())
        )))
}

pub(crate) fn footer(
    manager: &ArchiveManager,
    cx: &mut UiContext<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    let busy = manager.recovery.task.is_some();
    let can_export = manager.recovery.result.as_ref().is_some_and(|result| {
        result.original_format().is_some()
            && result.entries.iter().any(|entry| entry.file.is_some())
    });
    div()
        .id("recovery-actions")
        .debug_selector(|| "recovery-actions".to_owned())
        .flex()
        .flex_wrap()
        .justify_end()
        .gap_2()
        .child(
            Button::new("close-recovery")
                .ghost()
                .small()
                .label(language.text("Close"))
                .on_click(cx.listener(|this, _, window, cx| this.close_modal(window, cx))),
        )
        .child(
            Button::new("export-recovery")
                .debug_selector(|| "export-recovery".to_owned())
                .ghost()
                .small()
                .icon(gpui::assets::IconName::Save)
                .label(language.text("Export"))
                .disabled(busy || !can_export)
                .on_click(cx.listener(|this, _, _, cx| this.save_recovered_archive(cx))),
        )
        .child(
            Button::new("start-recovery")
                .debug_selector(|| "start-recovery".to_owned())
                .primary()
                .small()
                .icon(gpui::assets::IconName::Wrench)
                .label(language.text("Recover files"))
                .disabled(busy || manager.recovery.source.is_none())
                .on_click(cx.listener(|this, _, _, cx| this.start_recovery(cx))),
        )
}
