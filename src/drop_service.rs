//! Routes dropped filesystem paths to navigation, archive inspection, or creation.

use crate::{ArchiveManager, Modal, Theme, archive, file_actions::EntryTarget};
use gpui::{Context, IntoElement, Render, Window, div, prelude::*, px, rgb};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(crate) struct ArchiveFileDrag {
    pub target: EntryTarget,
    pub theme: Theme,
}

pub(crate) fn archive_drop_feedback_color(theme: Theme) -> u32 {
    if theme.window < 0x808080 {
        0x4ade80
    } else {
        0x15803d
    }
}

impl Render for ArchiveFileDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().flex().items_center().gap_2().px_3().py_2().max_w(px(280.0))
            .rounded_sm().border_1().border_color(rgb(self.theme.accent))
            .bg(rgb(self.theme.surface)).text_color(rgb(self.theme.text)).text_sm().shadow_md()
            .child(crate::app_icon(if self.target.is_directory { "folder" } else { "file" }, 16.0, self.theme.accent))
            .child(div().truncate().child(self.target.name.clone()))
    }
}

pub(crate) fn archive_drop_area(
    area: gpui::Stateful<gpui::Div>,
    destination: EntryTarget,
    theme: Theme,
    enabled: bool,
    cx: &mut Context<ArchiveManager>,
) -> gpui::Stateful<gpui::Div> {
    let feedback_color = archive_drop_feedback_color(theme);
    let hover_target = destination.clone();
    let external_hover = destination.clone();
    let external_target = destination.clone();
    area.border_2().border_color(gpui::transparent_black())
    .drag_over::<ArchiveFileDrag>(move |style, drag, _, _| {
        let accepted = enabled && accepts_archive_drop(&hover_target, std::slice::from_ref(&drag.target.path));
        style.bg(gpui::rgba(if accepted { (feedback_color << 8) | 0x2a } else { 0xef44441a }))
            .border_color(rgb(if accepted { feedback_color } else { 0xef4444 }))
    })
    .drag_over::<gpui::ExternalPaths>(move |style, paths, _, _| {
        let accepted = enabled && accepts_archive_drop(&external_hover, paths.paths());
        style.bg(gpui::rgba(if accepted { (feedback_color << 8) | 0x2a } else { 0xef44441a }))
            .border_color(rgb(if accepted { feedback_color } else { 0xef4444 }))
    })
    .on_drop(cx.listener(move |manager, drag: &ArchiveFileDrag, window, cx| {
        cx.stop_propagation();
        manager.receive_archive_drop(&destination, std::slice::from_ref(&drag.target.path), window, cx);
    }))
    .on_drop(cx.listener(move |manager, paths: &gpui::ExternalPaths, window, cx| {
        cx.stop_propagation();
        manager.receive_archive_drop(&external_target, paths.paths(), window, cx);
    }))
}

pub(crate) fn accepts_archive_drop(destination: &EntryTarget, sources: &[PathBuf]) -> bool {
    archive::can_add_to_archive(&destination.path)
        && (destination.archive_entry.is_none() || destination.is_directory)
        && (archive::ResolvedFormat::from_file(&destination.path) != Some(archive::ResolvedFormat::Ar)
            || (destination.archive_entry.is_none() && sources.iter().all(|source| source.is_file())))
        && !sources.is_empty() && sources.len() <= 256
        && sources.iter().all(|source| {
            let Ok(source) = std::fs::canonicalize(source) else { return false; };
            let Ok(path) = std::fs::canonicalize(&destination.path) else { return false; };
            source != path && (!source.is_dir() || !path.starts_with(source))
        })
}

pub(crate) fn accepts_entry_drop(destination: &EntryTarget, sources: &[PathBuf]) -> bool {
    if destination.is_directory && destination.archive_entry.is_none() {
        sources.len() == 1 && crate::transfer::can_move_to_directory(&sources[0], &destination.path)
    } else {
        accepts_archive_drop(destination, sources)
    }
}

