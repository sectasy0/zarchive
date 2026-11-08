//! Runs extraction and move jobs in the background and resumes password-gated work.

use crate::{
    ArchiveManager, archive, file_actions::EntryTarget, password::ArchivePassword, transfer,
};
use gpui::{Context, Window};
use anyhow::Context as _;
use std::path::{Path, PathBuf};

#[derive(Clone)]
/// A filesystem operation whose work can run independently from the UI thread.
enum Operation {
    /// Extracts an archive or selected archived entry to a destination.
    Extract {
        target: EntryTarget,
        destination: PathBuf,
    },
    /// Moves a filesystem item into an existing directory.
    Move {
        target: EntryTarget,
        directory: PathBuf,
    },
    Add {
        target: EntryTarget,
        sources: Vec<PathBuf>,
    },
    RemoveEntry { target: EntryTarget },
}

impl Operation {
    fn resources(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.target().path.clone()];
        match self {
            Self::Extract { destination, .. } => paths.push(destination.clone()),
            Self::Move { target, directory } => {
                if let Some(name) = target.path.file_name() { paths.push(directory.join(name)); }
            }
            Self::Add { sources, .. } => paths.extend(sources.iter().cloned()),
            Self::RemoveEntry { .. } => {},
        }
        paths
    }
    /// Returns the item whose source location is affected by this operation.
    fn target(&self) -> &EntryTarget {
        match self {
            Self::Extract { target, .. } | Self::Move { target, .. } | Self::Add { target, .. } | Self::RemoveEntry { target } => target,
        }
    }
    /// Returns the untranslated progress-message key for this operation kind.
    fn progress(&self) -> &'static str {
        match self {
            Self::Extract { .. } => "Extracting archive...",
            Self::Move { .. } => "Moving item...",
            Self::Add { .. } => "Adding files to archive...",
            Self::RemoveEntry { .. } => "Removing entry from archive...",
        }
    }
    /// Performs the extraction or move and returns its resulting path.
    fn run(&self) -> anyhow::Result<PathBuf> {
        match self {
            Self::Extract {
                target,
                destination,
            } => {
                let password = target.password.as_ref().map(|password| password.as_str());
                if archive::is_single_compressed_archive(&target.path) {
                    archive::extract_single_compressed_to(&target.path, destination)?;
                } else if let Some(entry) = &target.archive_entry {
                    archive::extract_archive_selection_to(
                        &target.path,
                        destination,
                        password,
                        Some(entry),
                    )?;
                } else {
                    archive::extract_archive_to(&target.path, destination, password)?;
                }
                Ok(destination.clone())
            }
            Self::Move { target, directory } => {
                transfer::move_to_directory(&target.path, directory)
            }
            Self::Add { target, sources } => {
                archive::add_to_archive_with_password(&target.path, sources, target.archive_entry.as_deref().unwrap_or(""),
                    target.password.as_ref().map(|password| password.as_str()))?;
                Ok(target.path.clone())
            }
            Self::RemoveEntry { target } => {
                archive::remove_archive_entry(&target.path, target.archive_entry.as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Archive entry not found"))?,
                    target.password.as_ref().map(|password| password.as_str()))?;
                Ok(target.path.clone())
            }
        }
    }
}

/// Classifies an operation failure for UI reporting or password recovery.
enum Failure {
    /// The archive requires credentials before extraction can continue.
    PasswordRequired,
    /// An operation failed for a reason other than missing credentials.
    Other(String),
}
#[derive(Default)]
/// Tracks active, queued, and password-paused background operations.
pub struct Operations {
    active: usize,
    pending_password: Option<Operation>,
    queued: Option<Operation>,
}

impl Operations {
    /// Reports whether a background operation is currently running.
    pub fn is_busy(&self) -> bool {
        self.active > 0
    }
    /// Discards an operation paused while waiting for an archive password.
    pub fn cancel_password(&mut self) {
        self.pending_password = None;
    }
}

