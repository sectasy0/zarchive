//! Builds application dialogs and the settings, about, and deletion views.

use super::*;
use gpui::{RenderImage, img};
use gpui::component::{input::Input, scroll::ScrollableElement};
use gpui::base::StyledExt as _;
use std::sync::{Arc, OnceLock};

impl ArchiveManager {
    fn receive_default_location(
        &mut self,
        picker: impl std::future::Future<Output = Option<PathBuf>> + 'static,
        preferences_path: Option<PathBuf>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        let executor = cx.background_executor().clone();
        window
            .spawn(cx, async move |cx| {
                let Some(path) = picker.await else {
                    return;
                };
                loop {
                    let delivered = cx.update(|_, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.default_location = Some(path.clone());
                            if let Some(preferences_path) = &preferences_path {
                                this.save_preferences_to(preferences_path);
                            }
                            cx.notify();
                        });
                    });
                    match delivered {
                        Ok(()) => break,
                        Err(error) if error.is::<std::cell::BorrowMutError>() => {
                            executor.timer(std::time::Duration::from_millis(16)).await;
                        }
                        Err(_) => break,
                    }
                }
            })
            .detach();
    }
}

/// Loads and caches the application icon used by the About dialog.
fn about_app_icon() -> impl IntoElement {
    static ICON: OnceLock<Arc<RenderImage>> = OnceLock::new();
    let image = ICON
        .get_or_init(|| {
            let mut image =
                image::load_from_memory(include_bytes!("../assets/app_icons/icon_folder_64.png"))
                    .expect("application icon is a valid PNG")
                    .into_rgba8();
            for pixel in image.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            Arc::new(RenderImage::new(vec![image::Frame::new(image)]))
        })
        .clone();

    div()
        .id("about-app-icon")
        .debug_selector(|| "about-app-icon".to_owned())
        .w(px(56.0))
        .h(px(56.0))
        .flex_shrink_0()
        .child(img(image).size_full().object_fit(gpui::ObjectFit::Fill))
}

fn setting_option(id: &'static str, label: &'static str, selected: bool, theme: Theme) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .selected(selected)
        .label(label)
        .text_color(rgb(if selected {
            theme.soft_accent()
        } else {
            theme.muted_text
        }))
        .when(selected, |this| this.bg(rgb(theme.selection)))
        .flex_1()
}