pub(crate) fn accepts_folder_drag(target: &EntryTarget, directory: &Path) -> bool {
    if target.archive_entry.is_some() {
        if target.is_directory || !archive::can_extract_archive(&target.path) {
            return false;
        }
        let Some(entry) = target.archive_entry.as_deref() else {
            return false;
        };
        let Ok(relative) = archive::safe_extraction_path(entry) else {
            return false;
        };
        let Some(name) = relative.file_name() else {
            return false;
        };
        return std::fs::canonicalize(directory)
            .ok()
            .filter(|directory| directory.is_dir())
            .is_some_and(|directory| {
                std::fs::symlink_metadata(directory.join(name)).is_err()
            });
    }
    crate::transfer::can_move_to_directory(&target.path, directory)
}

pub(crate) fn folder_drop_area(
    area: gpui::Stateful<gpui::Div>,
    directory: PathBuf,
    theme: Theme,
    enabled: bool,
    cx: &mut Context<ArchiveManager>,
) -> gpui::Stateful<gpui::Div> {
    let internal_hover = directory.clone();
    let external_hover = directory.clone();
    let external_target = directory.clone();
    area.border_l_2().border_color(gpui::transparent_black())
        .drag_over::<ArchiveFileDrag>(move |style, drag, _, _| {
            let accepted = enabled && accepts_folder_drag(&drag.target, &internal_hover);
            style.bg(gpui::rgba(if accepted { (theme.accent << 8) | 0x2a } else { 0xef44441a }))
                .border_color(rgb(if accepted { theme.accent } else { 0xef4444 }))
        })
        .drag_over::<gpui::ExternalPaths>(move |style, paths, _, _| {
            let accepted = enabled && paths.paths().len() == 1
                && crate::transfer::can_move_to_directory(&paths.paths()[0], &external_hover);
            style.bg(gpui::rgba(if accepted { (theme.accent << 8) | 0x2a } else { 0xef44441a }))
                .border_color(rgb(if accepted { theme.accent } else { 0xef4444 }))
        })
        .on_drop(cx.listener(move |manager, drag: &ArchiveFileDrag, window, cx| {
            cx.stop_propagation();
            manager.receive_folder_drag(&directory, &drag.target, window, cx);
        }))
        .on_drop(cx.listener(move |manager, paths: &gpui::ExternalPaths, window, cx| {
            cx.stop_propagation();
            manager.receive_folder_drop(&external_target, paths.paths(), window, cx);
        }))
}

impl ArchiveManager {
    pub(crate) fn receive_folder_drag(
        &mut self,
        directory: &Path,
        target: &EntryTarget,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !self.archive_drop_enabled() {
            return;
        }
        if target.archive_entry.is_some() {
            if accepts_folder_drag(target, directory) {
                self.extract_archive_entry_to_folder(target, directory, window, cx);
            } else {
                self.status_message = Some(format!(
                    "{}: {}",
                    self.language.text("Failed to extract"),
                    self.language.text("This location cannot accept the dropped files")
                ));
                cx.notify();
            }
        } else {
            self.receive_folder_drop(directory, std::slice::from_ref(&target.path), window, cx);
        }
    }

    pub(crate) fn receive_entry_drop(&mut self, destination: &EntryTarget, sources: &[PathBuf], window: &Window, cx: &mut Context<Self>) {
        if destination.is_directory && destination.archive_entry.is_none() {
            self.receive_folder_drop(&destination.path, sources, window, cx);
        } else {
            self.receive_archive_drop(destination, sources, window, cx);
        }
    }

    pub(crate) fn receive_folder_drop(&mut self, directory: &Path, sources: &[PathBuf], window: &Window, cx: &mut Context<Self>) {
        if !self.archive_drop_enabled() { return; }
        if sources.len() != 1 || !crate::transfer::can_move_to_directory(&sources[0], directory) {
            self.status_message = Some(format!("{}: {}", self.language.text("Failed to move item"),
                self.language.text("This location cannot accept the dropped files")));
            cx.notify();
            return;
        }
        let path = sources[0].clone();
        let target = EntryTarget {
            name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            key: path.to_string_lossy().into_owned(), is_directory: path.is_dir(), path,
            archive_entry: None, password: None,
        };
        self.move_entry_to_folder(target, directory.to_path_buf(), window, cx);
    }

    pub(crate) fn archive_drop_enabled(&self) -> bool {
        self.modal.is_none()
    }

