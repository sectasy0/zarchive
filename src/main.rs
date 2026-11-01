mod archive;
mod xar;
mod blur;
mod browser;
mod comparison;
mod creation;
mod dialogs;
mod drop_service;
mod file_actions;
mod filters;
mod i18n;
mod icons;
mod inspection;
mod inspector;
mod operations;
mod password;
mod preferences;
mod secret;
mod toolbar;
mod text_preview;
mod task_queue;
mod transfer;
mod ui;

use archive::{ArchiveEntry, is_supported_archive, list_archive};
use gpui::component::menu::ContextMenuExt;
use gpui::component::{
    Disableable, Selectable, Sizable, TitleBar,
    button::{Button, ButtonVariants},
};
use gpui::{
    App, Bounds, ClickEvent, Context, FocusHandle, Render, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, point, prelude::*, px, rgb, size, svg,
};
use i18n::Language;
use icons::IconAssets;
pub(crate) use preferences::{AccentColor, ThemeMode, UserPreferences};
use preferences::{load_user_preferences, user_preferences_path};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
pub(crate) use toolbar::toolbar_button;

pub(crate) use browser::{
    SortColumn, default_directory, folder_tree, folders_count, read_directory,
    sort_entries, visible_entries,
};
pub(crate) use dialogs::modal_view;

#[derive(Clone, Copy, PartialEq, Eq)]
/// Identifies the application dialog currently displayed.
enum Modal {
    /// Application settings.
    Settings,
    /// Application information and license.
    About,
    /// Archive creation form.
    CreateArchive,
    /// Password prompt for a protected archive.
    Password,
    /// Confirmation before moving an item to Trash.
    Delete,
    /// Selects two archives and displays their comparison report.
    CompareArchives,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
/// Selects the active page in the Settings dialog.
enum SettingsCategory {
    /// General preferences such as language and default location.
    #[default]
    General,
    /// Theme and accent appearance preferences.
    Appearance,
}

#[derive(Clone, Copy)]
/// Application color palette resolved for one light/dark mode and accent.
struct Theme {
    window: u32,
    titlebar: u32,
    sidebar: u32,
    surface: u32,
    border: u32,
    subtle_border: u32,
    text: u32,
    muted_text: u32,
    faint_text: u32,
    hover: u32,
    selection: u32,
    icon: u32,
    accent: u32,
}

impl Theme {
    /// Builds the base palette for the requested appearance mode.
    fn new(dark: bool) -> Self {
        if dark {
            Self {
                window: 0x1b222a,
                titlebar: 0x131a21,
                sidebar: 0x161e26,
                surface: 0x1b222a,
                border: 0x303b47,
                subtle_border: 0x252f3a,
                text: 0xdce6f1,
                muted_text: 0x9caec0,
                faint_text: 0x718296,
                hover: 0x273746,
                selection: 0x1d3c61,
                icon: 0xa5b8cc,
                accent: 0x78b7ff,
            }
        } else {
            Self {
                window: 0xf5f8fc,
                titlebar: 0xeaf0f6,
                sidebar: 0xeef3f8,
                surface: 0xfbfcfe,
                border: 0xd6e0ea,
                subtle_border: 0xe6edf4,
                text: 0x1c2938,
                muted_text: 0x607286,
                faint_text: 0x8a9bad,
                hover: 0xeaf1f8,
                selection: 0xdcecff,
                icon: 0x5b6d80,
                accent: 0x2563eb,
            }
        }
    }

    /// Replaces the palette's default accent with a user-selected color.
    fn with_accent(mut self, accent: u32) -> Self {
        self.accent = accent;
        self
    }

    /// Lightens the active accent for low-emphasis icon and focus treatments.
    fn soft_accent(self) -> u32 {
        let lighten_by = if self.window < 0x808080 { 45 } else { 10 };
        let lighten = |shift: u32| {
            let channel = (self.accent >> shift) & 0xff;
            channel + ((255 - channel) * lighten_by / 100)
        };
        (lighten(16) << 16) | (lighten(8) << 8) | lighten(0)
    }
}

/// Builds the initial back-navigation path from a directory's ancestors.
fn directory_history(path: Option<&Path>) -> Vec<PathBuf> {
    let Some(path) = path else {
        return Vec::new();
    };
    let mut history: Vec<_> = path.ancestors().map(Path::to_path_buf).collect();
    history.reverse();
    history
}

/// Owns application state and coordinates navigation, dialogs, and archive actions.
struct ArchiveManager {
    current_directory: Option<PathBuf>,
    directory_history: Vec<PathBuf>,
    navigation_history_index: usize,
    navigation_revision: u64,
    navigation_control: task_queue::TaskControl,
    archive_path: Option<PathBuf>,
    archive_name: Option<String>,
    archive_password: Option<password::ArchivePassword>,
    entries: Vec<ArchiveEntry>,
    error: Option<String>,
    selected_folder: String,
    selected_entry: Option<String>,
    content_focus: FocusHandle,
    sort_column: SortColumn,
    sort_ascending: bool,
    directory_sizes: Option<BTreeMap<String, u64>>,
    directory_size_task: Option<u64>,
    directory_size_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    opened_files: Vec<NamedTempFile>,
    status_message: Option<String>,
    notifications: Vec<(bool, String)>,
    notifications_visible: bool,
    language: Language,
    theme_mode: ThemeMode,
    accent_color: AccentColor,
    default_location: Option<PathBuf>,
    auto_calculate_folder_sizes: bool,
    modal: Option<Modal>,
    settings_category: SettingsCategory,
    modal_focus: FocusHandle,
    creation: creation::ArchiveCreationForm,
    creation_task: Option<u64>,
    password_prompt: password::PasswordPrompt,
    password_blur: blur::PasswordBlur,
    controls: ui::Controls,
    file_scroll: gpui::UniformListScrollHandle,
    folder_scroll: gpui::ScrollHandle,
    toolbar_search: toolbar::SearchField,
    filters: filters::FilterBar,
    file_actions: file_actions::FileActions,
    inspector: inspector::Inspector,
    comparison_dialog: inspector::ComparisonDialog,
    background_queue: task_queue::BackgroundQueue,
}

impl ArchiveManager {
    /// Persists language, appearance, accent, and startup-location preferences.
    fn save_preferences(&mut self) {
        let Some(path) = user_preferences_path() else {
            return;
        };
        self.save_preferences_to(&path);
    }

    fn save_preferences_to(&mut self, path: &std::path::Path) {
        let preferences = UserPreferences {
            language: self.language,
            theme_mode: self.theme_mode,
            accent_color: self.accent_color,
            default_location: self.default_location.clone(),
            auto_calculate_folder_sizes: self.auto_calculate_folder_sizes,
        };
        if let Err(error) = preferences.save(path) {
            self.status_message = Some(format!(
                "{}: {error:#}",
                self.language.text("Could not save settings")
            ));
        }
    }

    fn cancel_directory_size_calculation(&mut self) {
        if let Some(cancel) = self.directory_size_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.directory_size_task = None;
    }

    fn set_auto_calculate_folder_sizes(&mut self, enabled: bool, window: &Window, cx: &mut Context<Self>) {
        self.auto_calculate_folder_sizes = enabled;
        if enabled {
            self.calculate_directory_sizes(window, cx);
        } else {
            self.cancel_directory_size_calculation();
        }
        cx.notify();
    }

    /// Returns the total size when every filesystem folder size is known.
    fn location_size(&self, entries: &[ArchiveEntry]) -> Option<u64> {
        if self.error.is_some() {
            return None;
        }
        if self.current_directory.is_some()
            && entries.iter().any(|entry| {
                entry.is_directory
                    && !self
                        .directory_sizes
                        .as_ref()
                        .is_some_and(|sizes| sizes.contains_key(&entry.name))
            })
        {
            return None;
        }
        entries
            .iter()
            .try_fold(0_u64, |total, entry| total.checked_add(entry.size))
    }