fn open_picker_view(
    manager: &ArchiveManager,
    theme: Theme,
    window: &Window,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let picker = manager.file_actions.open_picker.as_ref().expect("Open picker is initialized");
    let directory = picker.directory.clone();
    let selected = picker.selected.clone();
    let search = picker.search_input.read(cx).value().trim().to_lowercase();
    let entries = picker.entries.clone();
    let matching_indices = picker.matching_indices.clone();
    let count = matching_indices.len();
    let language = manager.language;
    let column_widths = manager.column_widths;
    let active_directory = picker.directory.clone();
    let place_row = |id: String, label: String, icon: &'static str, path: PathBuf| {
        let selected = path == active_directory;
        let selector = id.clone();
        Button::new(gpui::SharedString::from(id))
            .debug_selector(move || selector.clone())
            .ghost()
            .small()
            .selected(selected)
            .w_full()
            .h(px(28.0))
            .px_2()
            .rounded_sm()
            .text_sm()
            .text_color(rgb(if selected { theme.text } else { theme.muted_text }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .min_w(px(0.0))
                    .child(crate::app_icon(icon, 14.0, theme.soft_accent()))
                    .child(div().flex_1().min_w(px(0.0)).truncate().text_left().child(label)),
            )
            .on_click(cx.listener(move |manager, _, window, cx| {
                manager.load_open_picker_directory(path.clone(), window, cx);
            }))
    };
    let header = || gpui::base::TableRow::new("open-picker-header", 0)
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
            gpui::base::TableHead::new("open-picker-name-head", 0)
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .pl_2()
                .child(language.text("NAME")),
        )
        .child(
            gpui::base::TableHead::new("open-picker-type-head", 1)
                .w(px(column_widths[0]))
                .flex_shrink_0()
                .pl_2()
                .child(language.text("TYPE")),
        )
        .child(
            gpui::base::TableHead::new("open-picker-size-head", 2)
                .w(px(column_widths[1]))
                .flex_shrink_0()
                .pl_2()
                .child(language.text("SIZE")),
        )
        .child(
            gpui::base::TableHead::new("open-picker-modified-head", 3)
                .w(px(column_widths[2]))
                .flex_shrink_0()
                .pl_2()
                .child(language.text("MODIFIED")),
        );
    let list = gpui::uniform_list(
        "open-picker-rows",
        count,
        cx.processor(move |_manager, range: std::ops::Range<usize>, _, cx| {
            range
                .filter_map(|row_index| {
                    let entry_index = *matching_indices.get(row_index)?;
                    let entry = entries.get(entry_index)?;
                    let name = entry.name.clone();
                    let path = directory.join(entry.source_path.as_deref().unwrap_or(&name));
                    let is_directory = entry.is_directory;
                    let is_selected = selected.as_ref() == Some(&path);
                    let icon = if is_directory {
                        "folder"
                    } else {
                        crate::app::entry_icon(entry)
                    };
                    let type_label = if is_directory {
                        language.text("Folder").to_owned()
                    } else {
                        name.rsplit_once('.')
                            .map(|(_, extension)| extension.to_ascii_uppercase())
                            .unwrap_or_else(|| language.text("File").to_owned())
                    };
                    let size = crate::format_size(entry.size);
                    let modified = entry
                        .modified
                        .map_or_else(|| "—".to_owned(), |date| date.format("%Y-%m-%d").to_string());
                    let click_path = path.clone();
                    let activate_path = path.clone();
                    Some(
                        gpui::base::TableRow::new(("open-picker-row", row_index), row_index)
                            .debug_selector(move || format!("open-picker-entry:{row_index}"))
                            .flex()
                            .items_center()
                            .h(px(28.0))
                            .w_full()
                            .flex_shrink_0()
                            .px_2()
                            .border_b_1()
                            .border_color(rgb(theme.subtle_border))
                            .cursor_pointer()
                            .bg(rgb(theme.surface))
                            .when(is_selected, |this| this.bg(rgb(theme.selection)))
                            .when(!is_selected, |this| this.hover(|style| style.bg(rgb(theme.hover))))
                            .on_click(cx.listener(move |manager, event: &gpui::ClickEvent, window, cx| {
                                if event.click_count() == 2 {
                                    manager.activate_open_picker_entry(
                                        activate_path.clone(),
                                        window,
                                        cx,
                                    );
                                } else {
                                    manager.select_open_picker_entry(click_path.clone(), cx);
                                }
                            }))
                            .child(
                                gpui::base::TableCell::new("open-picker-name-cell", 0)
                                    .flex()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .items_center()
                                    .gap_2()
                                    .child(crate::app_icon(
                                        icon,
                                        16.0,
                                        if is_directory || is_selected { theme.accent } else { theme.icon },
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .truncate()
                                            .text_color(rgb(theme.text))
                                            .child(name),
                                    ),
                            )
                            .child(
                                gpui::base::TableCell::new("open-picker-type-cell", 1)
                                    .w(px(column_widths[0]))
                                    .flex_shrink_0()
                                    .pl_2()
                                    .truncate()
                                    .text_color(rgb(theme.muted_text))
                                    .child(type_label),
                            )
                            .child(
                                gpui::base::TableCell::new("open-picker-size-cell", 2)
                                    .w(px(column_widths[1]))
                                    .flex_shrink_0()
                                    .pl_2()
                                    .text_color(rgb(theme.muted_text))
                                    .child(if is_directory { "—".to_owned() } else { size }),
                            )
                            .child(
                                gpui::base::TableCell::new("open-picker-modified-cell", 3)
                                    .w(px(column_widths[2]))
                                    .flex_shrink_0()
                                    .pl_2()
                                    .text_color(rgb(theme.muted_text))
                                    .child(modified),
                            ),
                    )
                })
                .collect()
        }),
    )
    .track_scroll(&picker.scroll)
    .flex()
    .flex_col()
    .flex_1()
    .min_h(px(0.0));
    let can_go_back = picker.history_index > 0;
    let can_go_forward = picker.history_index + 1 < picker.history.len();
    let picker_height = (window.viewport_size().height - px(160.0))
        .min(px(500.0))
        .max(px(220.0));
    let mut places_content = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_1()
        .py_2()
        .child(
            div()
                .px_2()
                .py_1()
                .text_xs()
                .font_semibold()
                .text_color(rgb(theme.muted_text))
                .child(language.text("PLACES")),
        );
    for (id, label, icon, path) in [
        ("open-picker-home", "Home", "home", dirs::home_dir()),
        ("open-picker-desktop", "Desktop", "folder", dirs::desktop_dir()),
        ("open-picker-documents", "Documents", "documents", dirs::document_dir()),
        ("open-picker-downloads", "Downloads", "downloads", dirs::download_dir()),
    ] {
        if let Some(path) = path.filter(|path| path.is_dir()) {
            places_content = places_content.child(place_row(
                id.to_owned(),
                language.text(label).to_owned(),
                icon,
                path,
            ));
        }
    }
    let favorites: Vec<_> = manager
        .favorite_folders
        .iter()
        .filter(|path| path.is_dir())
        .cloned()
        .collect();
    if !favorites.is_empty() {
        places_content = places_content.child(
            div()
                .mt_2()
                .px_2()
                .py_1()
                .text_xs()
                .font_semibold()
                .text_color(rgb(theme.muted_text))
                .child(language.text("FAVORITES")),
        );
        for (index, path) in favorites.iter().take(6).enumerate() {
            let label = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
            places_content = places_content.child(place_row(
                format!("open-picker-favorite-{index}"),
                label,
                "folder",
                path.clone(),
            ));
        }
    }
    let recent: Vec<_> = manager
        .recent_folders
        .iter()
        .filter(|path| path.is_dir() && **path != active_directory)
        .cloned()
        .collect();
    if !recent.is_empty() {
        places_content = places_content.child(
            div()
                .mt_2()
                .px_2()
                .py_1()
                .text_xs()
                .font_semibold()
                .text_color(rgb(theme.muted_text))
                .child(language.text("RECENT FOLDERS")),
        );
        for (index, path) in recent.iter().take(6).enumerate() {
            let label = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
            places_content = places_content.child(place_row(
                format!("open-picker-recent-{index}"),
                label,
                "folder",
                path.clone(),
            ));
        }
    }
    let places_content = places_content
        .overflow_y_scrollbar()
        .id("open-picker-places-scroll")
        .flex_1()
        .min_h(px(0.0));
    let places = div()
        .id("open-picker-places")
        .debug_selector(|| "open-picker-places".to_owned())
        .relative()
        .flex()
        .flex_col()
        .w(px(176.0))
        .flex_shrink_0()
        .min_h(px(0.0))
        .overflow_hidden()
        .bg(rgb(theme.surface))
        .border_r_1()
        .border_color(rgb(theme.subtle_border))
        .child(places_content);
    let loading_table = gpui::base::Table::new("open-picker-table")
        .debug_selector(|| "open-picker-table".to_owned())
        .flex()
        .flex_col()
        .flex_1()
        .w_full()
        .min_h(px(0.0))
        .overflow_hidden()
        .border_1()
        .border_color(rgb(theme.border))
        .child(header())
        .child(
            gpui::base::TableBody::new("open-picker-loading-body")
                .flex()
                .flex_col()
                .flex_1()
                .w_full()
                .min_h(px(0.0))
                .overflow_hidden()
                .child(
                    div()
                        .id("open-picker-loading")
                        .debug_selector(|| "open-picker-loading".to_owned())
                        .flex()
                        .flex_col()
                        .flex_1()
                        .w_full()
                        .children((0..12).map(|index| {
                            gpui::base::TableRow::new(("open-picker-loading-row", index), index)
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
                                gpui::base::TableCell::new("open-picker-loading-name", 0)
                                    .flex()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .items_center()
                                    .gap_2()
                                    .child(crate::inspector::skeleton_bar(theme, 16.0, 16.0))
                                    .child(crate::inspector::skeleton_bar(
                                        theme,
                                        148.0 + (index % 4) as f32 * 24.0,
                                        9.0,
                                    )),
                            )
                            .child(
                                gpui::base::TableCell::new("open-picker-loading-type", 1)
                                    .w(px(column_widths[0]))
                                    .flex_shrink_0()
                                    .pl_2()
                                    .child(crate::inspector::skeleton_bar(
                                        theme,
                                        40.0 + (index % 3) as f32 * 8.0,
                                        9.0,
                                    )),
                            )
                            .child(
                                gpui::base::TableCell::new("open-picker-loading-size", 2)
                                    .w(px(column_widths[1]))
                                    .flex_shrink_0()
                                    .pl_2()
                                    .child(crate::inspector::skeleton_bar(
                                        theme,
                                        44.0 + (index % 2) as f32 * 8.0,
                                        9.0,
                                    )),
                            )
                            .child(
                                gpui::base::TableCell::new("open-picker-loading-modified", 3)
                                    .w(px(column_widths[2]))
                                    .flex_shrink_0()
                                    .pl_2()
                                    .child(crate::inspector::skeleton_bar(theme, 80.0, 9.0)),
                            )
                        })),
                ),
        );
    let table_area = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.0))
        .min_h(px(0.0))
        .child(if picker.loading {
            loading_table.into_any_element()
        } else if count == 0 {
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(rgb(theme.muted_text))
                .child(language.text(if search.is_empty() {
                    "This folder is empty"
                } else {
                    "No matching items"
                }))
                .into_any_element()
        } else {
            gpui::base::Table::new("open-picker-table")
                .debug_selector(|| "open-picker-table".to_owned())
                .flex()
                .flex_col()
                .flex_1()
                .w_full()
                .min_h(px(0.0))
                .overflow_hidden()
                .border_1()
                .border_color(rgb(theme.border))
                .child(header())
                .child(
                    gpui::base::TableBody::new("open-picker-body")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .w_full()
                        .min_h(px(0.0))
                        .overflow_hidden()
                        .child(list)
                        .child(
                            gpui::component::scroll::Scrollbar::vertical(&picker.scroll)
                                .id("open-picker-scrollbar")
                                .mode(gpui::component::scroll::ScrollbarMode::Always),
                        ),
                )
                .into_any_element()
        });
    div()
        .id("open-picker")
        .debug_selector(|| "open-picker".to_owned())
        .flex()
        .flex_col()
        .gap_0()
        .w_full()
        .h(picker_height)
        .bg(rgb(theme.surface))
        .px_0()
        .child(
            div()
                .id("open-picker-toolbar")
                .debug_selector(|| "open-picker-toolbar".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .w_full()
                .h(px(48.0))
                .flex_shrink_0()
                .px_2()
                .border_b_1()
                .border_color(rgb(theme.subtle_border))
                .bg(rgb(theme.sidebar))
                .child(
                    div()
                        .id("open-picker-navigation")
                        .debug_selector(|| "open-picker-navigation".to_owned())
                        .flex()
                        .items_center()
                        .gap_0()
                        .h(px(26.0))
                        .p(px(0.0))
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.surface))
                        .child(
                                    crate::toolbar::toolbar_icon_button(
                                        "open-picker-back",
                                        gpui::component::Icon::default().path("icons/chevron-left.svg"),
                                        manager.language.text("Back"),
                                        9.0,
                                        if can_go_back { theme.soft_accent() } else { theme.faint_text },
                                        false,
                                        theme,
                                        cx,
                                    )
                                        .w(px(25.0))
                                        .h(px(24.0))
                                .disabled(!can_go_back)
                                .on_click(cx.listener(|manager, _, window, cx| {
                                    manager.navigate_open_picker_history(-1, window, cx);
                                })),
                        )
                        .child(
                            crate::toolbar::toolbar_icon_button(
                                "open-picker-forward",
                                gpui::component::Icon::default().path("icons/chevron-right.svg"),
                                manager.language.text("Forward"),
                                9.0,
                                if can_go_forward { theme.soft_accent() } else { theme.faint_text },
                                false,
                                theme,
                                cx,
                            )
                                .w(px(25.0))
                                .h(px(24.0))
                                .disabled(!can_go_forward)
                                .on_click(cx.listener(|manager, _, window, cx| {
                                    manager.navigate_open_picker_history(1, window, cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .id("open-picker-path-bar")
                        .debug_selector(|| "open-picker-path-bar".to_owned())
                        .flex()
                        .flex_1()
                        .min_w(px(0.0))
                        .h(px(26.0))
                        .items_center()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.sidebar))
                        .px_2()
                        .child(
                            Input::new(&picker.path_input)
                                .id("open-picker-path-input")
                                .small()
                                .appearance(false)
                                .flex_1()
                                .min_w(px(0.0)),
                        ),
                )
                .child(
                    div()
                    .id("open-picker-search")
                        .debug_selector(|| "open-picker-search".to_owned())
                        .w(px(190.0))
                        .flex_shrink_0()
                        .h(px(26.0))
                        .items_center()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.sidebar))
                        .px_2()
                        .child(
                            Input::new(&picker.search_input)
                            .id("open-picker-search-input")
                            .small()
                            .w_full()
                            .appearance(false)
                            .prefix(crate::app_icon("search", 13.0, theme.muted_text)),
                        ),
                    ),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_h(px(0.0))
                .gap_0()
                .child(places)
                .child(table_area),
        )
}

