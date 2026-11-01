//! Implements the titlebar toolbar, keyboard actions, and retained search input.

use crate::{ArchiveManager, Modal, Theme, app_icon, i18n::Language};
use gpui::component::menu::{DropdownMenu, PopupMenuItem};
use gpui::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use gpui::{App, Context, Entity, Focusable, IntoElement, Subscription, Window, div, prelude::*, px, rgb};
use std::{path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, Ordering}}};

pub(crate) struct ArchiveSearchMatch {
    pub(crate) entry: crate::archive::ArchiveEntry,
    pub(crate) target: crate::file_actions::EntryTarget,
    requires_password: bool,
}

fn search_directory_archives(
    directory: &Path,
    entries: &[crate::archive::ArchiveEntry],
    query: &str,
    cancel: &AtomicBool,
) -> Vec<ArchiveSearchMatch> {
    let parsed = crate::filters::parse_search(query);
    let query = parsed.text.trim().to_lowercase();
    let mut matches = Vec::new();
    if parsed.invalid || (query.is_empty() && !parsed.criteria.is_active()) { return matches; }
    for entry in entries.iter().filter(|entry| !entry.is_directory) {
        if cancel.load(Ordering::Relaxed) { break; }
        let path = directory.join(entry.source_path.as_deref().unwrap_or(&entry.name));
        if !crate::archive::is_supported_archive(&path) { continue; }
        let report = crate::inspection::inspect(&path, None, crate::inspection::InspectionMode::Preview, cancel);
        if report.error.is_some() { continue; }
        for info in report.entries {
            if cancel.load(Ordering::Relaxed) { return matches; }
            if info.directory || info.link || !info.name.to_lowercase().contains(&query) { continue; }
            let modified = info.modified.as_deref().and_then(crate::browser::parse_modification_date);
            let candidate = crate::archive::ArchiveEntry {
                name: info.name.clone(), source_path: None, size: info.size,
                is_directory: false, modified,
            };
            if !parsed.criteria.matches(&candidate, Some(info.size), modified) { continue; }
            let key = serde_json::to_string(&(path.as_path(), &info.name)).expect("archive search key is serializable");
            let name = Path::new(&info.name).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| info.name.clone());
            matches.push(ArchiveSearchMatch {
                requires_password: info.encrypted,
                entry: crate::archive::ArchiveEntry {
                    name: format!("{}/{}", entry.name, info.name),
                    source_path: Some(key.clone()), size: info.size, is_directory: false,
                    modified: info.modified.as_deref().and_then(crate::browser::parse_modification_date),
                },
                target: crate::file_actions::EntryTarget {
                    name, key, path: path.clone(), archive_entry: Some(info.name), password: None, is_directory: false,
                },
            });
            if matches.len() >= 5000 { return matches; }
        }
    }
    matches
}

#[cfg(test)]
mod archive_search_tests {
    use super::*;

