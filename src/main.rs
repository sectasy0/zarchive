mod archive;
mod conversion;
mod shortcuts;
mod app;
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
mod recovery;
mod rar_recovery;
mod parity;
mod secret;
mod security;
mod toolbar;
mod text_preview;
mod task_queue;
mod transfer;
mod ui;
mod volumes;

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
pub(crate) use preferences::{AccentColor, AdvancedSettings, ThemeMode, UserPreferences};
use preferences::{load_user_preferences, user_preferences_path};
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::NamedTempFile;
pub(crate) use toolbar::toolbar_button;
pub(crate) use app::{app_icon, format_size};
#[cfg(test)]
use app::{entry_icon, file_icon, icon_aspect_ratio};

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
    Recovery,
    Conversion,
    Info,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
/// Selects the active page in the Settings dialog.
enum SettingsCategory {
    /// General preferences such as language and default location.
    #[default]
    General,
    /// Theme and accent appearance preferences.
    Appearance,
    /// Preview size and table resource limits.
    Advanced,
    Keyboard,
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
    browsers: Vec<BrowserPane>,
    active_browser: usize,
    inspector_focus: bool,
    content_focus: FocusHandle,
    opened_files: Vec<NamedTempFile>,
    status_message: Option<String>,
    notifications: Vec<(bool, String)>,
    notifications_visible: bool,
    language: Language,
    theme_mode: ThemeMode,
    accent_color: AccentColor,
    default_location: Option<PathBuf>,
    auto_calculate_folder_sizes: bool,
    advanced_settings: AdvancedSettings,
    shortcut_settings: shortcuts::ShortcutSettings,
    shortcut_editor: shortcuts::Editor,
    preferences_path: Option<PathBuf>,
    modal: Option<Modal>,
    settings_category: SettingsCategory,
    modal_focus: FocusHandle,
    creation: creation::ArchiveCreationForm,
    creation_task: Option<u64>,
    password_prompt: password::PasswordPrompt,
    password_blur: blur::PasswordBlur,
    controls: ui::Controls,
    file_actions: file_actions::FileActions,
    inspector: inspector::Inspector,
    comparison_dialog: inspector::ComparisonDialog,
    recovery: recovery::State,
    conversion: conversion::State,
    background_queue: task_queue::BackgroundQueue,
}

/// Owns the navigation and view state for one browser tab.
struct BrowserTab {
    id: u64,
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
    sort_column: SortColumn,
    sort_ascending: bool,
    directory_sizes: Option<BTreeMap<String, u64>>,
    directory_size_task: Option<u64>,
    directory_size_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    file_scroll: gpui::UniformListScrollHandle,
    folder_scroll: gpui::ScrollHandle,
    toolbar_search: toolbar::SearchField,
    filters: filters::FilterBar,
    drop_marker_row: Option<usize>,
}

static NEXT_BROWSER_TAB_ID: AtomicU64 = AtomicU64::new(1);

impl BrowserTab {
    fn new(
        current_directory: Option<PathBuf>,
        archive_path: Option<PathBuf>,
        archive_name: Option<String>,
        entries: Vec<ArchiveEntry>,
        error: Option<String>,
    ) -> Self {
        let directory_history = directory_history(current_directory.as_deref());
        let navigation_history_index = directory_history.len().saturating_sub(1);
        Self {
            id: NEXT_BROWSER_TAB_ID.fetch_add(1, Ordering::Relaxed),
            current_directory,
            directory_history,
            navigation_history_index,
            navigation_revision: 0,
            navigation_control: task_queue::TaskControl::default(),
            archive_path,
            archive_name,
            archive_password: None,
            entries,
            error,
            selected_folder: String::new(),
            selected_entry: None,
            sort_column: SortColumn::Name,
            sort_ascending: true,
            directory_sizes: None,
            directory_size_task: None,
            directory_size_cancel: None,
            file_scroll: gpui::UniformListScrollHandle::new(),
            folder_scroll: gpui::ScrollHandle::new(),
            toolbar_search: toolbar::SearchField::default(),
            filters: filters::FilterBar::default(),
            drop_marker_row: None,
        }
    }
}