fn open_picker_footer(
    manager: &ArchiveManager,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let picker = manager.file_actions.open_picker.as_ref().expect("Open picker is initialized");
    let new_tab = picker.new_tab;
    div()
        .id("open-picker-footer")
        .debug_selector(|| "open-picker-footer".to_owned())
        .w_full()
        .flex()
        .items_center()
        .justify_end()
        .h(px(48.0))
        .gap_2()
        .px_2()
        .bg(rgb(theme.sidebar))
        .child(
            Button::new("open-picker-cancel")
                .debug_selector(|| "open-picker-cancel".to_owned())
                .ghost()
                .small()
                .label(manager.language.text("Cancel"))
                .on_click(cx.listener(|manager, _, window, cx| manager.close_modal(window, cx))),
        )
        .child(
            Button::new("open-picker-confirm")
                .debug_selector(|| "open-picker-confirm".to_owned())
                .primary()
                .small()
                .label(manager.language.text(if new_tab {
                    "Open in New Tab"
                } else {
                    "Open"
                }))
                .on_click(cx.listener(|manager, _, window, cx| {
                    manager.confirm_open_picker(window, cx);
                })),
        )
}

/// Creates a navigation button for a Settings category.
fn settings_category_button(
    id: &'static str,
    label: &'static str,
    icon: &'static str,
    selected: bool,
    theme: Theme,
    cx: &App,
) -> Button {
    let color = if selected {
        theme.soft_accent()
    } else {
        theme.muted_text
    };
    Button::new(id)
        .custom(ui::sidebar_button_style(theme, selected, cx))
        .small()
        .selected(selected)
        .w_full()
        .h(px(34.0))
        .rounded_sm()
        .text_color(rgb(color))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .w_full()
                .child(
                    div()
                        .w(px(2.0))
                        .h(px(20.0))
                        .rounded_sm()
                        .bg(rgb(if selected { theme.accent } else { theme.sidebar })),
                )
                .child(app_icon(icon, 13.0, color))
                .child(div().flex_1().min_w(px(0.0)).text_left().child(label)),
        )
}