    /// Updates the active sort column or toggles its direction, then refreshes the view.
    fn sort_by(&mut self, column: SortColumn, cx: &mut Context<Self>) {
        if self.sort_column == column {
            self.sort_ascending = !self.sort_ascending;
        } else {
            self.sort_column = column;
            self.sort_ascending = true;
        }
        cx.notify();
    }

    /// Calculates direct child-folder sizes off the UI thread when they are needed.
    fn calculate_directory_sizes(&mut self, _window: &Window, cx: &mut Context<Self>) {
        if !self.auto_calculate_folder_sizes
            || self.directory_sizes.is_some()
            || self.directory_size_task.is_some()
        {
            return;
        }
        let Some(directory) = self.current_directory.clone() else {
            return;
        };
        let folders: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| entry.is_directory)
            .map(|entry| (entry.name.clone(), directory.join(&entry.name)))
            .collect();
        if folders.is_empty() {
            self.directory_sizes = Some(BTreeMap::new());
            return;
        }
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.directory_size_cancel = Some(cancel.clone());
        let worker_cancel = cancel.clone();
        let total_folders = folders.len() as u64;
        let completion_cancel = cancel.clone();
        self.directory_size_task = Some(self.background_queue.enqueue(
            "Calculating folder sizes...",
            task_queue::TaskControl::with_cancel(cancel),
            move |control, reporter| async move {
                reporter.phase("Calculating folder sizes...");
                reporter.set(0, total_folders);
                let mut results = Vec::with_capacity(folders.len());
                for (index, (name, path)) in folders.into_iter().enumerate() {
                    if !control.checkpoint() || worker_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        anyhow::bail!("cancelled");
                    }
                    results.push((name, browser::directory_size_with_cancel(&path, &worker_cancel)));
                    reporter.set(index as u64 + 1, total_folders);
                }
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                Ok(results)
            },
            move |this, result, _, cx| {
                if completion_cancel.load(std::sync::atomic::Ordering::Relaxed)
                    || !this.auto_calculate_folder_sizes
                    || this.current_directory.as_ref() != Some(&directory)
                { return; }
                this.directory_size_task = None;
                this.directory_size_cancel = None;
                let results = match result {
                    Ok(results) => results,
                    Err(task_queue::TaskFailure::Cancelled) => {
                        this.directory_sizes = Some(BTreeMap::new());
                        cx.notify();
                        return;
                    }
                    Err(task_queue::TaskFailure::Failed(error)) => {
                        this.status_message = Some(format!("{}: {error}", this.language.text("Could not calculate all folder sizes")));
                        cx.notify();
                        return;
                    }
                };
                let mut sizes = BTreeMap::new();
                let mut errors = Vec::new();
                for (name, result) in results {
                    match result {
                        Ok(size) => { sizes.insert(name, size); }
                        Err(error) => errors.push(format!("{name}: {error:#}")),
                    }
                }
                for entry in &mut this.entries {
                    if entry.is_directory && let Some(size) = sizes.get(&entry.name) { entry.size = *size; }
                }
                this.directory_sizes = Some(sizes);
                if !errors.is_empty() {
                    this.status_message = Some(format!("{}: {}", this.language.text("Could not calculate all folder sizes"), errors.join("; ")));
                }
                cx.notify();
            },
            cx,
        ));
    }

    /// Reports whether Back can move to an earlier directory or out of an archive.
    fn can_navigate_back(&self) -> bool {
        if self.current_directory.is_none() {
            return self
                .archive_path
                .as_deref()
                .and_then(Path::parent)
                .is_some();
        }
        self.navigation_history_index > 0
    }

    /// Reports whether a later directory exists in the navigation history.
    fn can_navigate_forward(&self) -> bool {
        self.current_directory.is_some()
            && self.navigation_history_index + 1 < self.directory_history.len()
    }

    /// Loads a history entry and updates the history index only after a successful read.
    fn navigate_history_to(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(path) = self.directory_history.get(index).cloned() else {
            return;
        };
        match read_directory(&path) {
            Ok(entries) => {
                self.navigation_history_index = index;
                self.install_directory(path, entries, cx);
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                self.status_message = None;
                cx.notify();
            }
        }
    }

    /// Moves to the prior directory, or to the archive's containing directory.
    fn navigate_back(&mut self, cx: &mut Context<Self>) {
        if self.current_directory.is_none() {
            if let Some(parent) = self.archive_path.as_deref().and_then(Path::parent) {
                self.navigate_to(parent.to_path_buf(), cx);
            }
        } else if self.navigation_history_index > 0 {
            self.navigate_history_to(self.navigation_history_index - 1, cx);
        }
    }

    /// Moves to the next directory in history when one is available.
    fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        if self.can_navigate_forward() {
            self.navigate_history_to(self.navigation_history_index + 1, cx);
        }
    }

    /// Replaces the current listing with a successfully read filesystem directory.
    fn install_directory(
        &mut self,
        path: PathBuf,
        entries: Vec<ArchiveEntry>,
        _cx: &mut Context<Self>,
    ) {
        self.navigation_control.cancel();
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        self.cancel_directory_size_calculation();
        self.inspector.clear_text_preview();
        self.directory_sizes = None;
        self.current_directory = Some(path);
        self.archive_path = None;
        self.archive_name = None;
        self.archive_password = None;
        self.entries = entries;
        self.error = None;
        self.selected_folder.clear();
        self.selected_entry = None;
        self.status_message = None;
    }

    /// Captures the backdrop, opens a modal, and transfers focus into it.
    pub(crate) fn show_modal(&mut self, modal: Modal, window: &mut Window, cx: &mut Context<Self>) {
        if modal == Modal::CreateArchive && self.creation_task.is_some() {
            return;
        }
        if self.modal.is_none() {
            self.password_blur.capture(window, cx);
        }
        self.modal = Some(modal);
        window.focus(&self.modal_focus, cx);
        cx.notify();
    }

    /// Closes the active modal and clears modal-specific secrets or pending actions.
    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal == Some(Modal::Delete) {
            self.file_actions.pending_delete = None;
        }
        if self.modal == Some(Modal::Password) {
            self.password_prompt.clear();
            self.file_actions.operations.cancel_password();
        }
        self.password_blur.clear();
        if self.modal == Some(Modal::CreateArchive) {
            self.creation.password.clear();
            self.creation.confirmation.clear();
            self.creation.encrypt = false;
        }
        if self.modal == Some(Modal::CompareArchives) {
            self.comparison_dialog.close();
        }
        self.modal = None;
        window.focus(&self.content_focus, cx);
        cx.notify();
    }

    /// Opens a file picker, navigates into supported archives, or launches a regular file.
    fn open_file(&mut self, cx: &mut Context<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .set_title(self.language.text("Open file"))
            .pick_file()
        else {
            return;
        };

        if is_supported_archive(&path) {
            self.navigate_into_archive(path, cx);
            return;
        }

        self.open_entry(&file_actions::EntryTarget {
            name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            key: path.to_string_lossy().into_owned(), path,
            archive_entry: None, password: None, is_directory: false,
        }, cx);
    }

    /// Reads a filesystem directory, records it in history, and updates the listing.
    fn navigate_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match read_directory(&path) {
            Ok(entries) => {
                if self.current_directory.as_ref() != Some(&path) {
                    self.directory_history
                        .truncate(self.navigation_history_index.saturating_add(1));
                    if self.directory_history.last() != Some(&path) {
                        self.directory_history.push(path.clone());
                    }
                    self.navigation_history_index = self.directory_history.len().saturating_sub(1);
                }
                self.install_directory(path, entries, cx);
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                self.status_message = None;
            }
        }
        cx.notify();
    }

    /// Opens an archive or requests its password when the archive is protected.
    fn navigate_into_archive(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.load_archive(path, None, false, cx);
    }

    fn load_archive(&mut self, path: PathBuf, password: Option<password::ArchivePassword>, preserve_folder: bool, cx: &mut Context<Self>) {
        self.navigation_control.cancel();
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        let revision = self.navigation_revision;
        self.navigation_control = task_queue::TaskControl::default().for_paths([path.clone()]);
        let source = path.clone();
        let key = password.clone();
        let folder = self.selected_folder.clone();
        self.background_queue.enqueue(
            "Opening archive...",
            self.navigation_control.clone(),
            move |control, reporter| async move {
                reporter.phase(source.file_name().unwrap_or_default().to_string_lossy());
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                let entries = archive::list_archive_with_password(&source, key.as_ref().map(|key| key.as_str()))?;
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                Ok(entries)
            },
            move |manager, result, _, cx| {
                if manager.navigation_revision != revision { return; }
                match result {
                    Ok(entries) => {
                        manager.install_archive(path, entries, password, cx);
                        if preserve_folder { manager.selected_folder = folder; }
                    }
                    Err(task_queue::TaskFailure::Cancelled) => {},
                    Err(task_queue::TaskFailure::Failed(error)) => {
                        if error.downcast_ref::<archive::PasswordFailure>() == Some(&archive::PasswordFailure::Required) {
                            manager.request_archive_password(path, cx);
                        } else {
                            manager.status_message = Some(format!("{}: {error:#}", manager.language.text("Unable to open archive")));
                        }
                    }
                }
                cx.notify();
            },
            cx,
        );
        cx.notify();
    }
}

