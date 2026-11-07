use super::super::*;

impl Render for ArchiveManager {
    /// Synchronizes app services and renders the titlebar, browser, sidebar, and inspector.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::shortcuts::prepare(self, cx);
        ui::prepare(self, window, cx);
        creation::prepare(self, window, cx);
        secret::prepare(self, window, cx);
        operations::prepare(self, window, cx);
        toolbar::prepare_search(self, window, cx);
        filters::prepare(self, window, cx);
        let password_modal = self.modal == Some(Modal::Password);
        if password_modal
            && self.password_prompt.task.is_none()
            && !self.password_prompt.input.focus.is_focused(window)
        {
            window.focus(&self.password_prompt.input.focus, cx);
        } else if self.modal.is_none() && self.password_prompt.input.focus.is_focused(window) {
            window.focus(&self.content_focus, cx);
        }
        if self.error.is_none() {
            self.calculate_directory_sizes(window, cx);
        }
        task_queue::prepare(self, window, cx);
        let dark = self.theme_mode.is_dark(window.appearance());
        let theme = Theme::new(dark).with_accent(self.accent_color.color(dark));
        let language = self.language;
        let location_title = self
            .archive_name
            .as_deref()
            .or_else(|| {
                self.current_directory
                    .as_deref()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
            })
            .unwrap_or(language.text("File Browser"))
            .to_owned();
        let folders = folder_tree(&self.entries);
        let visible_entries = self.visible_browser_entries();
        let item_count = visible_entries.len();
        let location_size = self
            .location_size(&visible_entries)
            .map(format_size)
            .unwrap_or_else(|| {
                if !self.auto_calculate_folder_sizes {
                    "--".to_owned()
                } else if self.directory_size_task.is_some() {
                    "-".to_owned()
                } else {
                    language.text("Size unavailable").to_owned()
                }
            });
        let status_message = if self.comparison_dialog.is_busy() {
            Some(language.text("Comparing archives...").to_owned())
        } else if self.inspector.is_busy() {
            Some(language.text("Inspecting archive...").to_owned())
        } else if self.file_actions.delete_task.is_some() {
            Some(language.text("Moving to Trash...").to_owned())
        } else if self.creation_task.is_some() {
            Some(language.text("Creating archive...").to_owned())
        } else {
            self.status_message
                .as_ref()
                .filter(|message| !status_message_is_error(message))
                .cloned()
        }
        .or_else(|| {
            self.directory_size_task
                .is_some()
                .then(|| language.text("Calculating folder sizes...").to_owned())
        })
        .unwrap_or_default();
        let mut notifications = Vec::new();
        if let Some(error) = &self.error {
            notifications.push((true, error.clone()));
        }
        if let Some(message) = &self.status_message {
            if status_message_is_error(message)
                && !notifications.iter().any(|(_, text)| text == message)
            {
                notifications.push((true, message.clone()));
            }
        }
        if self.inspector.batch_limited {
            notifications.push((
                false,
                language.text("Only the first 32 archives were checked. Run the audit again for the remaining archives.").to_owned(),
            ));
        }
        if self
            .inspector
            .reports
            .get(self.inspector.active)
            .is_some_and(|report| report.limited)
        {
            notifications.push((
                false,
                language.text("The audit reached a safety limit while reading this archive. Some data may not have been checked, so the findings can be incomplete.").to_owned(),
            ));
        }
        if self.notifications != notifications {
            if notifications
                .iter()
                .any(|notice| !self.notifications.contains(notice))
            {
                self.notifications_visible = true;
            } else if notifications.is_empty() {
                self.notifications_visible = false;
            }
            self.notifications = notifications;
        }
        let error_count = self
            .notifications
            .iter()
            .filter(|(error, _)| *error)
            .count();
        let warning_count = self.notifications.len() - error_count;
        let inspection_warning_color = if theme.window < 0x808080 {
            0xfcd34d
        } else {
            0x854d0e
        };
        let mut folder_tree_view = div()
            .id("folder-tree")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&self.folder_scroll)
            .px(px(6.0))
            .pb_2()
            .gap_0();

        if self.archive_name.is_some() && self.error.is_none() {
            folder_tree_view = folder_tree_view.child(folder_row(
                &location_title,
                "",
                0,
                &self.selected_folder,
                self.selected_entry.as_deref(),
                None,
                theme,
                self.archive_drop_enabled(),
                cx,
            ));
            for (path, name, depth) in folders {
                folder_tree_view = folder_tree_view.child(folder_row(
                    &name,
                    &path,
                    depth + 1,
                    &self.selected_folder,
                    self.selected_entry.as_deref(),
                    None,
                    theme,
                    self.archive_drop_enabled(),
                    cx,
                ));
            }
        } else if let Some(directory) = &self.current_directory {
            folder_tree_view = folder_tree_view.child(folder_row(
                &location_title,
                "",
                0,
                "",
                self.selected_entry.as_deref(),
                Some(directory.clone()),
                theme,
                self.archive_drop_enabled(),
                cx,
            ));
            for entry in &self.entries {
                if entry.is_directory {
                    folder_tree_view = folder_tree_view.child(folder_row(
                        &entry.name,
                        &entry.name,
                        1,
                        "",
                        self.selected_entry.as_deref(),
                        Some(directory.clone()),
                        theme,
                        self.archive_drop_enabled(),
                        cx,
                    ));
                }
            }
        }

        let original_browser = self.active_browser;
        let mut build_file_list = |pane_index| {
        self.active_browser = pane_index;
        let file_list_id = if pane_index == 0 { "file-list" } else { "file-list-secondary" };
        let item_list_id = if pane_index == 0 { "file-list-items" } else { "file-list-items-secondary" };
        let background_menu_view = cx.entity().downgrade();
        let can_create_here = self.current_directory.is_some() && self.creation_task.is_none();
        let search_query = self.toolbar_search.text_query();
        let visible_entries = self.visible_browser_entries();
        let item_count = visible_entries.len();
        let archive_path = self.archive_path.clone();
        let current_directory = self.current_directory.clone();
        let archive_password = self.archive_password.clone();
        let selected_folder = self.selected_folder.clone();
        let selected_entry = self.selected_entry.clone();
        let file_list =
            if self.error.is_some() {
                gpui::base::TableBody::new(file_list_id)
                    .debug_selector(move || file_list_id.to_owned())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.0))
                    .bg(rgb(theme.surface))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .text_sm()
                            .text_color(rgb(theme.muted_text))
                            .child(language.text("Unable to open this archive")),
                    )
                    .context_menu(move |menu, _, _| {
                        file_actions::background_menu(
                            menu,
                            background_menu_view.clone(),
                            language,
                            can_create_here,
                        )
                    })
                    .into_any_element()
            } else if visible_entries.is_empty() && self.toolbar_search.is_searching() {
                gpui::base::TableBody::new(file_list_id)
                    .debug_selector(move || file_list_id.to_owned())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .bg(rgb(theme.surface))
                    .child(search_results_skeleton(theme))
                    .context_menu(move |menu, _, _| {
                        file_actions::background_menu(
                            menu,
                            background_menu_view.clone(),
                            language,
                            can_create_here,
                        )
                    })
                    .into_any_element()
            } else if visible_entries.is_empty() {
                gpui::base::TableBody::new(file_list_id)
                    .debug_selector(move || file_list_id.to_owned())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.0))
                    .bg(rgb(theme.surface))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .flex_1()
                            .gap_2()
                            .child(div().text_base().text_color(rgb(theme.text)).child(
                                language.text(
                                    if search_query.is_empty() && !self.filters.criteria.is_active()
                                    {
                                        "This folder is empty"
                                    } else {
                                        "No matching items"
                                    },
                                ),
                            ))
                            .child(div().text_sm().text_color(rgb(theme.muted_text)).child(
                                language.text(if self.filters.criteria.is_active() {
                                    "No items match the current filters"
                                } else if search_query.is_empty() {
                                    "There are no files or folders to display."
                                } else {
                                    "Try another search."
                                }),
                            )),
                    )
                    .context_menu(move |menu, _, _| {
                        file_actions::background_menu(
                            menu,
                            background_menu_view.clone(),
                            language,
                            can_create_here,
                        )
                    })
                    .into_any_element()
            } else {
                gpui::base::TableBody::new(file_list_id)
                    .debug_selector(move || file_list_id.to_owned())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .bg(rgb(theme.surface))
                    .child(
                        gpui::uniform_list(
                            item_list_id,
                            item_count,
                            cx.processor(move |manager, range: std::ops::Range<usize>, _, cx| {
                                let previous_browser = manager.active_browser;
                                manager.active_browser = pane_index;
                                let drop_marker_row = manager.active_browser_tab().drop_marker_row;
                                let rows = range
                                    .filter_map(|index| {
                                        let entry = visible_entries.get(index)?;
                                        let folder_size_known = current_directory.is_none()
                                            || !entry.is_directory
                                            || manager.directory_sizes.as_ref().is_some_and(
                                                |sizes| sizes.contains_key(&entry.name),
                                            );
                                        Some(
                                            file_row(
                                                entry,
                                                archive_path.as_deref(),
                                                current_directory.as_deref(),
                                                archive_password.clone(),
                                                &selected_folder,
                                                selected_entry.as_deref(),
                                                language,
                                                folder_size_known,
                                                index,
                                                pane_index,
                                                drop_marker_row == Some(index),
                                                theme,
                                                manager.archive_drop_enabled(),
                                                entry
                                                    .source_path
                                                    .as_deref()
                                                    .and_then(|key| {
                                                        manager.toolbar_search.matches.get(key)
                                                    })
                                                    .map(|result| result.target.clone()),
                                                cx,
                                            )
                                            .into_any_element(),
                                        )
                                    })
                                    .collect();
                                manager.active_browser = previous_browser;
                                rows
                            }),
                        )
                        .track_scroll(&self.file_scroll)
                        .flex()
                        .flex_col()
                        .flex_1()
                        .w_full()
                        .min_h(px(0.0)),
                    )
                    .context_menu(move |menu, _, _| {
                        file_actions::background_menu(
                            menu,
                            background_menu_view.clone(),
                            language,
                            can_create_here,
                        )
                    })
                    .into_any_element()
            };
            file_list
        };
        let file_lists = vec![build_file_list(0), build_file_list(1)];
        drop(build_file_list);
        self.active_browser = original_browser;
        let mut browser_panels = Vec::new();
        for (pane_index, file_list) in file_lists.into_iter().enumerate() {
            browser_panels.push(
                browser_panel(self, pane_index, file_list, language, theme, cx).into_any_element(),
            );
        }

        div()
            .id("archive-manager")
            .on_drag_move::<inspector::PreviewResize>(cx.listener(inspector::resize_preview))
            .track_focus(&self.content_focus)
            .key_context(if self.inspector_focus && self.inspector.visible && self.modal.is_none() {
                "ArchiveManager Inspector"
            } else if self.content_focus.is_focused(window) && self.modal.is_none() {
                "ArchiveManager Browser"
            } else {
                "ArchiveManager"
            })
            .on_action(cx.listener(|this, _: &toolbar::key_actions::SelectPrevious, window, cx| {
                this.move_browser_selection(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &toolbar::key_actions::SelectNext, window, cx| {
                this.move_browser_selection(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &toolbar::key_actions::EnterFolder, window, cx| {
                this.enter_browser_folder(window, cx);
            }))
            .on_action(cx.listener(|this, _: &toolbar::key_actions::LeaveFolder, window, cx| {
                this.leave_browser_folder(window, cx);
            }))
            .on_action(cx.listener(|this, _: &toolbar::key_actions::PreviewSelection, window, cx| {
                this.preview_browser_selection(window, cx);
            }))
            .on_action(cx.listener(|this, _: &toolbar::key_actions::CloseBrowserTab, _, cx| {
                if this.inspector_focus && this.inspector.visible {
                    this.inspector.close_active_tab();
                    cx.notify();
                } else {
                    this.close_active_browser_tab(cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &toolbar::key_actions::OpenFile, _, cx| {
                    if this.modal.is_none() {
                        this.open_file(cx);
                    }
                }),
            )
            .on_action(cx.listener(
                |this, _: &toolbar::key_actions::CreateArchive, window, cx| {
                    if this.modal.is_none() {
                        this.show_modal(Modal::CreateArchive, window, cx);
                    }
                },
            ))
            .on_action(
                cx.listener(|this, _: &toolbar::key_actions::ExtractHere, window, cx| {
                    if this.modal.is_none() {
                        this.extract_current_archive(true, window, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &toolbar::key_actions::ExtractTo, window, cx| {
                    if this.modal.is_none() {
                        this.extract_current_archive(false, window, cx);
                    }
                }),
            )
            .on_action(cx.listener(
                |this, _: &toolbar::key_actions::TestIntegrity, window, cx| {
                    if this.modal.is_none() {
                        this.inspect_current(inspection::InspectionMode::Integrity, window, cx);
                    }
                },
            ))
            .on_action(
                cx.listener(|this, _: &toolbar::key_actions::AuditArchive, window, cx| {
                    if this.modal.is_none() {
                        this.inspect_current(inspection::InspectionMode::Audit, window, cx);
                    }
                }),
            )
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme.window))
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    cx.stop_propagation();
                    this.receive_external_paths(paths.paths(), window, cx);
                }),
            )
            .child(
                TitleBar::new()
                    .h(px(44.0))
                    .pl(if cfg!(target_os = "macos") && !window.is_fullscreen() {
                        px(80.0)
                    } else {
                        px(0.0)
                    })
                    .bg(rgb(theme.titlebar))
                    .child(toolbar::toolbox_bar(
                        self,
                        language,
                        theme,
                        self.can_navigate_back(),
                        self.can_navigate_forward(),
                        self.can_extract_current_archive(),
                        self.inspection_target().is_some(),
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(
                        div()
                            .id("sidebar")
                            .flex()
                            .flex_col()
                            .w(px(232.0))
                            .flex_shrink_0()
                            .min_h(px(0.0))
                            .border_r_1()
                            .border_color(rgb(theme.border))
                            .bg(rgb(theme.sidebar))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_shrink_0()
                                    .pt_1()
                                    .child(sidebar_heading(language.text("Places"), None, theme))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .px(px(6.0))
                                            .pb_2()
                                            .child(location_shortcut(
                                                language.text("Documents"),
                                                dirs::document_dir(),
                                                self.current_directory.as_deref(),
                                                theme,
                                                self.archive_drop_enabled(),
                                                cx,
                                            ))
                                            .child(location_shortcut(
                                                language.text("Home"),
                                                dirs::home_dir(),
                                                self.current_directory.as_deref(),
                                                theme,
                                                self.archive_drop_enabled(),
                                                cx,
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .border_t_1()
                                    .border_color(rgb(theme.subtle_border))
                                    .child(sidebar_heading(
                                        language.text("Folders"),
                                        Some(folders_count(&self.entries)),
                                        theme,
                                    )),
                            )
                            .child(
                                div()
                                    .id("folder-tree-scroll-viewport")
                                    .relative()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .overflow_hidden()
                                    .child(folder_tree_view)
                                    .child(
                                        gpui::component::scroll::Scrollbar::vertical(
                                            &self.folder_scroll,
                                        )
                                        .id("folder-tree-scrollbar")
                                        .mode(gpui::component::scroll::ScrollbarMode::Always),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("main-panel")
                            .debug_selector(|| "main-panel".to_owned())
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.0))
                            .bg(rgb(theme.surface))
                            .child(
                                div()
                                    .id("browser-split")
                                    .flex()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .children(browser_panels),
                            )
                            .when(self.inspector.visible, |this| {
                                let height =
                                    self.inspector.preview_height(window.viewport_size().height);
                                let panel = inspector::view(self, theme, height, cx);
                                this.child(
                                    div()
                                        .id("resizable-preview")
                                        .debug_selector(|| "resizable-preview".to_owned())
                                        .relative()
                                        .h(height)
                                        .min_h(px(0.0))
                                        .flex_shrink_0()
                                        .child(panel)
                                        .child(
                                            gpui::component::resizable::resize_handle::<
                                                inspector::PreviewResize,
                                                inspector::PreviewResize,
                                            >(
                                                "preview-resize-handle", gpui::Axis::Vertical
                                            )
                                            .debug_selector(|| "preview-resize-handle".to_owned())
                                            .inside(gpui::component::resizable::HandleEdge::Leading)
                                            .on_drag(
                                                inspector::PreviewResize {
                                                    bottom: window.viewport_size().height
                                                        - px(24.0),
                                                    maximum: (window.viewport_size().height
                                                        - px(220.0))
                                                    .max(px(80.0)),
                                                },
                                                |drag, _, _, cx| cx.new(|_| drag.clone()),
                                            ),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .id("status-bar")
                                    .debug_selector(|| "status-bar".to_owned())
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .h(px(24.0))
                                    .flex_shrink_0()
                                    .min_w(px(0.0))
                                    .px_3()
                                    .border_t_1()
                                    .border_color(rgb(theme.border))
                                    .bg(rgb(theme.sidebar))
                                    .text_xs()
                                    .text_color(rgb(theme.muted_text))
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .child(language.item_count(item_count)),
                                    )
                                    .child(
                                        div()
                                            .id("status-message")
                                            .debug_selector(|| "status-message".to_owned())
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.0))
                                                    .truncate()
                                                    .child(status_message),
                                            ),
                                    )
                                    .child(task_queue::toggle(self, theme, cx))
                                    .child(
                                        Button::new("status-notifications-toggle")
                                            .ghost()
                                            .xsmall()
                                            .selected(
                                                self.notifications_visible
                                                    && !self.notifications.is_empty(),
                                            )
                                            .tooltip(language.text("Toggle notifications"))
                                            .accessibility_label(format!(
                                                "{}: {error_count}, {}: {warning_count}. {}",
                                                language.text("Errors"),
                                                language.text("Warnings"),
                                                language.text("Toggle notifications"),
                                            ))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.notifications_visible =
                                                    !this.notifications_visible;
                                                cx.notify();
                                            }))
                                            .child(
                                                div()
                                                    .id("status-notifications")
                                                    .debug_selector(|| {
                                                        "status-notifications".to_owned()
                                                    })
                                                    .flex()
                                                    .items_center()
                                                    .gap_1()
                                                    .child(
                                                        gpui::component::Icon::new(
                                                            gpui::assets::IconName::CircleX,
                                                        )
                                                        .size(px(12.0))
                                                        .text_color(rgb(if error_count > 0 {
                                                            0xef4444
                                                        } else {
                                                            theme.muted_text
                                                        })),
                                                    )
                                                    .child(error_count.to_string())
                                                    .child(
                                                        gpui::component::Icon::new(
                                                            gpui::assets::IconName::TriangleAlert,
                                                        )
                                                        .size(px(12.0))
                                                        .text_color(rgb(if warning_count > 0 {
                                                            inspection_warning_color
                                                        } else {
                                                            theme.muted_text
                                                        })),
                                                    )
                                                    .child(warning_count.to_string()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("location-size")
                                            .debug_selector(|| "location-size".to_owned())
                                            .flex_shrink_0()
                                            .child(format!(
                                                "{}: {location_size}",
                                                language.text("Size")
                                            )),
                                    ),
                            ),
                    ),
            )
            .when(
                self.notifications_visible
                    && !self.notifications.is_empty()
                    && self.modal.is_none(),
                |this| {
                    this.child(
                        div()
                            .id("notifications-toast")
                            .debug_selector(|| "notifications-toast".to_owned())
                            .absolute()
                            .right(px(12.0))
                            .bottom(px(36.0))
                            .w((window.viewport_size().width - px(24.0)).min(px(360.0)))
                            .max_h((window.viewport_size().height - px(100.0)).min(px(240.0)))
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(theme.border))
                            .bg(rgb(theme.surface))
                            .shadow_lg()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .px_3()
                                    .py_1()
                                    .child(div().text_xs().child(language.text("Notifications")))
                                    .child(
                                        Button::new("hide-notifications")
                                            .ghost()
                                            .xsmall()
                                            .icon(gpui::assets::IconName::X)
                                            .tooltip(language.text("Hide notifications"))
                                            .accessibility_label(
                                                language.text("Hide notifications"),
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.notifications_visible = false;
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .id("notification-messages")
                                    .flex()
                                    .flex_col()
                                    .min_h(px(0.0))
                                    .overflow_y_scroll()
                                    .children(self.notifications.iter().map(|(error, message)| {
                                        div()
                                            .flex()
                                            .items_start()
                                            .gap_2()
                                            .p_3()
                                            .border_t_1()
                                            .border_color(rgb(theme.subtle_border))
                                            .child(
                                                gpui::component::Icon::new(if *error {
                                                    gpui::assets::IconName::CircleX
                                                } else {
                                                    gpui::assets::IconName::TriangleAlert
                                                })
                                                .size(px(14.0))
                                                .flex_shrink_0()
                                                .text_color(rgb(if *error {
                                                    0xef4444
                                                } else {
                                                    inspection_warning_color
                                                })),
                                            )
                                            .child(div().flex_1().min_w(px(0.0)).text_xs().child(
                                                if *error {
                                                    language.error_message(message).to_owned()
                                                } else {
                                                    message.clone()
                                                },
                                            ))
                                    })),
                            ),
                    )
                },
            )
            .when(self.modal.is_some(), |this| {
                this.child(self.password_blur.view())
            })
    }
}

fn last_visible_drop_row(
    scroll: &gpui::UniformListScrollHandle,
    item_count: usize,
    y: gpui::Pixels,
) -> Option<usize> {
    if item_count == 0 {
        return None;
    }
    let handle = &scroll.0.borrow().base_handle;
    let viewport = handle.bounds();
    let offset = handle.offset().y;
    let rows_to_bottom = (f32::from(viewport.size.height - offset) / 28.0).ceil() as usize;
    let last_visible = rows_to_bottom.saturating_sub(1).min(item_count - 1);
    let row_bottom = viewport.top() + px((last_visible + 1) as f32 * 28.0) + offset;
    (y > row_bottom && y < viewport.bottom()).then_some(last_visible)
}

fn browser_tab_drop_allowed(path: &Path) -> bool {
    path.is_dir() || archive::is_supported_archive(path)
}

fn browser_tab_drop_accepts(paths: &[PathBuf]) -> bool {
    matches!(paths, [path] if browser_tab_drop_allowed(path))
}

fn open_browser_tab_drop(
    manager: &mut ArchiveManager,
    pane_index: usize,
    tab_index: usize,
    path: PathBuf,
    cx: &mut Context<ArchiveManager>,
) {
    if !browser_tab_drop_allowed(&path) {
        return;
    }
    manager.active_browser = pane_index;
    manager.browsers[pane_index].active_tab = tab_index;
    let is_directory = path.is_dir();
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let target = file_actions::EntryTarget {
        name,
        key: path.to_string_lossy().into_owned(),
        path,
        archive_entry: None,
        password: None,
        is_directory,
    };
    manager.open_entry(&target, cx);
}

fn open_browser_tab_drop_as_new(
    manager: &mut ArchiveManager,
    pane_index: usize,
    path: PathBuf,
    cx: &mut Context<ArchiveManager>,
) {
    if !browser_tab_drop_allowed(&path) {
        return;
    }
    manager.add_browser_tab(pane_index, cx);
    let tab_index = manager.browsers[pane_index].active_tab;
    open_browser_tab_drop(manager, pane_index, tab_index, path, cx);
}

fn browser_panel(
    manager: &mut ArchiveManager,
    pane_index: usize,
    file_list: impl IntoElement,
    language: Language,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let previous_browser = manager.active_browser;
    let focused = previous_browser == pane_index;
    manager.active_browser = pane_index;
    let pane = &manager.browsers[pane_index];
    let tab_labels: Vec<_> = pane
        .tabs
        .iter()
        .map(|tab| {
            let label = tab.archive_name
                .clone()
                .or_else(|| {
                    tab.current_directory
                        .as_deref()
                        .and_then(Path::file_name)
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .or_else(|| tab.current_directory.as_deref().map(|path| path.display().to_string()))
                .unwrap_or_else(|| language.text("File Browser").to_owned());
            let icon = if tab.archive_path.is_some() { "archive" } else { "folder" };
            (label, icon)
        })
        .collect();
    let active_tab = pane.active_tab;
    let tab_count = pane.tabs.len();
    let drop_enabled = manager.modal.is_none();
    let folder_drop_view = cx.entity().downgrade();
    let folder_drop_tab_id = manager.active_browser_tab().id;
    let folder_drop_scroll = manager.file_scroll.clone();
    let folder_drop_row_count = manager.visible_browser_entries().len();
    let mut tab_items = Vec::with_capacity(tab_count);
    for (tab_index, (label, icon)) in tab_labels.into_iter().enumerate() {
        let selected = active_tab == tab_index;
        let select_tab = cx.listener(move |this, _, _, cx| {
            this.active_browser = pane_index;
            this.browsers[pane_index].active_tab = tab_index;
            cx.notify();
        });
        let tab_selector = format!("browser-tab:{pane_index}:{tab_index}");
        let mut tab = div()
            .id(format!("browser-tab-group-{pane_index}-{tab_index}"))
            .debug_selector(move || tab_selector.clone())
            .relative()
            .flex()
            .items_center()
            .gap_1()
            .min_w(px(90.0))
            .max_w(px(180.0))
            .h(px(30.0))
            .flex_shrink_0()
            .px_2()
            .border_r_1()
            .border_color(rgb(theme.border))
            .bg(rgb(if selected { theme.surface } else { theme.titlebar }))
            .text_color(rgb(if selected { theme.text } else { theme.muted_text }))
            .text_xs()
            .cursor_pointer()
            .on_click(select_tab)
            .drag_over::<drop_service::ArchiveFileDrag>(move |style, drag, _, _| {
                let accepted = drop_enabled && browser_tab_drop_allowed(&drag.target.path);
                style
                    .bg(gpui::rgba(if accepted { (theme.accent << 8) | 0x2a } else { 0xef44441a }))
                    .border_color(rgb(if accepted { theme.accent } else { 0xef4444 }))
            })
            .drag_over::<gpui::ExternalPaths>(move |style, paths, _, _| {
                let accepted = drop_enabled && browser_tab_drop_accepts(paths.paths());
                style
                    .bg(gpui::rgba(if accepted { (theme.accent << 8) | 0x2a } else { 0xef44441a }))
                    .border_color(rgb(if accepted { theme.accent } else { 0xef4444 }))
            })
            .on_drop(cx.listener(move |manager, drag: &drop_service::ArchiveFileDrag, _, cx| {
                cx.stop_propagation();
                open_browser_tab_drop(manager, pane_index, tab_index, drag.target.path.clone(), cx);
            }))
            .on_drop(cx.listener(move |manager, paths: &gpui::ExternalPaths, _, cx| {
                cx.stop_propagation();
                if let [path] = paths.paths() {
                    open_browser_tab_drop(manager, pane_index, tab_index, path.clone(), cx);
                }
            }))
            .when(!selected, |tab| {
                tab.hover(|style| {
                    style
                        .bg(rgb(theme.hover))
                        .border_color(rgb(theme.border))
                        .text_color(rgb(theme.text))
                })
            })
            .child(crate::app_icon(
                icon,
                13.0,
                if selected { theme.accent } else { theme.icon },
            ))
            .child(div().flex_1().min_w(px(0.0)).truncate().child(label));
        if tab_count > 1 {
            let close_tab = cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.close_browser_tab(pane_index, tab_index, cx);
            });
            tab = tab.child(
                Button::new(format!("close-browser-tab-{pane_index}-{tab_index}"))
                    .ghost()
                    .xsmall()
                    .w(px(18.0))
                    .h(px(18.0))
                    .flex_shrink_0()
                    .icon(gpui::assets::IconName::X)
                    .tooltip(language.text("Close"))
                    .on_click(close_tab),
            );
        }
        if selected {
            tab = tab.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(2.0))
                    .bg(rgb(theme.accent)),
            );
        }
        tab_items.push(tab.into_any_element());
    }
    let scroll_selector = format!("browser-tabs-scroll:{pane_index}");
    let tab_scroll = div()
        .id(format!("browser-tabs-scroll-{pane_index}"))
        .debug_selector(move || scroll_selector.clone())
        .flex()
        .items_center()
        .gap_0()
        .flex_1()
        .min_w(px(0.0))
        .h(px(30.0))
        .track_scroll(&pane.tab_scroll)
        .overflow_x_scroll()
        .children(tab_items);
    let new_tab_drop_enabled = drop_enabled;
    let mut tab_bar = div()
        .id(format!("browser-tabs-{pane_index}"))
        .debug_selector(move || format!("browser-tabs-drop:{pane_index}"))
        .flex()
        .items_center()
        .gap_1()
        .h(px(30.0))
        .flex_shrink_0()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.sidebar))
        .drag_over::<drop_service::ArchiveFileDrag>(move |style, drag, _, _| {
            let accepted = new_tab_drop_enabled && browser_tab_drop_allowed(&drag.target.path);
            style
                .bg(gpui::rgba(if accepted { (theme.accent << 8) | 0x2a } else { 0xef44441a }))
                .border_color(rgb(if accepted { theme.accent } else { 0xef4444 }))
        })
        .drag_over::<gpui::ExternalPaths>(move |style, paths, _, _| {
            let accepted = new_tab_drop_enabled && browser_tab_drop_accepts(paths.paths());
            style
                .bg(gpui::rgba(if accepted { (theme.accent << 8) | 0x2a } else { 0xef44441a }))
                .border_color(rgb(if accepted { theme.accent } else { 0xef4444 }))
        })
        .on_drop(cx.listener(move |manager, drag: &drop_service::ArchiveFileDrag, _, cx| {
            cx.stop_propagation();
            open_browser_tab_drop_as_new(manager, pane_index, drag.target.path.clone(), cx);
        }))
        .on_drop(cx.listener(move |manager, paths: &gpui::ExternalPaths, _, cx| {
            cx.stop_propagation();
            if let [path] = paths.paths() {
                open_browser_tab_drop_as_new(manager, pane_index, path.clone(), cx);
            }
        }))
        .child(tab_scroll);
    let add_tab = cx.listener(move |this, _, _, cx| this.add_browser_tab(pane_index, cx));
    let new_tab = div()
        .id(format!("new-browser-tab-drop-{pane_index}"))
        .flex()
        .items_center()
        .justify_center()
        .h(px(30.0))
        .flex_shrink_0()
        .child(
            Button::new(format!("new-browser-tab-{pane_index}"))
                .ghost()
                .xsmall()
                .icon(gpui::assets::IconName::Plus)
                .tooltip(language.text("New tab"))
                .on_click(add_tab),
        );
    tab_bar = tab_bar.child(new_tab);

    let archive_path = manager.archive_path.clone();
    let drop_target = archive_path.clone().map(|path| file_actions::EntryTarget {
        name: manager.archive_name.clone().unwrap_or_default(),
        key: path.to_string_lossy().into_owned(),
        path,
        archive_entry: (!manager.selected_folder.is_empty()).then(|| manager.selected_folder.clone()),
        password: manager.archive_password.clone(),
        is_directory: true,
    });
    let folder_drop_target = manager.current_directory.clone();
    let folder_drop_enabled = manager.archive_drop_enabled();
    let table_id = if pane_index == 0 { "archive-table" } else { "archive-table-secondary" };
    let table_selector = table_id;
    let scroll_id = if pane_index == 0 { "file-scroll-viewport" } else { "file-scroll-viewport-secondary" };
    let scrollbar_id = if pane_index == 0 { "file-list-scrollbar" } else { "file-list-scrollbar-secondary" };
    let table = gpui::base::Table::new(table_id)
        .debug_selector(move || table_selector.to_owned())
        .flex()
        .flex_col()
        .flex_1()
        .w_full()
        .min_h(px(0.0))
        .overflow_hidden()
        .child(
            gpui::base::TableRow::new(format!("archive-table-header-{pane_index}"), 0)
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
                .child(sort_header(
                    language.text("NAME"), None, SortColumn::Name,
                    manager.sort_column, manager.sort_ascending, pane_index, theme, cx,
                ))
                .child(sort_header(
                    language.text("TYPE"), Some(130.0), SortColumn::Type,
                    manager.sort_column, manager.sort_ascending, pane_index, theme, cx,
                ))
                .child(sort_header(
                    language.text("SIZE"), Some(90.0), SortColumn::Size,
                    manager.sort_column, manager.sort_ascending, pane_index, theme, cx,
                )),
        )
        .child(
            div()
                .id(scroll_id)
                .w_full()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .overflow_hidden()
                .debug_selector(move || scroll_id.to_owned())
                .when_some(drop_target, |area, target| {
                    drop_service::archive_drop_area(
                        area,
                        target,
                        theme,
                        manager.archive_drop_enabled(),
                        cx,
                    )
                })
                .when_some(folder_drop_target, |area, directory| {
                    let internal_hover = directory.clone();
                    let external_hover = directory.clone();
                    let internal_target = directory.clone();
                    let external_target = directory;
                    let internal_feedback_view = folder_drop_view.clone();
                    let external_feedback_view = folder_drop_view.clone();
                    let internal_feedback_scroll = folder_drop_scroll.clone();
                    let external_feedback_scroll = folder_drop_scroll.clone();
                    area.drag_over::<drop_service::ArchiveFileDrag>(move |style, drag, window, cx| {
                        let accepted = folder_drop_enabled
                            && crate::transfer::can_move_to_directory(
                                &drag.target.path,
                                &internal_hover,
                            );
                        let show_last_marker = accepted
                            .then(|| last_visible_drop_row(
                                &internal_feedback_scroll,
                                folder_drop_row_count,
                                window.mouse_position().y,
                            ))
                            .flatten();
                        let _ = internal_feedback_view.update(cx, |manager, cx| {
                            let _ = manager.with_browser_tab(folder_drop_tab_id, |manager| {
                                if manager.drop_marker_row != show_last_marker {
                                    manager.drop_marker_row = show_last_marker;
                                    cx.notify();
                                }
                            });
                        });
                        style
                            .bg(gpui::rgba(if accepted {
                                (theme.accent << 8) | 0x42
                            } else {
                                0xef44442a
                            }))
                    })
                    .drag_over::<gpui::ExternalPaths>(move |style, paths, window, cx| {
                        let accepted = folder_drop_enabled
                            && paths.paths().len() == 1
                            && crate::transfer::can_move_to_directory(
                                &paths.paths()[0],
                                &external_hover,
                            );
                        let show_last_marker = accepted
                            .then(|| last_visible_drop_row(
                                &external_feedback_scroll,
                                folder_drop_row_count,
                                window.mouse_position().y,
                            ))
                            .flatten();
                        let _ = external_feedback_view.update(cx, |manager, cx| {
                            let _ = manager.with_browser_tab(folder_drop_tab_id, |manager| {
                                if manager.drop_marker_row != show_last_marker {
                                    manager.drop_marker_row = show_last_marker;
                                    cx.notify();
                                }
                            });
                        });
                        style
                            .bg(gpui::rgba(if accepted {
                                (theme.accent << 8) | 0x42
                            } else {
                                0xef44442a
                            }))
                    })
                    .on_drop(cx.listener(
                        move |manager, drag: &drop_service::ArchiveFileDrag, window, cx| {
                            cx.stop_propagation();
                            manager.active_browser = pane_index;
                            manager.inspector_focus = false;
                            manager.drop_marker_row = None;
                            manager.receive_folder_drop(
                                &internal_target,
                                std::slice::from_ref(&drag.target.path),
                                window,
                                cx,
                            );
                        },
                    ))
                    .on_drop(cx.listener(
                        move |manager, paths: &gpui::ExternalPaths, window, cx| {
                            cx.stop_propagation();
                            manager.active_browser = pane_index;
                            manager.inspector_focus = false;
                            manager.drop_marker_row = None;
                            manager.receive_folder_drop(&external_target, paths.paths(), window, cx);
                        },
                    ))
                })
                .child(file_list)
                .child(
                    gpui::component::scroll::Scrollbar::vertical(&manager.file_scroll)
                        .id(scrollbar_id)
                        .mode(gpui::component::scroll::ScrollbarMode::Always),
                ),
        );
    let activate_left = cx.listener(move |this, _, _, cx| {
        if this.active_browser != pane_index || this.inspector_focus {
            this.active_browser = pane_index;
            this.inspector_focus = false;
            cx.notify();
        }
    });
    let activate_right = cx.listener(move |this, _, _, cx| {
        if this.active_browser != pane_index || this.inspector_focus {
            this.active_browser = pane_index;
            this.inspector_focus = false;
            cx.notify();
        }
    });
    let panel_id = if pane_index == 0 { "browser-pane" } else { "browser-pane-secondary" };
    let panel_selector = panel_id;
    let panel = div()
        .id(panel_id)
        .debug_selector(move || panel_selector.to_owned())
        .on_mouse_down(gpui::MouseButton::Left, activate_left)
        .on_mouse_down(gpui::MouseButton::Right, activate_right)
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.0))
        .min_h(px(0.0))
        .bg(rgb(theme.surface))
        .border_1()
        .border_color(rgb(theme.border))
        .when(focused, |this| {
            this.border_color(gpui::rgba((theme.accent << 8) | 0x60))
        })
        .child(tab_bar)
        .child(table);
    manager.active_browser = previous_browser;
    panel
}