    pub(crate) fn receive_archive_drop(&mut self, destination: &EntryTarget, sources: &[PathBuf], window: &Window, cx: &mut Context<Self>) {
        if !self.archive_drop_enabled() { return; }
        if !accepts_archive_drop(destination, sources) {
            let reason = if archive::ResolvedFormat::from_file(&destination.path) == Some(archive::ResolvedFormat::Rar)
                && !archive::can_add_to_archive(&destination.path)
            {
                "Install the rar command-line tool to modify RAR archives"
            } else if archive::ResolvedFormat::from_file(&destination.path).is_some_and(|format| format.is_single_compressed()) {
                "A single compressed stream cannot contain additional files"
            } else if archive::ResolvedFormat::from_file(&destination.path) == Some(archive::ResolvedFormat::Ar) {
                "AR archives support files only. Select individual files instead of folders."
            } else { "This location cannot accept the dropped files" };
            self.status_message = Some(format!("{}: {}", self.language.text("Failed to add to archive"),
                self.language.text(reason)));
            cx.notify();
            return;
        }
        self.add_files_to_archive(destination.clone(), sources.to_vec(), window, cx);
    }

    /// Handles dropped paths according to the active modal and current workspace state.
    pub fn receive_external_paths(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty()
            || paths.len() > 256
            || self.creation_task.is_some()
            || (self.modal.is_some() && self.modal != Some(Modal::CreateArchive))
        {
            return;
        }
        if paths.iter().any(|path| !path.is_file() && !path.is_dir()) {
            self.status_message =
                Some(self.language.text("Dropped item is unavailable").to_owned());
            cx.notify();
            return;
        }
        if self.modal == Some(Modal::CreateArchive) {
            if self.creation.options.format.is_single_compressed() {
                let dropped_file = paths.iter().any(|path| path.is_file());
                let selected_file = self.creation.sources.iter().any(|path| path.is_file());
                if dropped_file || !selected_file {
                    self.creation.sources = paths.to_vec();
                    self.creation.normalize_single_compressed_sources();
                } else {
                    self.creation.error = Some(
                        "Compression streams require one file and cannot be created from folders."
                            .to_owned(),
                    );
                }
            } else {
                for path in paths {
                    if !self.creation.sources.contains(path) {
                        self.creation.sources.push(path.clone());
                    }
                }
                self.creation.error = None;
            }
            cx.notify();
            return;
        }
        if self.inspector.visible
            && self.inspector.mode.is_audit()
            && paths.iter().all(|path| path.is_file())
        {
            let sources = paths.iter().map(|path| (path.clone(), None)).collect();
            self.inspect_sources(sources, self.inspector.mode, window, cx);
            return;
        }
        if paths.len() == 1 {
            let path = &paths[0];
            if path.is_dir() {
                self.inspector.close();
                self.navigate_to(path.clone(), cx);
                return;
            }
            if archive::is_supported_archive(path) {
                self.navigate_into_archive(path.clone(), cx);
                return;
            }
        }
        self.creation.sources = paths.to_vec();
        self.creation.error = None;
        self.show_modal(Modal::CreateArchive, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn internal_drag_moves_files_archives_and_folders_to_directory_targets(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        fn drag(cx: &mut gpui::VisualTestContext, source: &Path, destination: gpui::SharedString) {
            let start = cx.update(|window, _| window.find(gpui::SharedString::from(source.to_string_lossy().into_owned())).bounds()).center();
            let end = match destination.as_ref() {
                "recent-folder-0" => cx.debug_bounds("recent-folder-0").unwrap().center(),
                "recent-folder-1" => cx.debug_bounds("recent-folder-1").unwrap().center(),
                _ => cx.update(|window, _| window.find(destination).bounds()).center(),
            };
            cx.simulate_mouse_move(start, None, gpui::Modifiers::default());
            cx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.simulate_mouse_move(start + gpui::point(px(20.0), px(0.0)), gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.run_until_parked();
            cx.simulate_mouse_move(end, gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.run_until_parked();
            cx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
        }
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("destination");
        let file = directory.path().join("notes.txt");
        let archive = directory.path().join("bundle.zip");
        let folder = directory.path().join("folder");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(&file, "notes 中文").unwrap();
        archive::create_zip(&archive, std::slice::from_ref(&file)).unwrap();
        std::fs::create_dir_all(folder.join("nested")).unwrap();
        std::fs::write(folder.join("nested/data.txt"), b"nested").unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = crate::read_directory(directory.path()).unwrap();
            manager.recent_folders = vec![destination.clone()];
            manager
        });
        drag(cx, &file, gpui::SharedString::from(destination.to_string_lossy().into_owned()));
        finish_addition(&view, cx);
        assert!(!file.exists());
        assert_eq!(std::fs::read_to_string(destination.join("notes.txt")).unwrap(), "notes 中文");
        cx.read(|cx| assert!(!view.read(cx).recent_folders.is_empty()));
        assert!(cx.debug_bounds("recent-folder-0").is_some());
        drag(cx, &archive, "recent-folder-0".into());
        finish_addition(&view, cx);
        assert!(!archive.exists());
        assert!(archive::list_archive(&destination.join("bundle.zip")).unwrap().iter().any(|entry| entry.name == "notes.txt"));
        drag(cx, &folder, gpui::SharedString::from(destination.to_string_lossy().into_owned()));
        finish_addition(&view, cx);
        assert!(!folder.exists());
        assert_eq!(std::fs::read(destination.join("folder/nested/data.txt")).unwrap(), b"nested");
        std::fs::write(&file, b"do not replace").unwrap();
        view.update(cx, |manager, cx| manager.refresh_location(cx));
        cx.run_until_parked();
        drag(cx, &file, gpui::SharedString::from(destination.to_string_lossy().into_owned()));
        finish_addition(&view, cx);
        assert_eq!(std::fs::read(&file).unwrap(), b"do not replace");
        assert_eq!(std::fs::read_to_string(destination.join("notes.txt")).unwrap(), "notes 中文");
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.status_message.as_ref().unwrap().starts_with("Failed to move item"));
            assert!(manager.modal.is_none());
        });
    }

    fn finish_addition(view: &gpui::Entity<ArchiveManager>, cx: &mut gpui::VisualTestContext) {
        for _ in 0..16 {
            cx.run_until_parked();
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            if !cx.read(|cx| view.read(cx).file_actions.operations.is_busy()) { return; }
        }
        panic!("archive update did not finish");
    }

    #[gpui::test]
    fn archive_file_drag_extracts_one_entry_into_the_other_browser_folder(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("destination");
        let source_file = directory.path().join("notes.txt");
        let archive_path = directory.path().join("source.zip");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(&source_file, b"archive entry contents").unwrap();
        archive::create_zip(&archive_path, std::slice::from_ref(&source_file)).unwrap();

        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            crate::tests::test_manager(cx)
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.receive_external_paths(&[directory.path().to_path_buf()], window, cx);
                manager.active_browser = 1;
                manager.receive_external_paths(&[directory.path().to_path_buf()], window, cx);
                manager.active_browser = 0;
                manager.receive_external_paths(&[archive_path.clone()], window, cx);
            });
        });
        cx.run_until_parked();

        let source = cx.debug_bounds("file:notes.txt").unwrap().center();
        let target = cx
            .update(|window, _| {
                window
                    .find(gpui::SharedString::from(
                        destination.to_string_lossy().into_owned(),
                    ))
                    .bounds()
            })
            .center();
        cx.simulate_mouse_move(source, None, gpui::Modifiers::default());
        cx.simulate_mouse_down(source, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(
            source + gpui::point(gpui::px(20.0), gpui::px(0.0)),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        cx.simulate_mouse_move(target, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.simulate_mouse_up(target, gpui::MouseButton::Left, gpui::Modifiers::default());
        finish_addition(&view, cx);

        assert_eq!(
            std::fs::read(destination.join("notes.txt")).unwrap(),
            b"archive entry contents"
        );
        assert!(archive_path.is_file());
        assert_eq!(archive::list_archive(&archive_path).unwrap().len(), 1);
    }

    #[gpui::test]
    fn internal_drag_highlights_the_archive_and_adds_a_file(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.txt");
        let source = directory.path().join("source.txt");
        std::fs::write(&original, b"old content").unwrap();
        std::fs::write(&source, b"new content").unwrap();
        let path = directory.path().join("target.tar.gz");
        archive::create_archive(&path, &[original], archive::CreateOptions {
            format: archive::ResolvedFormat::TarGz, ..Default::default()
        }).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = crate::read_directory(directory.path()).unwrap();
            for entry in &mut manager.entries { entry.source_path = None; }
            manager
        });
        let source_bounds = cx.debug_bounds("file:source.txt").unwrap();
        let destination_bounds = cx.debug_bounds("file:target.tar.gz").unwrap();
        cx.simulate_mouse_move(source_bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_mouse_down(source_bounds.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(source_bounds.center() + gpui::point(px(20.0), px(0.0)), gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.simulate_mouse_move(destination_bounds.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let manager = view.read(cx);
            let dark = manager.theme_mode.is_dark(window.appearance());
            let color = archive_drop_feedback_color(Theme::new(dark));
            assert!(window.painted_quads().iter().any(|quad| {
                quad.background == gpui::rgba((color << 8) | 0x2a).into()
                    && (quad.bounds.origin.y.0 - f32::from(destination_bounds.top()) * window.scale_factor()).abs() < 1.0
            }), "the destination must visibly react while hovered by a dragged file");
            assert!(window.painted_quads().iter().any(|quad| {
                quad.border_color == gpui::rgb(color).into()
                    && (quad.bounds.origin.y.0
                        - f32::from(destination_bounds.top()) * window.scale_factor())
                        .abs()
                        < 1.0
            }), "the hovered destination row must show an accent border");
        });
        cx.simulate_mouse_up(destination_bounds.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
        finish_addition(&view, cx);
        let entries = archive::list_archive(&path).unwrap();
        assert!(entries.iter().any(|entry| entry.name == "source.txt"));
        assert!(entries.iter().any(|entry| entry.name == "original.txt"));
        assert!(source.exists());
        cx.read(|cx| assert!(view.read(cx).status_message.as_ref().unwrap().starts_with("Added to archive")));
    }

    #[gpui::test]
    fn dropping_into_an_open_archive_updates_the_selected_folder(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.txt");
        let source = directory.path().join("source.txt");
        std::fs::write(&original, b"old").unwrap();
        std::fs::write(&source, b"new").unwrap();
        let path = directory.path().join("target.7z");
        archive::create_archive(&path, &[original], archive::CreateOptions {
            format: archive::ResolvedFormat::SevenZip, ..Default::default()
        }).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        cx.update(|window, cx| view.update(cx, |manager, cx| {
            manager.install_archive(path.clone(), archive::list_archive(&path).unwrap(), None, cx);
            manager.selected_folder = "documents".into();
            manager.receive_archive_drop(&EntryTarget {
                name: "documents".into(), key: "documents".into(), path: path.clone(),
                archive_entry: Some("documents".into()), password: None, is_directory: true,
            }, std::slice::from_ref(&source), window, cx);
        }));
        finish_addition(&view, cx);
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.selected_folder, "documents");
            assert!(manager.entries.iter().any(|entry| entry.name == "documents/source.txt"));
        });
        let target = EntryTarget { name: "target.7z".into(), key: "target".into(), path: path.clone(),
            archive_entry: None, password: None, is_directory: false };
        assert!(!accepts_archive_drop(&target, std::slice::from_ref(&path)));
        assert!(!accepts_archive_drop(&target, &[directory.path().to_path_buf()]));
    }

    #[gpui::test]
    /// Verifies that drops open archives/folders or populate archive creation sources.
    fn drop_opens_archives_and_folders_and_populates_creation_form(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("notes.txt");
        let other = root.path().join("other.txt");
        std::fs::write(&file, b"notes").unwrap();
        std::fs::write(&other, b"other").unwrap();
        let path = root.path().join("archive.zip");
        archive::create_archive(&path, &[file.clone()], archive::CreateOptions::default()).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.receive_external_paths(&[path.clone()], window, cx)
            })
        });
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(view.read(cx).archive_path.as_ref(), Some(&path)));
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.receive_external_paths(&[root.path().to_path_buf()], window, cx)
            })
        });
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).current_directory.as_deref(),
                Some(root.path())
            )
        });
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.receive_external_paths(&[file.clone(), other.clone()], window, cx)
            })
        });
        cx.read(|cx| {
            let this = view.read(cx);
            assert!(this.modal == Some(Modal::CreateArchive));
            assert_eq!(this.creation.sources.len(), 2);
        });
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.receive_external_paths(&[file], window, cx)
            })
        });
        cx.read(|cx| assert_eq!(view.read(cx).creation.sources.len(), 2));
    }
}