impl Render for ArchiveManager {
    /// Synchronizes app services and renders the titlebar, browser, sidebar, and inspector.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ui::prepare(self, window, cx);
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
            .unwrap_or(language.text("File Browser"));
        let folders = folder_tree(&self.entries);
        let search_query = self.toolbar_search.text_query();
        let mut visible_entries = if self.current_directory.is_some() {
            self.entries.clone()
        } else {
            browser::search_archive_entries(&self.entries, &self.selected_folder, &search_query)
        };
        if !search_query.is_empty() {
            visible_entries.retain(|entry| entry.name.to_lowercase().contains(&search_query));
        }
        if self.current_directory.is_some() {
            visible_entries.extend(self.toolbar_search.matches.values().map(|result| result.entry.clone()));
        }
        visible_entries.retain(|entry| {
            let size = if entry.is_directory && self.current_directory.is_some() {
                self.directory_sizes.as_ref().and_then(|sizes| sizes.get(&entry.name)).copied()
            } else {
                Some(entry.size)
            };
            !self.filters.query_invalid && self.filters.criteria.matches(entry, size, entry.modified)
        });
        sort_entries(&mut visible_entries, self.sort_column, self.sort_ascending);
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
        let background_menu_view = cx.entity().downgrade();
        let can_create_here = self.current_directory.is_some()
            && self.creation_task.is_none();

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
                location_title,
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
                location_title,
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

