//! Implements context-menu actions for filesystem items and archived entries.

use crate::{ArchiveManager, Modal, Theme, archive, i18n::Language, password::ArchivePassword};
use gpui::assets::IconName;
use gpui::component::{
    Icon, Sizable,
    button::{Button, ButtonVariants},
    input::{InputEvent, InputState},
    menu::{PopupMenu, PopupMenuItem},
};
use gpui::{
    App, ClipboardItem, Context, Entity, Focusable, IntoElement, Subscription, TestSupportExt,
    WeakEntity, Window, div, prelude::*,
    px, rgb,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Default)]
/// Stores background operations and pending state for file-context actions.
pub struct FileActions {
    /// Ongoing extraction or move operations.
    pub operations: crate::operations::Operations,
    /// Target awaiting confirmation before being moved to Trash.
    pub pending_delete: Option<EntryTarget>,
    /// Background Trash request, when one is active.
    pub delete_task: Option<u64>,
    /// Filesystem item or archive entry shown in the Info dialog.
    pub info_target: Option<EntryTarget>,
    /// Current path and selection shown in the Open picker.
    pub open_picker: Option<OpenPicker>,
}

pub(crate) struct OpenPicker {
    pub(crate) directory: PathBuf,
    pub(crate) entries: Arc<Vec<archive::ArchiveEntry>>,
    pub(crate) matching_indices: Arc<Vec<usize>>,
    pub(crate) loading: bool,
    pub(crate) selected: Option<PathBuf>,
    pub(crate) new_tab: bool,
    pub(crate) history: Vec<PathBuf>,
    pub(crate) history_index: usize,
    pub(crate) scroll: gpui::UniformListScrollHandle,
    pub(crate) path_input: Entity<InputState>,
    pub(crate) search_input: Entity<InputState>,
    _search_subscription: Subscription,
}

fn matching_picker_indices(entries: &[archive::ArchiveEntry], query: &str) -> Arc<Vec<usize>> {
    let query = query.trim().to_lowercase();
    Arc::new(
        entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                (query.is_empty() || entry.name.to_lowercase().contains(&query)).then_some(index)
            })
            .collect(),
    )
}

#[derive(Clone)]
/// Describes one filesystem item or one entry inside an archive.
pub struct EntryTarget {
    /// Display name shown in menus and status messages.
    pub name: String,
    /// Stable key used to identify the selected row.
    pub key: String,
    /// Filesystem path of the item or containing archive.
    pub path: PathBuf,
    /// Archive-internal path, or `None` for a filesystem item.
    pub archive_entry: Option<String>,
    /// Session password needed to read a protected archive entry.
    pub password: Option<ArchivePassword>,
    /// Whether the target represents a directory.
    pub is_directory: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Identifies an operation offered by an entry's context menu.
pub enum EntryAction {
    Open,
    OpenInNewTab,
    CreateArchive,
    Extract,
    ExtractArchive,
    ExtractHere,
    ConvertArchive,
    MoveTo,
    Inspect,
    TestIntegrity,
    Audit,
    SignArchive,
    VerifySignature,
    CopyPath,
    Info,
    Trash,
}

impl EntryAction {
    /// Returns the untranslated localization key for this action.
    fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::OpenInNewTab => "Open in new tab",
            Self::CreateArchive => "Create archive",
            Self::Extract => "Extract file",
            Self::ExtractArchive => "Extract archive to...",
            Self::ExtractHere => "Extract here",
            Self::ConvertArchive => "Convert archive...",
            Self::MoveTo => "Move to...",
            Self::Inspect => "Inspector",
            Self::TestIntegrity => "Test integrity",
            Self::Audit => "Audit archive",
            Self::SignArchive => "Sign archive",
            Self::VerifySignature => "Verify signature",
            Self::CopyPath => "Copy path",
            Self::Info => "Info",
            Self::Trash => "Move to Trash",
        }
    }

    /// Selects the icon representing this action and target kind.
    fn icon(self, _is_directory: bool) -> Icon {
        match self {
            Self::Open => Icon::default().path("icons/folder-open.svg"),
            Self::OpenInNewTab => IconName::ExternalLink.into(),
            Self::CreateArchive => Icon::default().path("icons/plus.svg"),
            Self::Extract => IconName::Download.into(),
            Self::ExtractArchive => IconName::FolderOutput.into(),
            Self::ExtractHere => IconName::Download.into(),
            Self::ConvertArchive => IconName::GitCompare.into(),
            Self::MoveTo => IconName::FolderInput.into(),
            Self::Inspect => IconName::FileSearch.into(),
            Self::TestIntegrity => IconName::ShieldCheck.into(),
            Self::Audit => IconName::ScanSearch.into(),
            Self::SignArchive => IconName::ShieldCheck.into(),
            Self::VerifySignature => IconName::ShieldCheck.into(),
            Self::CopyPath => IconName::Copy.into(),
            Self::Info => IconName::Info.into(),
            Self::Trash => IconName::Trash.into(),
        }.mr(px(4.0))
    }
}

impl EntryTarget {
    /// Lists only the actions valid for this filesystem or archive target.
    pub fn actions(&self) -> Vec<EntryAction> {
        let mut actions = vec![
            EntryAction::Open,
            EntryAction::OpenInNewTab,
            EntryAction::CopyPath,
            EntryAction::Info,
        ];
        if !self.is_directory {
            actions.push(EntryAction::Inspect);
        }
        if self.archive_entry.is_none() {
            if !self.is_directory && archive::is_supported_archive(&self.path) {
                actions.extend([
                    EntryAction::ExtractHere,
                    EntryAction::ExtractArchive,
                    EntryAction::ConvertArchive,
                ]);
                actions.extend([
                    EntryAction::TestIntegrity,
                    EntryAction::Audit,
                    EntryAction::VerifySignature,
                ]);
                if archive::can_add_to_archive(&self.path) {
                    actions.push(EntryAction::SignArchive);
                }
            }
            actions.extend([
                EntryAction::CreateArchive,
                EntryAction::MoveTo,
                EntryAction::Trash,
            ]);
        } else if !self.is_directory {
            actions.push(EntryAction::Extract);
        }
        actions
    }