/// Renders a sidebar section title and an optional item count.
fn sidebar_heading(label: &'static str, count: Option<usize>, theme: Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .h(px(30.0))
        .px_3()
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgb(theme.muted_text))
        .child(label)
        .when_some(count, |this, count| {
            this.child(
                div()
                    .text_color(rgb(theme.faint_text))
                    .child(count.to_string()),
            )
        })
}

/// Renders one archive or filesystem folder row with selection and navigation behavior.
fn folder_row(
    label: &str,
    path: &str,
    depth: usize,
    selected_folder: &str,
    selected_entry: Option<&str>,
    current_directory: Option<PathBuf>,
    theme: Theme,
    drop_enabled: bool,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let selected = path == selected_folder || selected_entry == Some(path);
    let path = path.to_owned();
    let drop_directory = current_directory
        .as_ref()
        .map(|directory| directory.join(&path));
    let item = gpui::component::sidebar::SidebarMenuItem::new(label.to_owned())
        .icon(
            gpui::component::Icon::default()
                .path("icons/folder.svg")
                .text_color(rgb(theme.accent)),
        )
        .active(selected)
        .h(px(28.0))
        .pl(px(8.0 + depth as f32 * 14.0))
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            if let Some(directory) = &current_directory {
                if !path.is_empty() {
                    if event.click_count() == 2 {
                        this.navigate_to(directory.join(&path), cx);
                    } else {
                        this.selected_entry = Some(path.clone());
                        cx.notify();
                    }
                }
            } else if event.click_count() == 2 {
                this.selected_folder = path.clone();
                this.selected_entry = None;
                this.status_message = None;
                cx.notify();
            } else {
                this.selected_entry = Some(path.clone());
                cx.notify();
            }
        }));
    let item = ui::NavigationItem {
        id: format!("folder:{label}:{depth}").into(),
        item,
    };
    div()
        .id(gpui::SharedString::from(format!(
            "folder-drop:{label}:{depth}"
        )))
        .w_full()
        .min_w(px(0.0))
        .child(item)
        .when_some(drop_directory, |area, directory| {
            drop_service::folder_drop_area(area, directory, theme, drop_enabled, cx)
        })
}