#[derive(Clone, Copy)]
enum AdvancedSettingField {
    PreviewLimit,
    CsvRows,
    CsvColumns,
    AuditDepth,
}

impl AdvancedSettingField {
    fn key(self) -> &'static str {
        match self {
            Self::PreviewLimit => "preview-limit",
            Self::CsvRows => "csv-row-limit",
            Self::CsvColumns => "csv-column-limit",
            Self::AuditDepth => "audit-depth",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::PreviewLimit => "Maximum preview size",
            Self::CsvRows => "CSV preview row limit",
            Self::CsvColumns => "CSV preview column limit",
            Self::AuditDepth => "Nested archive audit depth",
        }
    }
}

fn advanced_setting_bounds(
    manager: &ArchiveManager,
    field: AdvancedSettingField,
) -> (u32, u32, u32, &'static str) {
    match field {
        AdvancedSettingField::PreviewLimit => (
            manager.advanced_settings.preview_limit_kib,
            *AdvancedSettings::PREVIEW_LIMIT_RANGE.start(),
            *AdvancedSettings::PREVIEW_LIMIT_RANGE.end(),
            "KiB",
        ),
        AdvancedSettingField::CsvRows => (
            manager.advanced_settings.csv_max_rows,
            *AdvancedSettings::CSV_MAX_ROWS_RANGE.start(),
            *AdvancedSettings::CSV_MAX_ROWS_RANGE.end(),
            "",
        ),
        AdvancedSettingField::CsvColumns => (
            manager.advanced_settings.csv_max_columns,
            *AdvancedSettings::CSV_MAX_COLUMNS_RANGE.start(),
            *AdvancedSettings::CSV_MAX_COLUMNS_RANGE.end(),
            "",
        ),
        AdvancedSettingField::AuditDepth => (
            manager.advanced_settings.audit_max_depth,
            *AdvancedSettings::AUDIT_MAX_DEPTH_RANGE.start(),
            *AdvancedSettings::AUDIT_MAX_DEPTH_RANGE.end(),
            "",
        ),
    }
}