struct BrowserPane {
    tabs: Vec<BrowserTab>,
    active_tab: usize,
    tab_scroll: gpui::ScrollHandle,
}

impl BrowserPane {
    fn new(tab: BrowserTab) -> Self {
        Self {
            tabs: vec![tab],
            active_tab: 0,
            tab_scroll: gpui::ScrollHandle::new(),
        }
    }
}

impl Drop for BrowserTab {
    fn drop(&mut self) {
        self.navigation_control.cancel();
        if let Some(cancel) = &self.directory_size_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn two_browser_panes(
    current_directory: Option<PathBuf>,
    archive_path: Option<PathBuf>,
    archive_name: Option<String>,
    entries: Vec<ArchiveEntry>,
    error: Option<String>,
) -> Vec<BrowserPane> {
    (0..2)
        .map(|_| {
            BrowserPane::new(BrowserTab::new(
                current_directory.clone(),
                archive_path.clone(),
                archive_name.clone(),
                entries.clone(),
                error.clone(),
            ))
        })
        .collect()
}

impl ArchiveManager {
    fn active_browser_tab(&self) -> &BrowserTab {
        &self.browsers[self.active_browser].tabs[self.browsers[self.active_browser].active_tab]
    }

    fn active_browser_tab_mut(&mut self) -> &mut BrowserTab {
        let pane = &mut self.browsers[self.active_browser];
        &mut pane.tabs[pane.active_tab]
    }

    fn with_browser_tab<R>(
        &mut self,
        id: u64,
        action: impl FnOnce(&mut Self) -> R,
    ) -> Option<R> {
        let (pane_index, tab_index) = self.browsers.iter().enumerate().find_map(|(pane_index, pane)| {
            pane.tabs.iter().position(|tab| tab.id == id).map(|tab_index| (pane_index, tab_index))
        })?;
        let previous_pane = self.active_browser;
        let previous_tab = self.browsers[previous_pane].active_tab;
        self.active_browser = pane_index;
        self.browsers[pane_index].active_tab = tab_index;
        let result = action(self);
        self.active_browser = previous_pane;
        self.browsers[previous_pane].active_tab = previous_tab.min(self.browsers[previous_pane].tabs.len() - 1);
        Some(result)
    }

    fn add_browser_tab(&mut self, pane_index: usize, cx: &mut Context<Self>) {
        let source = &self.browsers[pane_index].tabs[self.browsers[pane_index].active_tab];
        let mut tab = BrowserTab::new(
            source.current_directory.clone(),
            source.archive_path.clone(),
            source.archive_name.clone(),
            source.entries.clone(),
            source.error.clone(),
        );
        tab.archive_password = source.archive_password.clone();
        let pane = &mut self.browsers[pane_index];
        pane.tabs.push(tab);
        pane.active_tab = pane.tabs.len() - 1;
        pane.tab_scroll.scroll_to_item(pane.active_tab);
        self.active_browser = pane_index;
        cx.notify();
    }

    fn close_browser_tab(
        &mut self,
        pane_index: usize,
        tab_index: usize,
        cx: &mut Context<Self>,
    ) {
        let pane = &mut self.browsers[pane_index];
        if pane.tabs.len() <= 1 || tab_index >= pane.tabs.len() {
            return;
        }
        pane.tabs.remove(tab_index);
        if pane.active_tab > tab_index {
            pane.active_tab -= 1;
        } else if pane.active_tab == tab_index {
            pane.active_tab = tab_index.min(pane.tabs.len() - 1);
        }
        self.active_browser = pane_index;
        cx.notify();
    }

    fn close_active_browser_tab(&mut self, cx: &mut Context<Self>) {
        let pane_index = self.active_browser;
        let tab_index = self.browsers[pane_index].active_tab;
        self.close_browser_tab(pane_index, tab_index, cx);
    }
}

impl Deref for ArchiveManager {
    type Target = BrowserTab;

    fn deref(&self) -> &Self::Target {
        self.active_browser_tab()
    }
}

impl DerefMut for ArchiveManager {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.active_browser_tab_mut()
    }
}

impl ArchiveManager {
    /// Persists language, appearance, accent, and startup-location preferences.
    fn save_preferences(&mut self) {
        let Some(path) = self.preferences_path.clone() else {
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
            advanced: self.advanced_settings,
            shortcuts: self.shortcut_settings.clone(),
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
        let browser_tab_id = self.active_browser_tab().id;
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
                let _ = this.with_browser_tab(browser_tab_id, |this| {
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
                });
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
        if self.modal == Some(Modal::Recovery) {
            self.clear_recovery(window, cx);
        }
        if self.modal == Some(Modal::Conversion) {
            self.clear_conversion(window, cx);
        }
        if self.modal == Some(Modal::Info) {
            self.file_actions.info_target = None;
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
                    let history_index = self.navigation_history_index;
                    self.directory_history
                        .truncate(history_index.saturating_add(1));
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
        let browser_tab_id = self.active_browser_tab().id;
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
                let _ = manager.with_browser_tab(browser_tab_id, |manager| {
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
                });
            },
            cx,
        );
        cx.notify();
    }
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
            preferences.shortcuts.install(false, cx);
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
                        let initial_password_archive = startup_requires_password
                            .then(|| archive_path.clone())
                            .flatten();
                        let browsers = two_browser_panes(
                            current_directory,
                            archive_path,
                            archive_name,
                            entries,
                            error,
                        );
                        ArchiveManager {
                            browsers,
                            active_browser: 0,
                            inspector_focus: false,
                            password_prompt: password::PasswordPrompt::new(
                                cx,
                                initial_password_archive,
                            ),
                            content_focus: cx.focus_handle(),
                            opened_files: Vec::new(),
                            status_message: None,
                            notifications: Vec::new(),
                            notifications_visible: false,
                            language: preferences.language,
                            theme_mode: preferences.theme_mode,
                            accent_color: preferences.accent_color,
                            default_location: preferences.default_location.clone(),
                            auto_calculate_folder_sizes: preferences.auto_calculate_folder_sizes,
                            advanced_settings: preferences.advanced,
                            shortcut_settings: preferences.shortcuts.clone(),
                            shortcut_editor: shortcuts::Editor::default(),
                            preferences_path: user_preferences_path(),
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
                            file_actions: file_actions::FileActions::default(),
                            inspector: inspector::Inspector::default(),
                            comparison_dialog: inspector::ComparisonDialog::default(),
                            recovery: recovery::State::new(cx),
                            conversion: conversion::State::new(cx),
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
        AdvancedSettings, ArchiveEntry, SortColumn, entry_icon, file_icon,
        icon_aspect_ratio, sort_entries, visible_entries,
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
        let browsers = super::two_browser_panes(
            Some(std::path::PathBuf::from("/test")),
            None,
            None,
            vec![
                entry("Beta.txt", 300, false),
                entry("alpha.zip", 20, false),
                entry("Folder", 0, true),
                entry("gamma.txt", 10, false),
            ],
            None,
        );
        ArchiveManager {
            browsers,
            active_browser: 0,
            inspector_focus: false,
            content_focus: cx.focus_handle(),
            opened_files: Vec::new(),
            status_message: None,
            notifications: Vec::new(),
            notifications_visible: false,
            language: Language::English,
            theme_mode: ThemeMode::System,
            accent_color: AccentColor::Blue,
            default_location: None,
            auto_calculate_folder_sizes: true,
            advanced_settings: AdvancedSettings::default(),
            shortcut_settings: crate::shortcuts::ShortcutSettings::default(),
            shortcut_editor: crate::shortcuts::Editor::default(),
            preferences_path: None,
            modal: None,
            settings_category: super::SettingsCategory::General,
            modal_focus: cx.focus_handle(),
            creation: crate::creation::ArchiveCreationForm::new(cx),
            creation_task: None,
            password_prompt: crate::password::PasswordPrompt::new(cx, None),
            password_blur: crate::blur::PasswordBlur::default(),
            controls: crate::ui::Controls::default(),
            file_actions: crate::file_actions::FileActions::default(),
            inspector: crate::inspector::Inspector::default(),
            comparison_dialog: crate::inspector::ComparisonDialog::default(),
            recovery: crate::recovery::State::new(cx),
            conversion: crate::conversion::State::new(cx),
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
            crate::shortcuts::ShortcutSettings::default().install(false, cx);
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
    fn browser_panes_and_tabs_keep_independent_locations(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let left = root.path().join("left");
        let right = root.path().join("right");
        let second = right.join("second");
        let third = right.join("third");
        for directory in [&left, &right, &second, &third] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(left.join("left.txt"), b"left").unwrap();
        std::fs::write(right.join("right.txt"), b"right").unwrap();
        let (view, cx) = component_window(cx, |_, cx| {
            let mut manager = test_manager(cx);
            manager.auto_calculate_folder_sizes = false;
            manager.browsers[0].tabs[0] = super::BrowserTab::new(
                Some(left.clone()), None, None, super::read_directory(&left).unwrap(), None,
            );
            manager.browsers[1].tabs[0] = super::BrowserTab::new(
                Some(right.clone()), None, None, super::read_directory(&right).unwrap(), None,
            );
            manager
        });

        let left_panel = cx.debug_bounds("browser-pane").unwrap();
        let right_panel = cx.debug_bounds("browser-pane-secondary").unwrap();
        cx.simulate_click(right_panel.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(cx.debug_bounds("browser-pane").unwrap(), left_panel);
        assert_eq!(cx.debug_bounds("browser-pane-secondary").unwrap(), right_panel);
        cx.read(|cx| assert_eq!(view.read(cx).active_browser, 1));
        view.update(cx, |manager, cx| {
            manager.active_browser = 1;
            manager.navigate_to(second.clone(), cx);
            manager.add_browser_tab(1, cx);
            manager.navigate_to(third.clone(), cx);
            assert_eq!(manager.browsers[0].tabs[0].current_directory.as_ref(), Some(&left));
            assert_eq!(manager.browsers[1].tabs[0].current_directory.as_ref(), Some(&second));
            assert_eq!(manager.browsers[1].tabs[1].current_directory.as_ref(), Some(&third));
        });
    }

    #[gpui::test]
    fn browser_tab_strip_scrolls_and_keeps_new_tab_button_visible(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let (view, cx) = component_window(cx, |_, cx| test_manager(cx));
        view.update(cx, |manager, cx| {
            for _ in 0..11 {
                manager.add_browser_tab(1, cx);
            }
        });
        cx.run_until_parked();
        let scroll = cx.debug_bounds("browser-tabs-scroll:1").unwrap();
        let last_tab = cx.debug_bounds("browser-tab:1:11").unwrap();
        let add_button = cx.update(|window, _| {
            window
                .within("browser-pane-secondary")
                .find("new-browser-tab-1")
                .bounds()
        });
        let panel = cx.debug_bounds("browser-pane-secondary").unwrap();
        let offset = cx.read(|cx| view.read(cx).browsers[1].tab_scroll.offset().x);
        assert!(offset < gpui::px(0.0));
        assert!(last_tab.right() + offset <= scroll.right());
        assert!(add_button.right() <= panel.right());
        cx.read(|cx| assert_eq!(view.read(cx).browsers[1].tabs.len(), 12));
    }

    #[gpui::test]
    fn ctrl_w_closes_only_the_active_browser_tab(cx: &mut gpui::TestAppContext) {
        let (view, cx) = component_window(cx, |_, cx| test_manager(cx));
        view.update(cx, |manager, cx| {
            manager.add_browser_tab(1, cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| window.focus(&manager.content_focus, cx));
        });
        cx.simulate_keystrokes("ctrl-w");
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.browsers[1].tabs.len(), 1);
            assert_eq!(manager.browsers[1].active_tab, 0);
            assert_eq!(manager.browsers[0].tabs.len(), 1);
            assert_eq!(manager.active_browser, 1);
        });
    }