    #[gpui::test]
    fn search_filter_badges_can_be_edited_removed_and_fit_the_field(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| manager.set_search_query(
                "alpha ext:zip min:1MiB".into(), window, cx,
            ));
        });
        cx.run_until_parked();
        let search = cx.debug_bounds("toolbar-search").unwrap();
        assert_eq!(search.size.height, px(34.0));
        assert!(cx.debug_bounds("search-filter-badge:ext:zip").is_some());
        assert!(cx.debug_bounds("search-filter-badge:min:1MiB").is_some());
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.toolbar_search.input.as_ref().unwrap().read(cx).value().as_str(), "alpha");
        });
        let badge = cx.debug_bounds("search-filter-badge:ext:zip").unwrap();
        cx.simulate_click(gpui::point(badge.left() + px(6.0), badge.center().y), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("search-filter-badge:ext:zip").is_none());
        cx.read(|cx| assert!(view.read(cx).toolbar_search.draft.contains("ext:zip")));
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-a");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_input("annual report ext:txt ");
        cx.run_until_parked();
        assert!(cx.debug_bounds("search-filter-badge:ext:txt").is_some());
        cx.read(|cx| assert_eq!(view.read(cx).toolbar_search.draft, "annual report "));
        cx.simulate_input("2026");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(view.read(cx).toolbar_search.text_query(), "annual report 2026"));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.simulate_resize(gpui::size(px(560.0), px(440.0)));
        cx.run_until_parked();
        let search = cx.debug_bounds("toolbar-search").unwrap();
        let badges = cx.debug_bounds("search-filter-badges").unwrap();
        let clear = cx.debug_bounds("clear-search").unwrap();
        assert!(badges.left() >= search.left());
        assert!(badges.right() < clear.left());
        assert!(clear.right() <= search.right());
        cx.update(|window, cx| window.click("remove-search-filter:0", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("search-filter-badge:min:1MiB").is_none());
        assert!(cx.debug_bounds("search-filter-badge:ext:txt").is_some());
        cx.update(|window, cx| window.click("remove-search-filter:0", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("search-filter-badges").is_none());
        cx.read(|cx| {
            assert_eq!(view.read(cx).toolbar_search.text_query(), "annual report 2026");
            assert!(!view.read(cx).filters.criteria.is_active());
        });
    }

    #[gpui::test]
    fn clear_search_button_resets_text_results_and_pending_scan(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let source = sources.path().join("needle.txt");
        std::fs::write(&source, b"clear search button test").unwrap();
        crate::archive::create_zip(&directory.path().join("archive.zip"), &[source]).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = crate::browser::read_directory(directory.path()).unwrap();
            manager
        });
        let search = cx.debug_bounds("toolbar-search").unwrap();
        let clear = cx.debug_bounds("clear-search").unwrap();
        assert!(clear.origin.x > search.origin.x + search.size.width / 2.0);
        assert!(clear.origin.x + clear.size.width <= search.origin.x + search.size.width);
        cx.update(|window, cx| window.click("toolbar-search-input", cx));
        cx.simulate_keystrokes("needle");
        cx.run_until_parked();
        let cancel = cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.toolbar_search.is_searching());
            manager.toolbar_search.cancel.clone()
        });
        cx.update(|window, cx| window.click("clear-search", cx));
        cx.run_until_parked();
        assert!(cancel.load(Ordering::Relaxed));
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.toolbar_search.query.is_empty());
            assert!(manager.toolbar_search.matches.is_empty());
            assert!(!manager.toolbar_search.is_searching());
            assert!(manager.toolbar_search.input.as_ref().unwrap().read(cx).value().is_empty());
        });
        cx.simulate_keystrokes("needle");
        cx.run_until_parked();
        cx.executor().advance_clock(std::time::Duration::from_millis(200));
        cx.run_until_parked();
        view.update(cx, |manager, _| {
            assert_eq!(manager.toolbar_search.query, "needle");
            assert_eq!(manager.toolbar_search.matches.len(), 1);
            manager.selected_entry = manager.toolbar_search.matches.keys().next().cloned();
        });
        cx.update(|window, cx| window.click("clear-search", cx));
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.toolbar_search.query.is_empty());
            assert!(manager.toolbar_search.matches.is_empty());
            assert!(manager.selected_entry.is_none());
            assert!(!manager.toolbar_search.is_searching());
            assert!(manager.toolbar_search.input.as_ref().unwrap().read(cx).value().is_empty());
        });
    }

    #[gpui::test]
    fn changing_location_clears_search_input_results_and_pending_scan(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        let source = root.path().join("nested");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("needle.txt"), b"navigation search test").unwrap();
        let path = first.join("archive.zip");
        crate::archive::create_zip(&path, &[source]).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(first.clone());
            manager.directory_history = vec![first.clone()];
            manager.navigation_history_index = 0;
            manager.entries = crate::browser::read_directory(&first).unwrap();
            manager
        });
        let assert_cleared = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| window.render_frame(cx));
            cx.run_until_parked();
            cx.read(|cx| {
                let manager = view.read(cx);
                assert!(manager.toolbar_search.query.is_empty());
                assert!(manager.toolbar_search.matches.is_empty());
                assert!(!manager.toolbar_search.is_searching());
                assert!(manager.toolbar_search.input.as_ref().unwrap().read(cx).value().is_empty());
            });
        };
        cx.update(|window, cx| window.click("toolbar-search-input", cx));
        cx.simulate_keystrokes("needle");
        cx.run_until_parked();
        let cancel = cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.toolbar_search.is_searching());
            manager.toolbar_search.cancel.clone()
        });
        view.update(cx, |manager, cx| manager.navigate_to(second.clone(), cx));
        cx.run_until_parked();
        assert_cleared(cx);
        assert!(cancel.load(Ordering::Relaxed));
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert_cleared(cx);

        cx.simulate_keystrokes("needle");
        view.update(cx, |manager, cx| manager.navigate_back(cx));
        cx.run_until_parked();
        assert_cleared(cx);
        cx.simulate_keystrokes("needle");
        view.update(cx, |manager, cx| manager.navigate_forward(cx));
        cx.run_until_parked();
        assert_cleared(cx);

        cx.simulate_keystrokes("needle");
        view.update(cx, |manager, cx| manager.install_archive(path.clone(), crate::archive::list_archive(&path).unwrap(), None, cx));
        cx.run_until_parked();
        assert_cleared(cx);
        cx.simulate_keystrokes("needle");
        view.update(cx, |manager, cx| manager.open_entry(&crate::file_actions::EntryTarget {
            name: "nested".into(), key: "nested".into(), path: path.clone(),
            archive_entry: Some("nested".into()), password: None, is_directory: true,
        }, cx));
        cx.run_until_parked();
        assert_cleared(cx);
    }

    #[test]
    fn directory_search_includes_files_inside_archives() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("needle.txt");
        std::fs::write(&source, b"archive search result").unwrap();
        for name in ["first.zip", "second.zip"] {
            crate::archive::create_zip(&directory.path().join(name), std::slice::from_ref(&source)).unwrap();
        }
        std::fs::write(directory.path().join("broken.zip"), b"not an archive").unwrap();
        let entries = crate::browser::read_directory(directory.path()).unwrap();
        let results = search_directory_archives(directory.path(), &entries, "NEEDLE", &AtomicBool::new(false));
        assert_eq!(results.len(), 2);
        assert_ne!(results[0].target.key, results[1].target.key);
        assert!(results.iter().all(|result| result.target.archive_entry.as_deref() == Some("needle.txt")));
        assert!(results.iter().all(|result| result.entry.name.ends_with(".zip/needle.txt")));
        assert!(search_directory_archives(directory.path(), &entries, "missing", &AtomicBool::new(false)).is_empty());
        assert!(search_directory_archives(directory.path(), &entries, "needle", &AtomicBool::new(true)).is_empty());
    }

    #[test]
    fn directory_search_supports_all_formats_and_identifies_protected_entries() {
        let directory = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("needle.txt");
        std::fs::write(&source, b"search all supported formats").unwrap();
        for (index, format) in crate::archive::ResolvedFormat::ALL.into_iter().enumerate() {
            let path = directory.path().join(format!("needle-{index}.{}", format.extension()));
            crate::archive::create_archive(&path, std::slice::from_ref(&source), crate::archive::CreateOptions {
                format, compression: format.compression_levels()[0], ..crate::archive::CreateOptions::default()
            }).unwrap();
        }
        let entries = crate::browser::read_directory(directory.path()).unwrap();
        let results = search_directory_archives(directory.path(), &entries, "needle", &AtomicBool::new(false));
        assert_eq!(results.len(), crate::archive::ResolvedFormat::ALL.len());
        let protected = tempfile::tempdir().unwrap();
        crate::archive::create_archive_with_password(&protected.path().join("protected.zip"), &[source], crate::archive::CreateOptions::default(), Some("search-test-password")).unwrap();
        let entries = crate::browser::read_directory(protected.path()).unwrap();
        let results = search_directory_archives(protected.path(), &entries, "needle", &AtomicBool::new(false));
        assert_eq!(results.len(), 1);
        assert!(results[0].requires_password);
    }
}