/// Renders a Documents/Home shortcut and disables it when its path is unavailable.
fn location_shortcut(
    label: &'static str,
    path: Option<PathBuf>,
    current_directory: Option<&Path>,
    theme: Theme,
    drop_enabled: bool,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let is_available = path.is_some();
    let is_home = path == dirs::home_dir();
    let selected = is_available && path.as_deref() == current_directory;
    let drop_directory = path.clone();
    let item = gpui::component::sidebar::SidebarMenuItem::new(label)
        .icon(
            gpui::component::Icon::default()
                .path(if is_home {
                    "icons/home.svg"
                } else {
                    "icons/documents.svg"
                })
                .text_color(rgb(if is_available {
                    theme.accent
                } else {
                    theme.faint_text
                })),
        )
        .active(selected)
        .h(px(28.0))
        .when(is_available, |this| {
            this.on_click(cx.listener(move |this, _, _, cx| {
                if let Some(path) = &path {
                    this.navigate_to(path.clone(), cx);
                }
            }))
        });
    let item = ui::NavigationItem {
        id: (if is_home {
            "place-home"
        } else {
            "place-documents"
        })
        .into(),
        item,
    };
    div()
        .id(if is_home {
            "place-drop-home"
        } else {
            "place-drop-documents"
        })
        .w_full()
        .min_w(px(0.0))
        .child(item)
        .when_some(drop_directory, |area, directory| {
            drop_service::folder_drop_area(area, directory, theme, drop_enabled, cx)
        })
}