/// Starts a queued operation once the current job has finished.
pub fn prepare(
    manager: &mut ArchiveManager,
    window: &mut Window,
    cx: &mut Context<ArchiveManager>,
) {
    if !manager.file_actions.operations.is_busy() {
        if let Some(operation) = manager.file_actions.operations.queued.take() {
            manager.start_operation(operation, window, cx);
        }
    }
}

impl ArchiveManager {
    pub(crate) fn extract_inspected_entry_here(&mut self, target: &EntryTarget, window: &Window, cx: &mut Context<Self>) {
        if !self.archive_drop_enabled() { return; }
        let Some(entry) = target.archive_entry.as_deref() else { return; };
        let Some(name) = std::path::Path::new(entry).file_name() else { return; };
        let Some(directory) = self.current_directory.clone().or_else(|| target.path.parent().map(std::path::Path::to_path_buf)) else { return; };
        self.start_operation(Operation::Extract { target: target.clone(), destination: directory.join(name) }, window, cx);
    }

    pub(crate) fn remove_inspected_entry(&mut self, target: EntryTarget, window: &Window, cx: &mut Context<Self>) {
        self.start_operation(Operation::RemoveEntry { target }, window, cx);
    }

    pub(crate) fn move_entry_to_folder(&mut self, target: EntryTarget, directory: PathBuf, window: &Window, cx: &mut Context<Self>) {
        self.start_operation(Operation::Move { target, directory }, window, cx);
    }

    pub(crate) fn add_files_to_archive(&mut self, target: EntryTarget, sources: Vec<PathBuf>, window: &Window, cx: &mut Context<Self>) {
        self.start_operation(Operation::Add { target, sources }, window, cx);
    }

    /// Resolves the selected row or open archive as an extraction target.
    fn current_extraction_target(&self) -> Option<EntryTarget> {
        if self.show_favorites {
            return None;
        }
        if let Some(target) = self.selected_search_target() { return Some(target.clone()); }
        if let Some(directory) = &self.current_directory {
            let selected = self.selected_entry.as_deref()?;
            let entry = self
                .entries
                .iter()
                .find(|entry| entry.source_path.as_deref().unwrap_or(&entry.name) == selected)?;
            let path = directory.join(entry.source_path.as_deref().unwrap_or(&entry.name));
            if entry.is_directory || !archive::can_extract_archive(&path) {
                return None;
            }
            return Some(EntryTarget {
                name: entry.name.clone(),
                key: selected.to_owned(),
                path,
                archive_entry: None,
                password: None,
                is_directory: false,
            });
        }
        let path = self.archive_path.clone()?;
        if let Some(selected) = self
            .selected_entry
            .as_deref()
            .filter(|selected| !selected.is_empty())
        {
            let visible = crate::visible_entries(&self.entries, &self.selected_folder);
            if let Some(entry) = visible.iter().find(|entry| {
                entry.source_path.as_deref().map_or_else(
                    || {
                        if self.selected_folder.is_empty() {
                            entry.name == selected
                        } else {
                            format!("{}/{}", self.selected_folder, entry.name) == selected
                        }
                    },
                    |source| source == selected,
                )
            }) {
                return Some(EntryTarget {
                    name: entry.name.clone(),
                    key: selected.to_owned(),
                    path,
                    archive_entry: Some(selected.to_owned()),
                    password: self.archive_password.clone(),
                    is_directory: entry.is_directory,
                });
            }
            let (_, name, _) = crate::folder_tree(&self.entries)
                .into_iter()
                .find(|(folder, _, _)| folder == selected)?;
            return Some(EntryTarget {
                name,
                key: selected.to_owned(),
                path,
                archive_entry: Some(selected.to_owned()),
                password: self.archive_password.clone(),
                is_directory: true,
            });
        }
        Some(EntryTarget {
            name: self
                .archive_name
                .clone()
                .unwrap_or_else(|| path.display().to_string()),
            key: path.display().to_string(),
            path,
            archive_entry: None,
            password: self.archive_password.clone(),
            is_directory: false,
        })
    }