fn adjust_advanced_setting(
    manager: &mut ArchiveManager,
    field: AdvancedSettingField,
    increase: bool,
) {
    let (value, minimum, maximum, _) = advanced_setting_bounds(manager, field);
    let step = match field {
        AdvancedSettingField::PreviewLimit => 16,
        AdvancedSettingField::CsvRows => 500,
        AdvancedSettingField::CsvColumns => 8,
        AdvancedSettingField::AuditDepth => 1,
    };
    let value = if increase {
        value.saturating_add(step).min(maximum)
    } else {
        value.saturating_sub(step).max(minimum)
    };
    match field {
        AdvancedSettingField::PreviewLimit => manager.advanced_settings.preview_limit_kib = value,
        AdvancedSettingField::CsvRows => manager.advanced_settings.csv_max_rows = value,
        AdvancedSettingField::CsvColumns => manager.advanced_settings.csv_max_columns = value,
        AdvancedSettingField::AuditDepth => manager.advanced_settings.audit_max_depth = value,
    }
    manager.save_preferences();
}

fn advanced_setting_stepper(
    manager: &ArchiveManager,
    field: AdvancedSettingField,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    let (value, minimum, maximum, unit) = advanced_setting_bounds(manager, field);
    let key = field.key();
    div()
        .id(gpui::SharedString::from(format!("advanced-setting-{key}")))
        .debug_selector(move || format!("advanced-setting-{key}"))
        .flex()
        .items_center()
        .gap_3()
        .w_full()
        .min_h(px(48.0))
        .py_2()
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_sm()
                .child(language.text(field.label())),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .flex_shrink_0()
                .child(
                    Button::new(gpui::SharedString::from(format!("advanced-{key}-decrease")))
                        .ghost()
                        .xsmall()
                        .icon(gpui::assets::IconName::Minus)
                        .tooltip(language.text("Decrease"))
                        .accessibility_label(language.text("Decrease"))
                        .disabled(value <= minimum)
                        .on_click(cx.listener(move |manager, _, _, cx| {
                            adjust_advanced_setting(manager, field, false);
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .w(px(88.0))
                        .text_center()
                        .text_sm()
                        .child(if unit.is_empty() {
                            value.to_string()
                        } else {
                            format!("{value} {unit}")
                        }),
                )
                .child(
                    Button::new(gpui::SharedString::from(format!("advanced-{key}-increase")))
                        .ghost()
                        .xsmall()
                        .icon(gpui::assets::IconName::Plus)
                        .tooltip(language.text("Increase"))
                        .accessibility_label(language.text("Increase"))
                        .disabled(value >= maximum)
                        .on_click(cx.listener(move |manager, _, _, cx| {
                            adjust_advanced_setting(manager, field, true);
                            cx.notify();
                        })),
                ),
        )
}

fn folder_size_setting(
    manager: &ArchiveManager,
    theme: Theme,
    preferences_path: Option<PathBuf>,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let label = manager.language.text("Automatically calculate folder sizes");
    div()
        .id("auto-calculate-folder-sizes")
        .debug_selector(|| "auto-calculate-folder-sizes".to_owned())
        .flex()
        .items_center()
        .gap_3()
        .w_full()
        .min_h(px(48.0))
        .flex_shrink_0()
        .py_3()
        .border_t_1()
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
        .child(div().flex_1().min_w(px(0.0)).text_sm().child(label))
        .child(
            gpui::component::switch::Switch::new("auto-calculate-folder-sizes-switch")
                .small()
                .flex_shrink_0()
                .accessibility_label(label)
                .checked(manager.auto_calculate_folder_sizes)
                .on_change(cx.listener(move |this, checked, window, cx| {
                    this.set_auto_calculate_folder_sizes(*checked, window, cx);
                    if let Some(path) = &preferences_path {
                        this.save_preferences_to(path);
                    }
                })),
        )
}

/// Renders both Settings categories and the controls for the selected category.
fn settings_view(
    manager: &ArchiveManager,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    let category = manager.settings_category;
    let theme_mode = manager.theme_mode;
    let accent_color = manager.accent_color;
    let dark = theme.window < 0x808080;
    let default_location = manager
        .default_location
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| language.text("System default").to_owned());

    let general_button = settings_category_button(
        "settings-category-general",
        language.text("General"),
        "folder",
        category == SettingsCategory::General,
        theme,
        cx,
    )
    .on_click(cx.listener(|this, _, _, cx| {
        this.settings_category = SettingsCategory::General;
        cx.notify();
    }));
    let appearance_button = settings_category_button(
        "settings-category-appearance",
        language.text("Appearance"),
        "swatchbook",
        category == SettingsCategory::Appearance,
        theme,
        cx,
    )
    .on_click(cx.listener(|this, _, _, cx| {
        this.settings_category = SettingsCategory::Appearance;
        cx.notify();
    }));
    let advanced_button = settings_category_button(
        "settings-category-advanced",
        language.text("Advanced"),
        "toolbox",
        category == SettingsCategory::Advanced,
        theme,
        cx,
    )
    .on_click(cx.listener(|this, _, _, cx| {
        this.settings_category = SettingsCategory::Advanced;
        cx.notify();
    }));

    let page_title = match category {
        SettingsCategory::General => language.text("General"),
        SettingsCategory::Appearance => language.text("Appearance"),
        SettingsCategory::Advanced => language.text("Advanced"),
        SettingsCategory::Keyboard => language.text("Keyboard shortcuts"),
    };
    let mut page = div()
        .id("settings-category-content")
        .debug_selector(|| "settings-category-content".to_owned())
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.0))
        .gap_4()
        .pl_4()
        .border_l_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .text_base()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(page_title),
        );

    match category {
        SettingsCategory::Keyboard => {
            page = page.child(crate::shortcuts::view(manager, theme, cx));
        }
        SettingsCategory::General => {
            page = page
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(language.text("Language"))
                        .child(language_select(manager, theme, cx)),
                )
                .child(folder_size_setting(manager, theme, user_preferences_path(), cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(language.text("Default location"))
                        .child(
                            Button::new("default-location-field")
                                .debug_selector(|| "default-location-field".to_owned())
                                .custom(
                                    gpui::component::button::ButtonCustomVariant::new(cx)
                                        .color(rgb(theme.surface).into())
                                        .foreground(rgb(theme.soft_accent()).into())
                                        .hover(rgb(theme.hover).into())
                                        .active(rgb(theme.selection).into()),
                                )
                                .w_full()
                                .h(px(24.0))
                                .rounded_sm()
                                .border_1()
                                .border_color(rgb(theme.border))
                                .px_2()
                                .xsmall()
                                .accessibility_label(language.text("Default location"))
                                .tooltip(language.text("Choose folder"))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .w_full()
                                        .min_w(px(0.0))
                                        .child(app_icon("folder", 14.0, theme.soft_accent()))
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .text_xs()
                                        .truncate()
                                        .text_color(rgb(theme.muted_text))
                                        .child(default_location),
                                )
                                .child(app_icon("folder-open", 14.0, theme.soft_accent()))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let mut dialog = rfd::AsyncFileDialog::new()
                                        .set_title(this.language.text("Default location"));
                                    let directory = this
                                        .default_location
                                        .as_deref()
                                        .or_else(|| this.current_directory.as_deref());
                                    if let Some(directory) = directory {
                                        dialog = dialog.set_directory(directory);
                                    }
                                    this.receive_default_location(
                                        async move {
                                            dialog
                                                .pick_folder()
                                                .await
                                                .map(|file| file.path().to_path_buf())
                                        },
                                        user_preferences_path(),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new("reset-default-location")
                                .ghost()
                                .xsmall()
                                .label(language.text("Use system default"))
                                .disabled(manager.default_location.is_none())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.default_location = None;
                                    this.save_preferences();
                                    cx.notify();
                                })),
                        ),
                );
        }
        SettingsCategory::Appearance => {
            page = page
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(language.text("Theme"))
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .rounded_sm()
                                .bg(rgb(theme.sidebar))
                                .p_1()
                                .children(
                                    [
                                        ("theme-system", "System", ThemeMode::System),
                                        ("theme-light", "Light", ThemeMode::Light),
                                        ("theme-dark", "Dark", ThemeMode::Dark),
                                    ]
                                    .into_iter()
                                    .map(
                                        |(id, label, value)| {
                                            setting_option(
                                                id,
                                                language.text(label),
                                                theme_mode == value,
                                                theme,
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.theme_mode = value;
                                                    this.save_preferences();
                                                    cx.notify();
                                                }),
                                            )
                                        },
                                    ),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(language.text("Font"))
                        .child(crate::ui::select(manager, crate::ui::Selector::Font)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(language.text("Accent color"))
                        .child(div().flex().items_center().gap_2().children(
                            AccentColor::ALL.into_iter().map(|color| {
                                let selected = accent_color == color;
                                let id = format!("accent-{}", color.key());
                                Button::new(id)
                                    .ghost()
                                    .small()
                                    .selected(selected)
                                    .accessibility_label(language.text(color.label()))
                                    .tooltip(language.text(color.label()))
                                    .child(
                                        div()
                                            .w(px(20.0))
                                            .h(px(20.0))
                                            .rounded_full()
                                            .border_2()
                                            .border_color(rgb(if selected {
                                                theme.text
                                            } else {
                                                theme.border
                                            }))
                                            .bg(rgb(color.color(dark))),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.accent_color = color;
                                        this.save_preferences();
                                        cx.notify();
                                    }))
                            }),
                        )),
                );
        }
        SettingsCategory::Advanced => {
            page = page
                .child(advanced_setting_stepper(
                    manager,
                    AdvancedSettingField::PreviewLimit,
                    theme,
                    cx,
                ))
                .child(advanced_setting_stepper(
                    manager,
                    AdvancedSettingField::CsvRows,
                    theme,
                    cx,
                ))
                .child(advanced_setting_stepper(
                    manager,
                    AdvancedSettingField::CsvColumns,
                    theme,
                    cx,
                ))
                .child(advanced_setting_stepper(
                    manager,
                    AdvancedSettingField::AuditDepth,
                    theme,
                    cx,
                ));
        }
    }

    div()
        .id("settings-two-column-layout")
        .debug_selector(|| "settings-two-column-layout".to_owned())
        .flex()
        .items_start()
        .gap_4()
        .min_h(px(248.0))
        .child(
            div()
                .id("settings-category-nav")
                .debug_selector(|| "settings-category-nav".to_owned())
                .w(px(144.0))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(general_button)
                .child(appearance_button)
                .child(advanced_button)
                .child(
                    settings_category_button(
                        "settings-category-keyboard", language.text("Keyboard"), "keyboard",
                        category == SettingsCategory::Keyboard, theme, cx,
                    ).on_click(cx.listener(|this, _, _, cx| {
                        this.settings_category = SettingsCategory::Keyboard;
                        cx.notify();
                    })),
                ),
            )
        .child(page)
}

