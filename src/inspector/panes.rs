use super::*;

/// Owns selection and scroll state local to the virtualized Entries tab.
pub(super) struct EntryPane {
    pub(super) manager: gpui::WeakEntity<ArchiveManager>,
    pub(super) password: Option<ArchivePassword>,
    pub(super) busy: bool,
    pub(super) report: Arc<Report>,
    pub(super) requested_name: Option<String>,
    pub(super) selected_index: Option<usize>,
    pub(super) scroll: gpui::UniformListScrollHandle,
    pub(super) language: crate::i18n::Language,
    pub(super) theme: Theme,
    pub(super) batch_limited: bool,
}

/// Owns report and scroll state local to the virtualized Findings tab.
pub(super) struct FindingsPane {
    pub(super) report: Arc<Report>,
    pub(super) scroll: gpui::UniformListScrollHandle,
    pub(super) language: crate::i18n::Language,
    pub(super) theme: Theme,
}

impl gpui::Render for FindingsPane {
    // Renders only the visible slice of findings inside a retained scrollable list.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let language = self.language;
        div()
            .id("inspector-findings-list")
            .debug_selector(|| "inspector-findings-list".to_owned())
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .overflow_hidden()
            .child(
                gpui::uniform_list(
                    "inspector-findings-items",
                    self.report.findings.len(),
                    cx.processor(move |pane, range: std::ops::Range<usize>, _, _| {
                        range
                            .filter_map(|index| {
                                pane.report.findings.get(index).map(|finding| {
                                    finding_row(finding, &pane.report.path, index, language, theme)
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
                    .id("inspector-findings-scrollbar")
                    .mode(gpui::component::scroll::ScrollbarMode::Always),
            )
    }
}

/// Renders one audit finding with severity coloring and a readable entry location.
fn finding_row(
    finding: &inspection::Finding,
    archive_path: &Path,
    index: usize,
    language: crate::i18n::Language,
    theme: Theme,
) -> gpui::Stateful<gpui::Div> {
    let severe = matches!(
        finding.kind,
        "Damaged archive"
            | "Damaged entry"
            | "Unsafe path"
            | "Format mismatch"
            | "Archive bomb"
            | "Parser anomaly"
            | "Suspicious link"
            | "Alternate data stream"
    );
    let foreground = if theme.window < 0x808080 {
        if severe { 0xfca5a5 } else { 0xfcd34d }
    } else if severe {
        0xb91c1c
    } else {
        0x854d0e
    };
    let location = if finding.entry.is_empty() {
        archive_label(archive_path)
    } else {
        finding.entry.clone()
    };
    div()
        .id(("inspector-audit-finding", index))
        .debug_selector(move || format!("inspector-audit-finding-{index}"))
        .flex()
        .items_stretch()
        .gap_3()
        .w_full()
        .h(px(80.0))
        .flex_shrink_0()
        .px_3()
        .py_3()
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
        .child(div().w(px(3.0)).h_full().rounded_md().bg(rgb(foreground)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(foreground))
                        .truncate()
                        .child(language.text(finding.kind)),
                )
                .child(div().text_xs().truncate().child(location))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.muted_text))
                        .truncate()
                        .child(
                            if matches!(
                                finding.kind,
                                "Damaged archive" | "Unsupported compression method"
                            ) {
                                language.error_message(&finding.detail).to_owned()
                            } else {
                                language.text_data(&finding.detail)
                            },
                        ),
                ),
        )
}

impl EntryPane {
    /// Resolves a requested entry name and scrolls the matching row into view.
    pub(super) fn select_name(&mut self, name: Option<String>) {
        self.selected_index = name.as_ref().and_then(|name| {
            self.report
                .entries
                .iter()
                .position(|entry| &entry.name == name)
        });
        self.requested_name = name;
        if let Some(index) = self.selected_index {
            self.scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Center);
        }
    }
}

impl gpui::Render for EntryPane {
    // Renders the entry table through a virtual list so large reports stay responsive.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let language = self.language;
        let report = &self.report;
        let archive_path = report.path.clone();
        let destination = EntryTarget {
            name: archive_label(&archive_path),
            key: archive_path.to_string_lossy().into_owned(),
            path: archive_path.clone(),
            archive_entry: None,
            password: self.password.clone(),
            is_directory: false,
        };
        let internal_target = destination.clone();
        let internal_hover = destination.clone();
        let external_hover = destination.clone();
        let internal_view = self.manager.clone();
        let external_view = self.manager.clone();
        let busy = self.busy;
        let headers = div()
            .id("inspector-entries-header")
            .debug_selector(|| "inspector-entries-header".to_owned())
            .flex()
            .items_center()
            .w_full()
            .h(px(28.0))
            .flex_shrink_0()
            .pr_2()
            .border_b_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.sidebar))
            .text_xs()
            .text_color(rgb(theme.muted_text))
            .child(
                gpui::base::TableHead::new("inspector-entry-name-head", 0)
                    .debug_selector(|| "inspector-entry-name-head".to_owned())
                    .flex()
                    .items_center()
                    .h_full()
                    .min_w(px(0.0))
                    .flex_1()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.muted_text))
                    .gap_1()
                    .pl_2()
                    .child(language.text("NAME")),
            )
            .child(
                gpui::base::TableHead::new("inspector-entry-type-head", 1)
                    .debug_selector(|| "inspector-entry-type-head".to_owned())
                    .flex()
                    .items_center()
                    .h_full()
                    .w(px(130.0))
                    .flex_shrink_0()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.muted_text))
                    .pl_2()
                    .child(language.text("TYPE")),
            )
            .child(
                gpui::base::TableHead::new("inspector-entry-size-head", 2)
                    .debug_selector(|| "inspector-entry-size-head".to_owned())
                    .flex()
                    .items_center()
                    .h_full()
                    .w(px(90.0))
                    .flex_shrink_0()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.muted_text))
                    .pl_2()
                    .child(language.text("SIZE")),
            );
        div()
            .id("inspector-entries-drop-area")
            .debug_selector(|| "inspector-entries-drop-area".to_owned())
            .flex()
            .flex_1()
            .min_h(px(0.0))
            .flex_col()
            .overflow_hidden()
            .drag_over::<crate::drop_service::ArchiveFileDrag>(move |style, drag, _, _| {
                let accepted = !busy
                    && crate::drop_service::accepts_archive_drop(
                        &internal_hover,
                        std::slice::from_ref(&drag.target.path),
                    );
                style.bg(gpui::rgba(if accepted {
                    (theme.accent << 8) | 0x2a
                } else {
                    0xef44441a
                }))
            })
            .drag_over::<gpui::ExternalPaths>(move |style, paths, _, _| {
                let accepted = !busy
                    && crate::drop_service::accepts_archive_drop(&external_hover, paths.paths());
                style.bg(gpui::rgba(if accepted {
                    (theme.accent << 8) | 0x2a
                } else {
                    0xef44441a
                }))
            })
            .on_drop(
                move |drag: &crate::drop_service::ArchiveFileDrag, window, cx| {
                    cx.stop_propagation();
                    let _ = internal_view.update(cx, |manager, cx| {
                        manager.receive_archive_drop(
                            &internal_target,
                            std::slice::from_ref(&drag.target.path),
                            window,
                            cx,
                        )
                    });
                },
            )
            .on_drop(move |paths: &gpui::ExternalPaths, window, cx| {
                cx.stop_propagation();
                let _ = external_view.update(cx, |manager, cx| {
                    manager.receive_archive_drop(&destination, paths.paths(), window, cx)
                });
            })
            .child(headers)
            .child(
                gpui::uniform_list(
                    "inspector-entries",
                    report.entries.len(),
                    cx.processor(move |pane, range: std::ops::Range<usize>, _, cx| {
                        range
                            .filter_map(|index| {
                                pane.report.entries.get(index).map(|info| {
                                    entry_row(
                                        info,
                                        &archive_path,
                                        index,
                                        pane.selected_index == Some(index),
                                        language,
                                        theme,
                                        pane.manager.clone(),
                                        pane.password.clone(),
                                        pane.busy,
                                        cx,
                                    )
                                })
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.scroll)
                .flex_1()
                .min_w(px(0.0))
                .min_h(px(0.0)),
            )
    }
}

/// Renders one virtualized archive-entry row and updates the pane-local selection.
fn entry_row(
    info: &inspection::EntryInfo,
    archive_path: &Path,
    index: usize,
    selected: bool,
    language: crate::i18n::Language,
    theme: Theme,
    manager_view: gpui::WeakEntity<ArchiveManager>,
    password: Option<ArchivePassword>,
    busy: bool,
    cx: &mut Context<EntryPane>,
) -> gpui::AnyElement {
    let target = EntryTarget {
        name: entry_display_name(&info.name, archive_path),
        key: info.name.clone(),
        path: archive_path.to_path_buf(),
        archive_entry: Some(info.name.clone()),
        password,
        is_directory: info.directory,
    };
    let destination = EntryTarget {
        archive_entry: info.directory.then(|| info.name.clone()),
        ..target.clone()
    };
    let internal_hover = destination.clone();
    let external_hover = destination.clone();
    let external_destination = destination.clone();
    let internal_view = manager_view.clone();
    let external_view = manager_view.clone();
    gpui::base::TableRow::new(
        gpui::SharedString::from(format!("inspected-entry:{index}")),
        index,
    )
    .debug_selector(move || format!("inspected-entry-{index}"))
    .flex()
    .items_center()
    .w_full()
    .h(px(32.0))
    .flex_shrink_0()
    .px_2()
    .border_b_1()
    .border_color(rgb(theme.subtle_border))
    .text_xs()
    .bg(rgb(if index % 2 == 1 {
        theme.sidebar
    } else {
        theme.surface
    }))
    .when(selected, |row| row.bg(rgb(theme.selection)))
    .when(!selected, |row| {
        row.hover(|style| style.bg(rgb(theme.hover)).cursor_pointer())
    })
    .child(
        gpui::base::TableCell::new("inspector-entry-name", 0)
            .debug_selector(move || format!("inspector-entry-name-cell-{index}"))
            .flex()
            .flex_1()
            .min_w(px(0.0))
            .items_center()
            .gap_2()
            .child(crate::app_icon(
                if info.directory { "folder" } else { "file" },
                16.0,
                if info.directory || selected {
                    theme.accent
                } else {
                    theme.icon
                },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .child(entry_display_name(&info.name, archive_path)),
            ),
    )
    .child(
        gpui::base::TableCell::new("inspector-entry-type", 1)
            .debug_selector(move || format!("inspector-entry-type-cell-{index}"))
            .w(px(130.0))
            .flex_shrink_0()
            .pl_2()
            .text_color(rgb(theme.muted_text))
            .child(if info.directory {
                language.text("Folder").to_owned()
            } else {
                info.name
                    .rsplit_once('.')
                    .map(|(_, extension)| extension.to_ascii_uppercase())
                    .unwrap_or_default()
            }),
    )
    .child(
        gpui::base::TableCell::new("inspector-entry-size", 2)
            .debug_selector(move || format!("inspector-entry-size-cell-{index}"))
            .w(px(90.0))
            .flex_shrink_0()
            .pl_2()
            .text_color(rgb(theme.muted_text))
            .child(crate::format_size(info.size)),
    )
    .on_click(cx.listener(move |this, _, _, cx| {
        this.selected_index = Some(index);
        cx.notify();
    }))
    .drag_over::<crate::drop_service::ArchiveFileDrag>(move |style, drag, _, _| {
        let accepted = !busy
            && crate::drop_service::accepts_archive_drop(
                &internal_hover,
                std::slice::from_ref(&drag.target.path),
            );
        style
            .bg(gpui::rgba(if accepted {
                (theme.accent << 8) | 0x2a
            } else {
                0xef44441a
            }))
            .border_color(rgb(if accepted { theme.accent } else { 0xef4444 }))
    })
    .drag_over::<gpui::ExternalPaths>(move |style, paths, _, _| {
        let accepted =
            !busy && crate::drop_service::accepts_archive_drop(&external_hover, paths.paths());
        style
            .bg(gpui::rgba(if accepted {
                (theme.accent << 8) | 0x2a
            } else {
                0xef44441a
            }))
            .border_color(rgb(if accepted { theme.accent } else { 0xef4444 }))
    })
    .on_drop(
        move |drag: &crate::drop_service::ArchiveFileDrag, window, cx| {
            cx.stop_propagation();
            let _ = internal_view.update(cx, |manager, cx| {
                manager.receive_archive_drop(
                    &destination,
                    std::slice::from_ref(&drag.target.path),
                    window,
                    cx,
                )
            });
        },
    )
    .on_drop(move |paths: &gpui::ExternalPaths, window, cx| {
        cx.stop_propagation();
        let _ = external_view.update(cx, |manager, cx| {
            manager.receive_archive_drop(&external_destination, paths.paths(), window, cx)
        });
    })
    .context_menu(move |menu, _, _| {
        crate::file_actions::inspected_entry_menu(
            menu,
            &target,
            manager_view.clone(),
            language,
            busy,
        )
    })
    .into_any_element()
}