#[derive(Default)]
/// Retains search text and the GPUI input state used by the toolbar.
pub(crate) struct SearchField {
    /// Current search query entered by the user.
    pub(crate) query: String,
    pub(crate) draft: String,
    badges: Vec<String>,
    input: Option<Entity<InputState>>,
    _subscription: Option<Subscription>,
    language: Option<Language>,
    location: Option<(Option<PathBuf>, Option<PathBuf>, String)>,
    pub(crate) matches: std::collections::BTreeMap<String, ArchiveSearchMatch>,
    directory: Option<PathBuf>,
    searched_query: String,
    sources: Vec<(String, u64)>,
    cancel: Arc<AtomicBool>,
    revision: u64,
    task: Option<u64>,
}

impl SearchField {
    pub(crate) fn is_searching(&self) -> bool { self.task.is_some() }

    pub(crate) fn text_query(&self) -> String {
        crate::filters::parse_search(&self.query).text.trim().to_lowercase()
    }

    pub(crate) fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.input.as_ref().expect("search input is initialized").read(cx).focus_handle(cx)
    }
}

impl Drop for SearchField {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl ArchiveManager {
    pub(crate) fn set_search_query(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.set_search_query_mode(query, true, window, cx);
    }

    fn set_search_query_mode(&mut self, query: String, commit_last: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (badges, draft) = crate::filters::search_badges(&query, commit_last);
        self.toolbar_search.query = query.clone();
        self.toolbar_search.badges = badges;
        self.toolbar_search.draft = draft.clone();
        if let Some(input) = self.toolbar_search.input.clone() {
            if input.read(cx).value().as_str() != draft {
                input.update(cx, |state, cx| state.set_value(draft, window, cx));
            }
        }
        self.selected_entry = None;
        self.file_scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        refresh_archive_search(self, window, cx);
        cx.notify();
    }