/// Builds a modal's content, footer, dimensions, and close/submit behavior.
pub(crate) fn modal_view(
    dialog: gpui::component::dialog::Dialog,
    modal: Modal,
    manager: &mut ArchiveManager,
    theme: Theme,
    window: &mut Window,
    cx: &mut Context<ArchiveManager>,
) -> gpui::component::dialog::Dialog {
    let language = manager.language;
    let title = (modal != Modal::OpenPicker).then(|| language.text(match modal {
            Modal::OpenPicker => {
                unreachable!("OpenPicker does not use a duplicate dialog title")
            }
            Modal::Settings => "Settings",
            Modal::About => "About",
            Modal::CreateArchive => "Create archive",
            Modal::CompareArchives => "Compare archives",
            Modal::Recovery => "Archive Repair & Recovery",
            Modal::Conversion => "Convert archive",
            Modal::SignArchive => "Sign archive",
            Modal::Info => "Info",
            Modal::Password => "Archive password",
            Modal::Delete => if manager.file_actions.pending_delete.as_ref().is_some_and(|target| target.archive_entry.is_some()) {
                "Remove from archive"
            } else { "Move to Trash" },
        }));
    let mut content = div()
        .flex()
        .flex_col()
        .gap_4()
        .p_5()
        .text_sm()
        .text_color(rgb(theme.text));
    match modal {
        Modal::OpenPicker => {
            content = content.p_0().gap_0().child(open_picker_view(manager, theme, window, cx));
        }
        Modal::Delete => {
            content = content.child(file_actions::delete_view(manager, theme, cx));
        }
        Modal::Password => {
            content = content.p_0().child(password::view(manager, theme, cx));
        }
        Modal::CreateArchive => {
            content = content.child(creation::view(manager, theme, cx));
        }
        Modal::Recovery => {
            content = content.child(crate::recovery::view(manager, theme, window, cx));
        }
        Modal::Conversion => {
            content = content.child(crate::conversion::view(manager, theme, cx));
        }
        Modal::SignArchive => {
            content = content.child(crate::archive_signature::view(manager, theme, cx));
        }
        Modal::Info => {
            content = content.child(file_actions::info_view(manager, theme, language));
        }
        Modal::CompareArchives => {
            content = content
                .p_0()
                .gap_3()
                .child(crate::inspector::comparison_dialog_content(
                    manager, theme, window, cx,
                ));
        }
        Modal::Settings => {
            content = content
                .p_0()
                .gap_0()
                .child(settings_view(manager, theme, cx));
        }
        Modal::About => {
            let author = env!("CARGO_PKG_AUTHORS");
            let email = author
                .split_once('<')
                .and_then(|(_, email)| email.strip_suffix('>'));
            content = content
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(about_app_icon())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().text_xl().child("ZArchive"))
                                .child(div().text_xs().text_color(rgb(theme.muted_text)).child(
                                    format!(
                                        "{} {}",
                                        language.text("Version"),
                                        env!("CARGO_PKG_VERSION")
                                    ),
                                )),
                        ),
                )
                .child(language.text("Browse, create, verify, and extract archives in one place."))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(language.text("License"))
                        .child(
                            div()
                                .id("application-license")
                                .debug_selector(|| "application-license".to_owned())
                                .text_xs()
                                .text_color(rgb(theme.muted_text))
                                .child(format!(
                                    "{} - Do What The Fuck You Want To Public License",
                                    env!("CARGO_PKG_LICENSE")
                                )),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(language.text("Supported formats"))
                        .child(div().text_xs().text_color(rgb(theme.muted_text)).child(
                            language.text(
                                "Read and extract: ZIP, TAR, compressed TAR, CPIO, AR, PAX, XAR, standalone GZIP, BZIP2, XZ, Zstandard, LZ4, LZIP, Brotli, 7z, RAR",
                            ),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(theme.muted_text))
                                .child(language.text(
                                    "Create: ZIP, TAR, CPIO, AR, PAX, XAR, standalone GZIP, BZIP2, XZ, Zstandard, LZ4, LZIP, Brotli, 7z",
                                )),
                        )
                        .child(div().text_xs().text_color(rgb(theme.muted_text)).child(
                            language.text("Password-protected ZIP and 7z archives are supported."),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(language.text("Author & contact"))
                        .child(
                            div()
                                .id("author-contact")
                                .text_xs()
                                .text_color(rgb(theme.accent))
                                .child(author)
                                .when_some(email, |this, email| {
                                    this.hover(|style| style.underline().cursor_pointer())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Err(error) =
                                                open::that(format!("mailto:{email}"))
                                            {
                                                this.status_message = Some(format!(
                                                    "{}: {error}",
                                                    this.language.text("Failed to open")
                                                ));
                                                cx.notify();
                                            }
                                        }))
                                }),
                        ),
                );
        }
    }

    let view = cx.entity().downgrade();
    let footer = match modal {
        Modal::OpenPicker => open_picker_footer(manager, theme, cx).into_any_element(),
        Modal::Password => password::footer(manager, cx).into_any_element(),
        Modal::CreateArchive => creation::footer(manager, cx).into_any_element(),
        Modal::Recovery => crate::recovery::footer(manager, cx).into_any_element(),
        Modal::Conversion => crate::conversion::footer(manager, cx).into_any_element(),
        Modal::SignArchive => crate::archive_signature::footer(manager, cx).into_any_element(),
        Modal::CompareArchives => {
            crate::inspector::comparison_footer(manager, cx).into_any_element()
        }
        _ => div().h(px(0.0)).into_any_element(),
    };
    ui::center_dialog(
        dialog,
        title,
        footer,
        if modal == Modal::OpenPicker { px(0.0) } else { px(16.0) },
    )
        .width(px(match modal {
            Modal::OpenPicker => 820.0,
            Modal::Settings => 620.0,
            Modal::CompareArchives => 900.0,
            Modal::Recovery => 800.0,
            Modal::Conversion => 760.0,
            _ => 460.0,
        }))
        .when(modal == Modal::OpenPicker, |dialog| dialog.gap_y(px(0.0)))
        .overlay_closable(modal != Modal::Password)
        .when(modal == Modal::Password, |this| {
            this.width(px(password::PASSWORD_WIDTH))
        })
        .close_button(modal != Modal::OpenPicker)
        .on_close(cx.listener(|this, _, window, cx| this.close_modal(window, cx)))
        .on_ok(move |_, window, cx| {
            let _ = view.update(cx, |this, cx| match this.modal {
                Some(Modal::Password) => this.unlock_archive(window, cx),
                Some(Modal::CreateArchive) => this.submit_archive(window, cx),
                Some(Modal::Recovery) => this.start_recovery(cx),
                Some(Modal::Conversion) => this.analyze_conversion(cx),
                Some(Modal::SignArchive) => this.submit_archive_signature(window, cx),
                Some(Modal::CompareArchives) => this.start_archive_comparison(cx),
                Some(Modal::Delete) => this.confirm_delete(window, cx),
                Some(Modal::OpenPicker) => this.submit_open_picker(window, cx),
                _ => {}
            });
            false
        })
        .child(content)
}

