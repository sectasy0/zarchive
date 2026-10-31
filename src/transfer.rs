//! Moves filesystem entries while preventing overwrites and self-descendant moves.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Moves a file, directory, or symbolic link into an existing directory safely.
pub fn move_to_directory(source: &Path, directory: &Path) -> Result<PathBuf> {
    let (source, directory, destination, metadata) = validate_move(source, directory)?;
    crate::task_queue::checkpoint()?;
    if metadata.is_symlink() {
        std::fs::rename(&source, &destination).context("Cannot move this symbolic link")?;
    } else if metadata.is_dir() {
        fs_extra::dir::move_dir_with_progress(
            &source, &directory, &fs_extra::dir::CopyOptions::new(),
            |progress| {
                if crate::task_queue::checkpoint().is_err() { return fs_extra::dir::TransitProcessResult::Abort; }
                crate::task_queue::report_progress(progress.copied_bytes, progress.total_bytes);
                fs_extra::dir::TransitProcessResult::ContinueOrAbort
            },
        )?;
    } else {
        let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
        crate::task_queue::copy(&mut std::fs::File::open(&source)?, staged.as_file_mut())?;
        staged.as_file().set_permissions(metadata.permissions())?;
        staged.as_file().sync_all()?;
        crate::task_queue::checkpoint()?;
        staged.persist_noclobber(&destination).map_err(|error| error.error)?;
        std::fs::remove_file(&source)?;
    }
    Ok(destination)
}

pub(crate) fn can_move_to_directory(source: &Path, directory: &Path) -> bool {
    validate_move(source, directory).is_ok()
}

fn validate_move(source: &Path, directory: &Path) -> Result<(PathBuf, PathBuf, PathBuf, std::fs::Metadata)> {
    let directory =
        std::fs::canonicalize(directory).context("Destination folder is unavailable")?;
    anyhow::ensure!(directory.is_dir(), "Destination is not a folder");
    let name = source.file_name().context("Source has no filename")?;
    let parent = std::fs::canonicalize(source.parent().context("Source has no parent")?)?;
    let source = parent.join(name);
    let metadata = std::fs::symlink_metadata(&source)?;
    let destination = directory.join(name);
    anyhow::ensure!(
        std::fs::symlink_metadata(&destination).is_err(),
        "Destination already contains an item with this name"
    );
    if metadata.is_dir() {
        let canonical = std::fs::canonicalize(&source)?;
        anyhow::ensure!(
            !directory.starts_with(canonical),
            "Cannot move a folder inside itself"
        );
    }
    anyhow::ensure!(metadata.is_file() || metadata.is_dir() || metadata.is_symlink(), "Only files and folders can be moved");
    Ok((source, directory, destination, metadata))
}

/// Derives a clean extraction directory name by removing a supported archive suffix.
pub fn extraction_folder_name(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("archive");
    let lower = name.to_ascii_lowercase();
    let stem = [
        ".tar.bz2", ".tar.gz", ".tar.xz", ".tar.zst", ".tar.lz4", ".tar.lz", ".tar.br", ".tbz2",
        ".tbz", ".txz", ".tgz", ".bz2", ".xz", ".zst", ".gz", ".zip", ".tar", ".7z", ".rar",
    ]
    .iter()
    .find_map(|suffix| {
        lower
            .ends_with(suffix)
            .then(|| &name[..name.len() - suffix.len()])
    })
    .unwrap_or(name);
    if stem.is_empty() || stem == "." || stem == ".." {
        "archive".into()
    } else {
        stem.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Moves files and nested directories while preserving data and rejecting conflicts.
    fn moves_files_and_nested_folders_without_overwriting() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("destination");
        std::fs::create_dir(&destination).unwrap();
        let file = root.path().join("notes.txt");
        std::fs::write(&file, b"notes").unwrap();
        assert!(can_move_to_directory(&file, &destination));
        assert!(!can_move_to_directory(&file, root.path()));
        let moved = move_to_directory(&file, &destination).unwrap();
        assert_eq!(std::fs::read(&moved).unwrap(), b"notes");
        assert!(!file.exists());
        std::fs::write(&file, b"keep").unwrap();
        assert!(!can_move_to_directory(&file, &destination));
        assert!(move_to_directory(&file, &destination).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"keep");
        assert_eq!(std::fs::read(moved).unwrap(), b"notes");
        let folder = root.path().join("folder");
        std::fs::create_dir_all(folder.join("nested")).unwrap();
        std::fs::write(folder.join("nested/file.txt"), b"nested").unwrap();
        assert!(!can_move_to_directory(&folder, &folder.join("nested")));
        assert!(!can_move_to_directory(&folder, &folder));
        assert!(move_to_directory(&folder, &folder.join("nested")).is_err());
        let moved = move_to_directory(&folder, &destination).unwrap();
        assert_eq!(
            std::fs::read(moved.join("nested/file.txt")).unwrap(),
            b"nested"
        );
        assert!(!folder.exists());
    }

    #[test]
    /// Removes supported archive extensions and falls back for empty stems.
    fn extracts_destination_names_for_supported_suffixes() {
        assert_eq!(extraction_folder_name(Path::new("backup.TAR.GZ")), "backup");
        assert_eq!(extraction_folder_name(Path::new("backup.tgz")), "backup");
        assert_eq!(
            extraction_folder_name(Path::new("backup.tar.bz2")),
            "backup"
        );
        assert_eq!(extraction_folder_name(Path::new("backup.tbz")), "backup");
        assert_eq!(extraction_folder_name(Path::new("backup.tar.xz")), "backup");
        assert_eq!(extraction_folder_name(Path::new("backup.txz")), "backup");
        assert_eq!(
            extraction_folder_name(Path::new("backup.tar.zst")),
            "backup"
        );
        assert_eq!(
            extraction_folder_name(Path::new("backup.tar.lz4")),
            "backup"
        );
        assert_eq!(extraction_folder_name(Path::new("backup.tar.lz")), "backup");
        assert_eq!(extraction_folder_name(Path::new("backup.tar.br")), "backup");
        assert_eq!(extraction_folder_name(Path::new("backup.zip")), "backup");
        assert_eq!(extraction_folder_name(Path::new("backup.RAR")), "backup");
        assert_eq!(extraction_folder_name(Path::new(".zip")), "archive");
    }
}