    fn remove_search_badge(&mut self, index: usize, edit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut badges = self.toolbar_search.badges.clone();
        let token = badges.remove(index);
        let mut draft = self.toolbar_search.draft.clone();
        if edit {
            if !draft.is_empty() && !draft.ends_with(char::is_whitespace) { draft.push(' '); }
            draft.push_str(&token);
        }
        let query = crate::filters::compose_search(&badges, &draft);
        self.set_search_query_mode(query, false, window, cx);
        window.focus(&self.toolbar_search.focus_handle(cx), cx);
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toolbar_search.query.clear();
        self.toolbar_search.draft.clear();
        self.toolbar_search.badges.clear();
        self.filters.visible = false;
        if let Some(input) = self.toolbar_search.input.clone() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
            window.focus(&input.read(cx).focus_handle(cx), cx);
        }
        refresh_archive_search(self, window, cx);
        cx.notify();
    }

    pub(crate) fn selected_search_target(&self) -> Option<&crate::file_actions::EntryTarget> {
        self.toolbar_search.matches.get(self.selected_entry.as_deref()?).map(|result| &result.target)
    }

    pub(crate) fn search_target_requires_password(&self, target: &crate::file_actions::EntryTarget) -> bool {
        self.toolbar_search.matches.get(&target.key).is_some_and(|result| result.requires_password)
    }
}

fn refresh_archive_search(manager: &mut ArchiveManager, _window: &Window, cx: &mut Context<ArchiveManager>) {
    let query = manager.toolbar_search.query.clone();
    let parsed = crate::filters::parse_search(&query);
    let eligible = !parsed.invalid && (!parsed.text.trim().is_empty() || parsed.criteria.is_active());
    let directory = manager.current_directory.clone();
    let sources = directory.as_ref().filter(|_| eligible).map(|directory| {
        manager.entries.iter().filter(|entry| !entry.is_directory)
            .filter(|entry| crate::archive::is_supported_archive(&directory.join(entry.source_path.as_deref().unwrap_or(&entry.name))))
            .map(|entry| (entry.source_path.clone().unwrap_or_else(|| entry.name.clone()), entry.size)).collect::<Vec<_>>()
    }).unwrap_or_default();
    if manager.toolbar_search.directory == directory && manager.toolbar_search.searched_query == query && manager.toolbar_search.sources == sources { return; }
    if manager.selected_search_target().is_some() { manager.selected_entry = None; }
    manager.toolbar_search.cancel.store(true, Ordering::Relaxed);
    manager.toolbar_search.cancel = Arc::new(AtomicBool::new(false));
    manager.toolbar_search.task = None;
    manager.toolbar_search.matches.clear();
    manager.toolbar_search.directory = directory.clone();
    manager.toolbar_search.searched_query = query.clone();
    manager.toolbar_search.sources = sources;
    manager.toolbar_search.revision = manager.toolbar_search.revision.wrapping_add(1);
    if manager.status_message.as_deref() == Some(manager.language.text("Search results limited. Refine your search.")) { manager.status_message = None; }
    let Some(directory) = directory.filter(|_| eligible) else { return; };
    if manager.toolbar_search.sources.is_empty() { return; }
    let entries = manager.entries.clone();
    let cancel = manager.toolbar_search.cancel.clone();
    let revision = manager.toolbar_search.revision;
    let executor = cx.background_executor().clone();
    let scan_directory = directory.clone();
    let scan_query = query.clone();
    manager.toolbar_search.task = Some(manager.background_queue.enqueue(
        "Searching archives...",
        crate::task_queue::TaskControl::with_cancel(cancel.clone()).for_paths(entries.iter().filter(|entry| !entry.is_directory).map(|entry| directory.join(entry.source_path.as_deref().unwrap_or(&entry.name)))),
        move |control, reporter| async move {
            executor.timer(std::time::Duration::from_millis(150)).await;
            let entries: Vec<_> = entries.into_iter().filter(|entry| !entry.is_directory && crate::archive::is_supported_archive(&scan_directory.join(entry.source_path.as_deref().unwrap_or(&entry.name)))).collect();
            let total = entries.len() as u64;
            reporter.set(0, total);
            let mut results = Vec::new();
            for entry in entries {
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                reporter.phase(entry.name.clone());
                results.extend(search_directory_archives(&scan_directory, std::slice::from_ref(&entry), &scan_query, &cancel));
                reporter.advance(1);
                if results.len() >= 5000 { results.truncate(5000); break; }
            }
            if !control.checkpoint() { anyhow::bail!("cancelled"); }
            Ok(results)
        },
        move |manager, result, _, cx| {
                    if manager.toolbar_search.revision != revision
                        || manager.current_directory.as_ref() != Some(&directory)
                        || manager.toolbar_search.query != query { return; }
                    manager.toolbar_search.task = None;
                    let Ok(results) = result else { cx.notify(); return; };
                    if results.len() >= 5000 {
                        manager.status_message = Some(manager.language.text("Search results limited. Refine your search.").to_owned());
                    }
                    manager.toolbar_search.matches = results.into_iter().map(|result| (result.target.key.clone(), result)).collect();
                    cx.notify();
        },
        cx,
    ));
}