    /// Reports whether the current archive or selection can be extracted now.
    pub fn can_extract_current_archive(&self) -> bool {
        self.current_extraction_target().is_some()
            && self.error.is_none()
            && self.modal != Some(crate::Modal::Password)
    }

    /// Requests extraction of the current selection or archive.
    pub fn extract_current_archive(
        &mut self,
        here: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_extract_current_archive() {
            return;
        }
        let Some(target) = self.current_extraction_target() else {
            return;
        };
        self.request_extract_archive(&target, here, window, cx);
    }

    /// Chooses an extraction destination and queues the resulting operation.
    pub fn request_extract_archive(
        &mut self,
        target: &EntryTarget,
        here: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if target.archive_entry.is_none() && target.is_directory {
            return;
        }
        if archive::is_single_compressed_archive(&target.path) {
            let Ok(name) = archive::single_compressed_entry_name(&target.path) else {
                return;
            };
            let destination = if here {
                let Some(parent) = target.path.parent() else {
                    return;
                };
                parent.join(name)
            } else {
                let mut dialog = rfd::FileDialog::new()
                    .set_title(self.language.text("Extract file"))
                    .set_file_name(&name);
                if let Some(parent) = target.path.parent() {
                    dialog = dialog.set_directory(parent);
                }
                let Some(destination) = dialog.save_file() else {
                    return;
                };
                destination
            };
            self.start_operation(
                Operation::Extract {
                    target: target.clone(),
                    destination,
                },
                window,
                cx,
            );
            return;
        }
        let directory = if here {
            target.path.parent().map(|path| path.to_path_buf())
        } else {
            let mut dialog =
                rfd::FileDialog::new().set_title(self.language.text("Select destination folder"));
            if let Some(parent) = target.path.parent() {
                dialog = dialog.set_directory(parent);
            }
            dialog.pick_folder()
        };
        let Some(directory) = directory else {
            return;
        };
        let name = if target.archive_entry.is_some() {
            target.name.clone()
        } else {
            transfer::extraction_folder_name(&target.path)
        };
        let destination = directory.join(name);
        self.start_operation(
            Operation::Extract {
                target: target.clone(),
                destination,
            },
            window,
            cx,
        );
    }

    pub(crate) fn extract_archive_entry_to_folder(
        &mut self,
        target: &EntryTarget,
        directory: &Path,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let destination = (|| -> anyhow::Result<PathBuf> {
            anyhow::ensure!(
                target.archive_entry.is_some() && !target.is_directory,
                "Only archive files can be dropped into a folder"
            );
            anyhow::ensure!(
                archive::can_extract_archive(&target.path),
                "This archive cannot be extracted"
            );
            let entry = target.archive_entry.as_deref().unwrap();
            let relative = archive::safe_extraction_path(entry)?;
            let name = relative
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Archive entry has no filename"))?;
            let directory = std::fs::canonicalize(directory)
                .context("Destination folder is unavailable")?;
            anyhow::ensure!(directory.is_dir(), "Destination is not a folder");
            let destination = directory.join(name);
            anyhow::ensure!(
                std::fs::symlink_metadata(&destination).is_err(),
                "The destination already exists"
            );
            Ok(destination)
        })();
        let destination = match destination {
            Ok(destination) => destination,
            Err(error) => {
                self.status_message = Some(format!(
                    "{}: {error:#}",
                    self.language.text("Failed to extract")
                ));
                cx.notify();
                return;
            }
        };
        self.start_operation(
            Operation::Extract {
                target: target.clone(),
                destination,
            },
            window,
            cx,
        );
    }

    /// Opens a destination picker and queues a filesystem move.
    pub fn request_move(
        &mut self,
        target: &EntryTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if target.archive_entry.is_some() {
            return;
        }
        let mut dialog =
            rfd::FileDialog::new().set_title(self.language.text("Select destination folder"));
        if let Some(parent) = target.path.parent() {
            dialog = dialog.set_directory(parent);
        }
        let Some(directory) = dialog.pick_folder() else {
            return;
        };
        self.move_entry_to_folder(target.clone(), directory, window, cx);
    }