        let archive_path = self.archive_path.clone();
        let current_directory = self.current_directory.clone();
        let archive_password = self.archive_password.clone();
        let selected_folder = self.selected_folder.clone();
        let selected_entry = self.selected_entry.clone();
        let file_list =
            if self.error.is_some() {
                gpui::base::TableBody::new("file-list")
                    .debug_selector(|| "file-list".to_owned())
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
                gpui::base::TableBody::new("file-list")
                    .debug_selector(|| "file-list".to_owned())
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
                gpui::base::TableBody::new("file-list")
                    .debug_selector(|| "file-list".to_owned())
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
                                language.text(if search_query.is_empty() && !self.filters.criteria.is_active() {
                                    "This folder is empty"
                                } else {
                                    "No matching items"
                                }),
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
                gpui::base::TableBody::new("file-list")
                    .debug_selector(|| "file-list".to_owned())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .bg(rgb(theme.surface))
                    .child(
                        gpui::uniform_list(
                            "file-list-items",
                            item_count,
                            cx.processor(move |manager, range: std::ops::Range<usize>, _, cx| {
                                range
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
                                                theme,
                                                manager.archive_drop_enabled(),
                                                entry.source_path.as_deref().and_then(|key| manager.toolbar_search.matches.get(key)).map(|result| result.target.clone()),
                                                cx,
                                            )
                                            .into_any_element(),
                                        )
                                    })
                                    .collect()
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

        div()
            .id("archive-manager")
            .on_drag_move::<inspector::PreviewResize>(cx.listener(inspector::resize_preview))
            .track_focus(&self.content_focus)
            .key_context("ArchiveManager")
            .on_action(cx.listener(|this, _: &toolbar::key_actions::OpenFile, _, cx| {
                if this.modal.is_none() {
                    this.open_file(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &toolbar::key_actions::CreateArchive, window, cx| {
                if this.modal.is_none() {
                    this.show_modal(Modal::CreateArchive, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &toolbar::key_actions::ExtractHere, window, cx| {
                if this.modal.is_none() {
                    this.extract_current_archive(true, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &toolbar::key_actions::ExtractTo, window, cx| {
                if this.modal.is_none() {
                    this.extract_current_archive(false, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &toolbar::key_actions::TestIntegrity, window, cx| {
                if this.modal.is_none() {
                    this.inspect_current(
                        inspection::InspectionMode::Integrity,
                        window,
                        cx,
                    );
                }
            }))
            .on_action(cx.listener(|this, _: &toolbar::key_actions::AuditArchive, window, cx| {
                if this.modal.is_none() {
                    this.inspect_current(inspection::InspectionMode::Audit, window, cx);
                }
            }))
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
                                gpui::base::Table::new("archive-table")
                                    .debug_selector(|| "archive-table".to_owned())
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .w_full()
                                    .min_h(px(0.0))
                                    .overflow_hidden()
                                    .child(
                                        gpui::base::TableRow::new("archive-table-header", 0)
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
                                                language.text("NAME"),
                                                None,
                                                SortColumn::Name,
                                                self.sort_column,
                                                self.sort_ascending,
                                                theme,
                                                cx,
                                            ))
                                            .child(sort_header(
                                                language.text("TYPE"),
                                                Some(130.0),
                                                SortColumn::Type,
                                                self.sort_column,
                                                self.sort_ascending,
                                                theme,
                                                cx,
                                            ))
                                            .child(sort_header(
                                                language.text("SIZE"),
                                                Some(90.0),
                                                SortColumn::Size,
                                                self.sort_column,
                                                self.sort_ascending,
                                                theme,
                                                cx,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .id("file-scroll-viewport")
                                            .w_full()
                                            .relative()
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .min_h(px(0.0))
                                            .overflow_hidden()
                                            .when_some(self.archive_path.clone(), |area, path| {
                                                drop_service::archive_drop_area(area, file_actions::EntryTarget {
                                                    name: self.archive_name.clone().unwrap_or_default(),
                                                    key: path.to_string_lossy().into_owned(), path,
                                                    archive_entry: (!self.selected_folder.is_empty()).then(|| self.selected_folder.clone()),
                                                    password: self.archive_password.clone(), is_directory: true,
                                                }, theme, self.archive_drop_enabled(), cx)
                                            })
                                            .child(file_list)
                                            .child(
                                                gpui::component::scroll::Scrollbar::vertical(
                                                    &self.file_scroll,
                                                )
                                                .id("file-list-scrollbar")
                                                .mode(
                                                    gpui::component::scroll::ScrollbarMode::Always,
                                                ),
                                            ),
                                    ),
                            )
                            .when(self.inspector.visible, |this| {
                                let height = self.inspector.preview_height(window.viewport_size().height);
                                let panel = inspector::view(
                                    self,
                                    theme,
                                    height,
                                    cx,
                                );
                                this.child(div()
                                    .id("resizable-preview")
                                    .debug_selector(|| "resizable-preview".to_owned())
                                    .relative().h(height).min_h(px(0.0)).flex_shrink_0()
                                    .child(panel)
                                    .child(gpui::component::resizable::resize_handle::<inspector::PreviewResize, inspector::PreviewResize>(
                                        "preview-resize-handle", gpui::Axis::Vertical,
                                    )
                                        .debug_selector(|| "preview-resize-handle".to_owned())
                                        .inside(gpui::component::resizable::HandleEdge::Leading)
                                        .on_drag(inspector::PreviewResize {
                                            bottom: window.viewport_size().height - px(24.0),
                                            maximum: (window.viewport_size().height - px(220.0)).max(px(80.0)),
                                        }, |drag, _, _, cx| cx.new(|_| drag.clone()))))
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
                                                this.notifications_visible = !this.notifications_visible;
                                                cx.notify();
                                            }))
                                            .child(
                                                div()
                                                    .id("status-notifications")
                                                    .debug_selector(|| "status-notifications".to_owned())
                                                    .flex()
                                                    .items_center()
                                                    .gap_1()
                                                    .child(
                                                        gpui::component::Icon::new(gpui::assets::IconName::CircleX)
                                                            .size(px(12.0))
                                                            .text_color(rgb(if error_count > 0 { 0xef4444 } else { theme.muted_text })),
                                                    )
                                                    .child(error_count.to_string())
                                                    .child(
                                                        gpui::component::Icon::new(gpui::assets::IconName::TriangleAlert)
                                                            .size(px(12.0))
                                                            .text_color(rgb(if warning_count > 0 { inspection_warning_color } else { theme.muted_text })),
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
                self.notifications_visible && !self.notifications.is_empty() && self.modal.is_none(),
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
                                            .accessibility_label(language.text("Hide notifications"))
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
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.0))
                                                    .text_xs()
                                                    .child(if *error {
                                                        language.error_message(message).to_owned()
                                                    } else {
                                                        message.clone()
                                                    }),
                                            )
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
    let drop_directory = current_directory.as_ref().map(|directory| directory.join(&path));
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
    div().id(gpui::SharedString::from(format!("folder-drop:{label}:{depth}"))).w_full().min_w(px(0.0))
        .child(item)
        .when_some(drop_directory, |area, directory| drop_service::folder_drop_area(area, directory, theme, drop_enabled, cx))
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
    div().id(if is_home { "place-drop-home" } else { "place-drop-documents" })
        .w_full().min_w(px(0.0)).child(item)
        .when_some(drop_directory, |area, directory| drop_service::folder_drop_area(area, directory, theme, drop_enabled, cx))
}

/// Renders a clickable table header that updates the active sort column and direction.
fn sort_header(
    label: &'static str,
    width: Option<f32>,
    column: SortColumn,
    active_column: SortColumn,
    ascending: bool,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let active = column == active_column;
    let id = match column {
        SortColumn::Name => "sort-name",
        SortColumn::Type => "sort-type",
        SortColumn::Size => "sort-size",
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
                .bg(rgb(if index % 2 == 1 { theme.sidebar } else { theme.surface }))
                .child(
                    gpui::base::TableCell::new("search-loading-name", 0)
                        .flex()
                        .flex_1()
                        .min_w(px(0.0))
                        .items_center()
                        .gap_2()
                        .overflow_hidden()
                        .child(inspector::skeleton_bar(theme, 16.0, 16.0))
                        .child(inspector::skeleton_bar(theme, 164.0 + (index % 4) as f32 * 24.0, 9.0)),
                )
                .child(
                    gpui::base::TableCell::new("search-loading-type", 1)
                        .w(px(130.0))
                        .flex_shrink_0()
                        .pl_2()
                        .child(inspector::skeleton_bar(theme, 36.0 + (index % 3) as f32 * 8.0, 9.0)),
                )
                .child(
                    gpui::base::TableCell::new("search-loading-size", 2)
                        .w(px(90.0))
                        .flex_shrink_0()
                        .pl_2()
                        .child(inspector::skeleton_bar(theme, 44.0 + (index % 2) as f32 * 8.0, 9.0)),
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
    let drop_candidate = (target.is_directory && target.archive_entry.is_none())
        || (archive::is_supported_archive(&target.path)
            && (target.archive_entry.is_none() || target.is_directory));
    let menu_view = cx.entity().downgrade();
    let striped = row_index % 2 == 1;
    gpui::base::TableRow::new(gpui::SharedString::from(entry_name.clone()), row_index)
        .debug_selector({
            let name = entry_name.clone();
            move || format!("file:{name}")
        })
        .flex()
        .items_center()
        .w_full()
        .h(px(28.0))
        .flex_shrink_0()
        .px_2()
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
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
        .when(can_drag, |row| row.on_drag(drop_service::ArchiveFileDrag { target: drag_target, theme }, |drag, _, _, cx| cx.new(|_| drag.clone())))
        .when(drop_candidate, |row| {
            let hover_target = drop_target.clone();
            let external_hover = drop_target.clone();
            let external_target = drop_target.clone();
            row.drag_over::<drop_service::ArchiveFileDrag>(move |style, drag, _, _| {
                let accepted = drop_enabled && drop_service::accepts_entry_drop(&hover_target, std::slice::from_ref(&drag.target.path));
                style.bg(gpui::rgba(if accepted { (theme.accent << 8) | 0x2a } else { 0xef44441a }))
                    .border_color(rgb(if accepted { theme.accent } else { 0xef4444 }))
            })
            .drag_over::<gpui::ExternalPaths>(move |style, paths, _, _| {
                let accepted = drop_enabled && drop_service::accepts_entry_drop(&external_hover, paths.paths());
                style.bg(gpui::rgba(if accepted { (theme.accent << 8) | 0x2a } else { 0xef44441a }))
                    .border_color(rgb(if accepted { theme.accent } else { 0xef4444 }))
            })
            .on_drop(cx.listener(move |manager, drag: &drop_service::ArchiveFileDrag, window, cx| {
                cx.stop_propagation();
                manager.receive_entry_drop(&drop_target, std::slice::from_ref(&drag.target.path), window, cx);
            }))
            .on_drop(cx.listener(move |manager, paths: &gpui::ExternalPaths, window, cx| {
                cx.stop_propagation();
                manager.receive_entry_drop(&external_target, paths.paths(), window, cx);
            }))
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
                this.preview_archive_target(&click_target, window, cx);
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
fn file_icon(extension: &str) -> &'static str {
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
        "bin" | "dat" | "exe" | "dll" | "so" | "dylib" | "class" | "o" | "obj"
        | "wasm" | "db" | "sqlite" | "sqlite3" | "iso" => "binary",
        _ => "file",
    }
}

fn entry_icon(entry: &ArchiveEntry) -> &'static str {
    let extension = entry
        .name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_uppercase())
        .unwrap_or_default();
    let icon = file_icon(&extension);
    if entry.is_encrypted && icon == "archive" {
        "archive-locked"
    } else {
        icon
    }
}

/// Renders a registered application SVG icon at the requested size and color.
fn app_icon(name: &str, size: f32, color: u32) -> impl IntoElement {
    svg()
        .path(format!("icons/{name}.svg"))
        .w(px(size * icon_aspect_ratio(name)))
        .h(px(size))
        .text_color(rgb(color))
}

/// Returns the width-to-height ratio used to preserve an icon's source proportions.
fn icon_aspect_ratio(name: &str) -> f32 {
    match name {
        "arrow-down" | "arrow-up" | "document"  | "image" | "audio"
        | "video" | "code" | "file" | "archive" | "archive-locked" => 0.75,
        "arrow-left" | "documents" => 0.875,
        "chevron-left" => 0.625,
        "home" => 1.125,
        "folder-open" => 1.125,
        _ => 1.0,
    }
}

/// Formats a byte count using binary units from bytes through tebibytes.
fn format_size(bytes: u64) -> String {
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
    .any(|key| Language::ALL.into_iter().any(|language| message.starts_with(language.text(key))))
}

/// Loads startup state and launches the GPUI archive manager window.
fn main() {
    let preferences = load_user_preferences();
    let startup_path = std::env::args_os().nth(1).map(PathBuf::from);
    let startup_is_archive = startup_path.as_deref().is_some_and(is_supported_archive);
    let mut startup_requires_password = false;
    let (current_directory, archive_path, archive_name, entries, error) = if startup_is_archive {
        let path = startup_path.expect("startup archive path is present");
        let archive_path = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let archive_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        match list_archive(&path) {
            Ok(entries) => (None, Some(archive_path), archive_name, entries, None),
            Err(error)
                if error.downcast_ref::<archive::PasswordFailure>()
                    == Some(&archive::PasswordFailure::Required) =>
            {
                startup_requires_password = true;
                (None, Some(archive_path), archive_name, Vec::new(), None)
            }
            Err(error) => (
                None,
                Some(archive_path),
                archive_name,
                Vec::new(),
                Some(format!("{error:#}")),
            ),
        }
    } else if let Some(path) = startup_path {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if path.is_dir() {
            match read_directory(&path) {
                Ok(entries) => (Some(path), None, None, entries, None),
                Err(error) => (
                    Some(path),
                    None,
                    None,
                    Vec::new(),
                    Some(format!("{error:#}")),
                ),
            }
        } else {
            let parent = path.parent().map(Path::to_path_buf);
            match parent {
                Some(directory) => match read_directory(&directory) {
                    Ok(entries) => (Some(directory), None, None, entries, None),
                    Err(error) => (
                        Some(path),
                        None,
                        None,
                        Vec::new(),
                        Some(format!("{error:#}")),
                    ),
                },
                None => (
                    None,
                    None,
                    None,
                    Vec::new(),
                    Some("The provided path has no parent directory.".to_owned()),
                ),
            }
        }
    } else {
        match default_directory(preferences.default_location.as_deref()) {
            Ok(directory) => match read_directory(&directory) {
                Ok(entries) => (Some(directory), None, None, entries, None),
                Err(error) => (
                    Some(directory),
                    None,
                    None,
                    Vec::new(),
                    Some(format!("{error:#}")),
                ),
            },
            Err(error) => (None, None, None, Vec::new(), Some(format!("{error:#}"))),
        }
    };

    gpui::application()
        .with_assets(IconAssets)
        .run(move |cx: &mut App| {
            gpui::init(cx);
            cx.bind_keys(toolbar::key_bindings());
            let default_size = size(px(1100.0), px(720.0));
            let bounds = Bounds::centered(None, default_size, cx);
            gpui::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(default_size),
                    titlebar: Some(TitlebarOptions {
                        title: Some("zarchive".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(12.0), px(14.0))),
                        ..Default::default()
                    }),
                    #[cfg(not(target_os = "macos"))]
                    window_decorations: Some(gpui::WindowDecorations::Client),
                    app_id: Some("zarchive".into()),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| {
                        let view = cx.entity().downgrade();
                        window
                            .observe_window_appearance(move |_, cx| {
                                let _ = view.update(cx, |_, cx| cx.notify());
                            })
                            .detach();
                        let directory_history = directory_history(current_directory.as_deref());
                        let navigation_history_index = directory_history.len().saturating_sub(1);
                        ArchiveManager {
                            current_directory,
                            directory_history,
                            navigation_history_index,
                            navigation_revision: 0,
                            navigation_control: task_queue::TaskControl::default(),
                            password_prompt: password::PasswordPrompt::new(
                                cx,
                                if startup_requires_password {
                                    archive_path.clone()
                                } else {
                                    None
                                },
                            ),
                            archive_path,
                            archive_name,
                            archive_password: None,
                            entries,
                            error,
                            selected_folder: String::new(),
                            selected_entry: None,
                            content_focus: cx.focus_handle(),
                            sort_column: SortColumn::Name,
                            sort_ascending: true,
                            directory_sizes: None,
                            directory_size_task: None,
                            directory_size_cancel: None,
                            opened_files: Vec::new(),
                            status_message: None,
                            notifications: Vec::new(),
                            notifications_visible: false,
                            language: preferences.language,
                            theme_mode: preferences.theme_mode,
                            accent_color: preferences.accent_color,
                            default_location: preferences.default_location.clone(),
                            auto_calculate_folder_sizes: preferences.auto_calculate_folder_sizes,
                            modal: if startup_requires_password {
                                Some(Modal::Password)
                            } else {
                                None
                            },
                            settings_category: SettingsCategory::General,
                            modal_focus: cx.focus_handle(),
                            creation: creation::ArchiveCreationForm::new(cx),
                            creation_task: None,
                            password_blur: blur::PasswordBlur::default(),
                            controls: ui::Controls::default(),
                            file_scroll: gpui::UniformListScrollHandle::new(),
                            folder_scroll: gpui::ScrollHandle::new(),
                            toolbar_search: toolbar::SearchField::default(),
                            filters: filters::FilterBar::default(),
                            file_actions: file_actions::FileActions::default(),
                            inspector: inspector::Inspector::default(),
                            comparison_dialog: inspector::ComparisonDialog::default(),
                            background_queue: task_queue::BackgroundQueue::default(),
                        }
                    })
                },
            )
            .expect("Failed to open the application window.");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::{
        ArchiveEntry, SortColumn, entry_icon, file_icon, icon_aspect_ratio, sort_entries,
        visible_entries,
    };

    #[gpui::test]
    fn folder_size_preference_controls_loading_sorting_navigation_and_cancellation(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for root in [directory.path(), second.path()] {
            std::fs::create_dir(root.join("nested")).unwrap();
            std::fs::write(root.join("nested/data.bin"), [0_u8; 128]).unwrap();
        }
        let (view, cx) = component_window(cx, |_, cx| {
            let mut manager = test_manager(cx);
            manager.auto_calculate_folder_sizes = false;
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = super::read_directory(directory.path()).unwrap();
            manager
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.directory_size_task.is_none());
            assert!(manager.directory_sizes.is_none());
            assert_eq!(manager.location_size(&manager.entries), None);
        });
        view.update(cx, |manager, cx| {
            manager.sort_by(SortColumn::Size, cx);
            assert!(manager.directory_size_task.is_none());
            manager.navigate_to(second.path().to_path_buf(), cx);
            assert!(manager.directory_size_task.is_none());
        });
        cx.run_until_parked();
        cx.update(|window, cx| view.update(cx, |manager, cx| {
            manager.set_auto_calculate_folder_sizes(true, window, cx);
            assert!(manager.directory_size_task.is_some());
            let cancel = manager.directory_size_cancel.as_ref().unwrap().clone();
            manager.set_auto_calculate_folder_sizes(false, window, cx);
            assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
            assert!(manager.directory_size_task.is_none());
        }));
        cx.run_until_parked();
        cx.read(|cx| assert!(view.read(cx).directory_sizes.is_none()));
        let settings_path = directory.path().join("settings.json");
        view.update(cx, |manager, _| manager.save_preferences_to(&settings_path));
        assert!(!super::UserPreferences::load(&settings_path).auto_calculate_folder_sizes);
        cx.update(|window, cx| view.update(cx, |manager, cx| manager.set_auto_calculate_folder_sizes(true, window, cx)));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.directory_size_task.is_none());
            assert!(manager.directory_size_cancel.is_none());
            assert_eq!(manager.directory_sizes.as_ref().unwrap().get("nested"), Some(&128));
            assert_eq!(manager.location_size(&manager.entries), Some(128));
        });
    }

    /// Builds a minimal archive entry for browser and sorting tests.
    fn entry(name: &str, size: u64, is_directory: bool) -> ArchiveEntry {
        ArchiveEntry {
            name: name.to_owned(),
            source_path: Some(name.to_owned()),
            size,
            is_directory,
            modified: None,
            is_encrypted: false,
        }
    }

    /// Creates an archive manager populated with deterministic test state.
    pub(crate) fn test_manager(
        cx: &mut gpui::Context<super::ArchiveManager>,
    ) -> super::ArchiveManager {
        use super::{AccentColor, ArchiveManager, Language, ThemeMode};
        ArchiveManager {
            current_directory: Some(std::path::PathBuf::from("/test")),
            directory_history: Vec::new(),
            navigation_history_index: 0,
            navigation_revision: 0,
            navigation_control: crate::task_queue::TaskControl::default(),
            archive_path: None,
            archive_name: None,
            archive_password: None,
            entries: vec![
                entry("Beta.txt", 300, false),
                entry("alpha.zip", 20, false),
                entry("Folder", 0, true),
                entry("gamma.txt", 10, false),
            ],
            error: None,
            selected_folder: String::new(),
            selected_entry: None,
            content_focus: cx.focus_handle(),
            sort_column: SortColumn::Name,
            sort_ascending: true,
            directory_sizes: None,
            directory_size_task: None,
            directory_size_cancel: None,
            opened_files: Vec::new(),
            status_message: None,
            notifications: Vec::new(),
            notifications_visible: false,
            language: Language::English,
            theme_mode: ThemeMode::System,
            accent_color: AccentColor::Blue,
            default_location: None,
            auto_calculate_folder_sizes: true,
            modal: None,
            settings_category: super::SettingsCategory::General,
            modal_focus: cx.focus_handle(),
            creation: crate::creation::ArchiveCreationForm::new(cx),
            creation_task: None,
            password_prompt: crate::password::PasswordPrompt::new(cx, None),
            password_blur: crate::blur::PasswordBlur::default(),
            controls: crate::ui::Controls::default(),
            file_scroll: gpui::UniformListScrollHandle::new(),
            folder_scroll: gpui::ScrollHandle::new(),
            toolbar_search: crate::toolbar::SearchField::default(),
            filters: crate::filters::FilterBar::default(),
            file_actions: crate::file_actions::FileActions::default(),
            inspector: crate::inspector::Inspector::default(),
            comparison_dialog: crate::inspector::ComparisonDialog::default(),
            background_queue: crate::task_queue::BackgroundQueue::default(),
        }
    }

    /// Opens a test window with GPUI Kit initialized and the app root installed.
    pub(crate) fn component_window<V: gpui::Render>(
        cx: &mut gpui::TestAppContext,
        build: impl FnOnce(&mut gpui::Window, &mut gpui::Context<V>) -> V,
    ) -> (gpui::Entity<V>, &mut gpui::VisualTestContext) {
        use gpui::AppContext;
        cx.update(|cx| {
            if !cx.has_global::<gpui::component::Theme>() {
                gpui::init(cx);
            }
            cx.bind_keys(crate::toolbar::key_bindings());
        });
        let mut content = None;
        let (_, window_cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| build(window, cx));
            content = Some(view.clone());
            gpui::base::Root::new(view, window, cx)
        });
        window_cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        window_cx.run_until_parked();
        (content.unwrap(), window_cx)
    }

    #[gpui::test]
    /// Covers Back, Forward, and discarding stale forward history on a new branch.
    fn directory_navigation_supports_back_forward_and_branch_reset(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let root_path = root.path().to_path_buf();
        let (view, cx) = component_window(cx, |_, cx| test_manager(cx));

        view.update(cx, |manager, cx| {
            manager.current_directory = Some(root_path.clone());
            manager.directory_history = super::directory_history(Some(&root_path));
            manager.navigation_history_index = manager.directory_history.len() - 1;

            manager.navigate_to(first.clone(), cx);
            assert!(manager.can_navigate_back());
            assert!(!manager.can_navigate_forward());

            manager.navigate_back(cx);
            assert_eq!(manager.current_directory.as_ref(), Some(&root_path));
            assert!(manager.can_navigate_forward());

            manager.navigate_forward(cx);
            assert_eq!(manager.current_directory.as_ref(), Some(&first));

            manager.navigate_back(cx);
            manager.navigate_to(second.clone(), cx);
            assert_eq!(manager.current_directory.as_ref(), Some(&second));
            assert!(!manager.can_navigate_forward());
        });
    }

    #[gpui::test]
    /// Opens archive creation from its platform keyboard shortcut.
    fn create_archive_shortcut_opens_the_create_modal(cx: &mut gpui::TestAppContext) {
        let (view, cx) = component_window(cx, |_, cx| test_manager(cx));
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| window.focus(&manager.content_focus, cx));
        });

        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-n");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-n");
        cx.run_until_parked();