/// Creates or synchronizes the toolbar search input and its change subscription.
pub(crate) fn prepare_search(
    manager: &mut ArchiveManager,
    window: &mut Window,
    cx: &mut Context<ArchiveManager>,
) {
    let location = (
        manager.current_directory.clone(),
        manager.archive_path.clone(),
        manager.selected_folder.clone(),
    );
    if manager.toolbar_search.location.as_ref().is_some_and(|previous| previous != &location) {
        manager.toolbar_search.query.clear();
        manager.toolbar_search.draft.clear();
        manager.toolbar_search.badges.clear();
        manager.filters.visible = false;
        if let Some(input) = manager.toolbar_search.input.clone() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
    }
    manager.toolbar_search.location = Some(location);
    refresh_archive_search(manager, window, cx);
    let language = manager.language;
    if let Some(input) = manager.toolbar_search.input.clone() {
        if manager.toolbar_search.language != Some(language) {
            input.update(cx, |state, cx| {
                state.set_placeholder(language.text("Search files"), window, cx);
            });
            manager.toolbar_search.language = Some(language);
        }
        return;
    }

    let input = cx.new(|cx| InputState::new(window, cx).placeholder(language.text("Search files")));
    let subscription = cx.subscribe_in(&input, window, |manager, state, event, window, cx| {
        if matches!(event, InputEvent::Change) {
            let draft = state.read(cx).value().to_string();
            if manager.toolbar_search.draft == draft { return; }
            let query = crate::filters::compose_search(&manager.toolbar_search.badges, &draft);
            if manager.toolbar_search.query == query { return; }
            if draft.ends_with(char::is_whitespace)
                && crate::filters::search_badges(&query, false).0 != manager.toolbar_search.badges
            {
                manager.set_search_query_mode(query, false, window, cx);
                manager.filters.visible = true;
                return;
            }
            manager.toolbar_search.query = query;
            manager.toolbar_search.draft = draft;
            manager.filters.visible = true;
            manager.selected_entry = None;
            manager.file_scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
            refresh_archive_search(manager, window, cx);
            cx.notify();
        }
    });
    manager.toolbar_search.input = Some(input);
    manager.toolbar_search._subscription = Some(subscription);
    manager.toolbar_search.language = Some(language);
}

/// Actions exposed by toolbar buttons and their platform-specific shortcuts.
pub(crate) mod key_actions {
    gpui::actions!(
        zarchive_toolbar,
        [
            OpenFile,
            CreateArchive,
            ExtractHere,
            ExtractTo,
            TestIntegrity,
            AuditArchive,
        ]
    );
}

/// Returns keyboard bindings using the platform's standard modifier key.
pub(crate) fn key_bindings() -> [gpui::KeyBinding; 6] {
    #[cfg(target_os = "macos")]
    {
        [
            gpui::KeyBinding::new("cmd-o", key_actions::OpenFile, None),
            gpui::KeyBinding::new("cmd-n", key_actions::CreateArchive, None),
            gpui::KeyBinding::new("cmd-e", key_actions::ExtractHere, None),
            gpui::KeyBinding::new("cmd-shift-e", key_actions::ExtractTo, None),
            gpui::KeyBinding::new("cmd-i", key_actions::TestIntegrity, None),
            gpui::KeyBinding::new("cmd-shift-i", key_actions::AuditArchive, None),
        ]
    }
    #[cfg(not(target_os = "macos"))]
    {
        [
            gpui::KeyBinding::new("ctrl-o", key_actions::OpenFile, None),
            gpui::KeyBinding::new("ctrl-n", key_actions::CreateArchive, None),
            gpui::KeyBinding::new("ctrl-e", key_actions::ExtractHere, None),
            gpui::KeyBinding::new("ctrl-shift-e", key_actions::ExtractTo, None),
            gpui::KeyBinding::new("ctrl-i", key_actions::TestIntegrity, None),
            gpui::KeyBinding::new("ctrl-shift-i", key_actions::AuditArchive, None),
        ]
    }
}