/// Renders a clickable table header that updates the active sort column and direction.
fn sort_header(
    label: &'static str,
    width: Option<f32>,
    column: SortColumn,
    active_column: SortColumn,
    ascending: bool,
    pane_index: usize,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let active = column == active_column;
    let base_id = match column {
        SortColumn::Name => "sort-name",
        SortColumn::Type => "sort-type",
        SortColumn::Size => "sort-size",
    };
    let id = if pane_index == 0 {
        base_id
    } else {
        match column {
            SortColumn::Name => "sort-name-secondary",
            SortColumn::Type => "sort-type-secondary",
            SortColumn::Size => "sort-size-secondary",
        }
    };
    let column_index = match column {
        SortColumn::Name => 0,
        SortColumn::Type => 1,
        SortColumn::Size => 2,
    };
    let header = gpui::base::TableHead::new(id, column_index)
        .debug_selector(move || id.to_owned())
        .flex()
        .items_center()
        .h_full()
        .min_w(px(0.0))
        .gap_1()
        .text_size(px(10.0))
        .text_color(rgb(if column == active_column {
            theme.text
        } else {
            theme.muted_text
        }))
        .child(label)
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(9.0))
                .h(px(9.0))
                .flex_shrink_0()
                .when(active, |this| {
                    this.child(app_icon(
                        if ascending {
                            "chevron-up"
                        } else {
                            "chevron-down"
                        },
                        9.0,
                        theme.muted_text,
                    ))
                }),
        );
    let header = if let Some(width) = width {
        header.w(px(width)).flex_shrink_0().pl_2()
    } else {
        header.flex_1().pl_2()
    };

    header
        .hover(|style| style.cursor_pointer().bg(rgb(theme.hover)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.sort_by(column, cx);
        }))
}