        cx.read(|cx| assert!(view.read(cx).modal == Some(super::Modal::CreateArchive)));
    }

    #[gpui::test]
    /// Filters visible rows case-insensitively as the toolbar search query changes.
    fn toolbar_search_filters_file_rows_case_insensitively(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let (view, cx) = component_window(cx, |_, cx| test_manager(cx));

        cx.update(|window, cx| window.click("toolbar-search-input", cx));
        cx.simulate_keystrokes("BETA");
        cx.run_until_parked();

        assert!(cx.debug_bounds("file:Beta.txt").is_some());
        assert!(cx.debug_bounds("file:alpha.zip").is_none());
        cx.read(|cx| assert_eq!(view.read(cx).toolbar_search.query, "beta"));
    }

    #[gpui::test]
    fn toolbar_search_in_directory_finds_files_inside_archives(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("needle.txt");
        for (name, contents) in [("first.zip", b"first archive".as_slice()), ("second.zip", b"second archive".as_slice())] {
            std::fs::write(&source, contents).unwrap();
            crate::archive::create_zip(&directory.path().join(name), std::slice::from_ref(&source)).unwrap();
        }
        let (view, cx) = component_window(cx, |_, cx| {
            let mut manager = test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = crate::browser::read_directory(directory.path()).unwrap();
            manager
        });
        cx.update(|window, cx| window.click("toolbar-search-input", cx));
        cx.simulate_keystrokes("needle");
        cx.run_until_parked();
        cx.update(|window, cx| window.render_frame(cx));
        assert!(cx.debug_bounds("search-results-skeleton").is_some());
        let first = cx.debug_bounds("search-loading-row-0").unwrap();
        let second = cx.debug_bounds("search-loading-row-1").unwrap();
        assert_eq!(first.size.height, gpui::px(28.0));
        assert_eq!(second.origin.y - first.origin.y, gpui::px(28.0));
        cx.executor().advance_clock(std::time::Duration::from_millis(200));
        cx.run_until_parked();
        cx.update(|window, cx| window.render_frame(cx));
        assert!(cx.debug_bounds("search-results-skeleton").is_none());
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.toolbar_search.matches.len(), 2);
            assert!(manager.current_directory.is_some());
            assert!(manager.archive_path.is_none());
        });
        let key = cx.read(|cx| view.read(cx).toolbar_search.matches.values()
            .find(|result| result.target.path == directory.path().join("first.zip"))
            .unwrap().target.key.clone());
        cx.update(|window, cx| window.click(key, cx));
        cx.read(|cx| {
            let manager = view.read(cx);
            let target = manager.selected_search_target().unwrap();
            assert_eq!(target.path, directory.path().join("first.zip"));
            assert_eq!(target.archive_entry.as_deref(), Some("needle.txt"));
            assert_eq!(manager.inspection_target().unwrap().0, target.path);
            assert!(manager.can_extract_current_archive());
        });
        cx.update(|window, cx| window.click("toolbar-search-input", cx));
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-a backspace");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-a backspace");
        cx.executor().advance_clock(std::time::Duration::from_millis(200));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.toolbar_search.matches.is_empty());
            assert!(manager.selected_search_target().is_none());
            assert!(manager.toolbar_search.query.is_empty());
        });
        cx.simulate_keystrokes("needle");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-a");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_keystrokes("missing");
        cx.executor().advance_clock(std::time::Duration::from_millis(200));
        cx.run_until_parked();
        cx.read(|cx| assert!(view.read(cx).toolbar_search.matches.is_empty()));
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-a");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_keystrokes("needle");
        view.update(cx, |manager, cx| manager.navigate_to(source_root.path().to_path_buf(), cx));
        cx.executor().advance_clock(std::time::Duration::from_millis(200));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.current_directory.as_deref(), Some(source_root.path()));
            assert!(manager.toolbar_search.matches.is_empty());
        });
    }

    #[gpui::test]
    fn toolbar_search_finds_nested_archive_files_and_keeps_actions_targeted(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        use std::io::{Read, Write};
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("search.zip");
        let mut archive = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
        for (name, contents) in [
            ("./docs/deep/Needle.TXT", b"first result".as_slice()),
            ("other/needle.txt", b"second result".as_slice()),
            ("readme.txt", b"unrelated".as_slice()),
        ] {
            archive.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            archive.write_all(contents).unwrap();
        }
        archive.finish().unwrap();
        let (view, cx) = component_window(cx, |_, cx| {
            let mut manager = test_manager(cx);
            manager.current_directory = None;
            manager.archive_path = Some(archive_path.clone());
            manager.archive_name = Some("search.zip".to_owned());
            manager.entries = crate::archive::list_archive(&archive_path).unwrap();
            manager
        });
        assert!(cx.debug_bounds("file:docs").is_some());
        assert!(cx.debug_bounds("file:./docs/deep/Needle.TXT").is_none());
        cx.update(|window, cx| window.click("toolbar-search-input", cx));
        cx.simulate_keystrokes("NEEDLE");
        cx.run_until_parked();
        let result = cx.debug_bounds("file:./docs/deep/Needle.TXT").unwrap();
        assert!(cx.debug_bounds("file:other/needle.txt").is_some());
        assert!(cx.debug_bounds("file:readme.txt").is_none());
        cx.simulate_click(result.center(), gpui::Modifiers::default());
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.selected_entry.as_deref(), Some("./docs/deep/Needle.TXT"));
            let mut extracted = crate::archive::extract_entry(&archive_path, manager.selected_entry.as_deref().unwrap()).unwrap();
            let mut contents = String::new();
            extracted.read_to_string(&mut contents).unwrap();
            assert_eq!(contents, "first result");
        });
        cx.update(|window, cx| window.click("toolbar-search-input", cx));
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-a backspace");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-a backspace");
        cx.run_until_parked();
        assert!(cx.debug_bounds("file:docs").is_some());
        assert!(cx.debug_bounds("file:./docs/deep/Needle.TXT").is_none());
    }

    #[gpui::test]
    /// Verifies rendered name/type/size sorting and status-bar size updates.
    fn clicking_type_and_size_headers_reorders_rendered_rows(cx: &mut gpui::TestAppContext) {
        use gpui::Modifiers;

        let (view, cx) = component_window(cx, |_, cx| test_manager(cx));

        for (selector, column, ascending, expected) in [
            (
                "sort-type",
                SortColumn::Type,
                true,
                [
                    "file:Folder",
                    "file:Beta.txt",
                    "file:gamma.txt",
                    "file:alpha.zip",
                ],
            ),
            (
                "sort-type",
                SortColumn::Type,
                false,
                [
                    "file:Folder",
                    "file:alpha.zip",
                    "file:gamma.txt",
                    "file:Beta.txt",
                ],
            ),
            (
                "sort-size",
                SortColumn::Size,
                true,
                [
                    "file:Folder",
                    "file:gamma.txt",
                    "file:alpha.zip",
                    "file:Beta.txt",
                ],
            ),
            (
                "sort-size",
                SortColumn::Size,
                false,
                [
                    "file:Folder",
                    "file:Beta.txt",
                    "file:alpha.zip",
                    "file:gamma.txt",
                ],
            ),
        ] {
            let bounds = cx.debug_bounds(selector).expect("header is rendered");
            cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();
            cx.read(|cx| {
                let manager = view.read(cx);
                assert_eq!(manager.sort_column, column);
                assert_eq!(manager.sort_ascending, ascending);
            });
            let positions: Vec<_> = expected
                .into_iter()
                .map(|selector| cx.debug_bounds(selector).expect("row is rendered").origin.y)
                .collect();
            assert!(
                positions.windows(2).all(|pair| pair[0] < pair[1]),
                "wrong rendered order for {column:?}, ascending={ascending}: {positions:?}"
            );
        }

        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("alpha-small")).unwrap();
        std::fs::create_dir_all(directory.path().join("middle-large/nested")).unwrap();
        std::fs::create_dir_all(directory.path().join("zeta-empty")).unwrap();
        std::fs::write(directory.path().join("alpha-small/file.txt"), [0_u8; 10]).unwrap();
        std::fs::write(
            directory.path().join("middle-large/nested/file.bin"),
            [0_u8; 100],
        )
        .unwrap();
        view.update(cx, |manager, cx| {
            manager.navigate_to(directory.path().to_path_buf(), cx);
            assert_eq!(manager.location_size(&manager.entries), None);
            manager.status_message = Some("Operation completed".to_owned());
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.directory_size_task.is_none());
            assert_eq!(manager.location_size(&manager.entries), Some(110));
            assert_eq!(
                manager.status_message.as_deref(),
                Some("Operation completed")
            );
            let sizes = manager
                .directory_sizes
                .as_ref()
                .expect("folder sizes were calculated");
            assert_eq!(sizes["alpha-small"], 10);
            assert_eq!(sizes["middle-large"], 100);
            assert_eq!(sizes["zeta-empty"], 0);
            let mut entries = manager.entries.clone();
            sort_entries(&mut entries, manager.sort_column, manager.sort_ascending);
            assert_eq!(
                entries
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>(),
                ["middle-large", "alpha-small", "zeta-empty"]
            );
        });
        let bar = cx
            .debug_bounds("status-bar")
            .expect("status bar is rendered");
        let message = cx
            .debug_bounds("status-message")
            .expect("status message is rendered");
        let size = cx
            .debug_bounds("location-size")
            .expect("location size is rendered");
        let list = cx.debug_bounds("file-list").expect("file list is rendered");
        assert!(message.origin.y >= bar.origin.y);
        assert!(message.bottom() <= bar.bottom());
        assert!(message.right() <= size.origin.x);
        assert!(size.right() <= bar.right());
        assert!(list.bottom() <= bar.origin.y);

        view.update(cx, |manager, cx| {
            manager.current_directory = None;
            manager.archive_path = Some(std::path::PathBuf::from("folders.zip"));
            manager.archive_name = Some("folders.zip".to_owned());
            manager.directory_sizes = None;
            manager.sort_column = SortColumn::Name;
            manager.sort_ascending = true;
            manager.entries = vec![
                entry("alpha-small/file.txt", 10, false),
                entry("middle-large/nested/file.bin", 100, false),
                entry("zeta-empty/", 0, true),
            ];
            let root = visible_entries(&manager.entries, "");
            assert_eq!(manager.location_size(&root), Some(110));
            let nested = visible_entries(&manager.entries, "middle-large");
            assert_eq!(manager.location_size(&nested), Some(100));
            let empty = visible_entries(&manager.entries, "zeta-empty");
            assert_eq!(manager.location_size(&empty), Some(0));
            cx.notify();
        });
        cx.run_until_parked();
        for expected in [
            ["file:zeta-empty", "file:alpha-small", "file:middle-large"],
            ["file:middle-large", "file:alpha-small", "file:zeta-empty"],
        ] {
            let bounds = cx.debug_bounds("sort-size").unwrap();
            cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();
            let positions: Vec<_> = expected
                .into_iter()
                .map(|selector| {
                    cx.debug_bounds(selector)
                        .expect("folder row is rendered")
                        .origin
                        .y
                })
                .collect();
            assert!(
                positions.windows(2).all(|pair| pair[0] < pair[1]),
                "archive folders are not sorted by total size: {positions:?}"
            );
        }
    }

    #[gpui::test]
    /// Keeps the table header and status bar fixed while a long file list scrolls.
    fn long_file_list_scrolls_without_moving_header_or_status_bar(cx: &mut gpui::TestAppContext) {
        let (view, cx) = component_window(cx, |_, cx| {
            let mut manager = test_manager(cx);
            manager.entries = (0..2_000)
                .map(|index| entry(&format!("file-{index:03}.txt"), index, false))
                .collect();
            manager
        });
        let header = cx.debug_bounds("sort-name").unwrap();
        let status = cx.debug_bounds("status-bar").unwrap();
        let main_panel = cx.debug_bounds("main-panel").unwrap();
        let file_list = cx.debug_bounds("file-list").unwrap();
        assert_eq!(file_list.size.width, main_panel.size.width);
        assert!(cx.debug_bounds("file:file-999.txt").is_none());
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.file_scroll.0.borrow().base_handle.max_offset().y > gpui::px(0.0));
        });
        cx.update(|_, cx| {
            view.update(cx, |manager, cx| {
                manager
                    .file_scroll
                    .scroll_to_item(1_999, gpui::ScrollStrategy::Bottom);
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(view.read(cx).file_scroll.0.borrow().base_handle.offset().y < gpui::px(0.0))
        });
        assert!(cx.debug_bounds("file:file-999.txt").is_some());
        assert_eq!(cx.debug_bounds("sort-name").unwrap(), header);
        assert_eq!(cx.debug_bounds("status-bar").unwrap(), status);
    }

    #[gpui::test]
    /// Displays loading, error, completion, and inspection-limit indicators as appropriate.
    fn status_bar_shows_loading_and_error_indicators(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, cx) = component_window(cx, |_, cx| {
            let mut manager = test_manager(cx);
            manager.directory_sizes = Some(std::collections::BTreeMap::new());
            manager.directory_size_task = Some(1);
            manager
        });
        assert!(cx.debug_bounds("status-loading-spinner").is_none());
        assert!(cx.debug_bounds("notifications-toast").is_none());
        assert!(cx.debug_bounds("status-notifications").is_some());

        view.update(cx, |manager, cx| {
            manager.directory_size_task = None;
            manager.status_message = Some(format!(
                "{}: invalid archive",
                manager.language.text("Failed to create archive")
            ));
            cx.notify();
        });
        cx.run_until_parked();
        let toast = cx.debug_bounds("notifications-toast").unwrap();
        let status = cx.debug_bounds("status-bar").unwrap();
        assert!(toast.size.width <= gpui::px(360.0));
        assert!(toast.size.height <= gpui::px(240.0));
        assert!(toast.bottom() < status.top());
        assert_eq!(toast.right(), status.right() - gpui::px(12.0));
        cx.read(|cx| {
            assert_eq!(view.read(cx).notifications.len(), 1);
            assert!(view.read(cx).notifications[0].0);
        });
        assert!(cx.debug_bounds("status-loading-spinner").is_none());

        let toggle = cx.debug_bounds("status-notifications").unwrap();
        cx.simulate_click(toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_none());
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_none());
        cx.read(|cx| assert_eq!(view.read(cx).notifications.len(), 1));
        cx.simulate_click(toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_some());

        cx.simulate_click(
            gpui::point(toast.right() - gpui::px(18.0), toast.top() + gpui::px(14.0)),
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_none());

        view.update(cx, |manager, cx| {
            manager.language = super::Language::Polish;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_none());
        cx.read(|cx| assert_eq!(view.read(cx).notifications.len(), 1));

        view.update(cx, |manager, cx| {
            manager.status_message = Some("Failed to extract: another invalid archive".to_owned());
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_some());

        view.update(cx, |manager, cx| {
            manager.status_message = Some("Operation completed".to_owned());
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_none());
        assert!(cx.debug_bounds("status-loading-spinner").is_none());

        view.update(cx, |manager, cx| {
            manager.inspector.visible = true;
            manager.inspector.batch_limited = true;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_some());
        cx.read(|cx| {
            assert_eq!(view.read(cx).notifications.len(), 1);
            assert!(!view.read(cx).notifications[0].0);
        });

        view.update(cx, |manager, cx| {
            manager.inspector.visible = false;
            manager.status_message = Some("Failed to extract: invalid archive".to_owned());
            manager.directory_size_task = Some(1);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_some());
        assert!(cx.debug_bounds("status-loading-spinner").is_none());
        cx.read(|cx| {
            let notifications = &view.read(cx).notifications;
            assert_eq!(notifications.iter().filter(|(error, _)| *error).count(), 1);
            assert_eq!(notifications.iter().filter(|(error, _)| !*error).count(), 1);
        });

        view.update(cx, |manager, cx| {
            manager.inspector.visible = false;
            manager.inspector.batch_limited = false;
            manager.status_message = None;
            manager.directory_size_task = None;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_none());

        view.update(cx, |manager, cx| {
            manager.status_message = None;
            manager.error = Some("Archive could not be read".to_owned());
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("notifications-toast").is_some());
        cx.read(|cx| assert_eq!(view.read(cx).notifications.len(), 1));
    }

    #[gpui::test]
    /// Distinguishes single-click selection from double-click archive opening.
    fn single_click_selects_files_and_double_click_opens_archive(cx: &mut gpui::TestAppContext) {
        use gpui::Modifiers;
        let directory = tempfile::tempdir().unwrap();
        let notes = directory.path().join("notes.txt");
        let archive = directory.path().join("archive.zip");
        std::fs::write(&notes, b"notes").unwrap();
        crate::archive::create_archive(
            &archive,
            &[notes],
            crate::archive::CreateOptions::default(),
        )
        .unwrap();
        let (view, cx) = component_window(cx, |_, cx| {
            let mut manager = test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = vec![
                entry("notes.txt", 5, false),
                entry("archive.zip", 100, false),
            ];
            for entry in &mut manager.entries {
                entry.source_path = None;
            }
            manager
        });
        for (selector, name) in [
            ("file:notes.txt", "notes.txt"),
            ("file:archive.zip", "archive.zip"),
        ] {
            let bounds = cx.debug_bounds(selector).unwrap();
            cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.read(|cx| {
                let manager = view.read(cx);
                assert_eq!(manager.selected_entry.as_deref(), Some(name));
                assert!(manager.archive_path.is_none());
                assert!(manager.status_message.is_none());
                assert!(manager.opened_files.is_empty());
            });
        }
        cx.update(|window, cx| {
            use gpui::test::TestWindowExt;
            window.double_click("archive.zip", cx);
        });
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(view.read(cx).archive_path.as_ref(), Some(&archive)));
        let bounds = cx.debug_bounds("file:notes.txt").unwrap();
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.selected_entry.as_deref(), Some("notes.txt"));
            assert!(manager.opened_files.is_empty());
            assert!(manager.status_message.is_none());
        });
    }

    #[test]
    /// Maps common extensions to meaningful file-type icons.
    fn selects_meaningful_icons_for_common_file_types() {
        assert_eq!(file_icon("pdf"), "pdf");
        assert_eq!(file_icon("PNG"), "image");
        assert_eq!(file_icon("csv"), "csv");
        assert_eq!(file_icon("zip"), "archive");
        for extension in ["bz2", "tbz", "tbz2", "xz", "txz", "zst", "lz4", "lz", "br"] {
            assert_eq!(file_icon(extension), "archive");
        }
        for extension in [
            "bin", "dat", "exe", "dll", "so", "dylib", "class", "o", "obj", "wasm",
            "db", "sqlite", "sqlite3", "iso",
        ] {
            assert_eq!(file_icon(extension), "binary");
        }
        assert_eq!(file_icon("unknown"), "file");
        let protected_archive = ArchiveEntry {
            name: "secret.zip".into(),
            source_path: None,
            size: 0,
            modified: None,
            is_directory: false,
            is_encrypted: true,
        };
        assert_eq!(entry_icon(&protected_archive), "archive-locked");
        let csv = ArchiveEntry {
            name: "data.csv".into(),
            source_path: None,
            size: 0,
            modified: None,
            is_directory: false,
            is_encrypted: false,
        };
        assert_eq!(entry_icon(&csv), "csv");
    }

    #[test]
    /// Preserves the expected aspect ratios for bundled icons with non-square view boxes.
    fn preserves_font_awesome_icon_aspect_ratios() {
        assert_eq!(icon_aspect_ratio("document"), 0.75);
        assert_eq!(icon_aspect_ratio("documents"), 0.875);
        assert_eq!(icon_aspect_ratio("code"), 0.75);
        assert_eq!(icon_aspect_ratio("folder"), 1.0);
        assert_eq!(icon_aspect_ratio("binary"), 1.0);
        assert_eq!(icon_aspect_ratio("csv"), 0.75);
        assert_eq!(icon_aspect_ratio("archive-locked"), 0.75);
        assert_eq!(icon_aspect_ratio("chevron-left"), 0.625);
        assert_eq!(icon_aspect_ratio("chevron-up"), 1.0);
        assert_eq!(icon_aspect_ratio("chevron-down"), 1.0);
    }
}