/// Builds a labeled toolbar button with an application or built-in icon.
pub(crate) fn toolbar_button(
    id: &'static str,
    icon: &'static str,
    label: &'static str,
    theme: Theme,
    cx: &App,
) -> Button {
    use gpui::component::IconName;
    let icon_size = if matches!(icon, "document" | "documents" | "file") {
        12.0
    } else {
        12.0
    };
    let icon: gpui::component::Icon = match icon {
        "archive" => gpui::component::Icon::default().path("icons/archive.svg"),
        "folder" => IconName::Folder.into(),
        "toolbox" => IconName::Settings.into(),
        "info" => IconName::Info.into(),
        "extract" => gpui::assets::IconName::Download.into(),
        "extract-to" => gpui::assets::IconName::FolderOutput.into(),
        "inspect" => gpui::assets::IconName::FileSearch.into(),
        "integrity" => gpui::assets::IconName::ShieldCheck.into(),
        "audit" => gpui::assets::IconName::ScanSearch.into(),
        _ => IconName::File.into(),
    };
    let style = gpui::component::button::ButtonCustomVariant::new(cx)
        .foreground(rgb(theme.text).into())
        .hover(rgb(theme.hover).into())
        .active(rgb(theme.selection).into());
    Button::new(id)
        .custom(style)
        .small()
        .accessibility_label(label)
        .tooltip(label)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .child(
                    div()
                        .id(gpui::SharedString::from(format!("toolbar-icon:{id}")))
                        .debug_selector(move || format!("toolbar-icon:{id}"))
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(icon_size))
                        .h(px(icon_size))
                        .flex_shrink_0()
                        .child(icon.with_size(gpui::component::Size::Size(px(icon_size)))),
                )
                .child(label),
        )
}

/// Builds a toolbar button that opens a dropdown menu.
fn dropdown_button(
    id: &'static str,
    icon: gpui::component::Icon,
    label: &'static str,
    theme: Theme,
    cx: &App,
) -> Button {
    let style = gpui::component::button::ButtonCustomVariant::new(cx)
        .foreground(rgb(theme.text).into())
        .hover(rgb(theme.hover).into())
        .active(rgb(theme.selection).into());
    Button::new(id)
        .custom(style)
        .small()
        .accessibility_label(label)
        .tooltip(label)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .id(gpui::SharedString::from(format!("toolbar-icon:{id}")))
                        .debug_selector(move || format!("toolbar-icon:{id}"))
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(20.0))
                        .h(px(20.0))
                        .flex_shrink_0()
                        .child(
                            icon.with_size(gpui::component::Size::Size(px(20.0)))
                                .text_color(rgb(theme.soft_accent())),
                        ),
                )
                .child(app_icon("chevron-down", 9.0, theme.muted_text)),
        )
}

/// Builds a compact icon-only toolbar button with an accessible label.
fn toolbar_icon_button(
    id: &'static str,
    icon: gpui::component::Icon,
    label: &'static str,
    icon_size: f32,
    icon_color: u32,
    theme: Theme,
    cx: &App,
) -> Button {
    let style = gpui::component::button::ButtonCustomVariant::new(cx)
        .foreground(rgb(theme.text).into())
        .hover(rgb(theme.hover).into())
        .active(rgb(theme.selection).into());
    Button::new(id)
        .custom(style)
        .small()
        .accessibility_label(label)
        .tooltip(label)
        .child(
            div()
                .id(gpui::SharedString::from(format!("toolbar-icon:{id}")))
                .debug_selector(move || format!("toolbar-icon:{id}"))
                .flex()
                .items_center()
                .justify_center()
                .w(px(icon_size + 2.0))
                .h(px(icon_size + 2.0))
                .flex_shrink_0()
                .child(
                    icon.with_size(gpui::component::Size::Size(px(icon_size)))
                        .text_color(rgb(icon_color)),
                ),
        )
}