fn search_results_skeleton(theme: Theme) -> impl IntoElement {
    div()
        .id("search-results-skeleton")
        .debug_selector(|| "search-results-skeleton".to_owned())
        .flex()
        .flex_col()
        .w_full()
        .children((0..10).map(|index| {
            gpui::base::TableRow::new(format!("search-loading-row-{index}"), index)
                .debug_selector(move || format!("search-loading-row-{index}"))
                .flex()
                .items_center()
                .w_full()
                .h(px(28.0))
                .flex_shrink_0()
                .px_2()
                .border_b_1()
                .border_color(rgb(theme.subtle_border))
                .bg(rgb(if index % 2 == 1 {
                    theme.sidebar
                } else {
                    theme.surface
                }))
                .child(
                    gpui::base::TableCell::new("search-loading-name", 0)
                        .flex()
                        .flex_1()
                        .min_w(px(0.0))
                        .items_center()
                        .gap_2()
                        .overflow_hidden()
                        .child(inspector::skeleton_bar(theme, 16.0, 16.0))
                        .child(inspector::skeleton_bar(
                            theme,
                            164.0 + (index % 4) as f32 * 24.0,
                            9.0,
                        )),
                )
                .child(
                    gpui::base::TableCell::new("search-loading-type", 1)
                        .w(px(130.0))
                        .flex_shrink_0()
                        .pl_2()
                        .child(inspector::skeleton_bar(
                            theme,
                            36.0 + (index % 3) as f32 * 8.0,
                            9.0,
                        )),
                )
                .child(
                    gpui::base::TableCell::new("search-loading-size", 2)
                        .w(px(90.0))
                        .flex_shrink_0()
                        .pl_2()
                        .child(inspector::skeleton_bar(
                            theme,
                            44.0 + (index % 2) as f32 * 8.0,
                            9.0,
                        )),
                )
        }))
}

