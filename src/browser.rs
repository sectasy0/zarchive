//! Builds archive folder navigation and prepares entries for display and sorting.

use crate::archive::ArchiveEntry;
use anyhow::Context as _;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(crate) fn date_from_filetime(ticks: u64) -> Option<chrono::NaiveDate> {
    let seconds = i64::try_from(ticks / 10_000_000).ok()?.checked_sub(11_644_473_600)?;
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|time| time.with_timezone(&chrono::Local).date_naive())
}

pub(crate) fn parse_modification_date(value: &str) -> Option<chrono::NaiveDate> {
    if let Some(seconds) = value.strip_prefix("Unix ").and_then(|value| value.parse::<i64>().ok()) {
        return chrono::DateTime::from_timestamp(seconds, 0)
            .map(|time| time.with_timezone(&chrono::Local).date_naive());
    }
    chrono::NaiveDate::parse_from_str(value.split(['T', ' ']).next()?, "%Y-%m-%d")
        .ok()
        .or_else(|| value.parse().ok().and_then(date_from_filetime))
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum EntryKind {
    #[default]
    All,
    Files,
    Folders,
}

#[derive(Default)]
pub(crate) struct EntryFilters {
    pub extensions: String,
    pub minimum_size: Option<u64>,
    pub maximum_size: Option<u64>,
    pub modified_after: Option<chrono::NaiveDate>,
    pub modified_before: Option<chrono::NaiveDate>,
    pub kind: EntryKind,
}

impl EntryFilters {
    pub(crate) fn is_active(&self) -> bool {
        !self.extensions.trim().is_empty()
            || self.minimum_size.is_some()
            || self.maximum_size.is_some()
            || self.modified_after.is_some()
            || self.modified_before.is_some()
            || self.kind != EntryKind::All
    }

    pub(crate) fn matches(
        &self,
        entry: &ArchiveEntry,
        size: Option<u64>,
        modified: Option<chrono::NaiveDate>,
    ) -> bool {
        if (self.kind == EntryKind::Files && entry.is_directory)
            || (self.kind == EntryKind::Folders && !entry.is_directory)
        {
            return false;
        }
        if !self.extensions.trim().is_empty() {
            let name = entry.name.to_lowercase();
            if entry.is_directory
                || !self.extensions.split([',', ';', ' ']).any(|extension| {
                    let extension = extension.trim().trim_start_matches("*.").trim_start_matches('.');
                    !extension.is_empty() && name.ends_with(&format!(".{}", extension.to_lowercase()))
                })
            {
                return false;
            }
        }
        if self.minimum_size.is_some_and(|minimum| size.is_none_or(|size| size < minimum))
            || self.maximum_size.is_some_and(|maximum| size.is_none_or(|size| size > maximum))
            || self.modified_after.is_some_and(|after| modified.is_none_or(|date| date < after))
            || self.modified_before.is_some_and(|before| modified.is_none_or(|date| date > before))
        {
            return false;
        }
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Selects the field used to order visible archive entries.
pub(crate) enum SortColumn {
    /// Sorts by the case-insensitive entry name.
    Name,
    /// Sorts files by extension while keeping directories first.
    Type,
    /// Sorts by the reported or calculated entry size.
    Size,
    /// Sorts by modification date.
    Modified,
}

/// Builds a sorted list of unique archive folders with their display depths.
pub(crate) fn folder_tree(entries: &[ArchiveEntry]) -> Vec<(String, String, usize)> {
    let mut folders = BTreeSet::new();

    for entry in entries {
        let components = path_components(&entry.name);
        let folder_count = if entry.is_directory {
            components.len()
        } else {
            components.len().saturating_sub(1)
        };
        for index in 1..=folder_count {
            folders.insert(components[..index].join("/"));
        }
    }

    folders
        .into_iter()
        .map(|path| {
            let depth = path.matches('/').count();
            let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
            (path, name, depth)
        })
        .collect()
}

/// Projects an archive listing to the immediate children of one selected folder.
pub(crate) fn visible_entries(
    entries: &[ArchiveEntry],
    selected_folder: &str,
) -> Vec<ArchiveEntry> {
    let selected_components = path_components(selected_folder);
    let mut children = BTreeMap::<String, ArchiveEntry>::new();

    for entry in entries {
        let components = path_components(&entry.name);
        if components.len() <= selected_components.len()
            || components[..selected_components.len()] != selected_components
        {
            continue;
        }

        let child_name = &components[selected_components.len()];
        let child_path = if selected_folder.is_empty() {
            child_name.clone()
        } else {
            format!("{selected_folder}/{child_name}")
        };
        let is_direct_child = components.len() == selected_components.len() + 1;
        let is_directory = !is_direct_child || entry.is_directory;
        let child_entry = ArchiveEntry {
            name: child_name.clone(),
            source_path: if is_direct_child && !entry.is_directory {
                entry.source_path.clone()
            } else {
                None
            },
            size: if !entry.is_directory { entry.size } else { 0 },
            modified: is_direct_child.then_some(entry.modified).flatten(),
            is_directory,
            is_encrypted: is_direct_child && entry.is_encrypted,
        };

        children
            .entry(child_path)
            .and_modify(|existing| {
                if is_direct_child {
                    existing.modified = entry.modified;
                }
                if is_direct_child && !entry.is_directory {
                    existing.size = entry.size;
                    existing.is_directory = false;
                    existing.source_path = entry.source_path.clone();
                } else if !entry.is_directory {
                    existing.size = existing.size.saturating_add(entry.size);
                } else {
                    existing.is_directory = true;
                }
            })
            .or_insert(child_entry);
    }

    let mut children: Vec<_> = children.into_values().collect();
    children.sort_by(|left, right| {
        right
            .is_directory
            .cmp(&left.is_directory)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    children
}

pub(crate) fn search_archive_entries(
    entries: &[ArchiveEntry],
    selected_folder: &str,
    query: &str,
) -> Vec<ArchiveEntry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return visible_entries(entries, selected_folder);
    }
    let selected = path_components(selected_folder);
    entries.iter().filter_map(|entry| {
        let components = path_components(&entry.name);
        if components.len() <= selected.len() || components[..selected.len()] != selected {
            return None;
        }
        let name = components[selected.len()..].join("/");
        if !name.to_lowercase().contains(&query) {
            return None;
        }
        Some(ArchiveEntry {
            name,
            source_path: Some(entry.source_path.clone().unwrap_or_else(|| entry.name.clone())),
            size: entry.size,
            modified: entry.modified,
            is_directory: entry.is_directory,
            is_encrypted: entry.is_encrypted,
        })
    }).collect()
}

/// Sorts entries while keeping directories ahead of files in either direction.
pub(crate) fn sort_entries(entries: &mut [ArchiveEntry], column: SortColumn, ascending: bool) {
    entries.sort_by(|left, right| {
        let directory_order = right.is_directory.cmp(&left.is_directory);
        let order = if directory_order != std::cmp::Ordering::Equal {
            directory_order
        } else {
            let order = match column {
                SortColumn::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
                SortColumn::Type => sortable_type(left).cmp(&sortable_type(right)),
                SortColumn::Size => left.size.cmp(&right.size),
                SortColumn::Modified => left.modified.cmp(&right.modified),
            };
            if order == std::cmp::Ordering::Equal {
                left.name.to_lowercase().cmp(&right.name.to_lowercase())
            } else {
                order
            }
        };
        if ascending || directory_order != std::cmp::Ordering::Equal {
            order
        } else {
            order.reverse()
        }
    });
}

/// Returns a lower-case extension, or an empty string when the name has none.
fn entry_type(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// Provides a common sortable type label for files and directories.
fn sortable_type(entry: &ArchiveEntry) -> String {
    if entry.is_directory {
        "folder".to_owned()
    } else {
        entry_type(&entry.name)
    }
}

/// Splits an archive path into normalized, non-empty components.
fn path_components(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .map(str::to_owned)
        .collect()
}

/// Chooses a usable startup directory from the preferred, standard, or current location.
pub(crate) fn default_directory(preferred: Option<&Path>) -> anyhow::Result<PathBuf> {
    preferred
        .filter(|path| path.is_dir())
        .map(Path::to_path_buf)
        .or_else(|| dirs::document_dir().filter(|path| path.is_dir()))
        .or_else(|| dirs::home_dir().filter(|path| path.is_dir()))
        .or_else(|| std::env::current_dir().ok().filter(|path| path.is_dir()))
        .ok_or_else(|| anyhow::anyhow!("Could not locate a Documents, home, or current directory"))
}

/// Recursively totals regular-file sizes without following symbolic links.
#[cfg(test)]
pub(crate) fn directory_size(root: &Path) -> anyhow::Result<u64> {
    directory_size_with_cancel(root, &std::sync::atomic::AtomicBool::new(false))
}

pub(crate) fn directory_size_with_cancel(
    root: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<u64> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(std::io::Error::from(std::io::ErrorKind::Interrupted).into());
    }
    let root_meta = std::fs::symlink_metadata(root)
        .with_context(|| format!("Failed to inspect {}", root.display()))?;
    if !root_meta.is_dir() {
        return Ok(if root_meta.is_file() {
            root_meta.len()
        } else {
            0
        });
    }

    let mut pending = vec![root.to_path_buf()];
    let mut total = 0_u64;

    while let Some(dir) = pending.pop() {
        crate::task_queue::checkpoint()?;
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("Failed to read directory {}", dir.display()))?;
        for entry in entries {
            crate::task_queue::checkpoint()?;
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(std::io::Error::from(std::io::ErrorKind::Interrupted).into());
            }
            let entry =
                entry.with_context(|| format!("Failed to read entry in {}", dir.display()))?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                let size = entry.metadata()?.len();
                crate::task_queue::report_bytes(size);
                total = total
                    .checked_add(size)
                    .context("Folder size exceeds the supported range")?;
            }
        }
    }
    Ok(total)
}