/// Renders the retained search input with the current query and theme.
fn search_input(manager: &ArchiveManager, theme: Theme, cx: &mut Context<ArchiveManager>) -> impl IntoElement {
    let input = manager
        .toolbar_search
        .input
        .as_ref()
        .expect("toolbar search input is initialized");
    div()
        .id("toolbar-search")
        .debug_selector(|| "toolbar-search".to_owned())
        .on_key_down(cx.listener(crate::filters::search_key_down))
        .flex()
        .flex_1()
        .min_w(px(160.0))
        .max_w(px(480.0))
        .mx_3()
        .h(px(34.0))
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .when(manager.filters.query_invalid, |search| search.border_color(rgb(0xef4444)))
        .bg(rgb(theme.sidebar))
        .when(!manager.toolbar_search.badges.is_empty(), |search| {
            search.child(
                div().id("search-filter-badges")
                    .debug_selector(|| "search-filter-badges".to_owned())
                    .flex().items_center().gap_1().min_w(px(0.0)).max_w(px(240.0))
                    .h(px(26.0)).ml_1().overflow_x_scroll()
                    .children(manager.toolbar_search.badges.iter().enumerate().map(|(index, token)| {
                        div().id(("search-filter-badge", index))
                            .debug_selector({ let token = token.clone(); move || format!("search-filter-badge:{token}") })
                            .flex().items_center().gap_1().flex_shrink_0().h(px(24.0)).pl_2().pr_1()
                            .rounded_sm().border_1()
                            .border_color(gpui::rgba((theme.accent << 8) | 0x60))
                            .bg(gpui::rgba((theme.accent << 8) | 0x24))
                            .text_color(rgb(theme.soft_accent())).text_xs().cursor_pointer()
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |manager, _, window, cx| manager.remove_search_badge(index, true, window, cx)))
                            .child(div().max_w(px(150.0)).truncate().child(token.clone()))
                            .child(Button::new(gpui::SharedString::from(format!("remove-search-filter:{index}")))
                                .ghost().xsmall().w(px(18.0)).h(px(18.0))
                                .icon(gpui::assets::IconName::X)
                                .text_color(rgb(theme.soft_accent()))
                                .tooltip(manager.language.text("Remove filter"))
                                .accessibility_label(manager.language.text("Remove filter"))
                                .on_click(cx.listener(move |manager, _, window, cx| {
                                    cx.stop_propagation();
                                    manager.remove_search_badge(index, false, window, cx);
                                })))
                    })),
            )
        })
        .child(
            div().flex().flex_1().min_w(px(60.0))
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(|manager, _, _, cx| {
                    manager.filters.visible = true;
                    cx.notify();
                }))
                .child(Input::new(input)
                .id("toolbar-search-input")
                .small()
                .flex_1()
                .min_w(px(0.0))
                .appearance(false)
                .aria_label(manager.language.text("Search files"))
                .prefix(app_icon("search", 13.0, theme.muted_text))),
        )
        .child(crate::filters::toggle(manager, cx))
        .child(
                Button::new("clear-search")
                .debug_selector(|| "clear-search".to_owned())
                .ghost()
                .xsmall()
                .w(px(24.0))
                .h(px(24.0))
                .mr(px(2.0))
                .flex_shrink_0()
                .icon(gpui::assets::IconName::X)
                .text_color(rgb(theme.muted_text))
                .accessibility_label(manager.language.text("Clear search"))
                .tooltip(manager.language.text("Clear search"))
                .disabled(manager.toolbar_search.query.is_empty())
                .on_click(cx.listener(|manager, _, window, cx| manager.clear_search(window, cx))),
            )
}

/// Adds spacing after a menu icon for consistent menu-item alignment.
fn spaced_icon(icon: impl Into<gpui::component::Icon>) -> gpui::component::Icon {
    gpui::component::Icon::new(icon).mr(px(4.0))
}