/// Renders a file-list row with preview, open, selection, and context-menu actions.
fn file_row(
    entry: &ArchiveEntry,
    archive_path: Option<&Path>,
    current_directory: Option<&Path>,
    archive_password: Option<password::ArchivePassword>,
    selected_folder: &str,
    selected_entry: Option<&str>,
    language: Language,
    folder_size_known: bool,
    row_index: usize,
    pane_index: usize,
    show_bottom_drop_border: bool,
    theme: Theme,
    drop_enabled: bool,
    search_target: Option<file_actions::EntryTarget>,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let (icon, kind) = if entry.is_directory {
        ("folder", language.text("Folder").to_owned())
    } else {
        let extension = entry
            .name
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_uppercase())
            .unwrap_or_default();
        (entry_icon(entry), extension)
    };

    let entry_name = entry.source_path.clone().unwrap_or_else(|| {
        if selected_folder.is_empty() {
            entry.name.clone()
        } else {
            format!("{selected_folder}/{}", entry.name)
        }
    });
    let selected = selected_entry == Some(entry_name.as_str());
    let is_directory = entry.is_directory;
    let target = search_target.unwrap_or_else(|| file_actions::EntryTarget {
        name: entry.name.clone(),
        key: entry_name.clone(),
        is_directory,
        path: current_directory
            .map(|directory| directory.join(entry.source_path.as_deref().unwrap_or(&entry.name)))
            .or_else(|| archive_path.map(Path::to_path_buf))
            .unwrap_or_default(),
        archive_entry: current_directory.is_none().then(|| entry_name.clone()),
        password: archive_password,
    });
    let click_target = target.clone();
    let drop_target = target.clone();
    let drag_target = target.clone();
    let can_drag = target.archive_entry.is_none() && drop_enabled;
    let drop_to_directory = current_directory
        .map(Path::to_path_buf)
        .filter(|_| {
            target.archive_entry.is_none()
                && !target.is_directory
                && !archive::is_supported_archive(&target.path)
        });
    let adds_to_archive = (!target.is_directory
        && target.archive_entry.is_none()
        && archive::is_supported_archive(&target.path))
        || (target.is_directory && target.archive_entry.is_some());
    let archive_feedback_color = drop_service::archive_drop_feedback_color(theme);
    let drop_candidate = drop_to_directory.is_some()
        || (target.is_directory && target.archive_entry.is_none())
        || (archive::is_supported_archive(&target.path)
            && (target.archive_entry.is_none() || target.is_directory));
    let menu_view = cx.entity().downgrade();
    let striped = row_index % 2 == 1;
    gpui::base::TableRow::new(gpui::SharedString::from(entry_name.clone()), row_index)
        .debug_selector({
            let name = entry_name.clone();
            let prefix = if pane_index == 0 { "file" } else { "file-secondary" };
            move || format!("{prefix}:{name}")
        })
        .flex()
        .items_center()
        .w_full()
        .h(px(28.0))
        .flex_shrink_0()
        .px_2()
        .relative()
        .border_b_1()
        .border_color(rgb(if show_bottom_drop_border { theme.accent } else { theme.subtle_border }))
        .text_xs()
        .cursor_pointer()
        .bg(rgb(if striped {
            theme.sidebar
        } else {
            theme.surface
        }))
        .when(selected, |this| this.bg(rgb(theme.selection)))
        .when(!selected, |this| {
            this.hover(|style| style.bg(rgb(theme.hover)))
        })
        .when(can_drag, |row| {
            row.on_drag(
                drop_service::ArchiveFileDrag {
                    target: drag_target,
                    theme,
                },
                |drag, _, _, cx| cx.new(|_| drag.clone()),
            )
        })
        .when(drop_candidate, |row| {
            let hover_target = drop_target.clone();
            let external_hover = drop_target.clone();
            let external_target = drop_target.clone();
            let internal_directory = drop_to_directory.clone();
            let external_directory = drop_to_directory.clone();
            let internal_destination = drop_to_directory.clone();
            let external_destination = drop_to_directory.clone();
            row.drag_over::<drop_service::ArchiveFileDrag>(move |style, drag, _, _| {
                let sources = std::slice::from_ref(&drag.target.path);
                let accepted = drop_enabled
                    && if let Some(directory) = &internal_directory {
                        sources.len() == 1
                            && crate::transfer::can_move_to_directory(&sources[0], directory)
                    } else {
                        drop_service::accepts_entry_drop(&hover_target, sources)
                    };
                let feedback_color = if adds_to_archive { archive_feedback_color } else { theme.accent };
                style
                    .bg(gpui::rgba(if accepted {
                        (feedback_color << 8) | 0x2a
                    } else {
                        0xef44441a
                    }))
                    .border_color(rgb(if accepted { feedback_color } else { 0xef4444 }))
            })
            .drag_over::<gpui::ExternalPaths>(move |style, paths, _, _| {
                let accepted = drop_enabled
                    && if let Some(directory) = &external_directory {
                        paths.paths().len() == 1
                            && crate::transfer::can_move_to_directory(
                                &paths.paths()[0],
                                directory,
                            )
                    } else {
                        drop_service::accepts_entry_drop(&external_hover, paths.paths())
                    };
                let feedback_color = if adds_to_archive { archive_feedback_color } else { theme.accent };
                style
                    .bg(gpui::rgba(if accepted {
                        (feedback_color << 8) | 0x2a
                    } else {
                        0xef44441a
                    }))
                    .border_color(rgb(if accepted { feedback_color } else { 0xef4444 }))
            })
            .on_drop(cx.listener(
                move |manager, drag: &drop_service::ArchiveFileDrag, window, cx| {
                    cx.stop_propagation();
                    manager.active_browser = pane_index;
                    manager.inspector_focus = false;
                    if let Some(directory) = &internal_destination {
                        manager.receive_folder_drop(
                            directory,
                            std::slice::from_ref(&drag.target.path),
                            window,
                            cx,
                        );
                    } else {
                        manager.receive_entry_drop(
                            &drop_target,
                            std::slice::from_ref(&drag.target.path),
                            window,
                            cx,
                        );
                    }
                },
            ))
            .on_drop(cx.listener(
                move |manager, paths: &gpui::ExternalPaths, window, cx| {
                    cx.stop_propagation();
                    manager.active_browser = pane_index;
                    manager.inspector_focus = false;
                    if let Some(directory) = &external_destination {
                        manager.receive_folder_drop(directory, paths.paths(), window, cx);
                    } else {
                        manager.receive_entry_drop(&external_target, paths.paths(), window, cx);
                    }
                },
            ))
        })
        .child(
            gpui::base::TableCell::new("entry-name", 0)
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .items_center()
                .gap_2()
                .child(app_icon(
                    icon,
                    16.0,
                    if is_directory || selected {
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
                        .text_color(rgb(theme.text))
                        .child(entry.name.clone()),
                ),
        )
        .child(
            gpui::base::TableCell::new("entry-type", 1)
                .w(px(130.0))
                .flex_shrink_0()
                .pl_2()
                .text_color(rgb(theme.muted_text))
                .child(kind),
        )
        .child(
            gpui::base::TableCell::new("entry-size", 2)
                .w(px(90.0))
                .flex_shrink_0()
                .pl_2()
                .text_color(rgb(theme.muted_text))
                .child(if entry.is_directory && !folder_size_known {
                    "-".to_owned()
                } else {
                    format_size(entry.size)
                }),
        )
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            if event.click_count() != 2 {
                this.selected_entry = Some(click_target.key.clone());
                window.focus(&this.content_focus, cx);
                if this.inspector.visible {
                    this.file_scroll
                        .scroll_to_item(row_index, gpui::ScrollStrategy::Nearest);
                }
                cx.notify();
                return;
            }
            this.open_entry(&click_target, cx);
        }))
        .context_menu(move |menu, _, cx| {
            let busy = menu_view
                .update(cx, |this, cx| {
                    if this.selected_entry.as_deref() != Some(target.key.as_str()) {
                        this.selected_entry = Some(target.key.clone());
                        cx.notify();
                    }
                    this.modal.is_some()
                })
                .unwrap_or(true);
            file_actions::menu(menu, &target, menu_view.clone(), language, busy, cx)
        })
}