/// Returns the retained language selector for the settings page.
fn language_select(
    manager: &ArchiveManager,
    _theme: Theme,
    _cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    ui::select(manager, ui::Selector::Language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{component_window, test_manager};

    struct FolderSizeSettingTest {
        manager: gpui::Entity<ArchiveManager>,
        preferences_path: PathBuf,
    }

    impl Render for FolderSizeSettingTest {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.manager.update(cx, |manager, cx| {
                folder_size_setting(
                    manager,
                    Theme::new(false),
                    Some(self.preferences_path.clone()),
                    cx,
                ).into_any_element()
            })
        }
    }

    #[gpui::test]
    fn folder_size_setting_switch_toggles_and_persists_both_states(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let preferences_path = directory.path().join("settings.json");
        let (view, cx) = component_window(cx, |_, cx| FolderSizeSettingTest {
            manager: cx.new(|cx| {
                let mut manager = test_manager(cx);
                manager.current_directory = None;
                manager
            }),
            preferences_path: preferences_path.clone(),
        });
        let manager = cx.read(|cx| view.read(cx).manager.clone());
        cx.read(|cx| assert!(manager.read(cx).auto_calculate_folder_sizes));
        for enabled in [false, true] {
            cx.update(|window, cx| window.click("auto-calculate-folder-sizes-switch", cx));
            cx.run_until_parked();
            cx.read(|cx| assert_eq!(manager.read(cx).auto_calculate_folder_sizes, enabled));
            assert_eq!(UserPreferences::load(&preferences_path).auto_calculate_folder_sizes, enabled);
        }
    }

    #[gpui::test]
    fn folder_size_setting_is_visible_and_enabled_by_default(cx: &mut gpui::TestAppContext) {
        let (view, cx) = component_window(cx, |window, cx| {
            let mut manager = test_manager(cx);
            manager.language = Language::Polish;
            manager.show_modal(Modal::Settings, window, cx);
            manager
        });
        cx.simulate_resize(size(px(1100.0), px(720.0)));
        cx.run_until_parked();
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let setting = cx.debug_bounds("auto-calculate-folder-sizes")
            .expect("folder-size setting must be rendered in General settings");
        let dialog = cx.debug_bounds("dialog-0").unwrap();
        assert!(setting.size.width > px(0.0));
        assert!(setting.size.height >= px(16.0));
        assert!(setting.left() >= dialog.left());
        assert!(setting.right() <= dialog.right());
        assert!(setting.top() >= dialog.top());
        assert!(setting.bottom() <= dialog.bottom());
        cx.read(|cx| assert!(view.read(cx).auto_calculate_folder_sizes));
        assert!(UserPreferences::default().auto_calculate_folder_sizes);
    }

    #[gpui::test]
    fn default_location_picker_saves_selection_after_callback(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let selected = directory.path().to_path_buf();
        let preferences_path = directory.path().join("preferences.json");
        let (view, cx) = component_window(cx, |_, cx| test_manager(cx));

        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.receive_default_location(
                    std::future::ready(Some(selected.clone())),
                    Some(preferences_path.clone()),
                    window,
                    cx,
                );
                assert!(manager.default_location.is_none());
                assert!(!preferences_path.exists());
            });
        });
        cx.run_until_parked();

        view.update(cx, |manager, _| {
            assert_eq!(manager.default_location.as_ref(), Some(&selected));
        });
        assert_eq!(
            UserPreferences::load(&preferences_path).default_location,
            Some(selected)
        );
    }

    #[gpui::test]
    fn default_location_picker_cancel_preserves_preferences(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().to_path_buf();
        let preferences_path = directory.path().join("preferences.json");
        std::fs::write(&preferences_path, "unchanged").unwrap();
        let (view, cx) = component_window(cx, |_, cx| test_manager(cx));

        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.default_location = Some(original.clone());
                manager.receive_default_location(
                    std::future::ready(None),
                    Some(preferences_path.clone()),
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();

        view.update(cx, |manager, _| {
            assert_eq!(manager.default_location.as_ref(), Some(&original));
        });
        assert_eq!(
            std::fs::read_to_string(preferences_path).unwrap(),
            "unchanged"
        );
    }
}