/// Builds the titlebar's navigation, file, extraction, inspection, and utility controls.
pub(crate) fn toolbox_bar(
    manager: &ArchiveManager,
    language: Language,
    theme: Theme,
    can_navigate_back: bool,
    can_navigate_forward: bool,
    can_extract: bool,
    can_inspect: bool,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let files_view = cx.entity().downgrade();
    let extract_view = cx.entity().downgrade();
    let audit_view = cx.entity().downgrade();

    let left = div()
        .flex()
        .items_center()
        .gap_1()
        .child(
            toolbar_icon_button(
                "navigate-back",
                gpui::component::Icon::default().path("icons/chevron-left.svg"),
                language.text("Back"),
                9.0,
                if can_navigate_back {
                    theme.soft_accent()
                } else {
                    theme.faint_text
                },
                theme,
                cx,
            )
            .disabled(!can_navigate_back)
            .on_click(cx.listener(|this, _, _, cx| this.navigate_back(cx))),
        )
        .child(
            toolbar_icon_button(
                "navigate-forward",
                gpui::component::Icon::default().path("icons/chevron-right.svg"),
                language.text("Forward"),
                9.0,
                if can_navigate_forward {
                    theme.soft_accent()
                } else {
                    theme.faint_text
                },
                theme,
                cx,
            )
            .disabled(!can_navigate_forward)
            .on_click(cx.listener(|this, _, _, cx| this.navigate_forward(cx))),
        )
        .child(
            dropdown_button(
                "toolbar-files-dropdown",
                gpui::component::Icon::default().path("icons/folder-open.svg"),
                language.text("Files"),
                theme,
                cx,
            )
            .dropdown_menu(move |menu, _, _| {
                let open_view = files_view.clone();
                let create_view = files_view.clone();
                menu.min_w(px(220.0))
                    .item(
                        PopupMenuItem::new(language.text("Open archive..."))
                            .icon(spaced_icon(
                                gpui::component::Icon::default().path("icons/folder-open-menu.svg"),
                            ))
                            .action(Box::new(key_actions::OpenFile))
                            .on_click(move |_, _, cx| {
                                let _ = open_view.update(cx, |this, cx| this.open_file(cx));
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(language.text("New archive..."))
                            .icon(spaced_icon(
                                gpui::component::Icon::default().path("icons/plus.svg"),
                            ))
                            .action(Box::new(key_actions::CreateArchive))
                            .on_click(move |_, window, cx| {
                                let _ = create_view.update(cx, |this, cx| {
                                    this.show_modal(Modal::CreateArchive, window, cx)
                                });
                            }),
                    )
            }),
        )
        .child(
            dropdown_button(
                "toolbar-extract-dropdown",
                gpui::component::Icon::default().path("icons/box-open.svg"),
                language.text("Extraction"),
                theme,
                cx,
            )
            .dropdown_menu(move |menu, _, _| {
                let here_view = extract_view.clone();
                let to_view = extract_view.clone();
                menu.min_w(px(220.0))
                    .item(
                        PopupMenuItem::new(language.text("Extract here"))
                            .icon(spaced_icon(gpui::assets::IconName::Download))
                            .action(Box::new(key_actions::ExtractHere))
                            .disabled(!can_extract)
                            .on_click(move |_, window, cx| {
                                let _ = here_view.update(cx, |this, cx| {
                                    this.extract_current_archive(true, window, cx)
                                });
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(language.text("Extract to..."))
                            .icon(spaced_icon(gpui::assets::IconName::FolderOutput))
                            .action(Box::new(key_actions::ExtractTo))
                            .disabled(!can_extract)
                            .on_click(move |_, window, cx| {
                                let _ = to_view.update(cx, |this, cx| {
                                    this.extract_current_archive(false, window, cx)
                                });
                            }),
                    )
            }),
        )
        .child(
            dropdown_button(
                "toolbar-inspect-dropdown",
                gpui::component::Icon::default().path("icons/shield-halved.svg"),
                language.text("Inspect"),
                theme,
                cx,
            )
            .dropdown_menu(move |menu, _, _| {
                let view = audit_view.clone();
                let audit_view = audit_view.clone();
                let full_audit_view = audit_view.clone();
                let compare_view = audit_view.clone();
                menu.min_w(px(240.0))
                    .item(
                        PopupMenuItem::new(language.text("Test integrity"))
                            .icon(spaced_icon(gpui::assets::IconName::ShieldCheck))
                            .action(Box::new(key_actions::TestIntegrity))
                            .disabled(!can_inspect)
                            .on_click(move |_, window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.inspect_current(
                                        crate::inspection::InspectionMode::Integrity,
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(language.text("Audit archive"))
                            .icon(spaced_icon(gpui::assets::IconName::ScanSearch))
                            .action(Box::new(key_actions::AuditArchive))
                            .disabled(!can_inspect)
                            .on_click(move |_, window, cx| {
                                let _ = audit_view.update(cx, |this, cx| {
                                    this.inspect_current(
                                        crate::inspection::InspectionMode::Audit,
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(language.text("Full audit"))
                            .icon(spaced_icon(gpui::assets::IconName::ScanSearch))
                            .disabled(!can_inspect)
                            .on_click(move |_, window, cx| {
                                let _ = full_audit_view.update(cx, |this, cx| {
                                    this.inspect_current(
                                        crate::inspection::InspectionMode::FullAudit,
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(language.text("Compare archives"))
                            .icon(spaced_icon(gpui::assets::IconName::GitCompare))
                            .on_click(move |_, window, cx| {
                                let _ = compare_view.update(cx, |this, cx| {
                                    this.choose_archives_for_comparison(window, cx);
                                });
                            }),
                    )
            }),
        );

    let right = div()
        .flex()
        .items_center()
        .gap_1()
        .child(
            toolbar_icon_button(
                "settings",
                gpui::component::IconName::Settings.into(),
                language.text("Settings"),
                16.0,
                theme.soft_accent(),
                theme,
                cx,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.show_modal(Modal::Settings, window, cx);
            })),
        )
        .child(
            toolbar_icon_button(
                "about",
                gpui::component::IconName::Info.into(),
                language.text("About"),
                16.0,
                theme.soft_accent(),
                theme,
                cx,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.show_modal(Modal::About, window, cx);
            })),
        );

    div()
        .id("toolbox-bar")
        .debug_selector(|| "toolbox-bar".to_owned())
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .h(px(44.0))
        .bg(rgb(theme.titlebar))
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
        .px_2()
        .child(left)
        .child(search_input(manager, theme, cx))
        .child(right)
}