    /// Extracts this archived file into a temporary file.
    pub(crate) fn extract(&self) -> anyhow::Result<tempfile::NamedTempFile> {
        let entry = self
            .archive_entry
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Not an archived file"))?;
        if let Some(password) = &self.password {
            archive::extract_entry_with_password(&self.path, entry, Some(password.as_str()))
        } else {
            archive::extract_entry(&self.path, entry)
        }
    }
}

impl ArchiveManager {
    pub(crate) fn show_open_picker(
        &mut self,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let directory = self
            .current_directory
            .clone()
            .or_else(|| self.archive_path.as_deref().and_then(std::path::Path::parent).map(Path::to_path_buf))
            .or_else(|| self.default_location.clone())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let path_value = directory.display().to_string();
        let path_input = cx.new(|cx| InputState::new(window, cx).default_value(path_value));
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(self.language.text("Search files and folders"))
        });
        let search_subscription = cx.subscribe_in(&search_input, window, |manager, state, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                if let Some(picker) = manager.file_actions.open_picker.as_mut() {
                    picker.matching_indices = matching_picker_indices(
                        &picker.entries,
                        state.read(cx).value().as_ref(),
                    );
                }
                cx.notify();
            }
        });
        self.file_actions.open_picker = Some(OpenPicker {
            directory: directory.clone(),
            entries: Arc::new(Vec::new()),
            matching_indices: Arc::new(Vec::new()),
            loading: true,
            selected: None,
            new_tab,
            history: vec![directory.clone()],
            history_index: 0,
            scroll: gpui::UniformListScrollHandle::new(),
            path_input,
            search_input,
            _search_subscription: search_subscription,
        });
        self.show_modal(Modal::OpenPicker, window, cx);
        let worker_directory = directory.clone();
        let worker = cx.background_executor().spawn(async move {
            let mut entries = crate::browser::read_picker_directory(&worker_directory)?;
            crate::sort_entries(&mut entries, crate::SortColumn::Name, true);
            Ok(entries)
        });
        let view = cx.entity().downgrade();
        let executor = cx.background_executor().clone();
        window
            .spawn(cx, async move |async_cx| {
                let mut result = Some(worker.await);
                loop {
                    let delivered = async_cx.update(|window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            let Some(result) = result.take() else { return; };
                            this.finish_open_picker_load(&directory, result, window, cx);
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

    fn finish_open_picker_load(
        &mut self,
        directory: &Path,
        result: anyhow::Result<Vec<archive::ArchiveEntry>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.file_actions.open_picker.as_mut() else {
            return;
        };
        if self.modal != Some(Modal::OpenPicker) || picker.directory != directory {
            return;
        }
        match result {
            Ok(entries) => {
                let query = picker.search_input.read(cx).value().to_string();
                picker.matching_indices = matching_picker_indices(&entries, &query);
                picker.entries = Arc::new(entries);
                picker.loading = false;
                cx.notify();
            }
            Err(_) => {
                self.file_actions.open_picker = None;
                self.close_modal(window, cx);
                self.status_message = Some(format!(
                    "{}: {}",
                    self.language.text("Unable to open folder"),
                    directory.display()
                ));
                cx.notify();
            }
        }
    }

    pub(crate) fn load_open_picker_directory(
        &mut self,
        directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match crate::browser::read_picker_directory(&directory) {
            Ok(mut entries) => {
                crate::sort_entries(&mut entries, crate::SortColumn::Name, true);
                let path_value = directory.display().to_string();
                let Some(picker) = self.file_actions.open_picker.as_ref() else {
                    return;
                };
                if picker.directory == directory {
                    return;
                }
                let history_index = picker.history_index;
                let path_input = picker.path_input.clone();
                let search_query = picker.search_input.read(cx).value().to_string();
                path_input.update(cx, |state, cx| state.set_value(path_value, window, cx));
                if let Some(picker) = &mut self.file_actions.open_picker {
                    picker.history.truncate(history_index + 1);
                    if picker.history.last() != Some(&directory) {
                        picker.history.push(directory.clone());
                    }
                    picker.history_index = picker.history.len().saturating_sub(1);
                    picker.directory = directory;
                    picker.matching_indices = matching_picker_indices(&entries, &search_query);
                    picker.entries = Arc::new(entries);
                    picker.loading = false;
                    picker.selected = None;
                    picker.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
                }
                cx.notify();
            }
            Err(error) => {
                self.status_message = Some(format!("{}: {error:#}", self.language.text("Unable to open folder")));
                cx.notify();
            }
        }
    }

    pub(crate) fn navigate_open_picker_history(
        &mut self,
        offset: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.file_actions.open_picker.as_ref() else {
            return;
        };
        let target_index = picker.history_index.saturating_add_signed(offset);
        let Some(directory) = picker.history.get(target_index).cloned() else { return; };
        let search_query = picker.search_input.read(cx).value().to_string();
        let path_input = picker.path_input.clone();
        let Ok(mut entries) = crate::browser::read_picker_directory(&directory) else { return; };
        crate::sort_entries(&mut entries, crate::SortColumn::Name, true);
        path_input.update(cx, |state, cx| state.set_value(directory.display().to_string(), window, cx));
        if let Some(picker) = &mut self.file_actions.open_picker {
            picker.history_index = target_index;
            picker.directory = directory;
            picker.matching_indices = matching_picker_indices(&entries, &search_query);
            picker.entries = Arc::new(entries);
            picker.loading = false;
            picker.selected = None;
            picker.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        }
        cx.notify();
    }

    pub(crate) fn select_open_picker_entry(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if let Some(picker) = &mut self.file_actions.open_picker {
            picker.selected = Some(path);
            cx.notify();
        }
    }

    pub(crate) fn submit_open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &self.file_actions.open_picker else { return; };
        if picker.path_input.focus_handle(cx).is_focused(window) {
            let path = PathBuf::from(picker.path_input.read(cx).value().trim());
            if path.is_dir() {
                self.load_open_picker_directory(path, window, cx);
            } else if path.is_file() {
                self.finish_open_picker(path, window, cx);
            } else {
                self.status_message = Some(format!("{}: {}", self.language.text("Location does not exist"), path.display()));
                cx.notify();
            }
            return;
        }
        self.confirm_open_picker(window, cx);
    }

    pub(crate) fn activate_open_picker_entry(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if path.is_dir() {
            self.load_open_picker_directory(path, window, cx);
        } else {
            self.finish_open_picker(path, window, cx);
        }
    }

    pub(crate) fn confirm_open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.file_actions.open_picker.as_ref() else { return; };
        let path = picker.selected.clone().unwrap_or_else(|| picker.directory.clone());
        if !path.is_file() && !path.is_dir() {
            self.status_message = Some(format!("{}: {}", self.language.text("Location does not exist"), path.display()));
            cx.notify();
            return;
        }
        self.finish_open_picker(path, window, cx);
    }

    fn finish_open_picker(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let new_tab = self.file_actions.open_picker.as_ref().is_some_and(|picker| picker.new_tab);
        self.close_modal(window, cx);
        let target = EntryTarget {
            name: path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned(),
            key: path.to_string_lossy().into_owned(),
            is_directory: path.is_dir(),
            path,
            archive_entry: None,
            password: None,
        };
        if new_tab {
            self.open_entry_in_new_tab(&target, cx);
        } else if target.is_directory {
            self.navigate_to(target.path, cx);
        } else {
            self.open_entry(&target, cx);
        }
    }

}

impl ArchiveManager {
    /// Opens archive creation with an optional filesystem target preselected.
    pub fn create_archive_from_target(
        &mut self,
        target: Option<&EntryTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.creation_task.is_some()
            || target.is_some_and(|target| target.archive_entry.is_some())
        {
            return;
        }
        self.creation.sources.clear();
        if let Some(target) = target {
            self.creation.sources.push(target.path.clone());
        }
        self.creation.error = None;
        self.show_modal(Modal::CreateArchive, window, cx);
    }

    /// Reloads the current directory or archive listing from its source.
    pub fn refresh_location(&mut self, cx: &mut Context<Self>) {
        if let Some(directory) = self.current_directory.clone() {
            self.navigate_to(directory, cx);
        } else if let Some(path) = self.archive_path.clone() {
            self.load_archive(path, self.archive_password.clone(), true, cx);
        }
    }

    /// Opens a confirmation dialog for a deletable filesystem target.
    pub fn request_delete(
        &mut self,
        target: &EntryTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_actions.delete_task.is_some() || self.file_actions.operations.is_busy()
            || (target.archive_entry.is_some() && !archive::can_add_to_archive(&target.path))
        {
            return;
        }
        self.file_actions.pending_delete = Some(target.clone());
        self.show_modal(Modal::Delete, window, cx);
    }

    /// Moves the confirmed target to Trash on a background worker.
    pub fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_actions.delete_task.is_some() {
            return;
        }
        let Some(target) = self.file_actions.pending_delete.take() else {
            return;
        };
        if target.archive_entry.is_some() {
            self.close_modal(window, cx);
            self.remove_inspected_entry(target, window, cx);
            return;
        }
        self.close_modal(window, cx);
        let path = target.path.clone();
        let browser_tab_id = self.active_browser_tab().id;
        self.file_actions.delete_task = Some(self.background_queue.enqueue(
            "Moving to Trash...",
            crate::task_queue::TaskControl::default().for_paths([target.path.clone()]),
            move |control, reporter| async move {
                reporter.set(0, 1);
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                trash::delete(path)?;
                reporter.set(1, 1);
                Ok(())
            },
            move |this, result, _, cx| {
                let _ = this.with_browser_tab(browser_tab_id, |this| {
                    this.complete_delete(target, result, cx);
                });
            },
            cx,
        ));
        cx.notify();
    }

    /// Applies a Trash result from the background queue.
    fn complete_delete(
        &mut self,
        target: EntryTarget,
        result: Result<(), crate::task_queue::TaskFailure>,
        cx: &mut Context<Self>,
    ) {
        self.file_actions.delete_task = None;
        if self.current_directory.as_deref() == target.path.parent() { self.refresh_location(cx); }
        match result {
                    Ok(()) => {
                        self.status_message = Some(format!(
                            "{} {}",
                            self.language.text("Moved to Trash"),
                            target.name
                        ));
                    }
                    Err(crate::task_queue::TaskFailure::Cancelled) => {},
                    Err(crate::task_queue::TaskFailure::Failed(error)) => {
                        self.status_message = Some(format!(
                            "{}: {error}",
                            self.language.text("Failed to move to Trash")
                        ))
                    }
        }
        cx.notify();
    }

    /// Opens a directory, archive, extracted temporary entry, or ordinary file.
    pub fn open_entry(&mut self, target: &EntryTarget, cx: &mut Context<Self>) {
        if self.search_target_requires_password(target) {
            self.navigate_into_archive(target.path.clone(), cx);
            return;
        }
        if target.is_directory {
            if let Some(entry) = &target.archive_entry {
                self.selected_folder = entry.trim_end_matches('/').to_owned();
                self.selected_entry = None;
                self.status_message = None;
                cx.notify();
            } else if self.show_favorites {
                self.navigate_to_favorite(target.path.clone(), cx);
            } else {
                self.navigate_to(target.path.clone(), cx);
            }
            return;
        }
        if target.archive_entry.is_none() && archive::is_supported_archive(&target.path) {
            self.navigate_into_archive(target.path.clone(), cx);
            return;
        }
        let work = target.clone();
        let name = target.name.clone();
        self.background_queue.enqueue(
            "Opening file...",
            crate::task_queue::TaskControl::default().for_paths([target.path.clone()]),
            move |control, reporter| async move {
                reporter.phase(work.name.clone());
                reporter.set(0, 1);
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                let file = if work.archive_entry.is_some() { Some(work.extract()?) } else { None };
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                open::that(file.as_ref().map(|file| file.path()).unwrap_or(&work.path))?;
                reporter.set(1, 1);
                Ok(file)
            },
            move |manager, result, _, cx| {
                match result {
                    Ok(file) => {
                        if let Some(file) = file { manager.opened_files.push(file); }
                        manager.status_message = Some(format!("{} {name} {}", manager.language.text("Opened"), manager.language.text("with the system default application.")));
                    }
                    Err(crate::task_queue::TaskFailure::Cancelled) => {},
                    Err(crate::task_queue::TaskFailure::Failed(error)) => {
                        manager.status_message = Some(format!("{} {name}: {error:#}", manager.language.text("Failed to open")));
                    }
                }
                cx.notify();
            },
            cx,
        );
        cx.notify();
    }

    /// Copies a filesystem path or archive-qualified entry path to the clipboard.
    pub fn copy_entry_path(&mut self, target: &EntryTarget, cx: &mut Context<Self>) {
        let path = if let Some(entry) = &target.archive_entry {
            format!("{}!/{entry}", target.path.display())
        } else {
            target.path.display().to_string()
        };
        cx.write_to_clipboard(ClipboardItem::new_string(path));
        self.status_message = Some(self.language.text("Path copied").to_owned());
        cx.notify();
    }

    /// Extracts one archived file to a user-selected path without overwriting its archive.
    pub fn save_archived_file(&mut self, target: &EntryTarget, cx: &mut Context<Self>) {
        if self.search_target_requires_password(target) {
            self.navigate_into_archive(target.path.clone(), cx);
            return;
        }
        let Some(output) = rfd::FileDialog::new()
            .set_title(self.language.text("Extract file"))
            .set_file_name(&target.name)
            .save_file()
        else {
            return;
        };
        let work = target.clone();
        let destination = output.clone();
        self.background_queue.enqueue(
            "Extracting archive...",
            crate::task_queue::TaskControl::default().for_paths([target.path.clone(), output.clone()]),
            move |control, reporter| async move {
            reporter.phase(work.name.clone());
            if !control.checkpoint() { anyhow::bail!("cancelled"); }
            let file = work.extract()?;
            anyhow::ensure!(
                output != work.path
                    && (!output.exists()
                        || std::fs::canonicalize(&output)? != std::fs::canonicalize(&work.path)?),
                "Cannot overwrite the source archive"
            );
            let mut staged = tempfile::NamedTempFile::new_in(
                output
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("No output directory"))?,
            )?;
            crate::task_queue::copy(&mut std::fs::File::open(file.path())?, staged.as_file_mut())?;
            if !control.checkpoint() { anyhow::bail!("cancelled"); }
            staged.persist(&output).map_err(|error| error.error)?;
            Ok(())
            },
            move |manager, result, _, cx| {
                match result {
                    Ok(()) => manager.status_message = Some(format!("{} {}", manager.language.text("Extracted to"), destination.display())),
                    Err(crate::task_queue::TaskFailure::Cancelled) => {},
                    Err(crate::task_queue::TaskFailure::Failed(error)) => manager.status_message = Some(format!("{}: {error:#}", manager.language.text("Failed to extract"))),
                }
                cx.notify();
            },
            cx,
        );
        cx.notify();
    }
}