/// Maps a filename extension to the corresponding bundled file-type icon.
pub(crate) fn file_icon(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "tbz" | "tbz2" | "xz" | "txz"
        | "zst" | "lz4" | "lz" | "br" | "cpio" | "ar" | "a" | "pax" | "xar" => "archive",
        "pdf" => "pdf",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "heic" => "image",
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" => "audio",
        "mp4" | "mov" | "mkv" | "avi" | "webm" => "video",
        "rs" | "c" | "h" | "cpp" | "py" | "js" | "ts" | "go" | "java" | "html" | "css" => "code",
        "txt" | "md" | "rtf" | "doc" | "docx" | "odt" => "document",
        "csv" => "csv",
        "xls" | "xlsx" | "ods" => "table",
        "ppt" | "pptx" | "odp" => "presentation",
        "bin" | "dat" | "exe" | "dll" | "so" | "dylib" | "class" | "o" | "obj" | "wasm" | "db"
        | "sqlite" | "sqlite3" | "iso" => "binary",
        _ => "file",
    }
}

pub(crate) fn entry_icon(entry: &ArchiveEntry) -> &'static str {
    let extension = entry
        .name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_uppercase())
        .unwrap_or_default();
    let icon = if archive::is_supported_archive(std::path::Path::new(&entry.name)) {
        "archive"
    } else {
        file_icon(&extension)
    };
    if entry.is_encrypted && icon == "archive" {
        "archive-locked"
    } else {
        icon
    }
}