    /// Resumes a password-paused extraction with the supplied session credential.
    pub fn resume_extract_with_password(&mut self, password: ArchivePassword) {
        match self.file_actions.operations.pending_password.take() {
            Some(Operation::Extract { mut target, destination }) => {
                target.password = Some(password);
                self.file_actions.operations.queued = Some(Operation::Extract { target, destination });
            }
            Some(Operation::Add { mut target, sources }) => {
                target.password = Some(password);
                self.file_actions.operations.queued = Some(Operation::Add { target, sources });
            }
            Some(Operation::RemoveEntry { mut target }) => {
                target.password = Some(password);
                self.file_actions.operations.queued = Some(Operation::RemoveEntry { target });
            }
            _ => {}
        }
    }

    /// Submits an operation to the shared background worker pool.
    fn start_operation(&mut self, operation: Operation, _window: &Window, cx: &mut Context<Self>) {
        let work = operation.clone();
        let label = operation.progress();
        let browser_tab_id = self.active_browser_tab().id;
        self.file_actions.operations.active += 1;
        self.status_message = None;
        self.background_queue.enqueue(
            label,
            crate::task_queue::TaskControl::default().for_paths(operation.resources()),
            move |control, reporter| async move {
                reporter.phase(work.target().name.clone());
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                work.run()
            },
            move |this, result, window, cx| {
                let _ = this.with_browser_tab(browser_tab_id, |this| {
                this.file_actions.operations.active = this.file_actions.operations.active.saturating_sub(1);
                let completed = match result {
                    Ok(destination) => Some(Ok(destination)),
                    Err(crate::task_queue::TaskFailure::Cancelled) => None,
                    Err(crate::task_queue::TaskFailure::Failed(error)) => Some(Err(
                        if error.downcast_ref::<archive::PasswordFailure>() == Some(&archive::PasswordFailure::Required) {
                            Failure::PasswordRequired
                        } else { Failure::Other(format!("{error:#}")) }
                    )),
                };
                let Some(completed) = completed else {
                    if this.current_directory.as_deref() == operation.target().path.parent()
                        || matches!(&operation, Operation::Move { directory, .. } if this.current_directory.as_ref() == Some(directory))
                    { this.refresh_location(cx); }
                    cx.notify();
                    return;
                };
                match completed {
                    Ok(destination) => {
                        let refresh_inspector = matches!(operation, Operation::Add { .. } | Operation::RemoveEntry { .. })
                            && this.inspector.visible && this.inspector.reports.iter().any(|report| report.path == destination);
                        if matches!(operation, Operation::Move { .. }) { this.inspector.close(); }
                        if matches!(operation, Operation::Add { .. } | Operation::RemoveEntry { .. }) {
                            this.inspector.close();
                            if this.archive_path.as_ref() == Some(&destination) {
                                let folder = this.selected_folder.clone();
                                this.refresh_location(cx);
                                this.selected_folder = folder;
                            }
                        }
                        if this.current_directory.as_deref() == operation.target().path.parent()
                            || this.current_directory.as_deref() == destination.parent()
                            || matches!(
                                &operation,
                                Operation::Move { directory, .. }
                                    if this.current_directory.as_ref() == Some(directory)
                            )
                        { this.refresh_location(cx); }
                        if refresh_inspector {
                            this.inspect_sources(vec![(destination.clone(), operation.target().password.clone())],
                                crate::inspection::InspectionMode::Preview, window, cx);
                        }
                        let message = match operation {
                            Operation::Extract { .. } => "Extracted to",
                            Operation::Move { .. } => "Moved to",
                            Operation::Add { .. } => "Added to archive",
                            Operation::RemoveEntry { .. } => "Removed from archive",
                        };
                        this.status_message = Some(format!("{} {}", this.language.text(message), destination.display()));
                    }
                    Err(Failure::PasswordRequired) => {
                        let path = operation.target().path.clone();
                        this.file_actions.operations.pending_password = Some(operation);
                        this.request_archive_password(path, cx);
                        this.password_prompt.for_extraction = true;
                    }
                    Err(Failure::Other(error)) => {
                        let label = match operation {
                            Operation::Extract { .. } => "Failed to extract",
                            Operation::Move { .. } => "Failed to move item",
                            Operation::Add { .. } => "Failed to add to archive",
                            Operation::RemoveEntry { .. } => "Failed to remove entry from archive",
                        };
                        this.status_message = Some(format!("{}: {error}", this.language.text(label)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::{
        Modal,
        archive::{ArchiveEntry, CompressionLevel, CreateOptions, ResolvedFormat},
    };

    /// Checks that a toolbar menu contains the requested label.
    fn toolbar_menu_contains(
        cx: &mut gpui::VisualTestContext,
        trigger: &'static str,
        label: &'static str,
    ) -> bool {
        use gpui::test::TestWindowExt;
        cx.update(|window, cx| window.click(trigger, cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.render_frame(cx));
        let found = cx.update(|window, _| {
            window
                .within("popup-menu")
                .try_find_by_label(label)
                .is_some()
        });
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        found
    }

    /// Checks that a toolbar item displays its expected keyboard shortcut.
    fn toolbar_menu_has_shortcut(
        cx: &mut gpui::VisualTestContext,
        trigger: &'static str,
        selector: &'static str,
    ) -> bool {
        use gpui::test::TestWindowExt;
        cx.update(|window, cx| window.click(trigger, cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.render_frame(cx));
        let found = cx.debug_bounds(selector).is_some();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        found
    }

    /// Opens a toolbar dropdown and activates one of its items in a UI test.
    fn click_toolbar_menu_item(
        cx: &mut gpui::VisualTestContext,
        trigger: &'static str,
        label: &'static str,
    ) {
        use gpui::test::TestWindowExt;
        cx.update(|window, cx| window.click(trigger, cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.render_frame(cx));
        let bounds =
            cx.update(|window, _| window.within("popup-menu").find_by_label(label).bounds());
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();
    }

    #[gpui::test]
    /// Validates extraction availability, password reuse, and extraction destinations.
    fn toolbar_extraction_requires_open_archive_and_reuses_its_password(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::test::TestWindowExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let archive_path = root.path().join("protected.7z");
        std::fs::write(&source, b"private notes").unwrap();
        archive::create_archive_with_password(
            &archive_path,
            &[source],
            CreateOptions {
                format: ResolvedFormat::SevenZip,
                compression: CompressionLevel::Fast,
                dictionary_mib: 1,
                ..CreateOptions::default()
            },
            Some("toolbar-password"),
        )
        .unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(root.path().to_path_buf());
            manager.entries.clear();
            manager
        });
        for (trigger, item) in [
            ("toolbar-files-dropdown", "Open archive..."),
            ("toolbar-files-dropdown", "New archive..."),
            ("toolbar-extract-dropdown", "Extract here"),
            ("toolbar-extract-dropdown", "Extract to..."),
            ("toolbar-inspect-dropdown", "Test integrity"),
            ("toolbar-inspect-dropdown", "Audit archive"),
            ("toolbar-inspect-dropdown", "Full audit"),
            ("toolbar-inspect-dropdown", "Compare archives"),
        ] {
            assert!(toolbar_menu_contains(cx, trigger, item));
        }
        #[cfg(target_os = "macos")]
        let shortcut_hints = [
            ("toolbar-files-dropdown", "kbd:cmd-o"),
            ("toolbar-files-dropdown", "kbd:cmd-n"),
            ("toolbar-extract-dropdown", "kbd:cmd-e"),
            ("toolbar-extract-dropdown", "kbd:cmd-shift-e"),
            ("toolbar-inspect-dropdown", "kbd:cmd-i"),
            ("toolbar-inspect-dropdown", "kbd:cmd-shift-i"),
        ];
        #[cfg(not(target_os = "macos"))]
        let shortcut_hints = [
            ("toolbar-files-dropdown", "kbd:ctrl-o"),
            ("toolbar-files-dropdown", "kbd:ctrl-n"),
            ("toolbar-extract-dropdown", "kbd:ctrl-e"),
            ("toolbar-extract-dropdown", "kbd:ctrl-shift-e"),
            ("toolbar-inspect-dropdown", "kbd:ctrl-i"),
            ("toolbar-inspect-dropdown", "kbd:ctrl-shift-i"),
        ];
        for (trigger, selector) in shortcut_hints {
            assert!(
                toolbar_menu_has_shortcut(cx, trigger, selector),
                "missing shortcut hint {selector}"
            );
        }
        for viewport in [
            gpui::size(gpui::px(560.0), gpui::px(440.0)),
            gpui::size(gpui::px(1100.0), gpui::px(720.0)),
        ] {
            cx.simulate_resize(viewport);
            cx.run_until_parked();
            let bar = cx.debug_bounds("toolbox-bar").unwrap();
            cx.update(|window, _| {
                for id in [
                    "toolbar-files-dropdown",
                    "toolbar-extract-dropdown",
                    "toolbar-inspect-dropdown",
                    "settings",
                    "about",
                ] {
                    let bounds = window.find(id).bounds();
                    assert!(
                        bounds.origin.x >= bar.origin.x && bounds.right() <= bar.right(),
                        "{id} bounds {bounds:?} exceed toolbar bounds {bar:?}"
                    );
                    assert!(
                        bounds.origin.y >= bar.origin.y && bounds.bottom() <= bar.bottom(),
                        "{id} bounds {bounds:?} exceed toolbar bounds {bar:?}"
                    );
                }
            });
        }
        for label in ["Extract here", "Extract to..."] {
            assert!(toolbar_menu_contains(cx, "toolbar-extract-dropdown", label));
        }
        cx.read(|cx| assert!(!view.read(cx).file_actions.operations.is_busy()));
        view.update(cx, |manager, cx| {
            let entries =
                archive::list_archive_with_password(&archive_path, Some("toolbar-password"))
                    .unwrap();
            manager.install_archive(
                archive_path.clone(),
                entries,
                Some(Arc::new(zeroize::Zeroizing::new(
                    "toolbar-password".to_owned(),
                ))),
                cx,
            );
        });
        cx.run_until_parked();
        for label in ["Extract here", "Extract to..."] {
            assert!(toolbar_menu_contains(cx, "toolbar-extract-dropdown", label));
        }
        click_toolbar_menu_item(cx, "toolbar-extract-dropdown", "Extract here");
        for label in ["Extract here", "Extract to..."] {
            assert!(toolbar_menu_contains(cx, "toolbar-extract-dropdown", label));
        }
        finish_frames(cx, &view);
        assert_eq!(
            std::fs::read(root.path().join("protected/notes.txt")).unwrap(),
            b"private notes"
        );
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.modal.is_none());
            assert!(manager.can_extract_current_archive());
            assert_eq!(manager.archive_path.as_ref(), Some(&archive_path));
        });
        assert!(toolbar_menu_contains(
            cx,
            "toolbar-extract-dropdown",
            "Extract here"
        ));
        std::fs::remove_file(root.path().join("notes.txt")).unwrap();
        cx.update(|window, cx| window.click("notes.txt", cx));
        click_toolbar_menu_item(cx, "toolbar-extract-dropdown", "Extract here");
        finish_frames(cx, &view);
        assert_eq!(
            std::fs::read(root.path().join("notes.txt")).unwrap(),
            b"private notes"
        );
        cx.read(|cx| assert!(view.read(cx).modal.is_none()));
        view.update(cx, |manager, cx| {
            manager.navigate_to(root.path().to_path_buf(), cx)
        });
        cx.run_until_parked();
        for label in ["Extract here", "Extract to..."] {
            assert!(toolbar_menu_contains(cx, "toolbar-extract-dropdown", label));
        }
    }

    #[gpui::test]
    /// Ensures a selected archive entry is extracted instead of the whole archive.
    fn toolbar_extracts_selected_file_instead_of_whole_archive(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let root = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let notes = sources.path().join("notes.txt");
        let other = sources.path().join("other.txt");
        std::fs::write(&notes, b"selected notes").unwrap();
        std::fs::write(&other, b"not selected").unwrap();
        let archive_path = root.path().join("archive.zip");
        archive::create_archive(&archive_path, &[notes, other], CreateOptions::default()).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.install_archive(
                archive_path.clone(),
                archive::list_archive(&archive_path).unwrap(),
                None,
                cx,
            );
            manager
        });
        cx.update(|window, cx| window.click("notes.txt", cx));
        click_toolbar_menu_item(cx, "toolbar-extract-dropdown", "Extract here");
        finish_frames(cx, &view);
        assert_eq!(
            std::fs::read(root.path().join("notes.txt")).unwrap(),
            b"selected notes"
        );
        assert!(!root.path().join("other.txt").exists());
        assert!(!root.path().join("archive").exists());
    }

    #[gpui::test]
    /// Enables toolbar extraction when an archive on disk is selected.
    fn selecting_archive_on_disk_enables_toolbar_extraction(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let root = tempfile::tempdir().unwrap();
        let notes = root.path().join("notes.txt");
        let path = root.path().join("archive.zip");
        std::fs::write(&notes, b"notes").unwrap();
        archive::create_archive(&path, &[notes], CreateOptions::default()).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(root.path().to_path_buf());
            manager.entries = vec![
                ArchiveEntry {
                    name: "notes.txt".into(),
                    source_path: None,
                    size: 5,
                    is_directory: false,
                    modified: None,
                    is_encrypted: false,
                },
                ArchiveEntry {
                    name: "archive.zip".into(),
                    source_path: None,
                    size: 100,
                    is_directory: false,
                    modified: None,
                    is_encrypted: false,
                },
            ];
            manager
        });
        cx.update(|window, cx| window.click("notes.txt", cx));
        assert!(toolbar_menu_contains(
            cx,
            "toolbar-extract-dropdown",
            "Extract here"
        ));
        cx.update(|window, cx| window.click("archive.zip", cx));
        assert!(toolbar_menu_contains(
            cx,
            "toolbar-extract-dropdown",
            "Extract here"
        ));
        click_toolbar_menu_item(cx, "toolbar-extract-dropdown", "Extract here");
        finish_frames(cx, &view);
        assert_eq!(
            std::fs::read(root.path().join("archive/notes.txt")).unwrap(),
            b"notes"
        );
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.current_directory.as_deref(), Some(root.path()));
            assert!(manager.archive_path.is_none());
        });
    }