    #[gpui::test]
    fn ctrl_w_closes_inspector_tab_when_inspector_has_focus(cx: &mut gpui::TestAppContext) {
        let (view, cx) = component_window(cx, |_, cx| {
            let mut manager = test_manager(cx);
            manager
                .inspector
                .select_or_create(Some(std::path::PathBuf::from("first.zip")));
            manager
                .inspector
                .select_or_create(Some(std::path::PathBuf::from("second.zip")));
            manager
        });
        let inspector_tab = cx.debug_bounds("inspector-target-tab:1").unwrap();
        cx.simulate_click(inspector_tab.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| window.focus(&manager.content_focus, cx));
        });
        cx.simulate_keystrokes("ctrl-w");
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.inspector.test_session_count(), 1);
            assert!(manager.inspector.visible);
            assert_eq!(manager.browsers[0].tabs.len(), 1);
            assert_eq!(manager.browsers[1].tabs.len(), 1);
        });
    }

    #[gpui::test]
    fn dropping_folder_and_archive_onto_browser_tab_opens_them_there(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("Folder");
        std::fs::create_dir(&folder).unwrap();
        let source = root.path().join("inside.txt");
        std::fs::write(&source, b"inside").unwrap();
        let archive = root.path().join("bundle.zip");
        crate::archive::create_zip(&archive, std::slice::from_ref(&source)).unwrap();
        let root_path = root.path().to_path_buf();
        let entries = super::read_directory(&root_path).unwrap();
        let (view, cx) = component_window(cx, |_, cx| {
            let mut manager = test_manager(cx);
            manager.auto_calculate_folder_sizes = false;
            manager.browsers[0].tabs[0] = super::BrowserTab::new(
                Some(root_path.clone()), None, None, entries.clone(), None,
            );
            manager.browsers[1].tabs[0] = super::BrowserTab::new(
                Some(root_path.clone()), None, None, entries.clone(), None,
            );
            manager
        });

        fn drag_to_target(
            cx: &mut gpui::VisualTestContext,
            source: gpui::SharedString,
            destination: &'static str,
        ) {
            use gpui::test::TestWindowExt;
            let start = cx.update(|window, _| {
                window.within("browser-pane").find(source).bounds().center()
            });
            let end = cx.debug_bounds(destination).unwrap().center();
            cx.simulate_mouse_move(start, None, gpui::Modifiers::default());
            cx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.simulate_mouse_move(
                start + gpui::point(gpui::px(20.0), gpui::px(0.0)),
                gpui::MouseButton::Left,
                gpui::Modifiers::default(),
            );
            cx.run_until_parked();
            cx.simulate_mouse_move(end, gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.run_until_parked();
            cx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.run_until_parked();
        }

        drag_to_target(
            cx,
            gpui::SharedString::from(folder.to_string_lossy().into_owned()),
            "browser-tab:1:0",
        );
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.active_browser, 1);
            assert_eq!(manager.browsers[1].tabs[0].current_directory.as_ref(), Some(&folder));
            assert_eq!(manager.browsers[0].tabs[0].current_directory.as_ref(), Some(&root_path));
        });

        drag_to_target(
            cx,
            gpui::SharedString::from(archive.to_string_lossy().into_owned()),
            "browser-tab:1:0",
        );
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.active_browser, 1);
            assert_eq!(manager.browsers[1].tabs[0].archive_path.as_ref(), Some(&archive));
            assert!(manager.browsers[1].tabs[0].entries.iter().any(|entry| entry.name == "inside.txt"));
            assert_eq!(manager.browsers[0].tabs[0].current_directory.as_ref(), Some(&root_path));
        });

        drag_to_target(
            cx,
            gpui::SharedString::from(folder.to_string_lossy().into_owned()),
            "browser-tabs-scroll:1",
        );
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.browsers[1].tabs.len(), 2);
            assert_eq!(manager.browsers[1].active_tab, 1);
            assert_eq!(manager.browsers[1].tabs[1].current_directory.as_ref(), Some(&folder));
            assert_eq!(manager.browsers[1].tabs[0].archive_path.as_ref(), Some(&archive));
        });

        drag_to_target(
            cx,
            gpui::SharedString::from(archive.to_string_lossy().into_owned()),
            "browser-tabs-scroll:1",
        );
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.browsers[1].tabs.len(), 3);
            assert_eq!(manager.browsers[1].active_tab, 2);
            assert_eq!(manager.browsers[1].tabs[2].archive_path.as_ref(), Some(&archive));
            assert_eq!(manager.browsers[1].tabs[1].current_directory.as_ref(), Some(&folder));
            assert_eq!(manager.browsers[0].tabs[0].current_directory.as_ref(), Some(&root_path));
        });
    }

    #[gpui::test]
    fn dropping_file_on_other_browser_moves_it_to_that_location(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let root = tempfile::tempdir().unwrap();
        let source_directory = root.path().join("source");
        let destination_directory = root.path().join("destination");
        let target_folder = destination_directory.join("target-folder");
        let target_file = destination_directory.join("existing.txt");
        std::fs::create_dir(&source_directory).unwrap();
        std::fs::create_dir_all(&target_folder).unwrap();
        std::fs::write(&target_file, b"existing target").unwrap();
        let source = source_directory.join("move-me.txt");
        let source_below = source_directory.join("move-below.txt");
        let destination = destination_directory.join("move-me.txt");
        let destination_below = destination_directory.join("move-below.txt");
        std::fs::write(&source, b"move between browsers").unwrap();
        std::fs::write(&source_below, b"drop below the list").unwrap();
        let (view, cx) = component_window(cx, |_, cx| {
            let mut manager = test_manager(cx);
            manager.auto_calculate_folder_sizes = false;
            manager.browsers[0].tabs[0] = super::BrowserTab::new(
                Some(source_directory.clone()),
                None,
                None,
                super::read_directory(&source_directory).unwrap(),
                None,
            );
            manager.browsers[1].tabs[0] = super::BrowserTab::new(
                Some(destination_directory.clone()),
                None,
                None,
                super::read_directory(&destination_directory).unwrap(),
                None,
            );
            manager
        });

        let start = cx
            .update(|window, _| {
                window
                    .find(gpui::SharedString::from(source.to_string_lossy().into_owned()))
                    .bounds()
                    .center()
            });
        let destination_bounds = cx.update(|window, _| {
            window
            .find(gpui::SharedString::from(target_file.to_string_lossy().into_owned()))
                .bounds()
        });
        let end = destination_bounds.center();
        cx.simulate_mouse_move(start, None, gpui::Modifiers::default());
        cx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(
            start + gpui::point(gpui::px(20.0), gpui::px(0.0)),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        cx.simulate_mouse_move(end, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let manager = view.read(cx);
            let dark = manager.theme_mode.is_dark(window.appearance());
            let color = manager.accent_color.color(dark);
            assert!(window.painted_quads().iter().any(|quad| {
                quad.background == gpui::rgba((color << 8) | 0x2a).into()
                    && (quad.bounds.origin.y.0
                        - f32::from(destination_bounds.top()) * window.scale_factor())
                        .abs()
                        < 1.0
            }), "the destination list must visibly highlight while a file is dragged over it");
            assert!(window.painted_quads().iter().any(|quad| {
                quad.border_color == gpui::rgb(color).into()
                    && (quad.bounds.origin.y.0
                        - f32::from(destination_bounds.top()) * window.scale_factor())
                        .abs()
                        < 1.0
            }), "the target folder row must show an accent border while hovered");
        });
        cx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());

        for _ in 0..16 {
            cx.run_until_parked();
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            if !cx.read(|cx| view.read(cx).file_actions.operations.is_busy()) {
                break;
            }
        }
        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"move between browsers");
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.active_browser, 1);
            assert_eq!(
                manager.browsers[1].tabs[0].current_directory.as_ref(),
                Some(&destination_directory)
            );
            assert!(manager.browsers[1].tabs[0]
                .entries
                .iter()
                .any(|entry| entry.name == "target-folder" && entry.is_directory));
            assert!(manager.browsers[1].tabs[0]
                .entries
                .iter()
                .any(|entry| entry.name == "existing.txt"));
        });

        let below_start = cx.update(|window, _| {
            window
                .find(gpui::SharedString::from(source_below.to_string_lossy().into_owned()))
                .bounds()
                .center()
        });
        let viewport = cx.debug_bounds("file-scroll-viewport-secondary").unwrap();
        let last_row = cx.update(|window, _| {
            [target_folder.clone(), target_file.clone(), destination.clone()]
                .into_iter()
                .map(|path| {
                    window
                        .find(gpui::SharedString::from(path.to_string_lossy().into_owned()))
                        .bounds()
                })
                .reduce(|current, candidate| {
                    if candidate.bottom() > current.bottom() { candidate } else { current }
                })
                .unwrap()
        });
        assert!(viewport.bottom() > last_row.bottom() + gpui::px(4.0));
        let below_end = gpui::point(viewport.center().x, viewport.bottom() - gpui::px(3.0));
        cx.simulate_mouse_move(below_start, None, gpui::Modifiers::default());
        cx.simulate_mouse_down(below_start, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(
            below_start + gpui::point(gpui::px(20.0), gpui::px(0.0)),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        cx.simulate_mouse_move(below_end, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        let marker_row = cx.read(|cx| view.read(cx).browsers[1].tabs[0].drop_marker_row);
        assert_eq!(marker_row, Some(2));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let manager = view.read(cx);
            let dark = manager.theme_mode.is_dark(window.appearance());
            let color = manager.accent_color.color(dark);
            let accent_borders: Vec<_> = window
                .painted_quads()
                .iter()
                .filter(|quad| quad.border_color == gpui::rgb(color).into())
                .map(|quad| quad.bounds)
                .collect();
            let row_top = f32::from(last_row.top()) * window.scale_factor();
            let row_bottom = f32::from(last_row.bottom()) * window.scale_factor();
            let row_left = f32::from(last_row.left()) * window.scale_factor();
            let row_right = f32::from(last_row.right()) * window.scale_factor();
            assert!(
                accent_borders.iter().any(|bounds| {
                    bounds.origin.y.0 <= row_bottom
                        && bounds.bottom().0 >= row_top
                        && bounds.origin.x.0 <= row_left + 1.0
                        && bounds.right().0 >= row_right - 1.0
                }),
                "last row={last_row:?}, accent bounds={accent_borders:?}"
            );
        });
        cx.simulate_mouse_up(below_end, gpui::MouseButton::Left, gpui::Modifiers::default());
        for _ in 0..16 {
            cx.run_until_parked();
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            if !cx.read(|cx| view.read(cx).file_actions.operations.is_busy()) {
                break;
            }
        }
        assert!(!source_below.exists());
        assert_eq!(std::fs::read(&destination_below).unwrap(), b"drop below the list");
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
        cx.simulate_keystrokes("escape");
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
        cx.simulate_keystrokes("escape");
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
        let secondary_file_list = cx.debug_bounds("file-list-secondary").unwrap();
        let combined_width = file_list.size.width + secondary_file_list.size.width;
        assert!(combined_width >= main_panel.size.width - gpui::px(8.0));
        assert!(combined_width <= main_panel.size.width + gpui::px(8.0));
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
        use gpui::AssetSource;
        let assets = crate::icons::IconAssets;
        for name in [
            "document", "documents", "code", "folder", "binary", "csv",
            "archive-locked", "chevron-left", "chevron-up", "chevron-down",
        ] {
            let bytes = assets.load(&format!("icons/{name}.svg")).unwrap().unwrap();
            let document = roxmltree::Document::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
            let dimensions = document.root_element().attribute("viewBox").unwrap()
                .split_whitespace().map(|value| value.parse::<f32>().unwrap()).collect::<Vec<_>>();
            let expected = dimensions[2] / dimensions[3];
            assert!((icon_aspect_ratio(name) - expected).abs() < 0.00001, "incorrect icon proportions: {name}");
        }
    }
}