/// Renders a registered application SVG icon at the requested size and color.
pub(crate) fn app_icon(name: &str, size: f32, color: u32) -> impl IntoElement {
    svg()
        .path(format!("icons/{name}.svg"))
        .w(px(size * icon_aspect_ratio(name)))
        .h(px(size))
        .text_color(rgb(color))
}

/// Returns the width-to-height ratio used to preserve an icon's source proportions.
pub(crate) fn icon_aspect_ratio(name: &str) -> f32 {
    match name {
        "arrow-down" | "arrow-up" | "document" | "image" | "audio" | "video" | "code" | "file"
        | "archive" | "archive-locked" => 0.75,
        "arrow-left" | "documents" => 0.875,
        "csv" => 547.8 / 548.0,
        "chevron-left" => 0.625,
        "home" => 1.125,
        "folder-open" => 1.125,
        _ => 1.0,
    }
}

/// Formats a byte count using binary units from bytes through tebibytes.
pub(crate) fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

    if bytes < 1024 {
        return format!("{bytes} B");
    }
    // every 10 bits corresponds to the next unit (2^10 = 1024).
    let unit = (((63 - bytes.leading_zeros()) / 10) as usize).min(UNITS.len() - 1);
    let shift = unit as u32 * 10;

    let whole = bytes >> shift;
    let decimal = ((bytes & ((1_u64 << shift) - 1)) * 10) >> shift;

    format!("{whole}.{decimal} {}", UNITS[unit])
}

/// Detects whether a localized status message begins with a known error label.
fn status_message_is_error(message: &str) -> bool {
    [
        "Could not calculate all folder sizes",
        "Could not save settings",
        "Dropped item is unavailable",
        "Failed to create archive",
        "Failed to create PAR2 recovery files",
        "Failed to extract",
        "Failed to add to archive",
        "Failed to remove entry from archive",
        "Failed to move item",
        "Failed to move to Trash",
        "Failed to open file",
        "Failed to open",
        "Unable to open archive",
    ]
    .into_iter()
    .any(|key| {
        Language::ALL
            .into_iter()
            .any(|language| message.starts_with(language.text(key)))
    })
}