    #[gpui::test]
    /// Extracts a standalone compressed file beside its source instead of making a folder.
    fn standalone_compression_extracts_to_a_file_here(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("notes.txt");
        let compressed = root.path().join("notes.txt.gz");
        std::fs::write(&source, b"standalone extraction").unwrap();
        archive::create_archive(
            &compressed,
            std::slice::from_ref(&source),
            CreateOptions {
                format: ResolvedFormat::Gzip,
                ..CreateOptions::default()
            },
        )
        .unwrap();
        let target = EntryTarget {
            name: "notes.txt.gz".into(),
            key: "notes.txt.gz".into(),
            path: compressed.clone(),
            archive_entry: None,
            password: None,
            is_directory: false,
        };
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(root.path().to_path_buf());
            manager.entries = crate::read_directory(root.path()).unwrap();
            manager
        });

        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.request_extract_archive(&target, true, window, cx)
            });
        });
        finish_frames(cx, &view);

        assert_eq!(
            std::fs::read(root.path().join("notes.txt")).unwrap(),
            b"standalone extraction"
        );
        assert!(compressed.exists());
        assert!(!root.path().join("notes.txt").is_dir());
    }

    /// Advances pending frame callbacks until the current operation finishes.
    fn finish_frames(cx: &mut gpui::VisualTestContext, view: &gpui::Entity<ArchiveManager>) {
        for _ in 0..8 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
            });
            cx.run_until_parked();
            if !cx.read(|cx| view.read(cx).file_actions.operations.is_busy()) {
                break;
            }
        }
    }

    #[gpui::test]
    /// Resumes extraction after password entry without opening the archive in the UI.
    fn extract_here_requests_password_and_resumes_without_navigating_into_archive(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let path = root.path().join("protected.7z");
        std::fs::write(&source, b"private notes").unwrap();
        archive::create_archive_with_password(
            &path,
            &[source],
            CreateOptions {
                format: ResolvedFormat::SevenZip,
                compression: CompressionLevel::Fast,
                dictionary_mib: 1,
                ..CreateOptions::default()
            },
            Some("extract🙂password"),
        )
        .unwrap();
        let target = EntryTarget {
            name: "protected.7z".into(),
            key: "protected.7z".into(),
            path: path.clone(),
            archive_entry: None,
            password: None,
            is_directory: false,
        };
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(root.path().to_path_buf());
            manager.entries = vec![ArchiveEntry {
                name: "protected.7z".into(),
                source_path: None,
                size: 100,
                is_directory: false,
                modified: None,
                is_encrypted: false,
            }];
            manager
        });
        cx.update(|window, cx| {
            use gpui::test::TestWindowExt;
            window.right_click("protected.7z", cx);
        });
        cx.run_until_parked();
        let bounds = cx.update(|window, cx| {
            use gpui::test::TestWindowExt;
            window.render_frame(cx);
            window
                .within("popup-menu")
                .find_by_label("Extract here")
                .bounds()
        });
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        finish_frames(cx, &view);
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.modal == Some(Modal::Password));
            assert!(manager.password_prompt.for_extraction);
            assert_eq!(manager.current_directory.as_deref(), Some(root.path()));
        });
        cx.simulate_keystrokes("escape");
        assert!(!root.path().join("protected").exists());
        cx.read(|cx| {
            assert!(
                view.read(cx)
                    .file_actions
                    .operations
                    .pending_password
                    .is_none()
            )
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.request_extract_archive(&target, true, window, cx)
            });
        });
        finish_frames(cx, &view);
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        cx.run_until_parked();
        cx.simulate_input("extract🙂password");
        cx.simulate_keystrokes("enter");
        finish_frames(cx, &view);
        assert_eq!(
            std::fs::read(root.path().join("protected/notes.txt")).unwrap(),
            b"private notes"
        );
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.modal.is_none());
            assert!(manager.archive_path.is_none());
            assert_eq!(manager.current_directory.as_deref(), Some(root.path()));
            assert!(
                manager
                    .entries
                    .iter()
                    .any(|entry| entry.name == "protected")
            );
        });
    }

    #[gpui::test]
    /// Refreshes the source directory after a move and reports destination collisions.
    fn move_operation_refreshes_source_directory_and_reports_collisions(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        std::fs::write(&source, b"notes").unwrap();
        let target = EntryTarget {
            name: "notes.txt".into(),
            key: "notes.txt".into(),
            path: source.clone(),
            archive_entry: None,
            password: None,
            is_directory: false,
        };
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(root.path().to_path_buf());
            manager.entries = crate::read_directory(root.path()).unwrap();
            manager
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.start_operation(
                    Operation::Move {
                        target: target.clone(),
                        directory: destination.path().to_path_buf(),
                    },
                    window,
                    cx,
                );
            })
        });
        finish_frames(cx, &view);
        assert!(!source.exists());
        assert_eq!(
            std::fs::read(destination.path().join("notes.txt")).unwrap(),
            b"notes"
        );
        cx.read(|cx| assert!(view.read(cx).entries.is_empty()));
        std::fs::write(&source, b"keep source").unwrap();
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.start_operation(
                    Operation::Move {
                        target,
                        directory: destination.path().to_path_buf(),
                    },
                    window,
                    cx,
                );
            })
        });
        finish_frames(cx, &view);
        assert_eq!(std::fs::read(&source).unwrap(), b"keep source");
        assert_eq!(
            std::fs::read(destination.path().join("notes.txt")).unwrap(),
            b"notes"
        );
        cx.read(|cx| {
            assert!(
                view.read(cx)
                    .status_message
                    .as_ref()
                    .unwrap()
                    .starts_with("Failed to move item")
            )
        });
    }
}
