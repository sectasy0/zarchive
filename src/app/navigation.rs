use crate::{
    ArchiveManager, archive::ArchiveEntry, browser, file_actions::EntryTarget, sort_entries,
};
use gpui::{Context, Window};
use std::path::{Path, PathBuf};

impl ArchiveManager {
    pub(super) fn visible_browser_entries(&self) -> Vec<ArchiveEntry> {
        let query = self.toolbar_search.text_query();
        let mut entries = if self.show_favorites {
            self.favorite_folders
                .iter()
                .map(|path| ArchiveEntry {
                    name: path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned(),
                    source_path: Some(path.to_string_lossy().into_owned()),
                    size: 0,
                    modified: None,
                    is_directory: true,
                    is_encrypted: false,
                })
                .collect()
        } else if self.current_directory.is_some() {
            self.entries.clone()
        } else {
            browser::search_archive_entries(&self.entries, &self.selected_folder, &query)
        };
        if !query.is_empty() {
            entries.retain(|entry| entry.name.to_lowercase().contains(&query));
        }
        if self.current_directory.is_some() && !self.show_favorites {
            entries.extend(
                self.toolbar_search
                    .matches
                    .values()
                    .map(|result| result.entry.clone()),
            );
        }
        entries.retain(|entry| {
            let size = if entry.is_directory && self.current_directory.is_some() {
                self.directory_sizes
                    .as_ref()
                    .and_then(|sizes| sizes.get(&entry.name))
                    .copied()
            } else {
                Some(entry.size)
            };
            !self.filters.query_invalid
                && self.filters.criteria.matches(entry, size, entry.modified)
        });
        sort_entries(&mut entries, self.sort_column, self.sort_ascending);
        entries
    }