fn info_time(language: Language, value: Option<std::time::SystemTime>) -> String {
    value.map_or_else(
        || language.text("Unavailable").to_owned(),
        |time| {
            chrono::DateTime::<chrono::Local>::from(time)
                .format("%Y-%m-%d %H:%M:%S %:z")
                .to_string()
        },
    )
}

fn info_row(label: String, value: String, theme: Theme) -> impl IntoElement {
    let selector = format!("file-info-row:{label}");
    div()
        .id(gpui::SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_start()
        .gap_4()
        .w_full()
        .min_h(px(28.0))
        .py_1()
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
        .child(
            div()
                .w(px(150.0))
                .flex_shrink_0()
                .text_color(rgb(theme.muted_text))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_color(rgb(theme.text))
                .child(value),
        )
}

pub(crate) fn info_view(
    manager: &ArchiveManager,
    theme: Theme,
    language: Language,
) -> impl IntoElement {
    let mut rows = Vec::new();
    if let Some(target) = &manager.file_actions.info_target {
        rows.push((language.text("Name").to_owned(), target.name.clone()));
        if let Some(entry_path) = target.archive_entry.as_deref() {
            rows.push((
                language.text("Archive file").to_owned(),
                target.path.display().to_string(),
            ));
            rows.push((
                language.text("Path").to_owned(),
                format!("{}!/{entry_path}", target.path.display()),
            ));
            if let Some(entry) = manager.entries.iter().find(|entry| {
                entry.source_path.as_deref() == Some(entry_path)
                    || entry.name == target.name
            }) {
                rows.push((
                    language.text("File type").to_owned(),
                    language
                        .text(if entry.is_directory { "Folder" } else { "File" })
                        .to_owned(),
                ));
                rows.push((
                    language.text("Size").to_owned(),
                    crate::format_size(entry.size),
                ));
                rows.push((
                    language.text("Modified").to_owned(),
                    entry
                        .modified
                        .map_or_else(|| language.text("Unavailable").to_owned(), |date| date.to_string()),
                ));
                rows.push((
                    language.text("Encrypted").to_owned(),
                    language
                        .text(if entry.is_encrypted { "Yes" } else { "No" })
                        .to_owned(),
                ));
            }
        } else {
            rows.push((
                language.text("Path").to_owned(),
                target.path.display().to_string(),
            ));
            match std::fs::symlink_metadata(&target.path) {
                Ok(metadata) => {
                    let file_type = metadata.file_type();
                    let type_label = if file_type.is_symlink() {
                        "Symbolic link"
                    } else if metadata.is_dir() {
                        "Folder"
                    } else if metadata.is_file() {
                        "File"
                    } else {
                        "Unknown"
                    };
                    rows.push((
                        language.text("File type").to_owned(),
                        language.text(type_label).to_owned(),
                    ));
                    if !metadata.is_dir() {
                        rows.push((
                            language.text("Size").to_owned(),
                            crate::format_size(metadata.len()),
                        ));
                    }
                    rows.push((
                        language.text("Created").to_owned(),
                        info_time(language, metadata.created().ok()),
                    ));
                    rows.push((
                        language.text("Modified").to_owned(),
                        info_time(language, metadata.modified().ok()),
                    ));
                    rows.push((
                        language.text("Accessed").to_owned(),
                        info_time(language, metadata.accessed().ok()),
                    ));
                    rows.push((
                        language.text("Read-only").to_owned(),
                        language
                            .text(if metadata.permissions().readonly() { "Yes" } else { "No" })
                            .to_owned(),
                    ));
                    let hidden = target
                        .path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with('.'));
                    rows.push((
                        language.text("Hidden").to_owned(),
                        language.text(if hidden { "Yes" } else { "No" }).to_owned(),
                    ));
                    if file_type.is_symlink() {
                        rows.push((
                            language.text("Link target").to_owned(),
                            std::fs::read_link(&target.path)
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|_| language.text("Unavailable").to_owned()),
                        ));
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        rows.push((
                            language.text("Filesystem permissions").to_owned(),
                            format!("{:04o}", metadata.mode() & 0o7777),
                        ));
                        rows.push((
                            language.text("Owner/group").to_owned(),
                            format!("{} / {}", metadata.uid(), metadata.gid()),
                        ));
                        rows.push((language.text("Inode").to_owned(), metadata.ino().to_string()));
                        rows.push((
                            language.text("Hard links").to_owned(),
                            metadata.nlink().to_string(),
                        ));
                    }
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::MetadataExt;
                        rows.push((
                            language.text("Attributes").to_owned(),
                            format!("0x{:08x}", metadata.file_attributes()),
                        ));
                    }
                }
                Err(error) => rows.push((
                    language.text("Metadata unavailable").to_owned(),
                    error.to_string(),
                )),
            }
        }
    }
    div()
        .id("file-info-metadata")
        .debug_selector(|| "file-info-metadata".to_owned())
        .flex()
        .flex_col()
        .w_full()
        .max_h(px(480.0))
        .overflow_y_scroll()
        .children(rows.into_iter().map(|(label, value)| info_row(label, value, theme)))
}