/// Reads one filesystem directory into the common archive-entry representation.
pub(crate) fn read_directory(path: &Path) -> anyhow::Result<Vec<ArchiveEntry>> {
    let directory = std::fs::read_dir(path)
        .with_context(|| format!("Failed to read directory: {}", path.display()))?;
    let mut entries = Vec::new();

    for result in directory {
        let entry =
            result.with_context(|| format!("Failed to read an entry in {}", path.display()))?;
        let entry_path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("Failed to inspect {}", entry_path.display()))?;
        let is_directory = metadata.is_dir();
        entries.push(ArchiveEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            source_path: Some(entry_path.to_string_lossy().into_owned()),
            size: if is_directory { 0 } else { metadata.len() },
            modified: metadata.modified().ok()
                .map(|time| chrono::DateTime::<chrono::Local>::from(time).date_naive()),
            is_directory,
            is_encrypted: !is_directory && crate::archive::is_password_protected(&entry_path),
        });
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::{
        SortColumn, default_directory, directory_size, folder_tree, read_directory, sort_entries,
        visible_entries, search_archive_entries,
    };
    use crate::archive::ArchiveEntry;

    #[test]
    fn filters_combine_extensions_size_dates_and_kind() {
        use super::{EntryFilters, EntryKind};
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
        let mut filters = EntryFilters::default();
        let file = entry("BACKUP.TAR.GZ", 1024, false);
        assert!(!filters.is_active());
        assert!(filters.matches(&file, Some(1024), None));
        filters.extensions = ".zip, *.tar.gz; txt".into();
        filters.minimum_size = Some(1024);
        filters.maximum_size = Some(2048);
        filters.modified_after = Some(date);
        filters.modified_before = Some(date);
        filters.kind = EntryKind::Files;
        assert!(filters.is_active());
        assert!(filters.matches(&file, Some(1024), Some(date)));
        assert!(filters.matches(&file, Some(2048), Some(date)));
        assert!(!filters.matches(&file, Some(1023), Some(date)));
        assert!(!filters.matches(&file, Some(2049), Some(date)));
        assert!(!filters.matches(&file, Some(1024), date.pred_opt()));
        assert!(!filters.matches(&file, Some(1024), date.succ_opt()));
        assert!(!filters.matches(&file, Some(1024), None));
        assert!(!filters.matches(&file, None, Some(date)));
        assert!(!filters.matches(&entry("photo.png", 1024, false), Some(1024), Some(date)));
        assert!(!filters.matches(&entry("folder.zip", 1024, true), Some(1024), Some(date)));
        filters = EntryFilters { kind: EntryKind::Folders, ..EntryFilters::default() };
        assert!(filters.matches(&entry("folder", 0, true), None, None));
        assert!(!filters.matches(&file, Some(1024), Some(date)));
    }

    #[test]
    fn modification_dates_survive_listing_projection_and_search() {
        use crate::archive::{CreateOptions, ResolvedFormat, create_archive, list_archive};
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        std::fs::write(&source, b"dated entry").unwrap();
        let file = std::fs::File::options().write(true).open(&source).unwrap();
        let timestamp = chrono::DateTime::parse_from_rfc3339("2024-02-29T12:00:00Z").unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(timestamp.into())).unwrap();
        let expected = timestamp.with_timezone(&chrono::Local).date_naive();
        assert_eq!(read_directory(directory.path()).unwrap()[0].modified, Some(expected));
        for format in [ResolvedFormat::Zip, ResolvedFormat::Tar, ResolvedFormat::SevenZip, ResolvedFormat::Cpio] {
            let path = directory.path().join(format!("dated.{}", format.extension()));
            create_archive(&path, std::slice::from_ref(&source), CreateOptions {
                format,
                compression: format.compression_levels()[0],
                ..CreateOptions::default()
            }).unwrap();
            let entries = list_archive(&path).unwrap();
            let modified = entries[0].modified;
            assert!(modified.is_some(), "{}", format.label());
            if format != ResolvedFormat::Zip {
                assert_eq!(modified, Some(expected), "{}", format.label());
            }
            assert_eq!(visible_entries(&entries, "")[0].modified, modified);
            assert_eq!(search_archive_entries(&entries, "", "source")[0].modified, modified);
            let report = crate::inspection::inspect(
                &path, None, crate::inspection::InspectionMode::Preview,
                &std::sync::atomic::AtomicBool::new(false),
            );
            let stored = report.entries[0].modified.as_deref().unwrap();
            assert_eq!(super::parse_modification_date(stored), modified, "{}: {stored}", format.label());
        }
    }

    #[test]
    fn directory_listing_marks_password_protected_zip_and_seven_zip_archives() {
        use crate::archive::{
            CompressionLevel, CreateOptions, ResolvedFormat, create_archive,
            create_archive_with_password,
        };
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("secret.txt");
        std::fs::write(&source, b"protected data").unwrap();
        for format in [ResolvedFormat::Zip, ResolvedFormat::SevenZip] {
            let protected = directory.path().join(format!("protected.{}", format.extension()));
            let unprotected = directory.path().join(format!("plain.{}", format.extension()));
            let options = CreateOptions {
                format,
                compression: CompressionLevel::Fast,
                dictionary_mib: 1,
                ..CreateOptions::default()
            };
            create_archive_with_password(
                &protected,
                std::slice::from_ref(&source),
                options,
                Some("icon-test-password"),
            )
            .unwrap();
            create_archive(&unprotected, std::slice::from_ref(&source), options).unwrap();
            let entries = read_directory(directory.path()).unwrap();
            assert!(entries.iter().find(|entry| entry.name == protected.file_name().unwrap().to_string_lossy())
                .unwrap().is_encrypted);
            assert!(!entries.iter().find(|entry| entry.name == unprotected.file_name().unwrap().to_string_lossy())
                .unwrap().is_encrypted);
        }
    }

    /// Builds a minimal archive entry for browser projection and sorting tests.
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

    #[test]
    /// Builds parent rows from nested archive paths without adding file names.
    fn builds_a_hierarchical_folder_tree_from_archive_paths() {
        let entries = vec![
            entry("readme.txt", 12, false),
            entry("docs/guide.md", 24, false),
            entry("docs/reference/api.md", 36, false),
            entry("empty/", 0, true),
        ];
        let tree = folder_tree(&entries);
        let paths: Vec<_> = tree.iter().map(|(path, _, _)| path.as_str()).collect();
        assert_eq!(paths, ["docs", "docs/reference", "empty"]);
        assert_eq!(tree[0].2, 0);
        assert_eq!(tree[1].2, 1);
    }

    #[test]
    /// Shows only direct children and retains source paths for original archive entries.
    fn lists_immediate_children_and_preserves_original_archive_paths() {
        let entries = vec![
            entry("readme.txt", 12, false),
            entry("docs/guide.md", 24, false),
            entry("docs/reference/api.md", 36, false),
            entry("empty/", 0, true),
            entry("./reports/annual.pdf", 42, false),
        ];
        let root = visible_entries(&entries, "");
        assert_eq!(
            root.iter()
                .map(|entry| (entry.name.as_str(), entry.is_directory))
                .collect::<Vec<_>>(),
            [
                ("docs", true),
                ("empty", true),
                ("reports", true),
                ("readme.txt", false)
            ]
        );
        let docs = visible_entries(&entries, "docs");
        assert_eq!(
            docs.iter()
                .map(|entry| (entry.name.as_str(), entry.is_directory))
                .collect::<Vec<_>>(),
            [("reference", true), ("guide.md", false)]
        );
        let report = visible_entries(&entries, "reports");
        assert_eq!(
            report[0].source_path.as_deref(),
            Some("./reports/annual.pdf")
        );
    }

    #[test]
    fn archive_search_finds_nested_files_and_preserves_source_paths() {
        let entries = vec![
            entry("readme.txt", 12, false),
            entry("./docs/nested/Report.TXT", 24, false),
            entry("other/report.txt", 36, false),
            entry("docs/nested/中文.txt", 48, false),
        ];
        let results = search_archive_entries(&entries, "", " REPORT ");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "docs/nested/Report.TXT");
        assert_eq!(results[0].source_path.as_deref(), Some("./docs/nested/Report.TXT"));
        assert_eq!(results[0].size, 24);
        let nested = search_archive_entries(&entries, "docs", "report");
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].name, "nested/Report.TXT");
        assert_eq!(nested[0].source_path, results[0].source_path);
        assert_eq!(search_archive_entries(&entries, "docs", "中文")[0].name, "nested/中文.txt");
        assert_eq!(search_archive_entries(&entries, "", "nested/report").len(), 1);
        assert!(search_archive_entries(&entries, "docs", "missing").is_empty());
        let cleared = search_archive_entries(&entries, "docs", " ");
        assert_eq!(cleared.len(), 1);
        assert_eq!(cleared[0].name, "nested");
        assert!(cleared[0].is_directory);
    }

    #[test]
    /// Covers stable name, type, and size sorting together with folder size aggregation.
    fn sorts_columns_and_aggregates_folder_sizes() {
        for (column, ascending, expected) in [
            (
                SortColumn::Name,
                true,
                ["Folder", "alpha.zip", "Beta.txt", "gamma.txt"],
            ),
            (
                SortColumn::Name,
                false,
                ["Folder", "gamma.txt", "Beta.txt", "alpha.zip"],
            ),
            (
                SortColumn::Type,
                true,
                ["Folder", "Beta.txt", "gamma.txt", "alpha.zip"],
            ),
            (
                SortColumn::Type,
                false,
                ["Folder", "alpha.zip", "gamma.txt", "Beta.txt"],
            ),
            (
                SortColumn::Size,
                true,
                ["Folder", "gamma.txt", "alpha.zip", "Beta.txt"],
            ),
            (
                SortColumn::Size,
                false,
                ["Folder", "Beta.txt", "alpha.zip", "gamma.txt"],
            ),
        ] {
            let mut entries = vec![
                entry("Beta.txt", 300, false),
                entry("alpha.zip", 20, false),
                entry("Folder", 0, true),
                entry("gamma.txt", 10, false),
            ];
            sort_entries(&mut entries, column, ascending);
            assert_eq!(
                entries
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
        }

        let archive_entries = vec![
            entry("alpha-small/a.txt", 10, false),
            entry("middle-large/a.bin", 900, false),
            entry("middle-large/nested/b.bin", 100, false),
            entry("zeta-empty/", 0, true),
        ];
        let mut folders = visible_entries(&archive_entries, "");
        sort_entries(&mut folders, SortColumn::Size, false);
        assert_eq!(
            folders
                .iter()
                .map(|entry| (entry.name.as_str(), entry.size))
                .collect::<Vec<_>>(),
            [
                ("middle-large", 1000),
                ("alpha-small", 10),
                ("zeta-empty", 0)
            ]
        );
    }

    #[test]
    /// Checks recursive sizing, directory loading, and safe handling of symlink cycles.
    fn directory_operations_handle_nested_files_empty_dirs_and_symlink_cycles() {
        let directory = tempfile::tempdir().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(true);
        let error = super::directory_size_with_cancel(directory.path(), &cancel).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted,
        );
        std::fs::create_dir_all(directory.path().join("nested/deep")).unwrap();
        std::fs::create_dir(directory.path().join("empty")).unwrap();
        std::fs::write(directory.path().join("first.txt"), [0_u8; 10]).unwrap();
        std::fs::write(directory.path().join("nested/deep/second.bin"), [0_u8; 90]).unwrap();
        assert_eq!(directory_size(directory.path()).unwrap(), 100);
        assert_eq!(directory_size(&directory.path().join("empty")).unwrap(), 0);
        assert_eq!(
            default_directory(Some(directory.path())).unwrap(),
            directory.path()
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(directory.path(), directory.path().join("cycle")).unwrap();
            assert_eq!(directory_size(directory.path()).unwrap(), 100);
        }

        let entries = read_directory(directory.path()).unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == "nested" && entry.is_directory)
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == "first.txt" && entry.size == 10)
        );
        assert!(entries.iter().all(|entry| entry.source_path.is_some()));
    }
}

    #[test]
    fn sorts_modification_dates_chronologically() {
        let earlier = chrono::NaiveDate::from_ymd_opt(2024, 1, 10).unwrap();
        let later = chrono::NaiveDate::from_ymd_opt(2025, 6, 20).unwrap();
        let mut entries = vec![
            ArchiveEntry {
                name: "later.txt".into(),
                source_path: None,
                size: 1,
                modified: Some(later),
                is_directory: false,
                is_encrypted: false,
            },
            ArchiveEntry {
                name: "earlier.txt".into(),
                source_path: None,
                size: 1,
                modified: Some(earlier),
                is_directory: false,
                is_encrypted: false,
            },
        ];
        sort_entries(&mut entries, SortColumn::Modified, true);
        assert_eq!(entries[0].name, "earlier.txt");
        sort_entries(&mut entries, SortColumn::Modified, false);
        assert_eq!(entries[0].name, "later.txt");
    }