    fn browser_entry_target(&self, entry: &ArchiveEntry) -> EntryTarget {
        if self.show_favorites {
            let path = PathBuf::from(entry.source_path.as_deref().unwrap_or(&entry.name));
            return EntryTarget {
                name: entry.name.clone(),
                key: path.to_string_lossy().into_owned(),
                path,
                archive_entry: None,
                password: None,
                is_directory: true,
            };
        }
        let key = entry.source_path.clone().unwrap_or_else(|| {
            if self.selected_folder.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", self.selected_folder, entry.name)
            }
        });
        if let Some(result) = self.toolbar_search.matches.get(&key) {
            return result.target.clone();
        }
        EntryTarget {
            name: entry.name.clone(),
            path: self
                .current_directory
                .as_ref()
                .map(|directory| {
                    directory.join(entry.source_path.as_deref().unwrap_or(&entry.name))
                })
                .or_else(|| self.archive_path.clone())
                .unwrap_or_default(),
            archive_entry: self.current_directory.is_none().then(|| key.clone()),
            key,
            password: self.archive_password.clone(),
            is_directory: entry.is_directory,
        }
    }

    fn browser_keyboard_active(&self, window: &Window) -> bool {
        self.modal.is_none() && self.content_focus.is_focused(window) && self.error.is_none()
    }

    fn selected_browser_target(&self) -> Option<EntryTarget> {
        self.visible_browser_entries()
            .iter()
            .map(|entry| self.browser_entry_target(entry))
            .find(|target| Some(target.key.as_str()) == self.selected_entry.as_deref())
    }

    pub(super) fn move_browser_selection(
        &mut self,
        offset: isize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !self.browser_keyboard_active(window) {
            return;
        }
        let entries = self.visible_browser_entries();
        if entries.is_empty() {
            return;
        }
        let current = entries.iter().position(|entry| {
            Some(self.browser_entry_target(entry).key.as_str()) == self.selected_entry.as_deref()
        });
        let index = current.map_or_else(
            || if offset < 0 { entries.len() - 1 } else { 0 },
            |index| index.saturating_add_signed(offset).min(entries.len() - 1),
        );
        self.selected_entry = Some(self.browser_entry_target(&entries[index]).key);
        self.file_scroll
            .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
        cx.notify();
    }

    pub(super) fn preview_browser_selection(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.browser_keyboard_active(window) {
            return;
        }
        if let Some(target) = self.selected_browser_target() {
            let same_target = if let Some(preview) = &self.inspector.preview {
                preview.read(cx).matches_target(&target)
            } else {
                target.archive_entry.is_none()
                    && self
                        .inspector
                        .reports
                        .get(self.inspector.active)
                        .is_some_and(|report| report.path == target.path)
            };
            if self.inspector.visible && same_target {
                self.inspector.close();
                cx.notify();
            } else {
                self.preview_archive_target(&target, window, cx);
            }
        } else if self.inspector.visible {
            self.inspector.close();
            cx.notify();
        } else if self.archive_path.is_some() {
            self.inspect_current(crate::inspection::InspectionMode::Preview, window, cx);
        }
    }

    pub(super) fn enter_browser_folder(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.browser_keyboard_active(window) {
            return;
        }
        if let Some(target) = self
            .selected_browser_target()
            .filter(|target| target.is_directory)
        {
            self.open_entry(&target, cx);
        }
    }

    pub(super) fn leave_browser_folder(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.browser_keyboard_active(window) {
            return;
        }
        if let Some(directory) = &self.current_directory {
            if let Some(parent) = directory.parent() {
                self.navigate_to(parent.to_path_buf(), cx);
            }
        } else if !self.selected_folder.is_empty() {
            self.selected_folder = Path::new(&self.selected_folder)
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.selected_entry = None;
            cx.notify();
        } else {
            self.navigate_back(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> ArchiveEntry {
        ArchiveEntry {
            name: name.into(),
            source_path: None,
            size: 1,
            modified: None,
            is_directory: false,
            is_encrypted: false,
        }
    }

    #[gpui::test]
    fn arrows_follow_sorted_filtered_rows_without_opening_inspector(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(root.path().to_owned());
            manager.entries = vec![file("zeta.txt"), file("alpha.txt"), file("beta.txt")];
            manager.sort_column = crate::SortColumn::Name;
            manager.sort_ascending = true;
            manager
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| window.focus(&manager.content_focus, cx))
        });
        for (keys, selected) in [
            ("down", "alpha.txt"),
            ("down", "beta.txt"),
            ("up", "alpha.txt"),
            ("up", "alpha.txt"),
            ("down down down", "zeta.txt"),
        ] {
            cx.simulate_keystrokes(keys);
            cx.read(|cx| {
                let manager = view.read(cx);
                assert_eq!(manager.selected_entry.as_deref(), Some(selected));
                assert!(!manager.inspector.visible);
            });
        }
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.set_search_query("beta".into(), window, cx);
                manager.selected_entry = None;
                window.focus(&manager.content_focus, cx);
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("down");
        cx.read(|cx| assert_eq!(view.read(cx).selected_entry.as_deref(), Some("beta.txt")));
    }

    #[gpui::test]
    fn arrows_enter_and_leave_archive_folders(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("notes.txt"), b"archived preview").unwrap();
        let archive = root.path().join("source.zip");
        crate::archive::create_archive(&archive, &[folder], Default::default()).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.archive_path = Some(archive.clone());
            manager.entries = crate::archive::list_archive(&archive).unwrap();
            manager
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| window.focus(&manager.content_focus, cx))
        });
        cx.simulate_keystrokes("down right");
        cx.read(|cx| assert_eq!(view.read(cx).selected_folder, "folder"));
        cx.simulate_keystrokes("down");
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).selected_entry.as_deref(),
                Some("folder/notes.txt")
            );
            assert!(!view.read(cx).inspector.visible);
        });
        cx.simulate_keystrokes("left");
        cx.read(|cx| {
            assert!(view.read(cx).selected_folder.is_empty());
            assert!(view.read(cx).selected_entry.is_none());
        });
    }

    #[gpui::test]
    fn space_and_arrows_do_not_take_focus_from_search_input(cx: &mut gpui::TestAppContext) {
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.entries = vec![file("hello world.txt")];
            manager
        });
        let bounds = cx.debug_bounds("toolbar-search").unwrap();
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.simulate_input("hello");
        cx.simulate_keystrokes("space");
        cx.simulate_input("world");
        cx.simulate_keystrokes("left right down");
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.toolbar_search.text_query(), "hello world");
            assert!(manager.selected_entry.is_none());
            assert!(!manager.inspector.visible);
        });
    }
}