fn context_menu_groups(target: &EntryTarget) -> Vec<Vec<EntryAction>> {
    use EntryAction::*;
    let available = target.actions();
    let groups: &[&[EntryAction]] = &[
        &[Open, OpenInNewTab],
        &[CreateArchive, ExtractHere, ExtractArchive, ConvertArchive, Extract],
        &[Inspect, TestIntegrity, Audit, VerifySignature, SignArchive],
        &[CopyPath, MoveTo],
        &[Info, Trash],
    ];
    groups.iter()
        .map(|group| group.iter().copied().filter(|action| available.contains(action)).collect::<Vec<_>>())
        .filter(|group| !group.is_empty())
        .collect()
}

/// Builds the context menu for one target and dispatches its selected action.
pub fn menu(
    mut menu: PopupMenu,
    target: &EntryTarget,
    view: WeakEntity<ArchiveManager>,
    language: Language,
    busy: bool,
    is_favorite: bool,
    cx: &App,
) -> PopupMenu {
    menu = menu.min_w(px(260.0)).max_w(px(360.0));
    for (index, group) in context_menu_groups(target).into_iter().enumerate() {
        if index > 0 {
            menu = menu.separator();
        }
        for action in group {
            let target = target.clone();
            let view = view.clone();
            let label = language.text(action.label());
            let item = if action == EntryAction::Trash {
                let colors = gpui::component::Theme::global(cx).colors;
                let color = if busy {
                    colors.muted_foreground
                } else {
                    colors.danger
                };
                PopupMenuItem::element(move |_, _| {
                    div()
                        .id("trash-menu-label")
                        .test_support()
                        .aria_label(label)
                        .text_color(color)
                        .child(label)
                })
                .icon(action.icon(target.is_directory).text_color(color))
            } else {
                PopupMenuItem::new(label).icon(action.icon(target.is_directory))
            };
            menu = menu.item(
                item.disabled(
                    busy && matches!(
                        action,
                        EntryAction::CreateArchive
                            | EntryAction::Trash
                            | EntryAction::MoveTo
                            | EntryAction::ExtractArchive
                            | EntryAction::ExtractHere
                            | EntryAction::ConvertArchive
                            | EntryAction::SignArchive
                            | EntryAction::VerifySignature
                    ),
                )
                .on_click(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| match action {
                        EntryAction::Open => this.open_entry(&target, cx),
                        EntryAction::OpenInNewTab => this.open_entry_in_new_tab(&target, cx),
                        EntryAction::CopyPath => this.copy_entry_path(&target, cx),
                        EntryAction::Info => {
                            this.file_actions.info_target = Some(target.clone());
                            this.show_modal(Modal::Info, window, cx);
                        }
                        EntryAction::Extract => this.save_archived_file(&target, cx),
                        EntryAction::ExtractArchive => {
                            this.request_extract_archive(&target, false, window, cx)
                        }
                        EntryAction::ExtractHere => {
                            this.request_extract_archive(&target, true, window, cx)
                        }
                        EntryAction::ConvertArchive => this.open_conversion(window, cx),
                        EntryAction::SignArchive => {
                            this.open_sign_archive(target.clone(), window, cx)
                        }
                        EntryAction::VerifySignature => {
                            this.verify_archive_signature(&target, cx)
                        }
                        EntryAction::MoveTo => this.request_move(&target, window, cx),
                        EntryAction::Inspect => this.preview_archive_target(&target, window, cx),
                        EntryAction::TestIntegrity | EntryAction::Audit => {
                            let mode = match action {
                                EntryAction::TestIntegrity => {
                                    crate::inspection::InspectionMode::Integrity
                                }
                                EntryAction::Audit => crate::inspection::InspectionMode::Audit,
                                _ => crate::inspection::InspectionMode::Preview,
                            };
                            this.inspect_sources(
                                vec![(target.path.clone(), target.password.clone())],
                                mode,
                                window,
                                cx,
                            );
                        }
                        EntryAction::CreateArchive => {
                            this.create_archive_from_target(Some(&target), window, cx)
                        }
                        EntryAction::Trash => this.request_delete(&target, window, cx),
                    });
                }),
            );
        }
    }
    if target.is_directory && target.archive_entry.is_none() {
        menu = menu.separator();
        menu = favorite_location_menu(
            menu,
            target.path.clone(),
            view,
            language,
            is_favorite,
            !busy,
        );
    }
    menu
}

pub(crate) fn favorite_location_menu(
    menu: PopupMenu,
    path: PathBuf,
    view: WeakEntity<ArchiveManager>,
    language: Language,
    is_favorite: bool,
    enabled: bool,
) -> PopupMenu {
    let label = language.text(if is_favorite {
        "Remove from Favorites"
    } else {
        "Add to Favorites"
    });
    menu.item(
        PopupMenuItem::new(label)
            .icon(IconName::Star)
            .disabled(!enabled)
            .on_click(move |_, _, cx| {
                let _ = view.update(cx, |manager, cx| {
                    manager.toggle_favorite_folder(path.clone());
                    cx.notify();
                });
            }),
    )
}

pub(crate) fn inspected_entry_menu(
    menu: PopupMenu,
    target: &EntryTarget,
    view: WeakEntity<ArchiveManager>,
    language: Language,
    busy: bool,
) -> PopupMenu {
    let extract_target = target.clone();
    let extract_view = view.clone();
    let copy_target = target.clone();
    let copy_view = view.clone();
    let remove_target = target.clone();
    let can_remove = archive::can_add_to_archive(&target.path);
    menu.min_w(px(260.0)).max_w(px(360.0))
        .item(PopupMenuItem::new(language.text("Extract here"))
            .icon(EntryAction::ExtractHere.icon(target.is_directory))
            .disabled(busy)
            .on_click(move |_, window, cx| {
                let _ = extract_view.update(cx, |manager, cx| manager.extract_inspected_entry_here(&extract_target, window, cx));
            }))
        .separator()
        .item(PopupMenuItem::new(language.text("Copy path"))
            .icon(EntryAction::CopyPath.icon(target.is_directory))
            .on_click(move |_, _, cx| {
                let _ = copy_view.update(cx, |manager, cx| manager.copy_entry_path(&copy_target, cx));
            }))
        .separator()
        .item(PopupMenuItem::new(language.text("Remove from archive"))
            .icon(EntryAction::Trash.icon(target.is_directory))
            .disabled(busy || !can_remove)
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |manager, cx| manager.request_delete(&remove_target, window, cx));
            }))
}

/// Builds actions available in the empty area of the file list.
pub fn background_menu(
    menu: PopupMenu,
    view: WeakEntity<ArchiveManager>,
    language: Language,
    can_create: bool,
) -> PopupMenu {
    let create = view.clone();
    menu.min_w(px(260.0))
        .max_w(px(360.0))
        .when(can_create, |menu| {
            menu.item(
                PopupMenuItem::new(language.text("Create archive"))
                    .icon(EntryAction::CreateArchive.icon(false))
                    .on_click(move |_, window, cx| {
                        let _ = create.update(cx, |this, cx| {
                            this.create_archive_from_target(None, window, cx)
                        });
                    }),
            ).separator()
        })
        .item(
            PopupMenuItem::new(language.text("Refresh"))
                .icon(Icon::new(IconName::RefreshCw).mr(px(4.0)))
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.refresh_location(cx));
                }),
        )
}

/// Renders the confirmation prompt for moving an item to Trash.
pub fn delete_view(
    manager: &ArchiveManager,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    let archived = manager.file_actions.pending_delete.as_ref().is_some_and(|target| target.archive_entry.is_some());
    let name = manager
        .file_actions
        .pending_delete
        .as_ref()
        .map(|target| target.name.clone())
        .unwrap_or_default();
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .text_sm()
                .text_color(rgb(theme.text))
                .child(language.text(if archived { "Remove this entry from the archive?" } else { "Move this item to the Trash?" })),
        )
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(name),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("cancel-delete")
                        .ghost()
                        .small()
                        .label(language.text("Cancel"))
                        .on_click(cx.listener(|this, _, window, cx| this.close_modal(window, cx))),
                )
                .child(
                    Button::new("confirm-delete")
                        .danger()
                        .small()
                        .label(language.text(if archived { "Remove from archive" } else { "Move to Trash" }))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.confirm_delete(window, cx)),
                        ),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_actions_are_grouped_without_changing_available_operations() {
        use EntryAction::*;
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("archive.zip");
        std::fs::write(&archive_path, b"PK\x03\x04").unwrap();
        let mut target = EntryTarget {
            name: "archive.zip".into(), key: "archive.zip".into(), path: archive_path,
            archive_entry: None, password: None, is_directory: false,
        };
        assert_eq!(context_menu_groups(&target), vec![
            vec![Open, OpenInNewTab],
            vec![CreateArchive, ExtractHere, ExtractArchive, ConvertArchive],
            vec![Inspect, TestIntegrity, Audit, VerifySignature, SignArchive],
            vec![CopyPath, MoveTo],
            vec![Info, Trash],
        ]);
        target.path = "notes.txt".into();
        assert_eq!(context_menu_groups(&target), vec![
            vec![Open, OpenInNewTab], vec![CreateArchive], vec![Inspect], vec![CopyPath, MoveTo], vec![Info, Trash],
        ]);
        target.is_directory = true;
        assert_eq!(context_menu_groups(&target), vec![
            vec![Open, OpenInNewTab], vec![CreateArchive], vec![CopyPath, MoveTo], vec![Info, Trash],
        ]);
        target.archive_entry = Some("folder".into());
        assert_eq!(context_menu_groups(&target), vec![vec![Open, OpenInNewTab], vec![CopyPath], vec![Info]]);
        target.is_directory = false;
        target.archive_entry = Some("notes.txt".into());
        assert_eq!(context_menu_groups(&target), vec![vec![Open, OpenInNewTab], vec![Extract], vec![Inspect], vec![CopyPath], vec![Info]]);
        let grouped: Vec<_> = context_menu_groups(&target).into_iter().flatten().collect();
        assert_eq!(grouped.len(), target.actions().len());
        assert!(target.actions().iter().all(|action| grouped.contains(action)));
    }

    /// Opens a context menu and clicks the item matching the requested label.
    fn choose_menu_item(cx: &mut gpui::VisualTestContext, label: &str) {
        use gpui::test::TestWindowExt;
        cx.run_until_parked();
        cx.update(|window, cx| window.render_frame(cx));
        let bounds = cx.update(|window, _| {
            assert!(window.find("popup-menu").bounds().size.width >= px(260.0));
            window.within("popup-menu").find_by_label(label).bounds()
        });
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();
    }

    #[gpui::test]
    /// Covers path copying, archive creation, Trash confirmation, and read-only archive rules.
    fn context_menu_copies_paths_prepares_archives_and_confirms_deletion(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.txt");
        std::fs::write(&path, b"notes").unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = vec![archive::ArchiveEntry {
                name: "notes.txt".into(),
                source_path: None,
                size: 5,
                is_directory: false,
                modified: None,
                is_encrypted: false,
            }];
            manager
        });
        cx.update(|window, cx| window.right_click("notes.txt", cx));
        cx.read(|cx| assert_eq!(view.read(cx).selected_entry.as_deref(), Some("notes.txt")));
        choose_menu_item(cx, "Copy path");
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            path.display().to_string()
        );

        cx.update(|window, cx| window.right_click("notes.txt", cx));
        choose_menu_item(cx, "Info");
        cx.run_until_parked();
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(matches!(manager.modal, Some(Modal::Info)));
            assert_eq!(manager.file_actions.info_target.as_ref().unwrap().path, path);
        });
        assert!(cx.debug_bounds("file-info-row:Name").is_some());
        assert!(cx.debug_bounds("file-info-row:Path").is_some());
        #[cfg(unix)]
        assert!(cx.debug_bounds("file-info-row:Filesystem permissions").is_some());
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        cx.update(|window, cx| window.right_click("notes.txt", cx));
        choose_menu_item(cx, "Create archive");
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.modal == Some(Modal::CreateArchive));
            assert_eq!(manager.creation.sources, [path.clone()]);
        });
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");

        cx.update(|window, cx| window.right_click("notes.txt", cx));
        choose_menu_item(cx, "Move to Trash");
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.modal == Some(Modal::Delete));
            assert_eq!(
                manager.file_actions.pending_delete.as_ref().unwrap().path,
                path
            );
            assert!(manager.file_actions.delete_task.is_none());
        });
        assert!(path.exists());
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.read(|cx| assert!(view.read(cx).file_actions.pending_delete.is_none()));
        assert!(path.exists());
        cx.update(|window, cx| window.right_click("file-list", cx));
        choose_menu_item(cx, "Create archive");
        cx.read(|cx| assert!(view.read(cx).creation.sources.is_empty()));
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");

        view.update(cx, |manager, cx| {
            manager.current_directory = None;
            manager.archive_path = Some(directory.path().join("archive.zip"));
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.right_click("notes.txt", cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.render_frame(cx);
            let menu = window.within("popup-menu");
            assert!(menu.try_find_by_label("Extract file").is_some());
            assert!(menu.try_find_by_label("Move to Trash").is_none());
            assert!(menu.try_find_by_label("Move to...").is_none());
            assert!(menu.try_find_by_label("Create archive").is_none());
        });
    }

    #[gpui::test]
    fn filesystem_folder_context_menu_adds_and_removes_favorite(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let settings_path = directory.path().join("settings.json");
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = vec![archive::ArchiveEntry {
                name: "folder".into(),
                source_path: None,
                size: 0,
                is_directory: true,
                modified: None,
                is_encrypted: false,
            }];
            manager.preferences_path = Some(settings_path.clone());
            manager
        });

        cx.update(|window, cx| window.right_click("folder", cx));
        choose_menu_item(cx, "Add to Favorites");
        cx.read(|cx| assert_eq!(view.read(cx).favorite_folders, [folder.clone()]));
        assert_eq!(
            crate::UserPreferences::load(&settings_path).favorite_folders,
            [folder.clone()]
        );

        cx.update(|window, cx| window.right_click("folder", cx));
        choose_menu_item(cx, "Remove from Favorites");
        cx.read(|cx| assert!(view.read(cx).favorite_folders.is_empty()));
        assert!(crate::UserPreferences::load(&settings_path).favorite_folders.is_empty());
    }

    #[gpui::test]
    fn context_menu_opens_conversion_for_the_selected_archive(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.zip");
        std::fs::write(&path, b"PK\x03\x04").unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = vec![archive::ArchiveEntry {
                name: "source.zip".into(),
                source_path: None,
                size: 7,
                is_directory: false,
                modified: None,
                is_encrypted: false,
            }];
            manager
        });

        cx.update(|window, cx| window.right_click("source.zip", cx));
        choose_menu_item(cx, "Convert archive...");
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(matches!(manager.modal, Some(Modal::Conversion)));
            assert_eq!(manager.selected_entry.as_deref(), Some("source.zip"));
        });
    }

    #[test]
    /// Checks that menu icon assets resolve through the application asset registry.
    fn context_menu_icons_are_available_from_registered_assets() {
        for name in [
            IconName::File,
            IconName::Folder,
            IconName::Download,
            IconName::FolderOutput,
            IconName::FolderInput,
            IconName::FileSearch,
            IconName::ShieldCheck,
            IconName::ScanSearch,
            IconName::GitCompare,
            IconName::Copy,
            IconName::Trash,
            IconName::RefreshCw,
        ] {
            let path = name.path();
            let data = gpui::AssetSource::load(&crate::icons::IconAssets, &path).unwrap();
            assert!(data.is_some(), "missing menu icon: {path}");
        }
        for path in ["icons/file.svg", "icons/folder-open.svg", "icons/plus.svg"] {
            assert!(gpui::AssetSource::load(&crate::icons::IconAssets, path).unwrap().is_some(), "missing menu icon: {path}");
        }
    }

    #[test]
    /// Restricts archive-entry actions to operations that do not modify the archive.
    fn archive_entries_only_offer_supported_read_only_operations() {
        let mut target = EntryTarget {
            name: "notes.txt".into(),
            key: "notes.txt".into(),
            path: PathBuf::from("archive.zip"),
            archive_entry: Some("notes.txt".into()),
            password: None,
            is_directory: false,
        };
        assert_eq!(
            target.actions(),
            [
                EntryAction::Open,
                EntryAction::OpenInNewTab,
                EntryAction::CopyPath,
                EntryAction::Info,
                EntryAction::Inspect,
                EntryAction::Extract
            ]
        );
        target.is_directory = true;
        assert_eq!(
            target.actions(),
            [EntryAction::Open, EntryAction::OpenInNewTab, EntryAction::CopyPath, EntryAction::Info]
        );
        target.archive_entry = None;
        target.is_directory = false;
        target.path = PathBuf::from("notes.txt");
        assert_eq!(
            target.actions(),
            [
                EntryAction::Open,
                EntryAction::OpenInNewTab,
                EntryAction::CopyPath,
                EntryAction::Info,
                EntryAction::Inspect,
                EntryAction::CreateArchive,
                EntryAction::MoveTo,
                EntryAction::Trash
            ]
        );
    }

    #[gpui::test]
    /// Ensures a successful Trash result refreshes only the affected directory.
    fn successful_removal_refreshes_the_matching_directory(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.txt");
        std::fs::write(&path, b"notes").unwrap();
        let target = EntryTarget {
            name: "notes.txt".into(),
            key: "notes.txt".into(),
            path: path.clone(),
            archive_entry: None,
            password: None,
            is_directory: false,
        };
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = crate::read_directory(directory.path()).unwrap();
            manager
        });
        let simulated_trash = tempfile::tempdir().unwrap();
        std::fs::rename(&path, simulated_trash.path().join("notes.txt")).unwrap();
        view.update(cx, |manager, cx| manager.complete_delete(target, Ok(()), cx));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.entries.is_empty());
            assert!(manager.file_actions.delete_task.is_none());
            assert_eq!(manager.current_directory.as_deref(), Some(directory.path()));
            assert!(
                manager
                    .status_message
                    .as_ref()
                    .unwrap()
                    .contains("Moved to Trash")
            );
        });
    }
}
