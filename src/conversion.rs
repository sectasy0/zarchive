use crate::{archive::{self, ResolvedFormat}, inspection::{self, InspectionMode, Report}};
use anyhow::{Context, Result};
use std::{collections::BTreeSet, fs::File, io::Write, path::{Path, PathBuf}, sync::atomic::AtomicBool};

mod ui;
pub(crate) use ui::{State, footer, view};

const MAX_FILES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) const FORMATS: [ResolvedFormat; 15] = [
    ResolvedFormat::Zip, ResolvedFormat::SevenZip, ResolvedFormat::Tar,
    ResolvedFormat::TarGz, ResolvedFormat::TarBz2, ResolvedFormat::TarXz,
    ResolvedFormat::TarZstd, ResolvedFormat::TarLz4, ResolvedFormat::TarLzip,
    ResolvedFormat::TarBrotli, ResolvedFormat::Cpio, ResolvedFormat::Ar,
    ResolvedFormat::Pax, ResolvedFormat::Xar, ResolvedFormat::Rar,
];

pub(crate) struct Analysis {
    source: PathBuf,
    source_format: ResolvedFormat,
    report: Report,
    stamp: Vec<(PathBuf, u64, Option<std::time::SystemTime>)>,
    encrypted: bool,
    multipart: bool,
}

pub(crate) struct Assessment {
    pub warnings: Vec<(&'static str, Option<usize>)>,
    pub blockers: Vec<&'static str>,
}

fn stamp(source: &Path) -> Result<Vec<(PathBuf, u64, Option<std::time::SystemTime>)>> {
    let mut paths = crate::volumes::paths(source)?;
    if ResolvedFormat::from_path(source) == Some(ResolvedFormat::Rar) && crate::volumes::numbered_base(source).is_none() {
        let first = unrar::Archive::new(source).first_part();
        paths = std::fs::read_dir(source.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new(".")))?
            .map(|entry| entry.map(|entry| entry.path())).collect::<std::io::Result<Vec<_>>>()?
            .into_iter().filter(|path| unrar::Archive::new(path).first_part() == first).collect();
        anyhow::ensure!(paths.contains(&first), "Missing archive volume. Place all parts in the same folder.");
    }
    paths.sort();
    paths.into_iter().map(|path| {
        let metadata = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink(), "Conversion requires regular archive files");
        Ok((path, metadata.len(), metadata.modified().ok()))
    }).collect()
}

pub(crate) fn analyze(source: &Path, password: Option<&str>, cancel: &AtomicBool) -> Result<Analysis> {
    let before = stamp(source)?;
    let source_format = ResolvedFormat::from_path(source).context("Unsupported archive format")?;
    let report = inspection::inspect(source, password, InspectionMode::Preview, cancel);
    crate::task_queue::checkpoint()?;
    anyhow::ensure!(!cancel.load(std::sync::atomic::Ordering::Relaxed), "Task cancelled");
    let encrypted = report.entries.iter().any(|entry| entry.encrypted) || archive::is_password_protected(source);
    let multipart = archive::is_multipart_archive(source);
    anyhow::ensure!(before == stamp(source)?, "The source archive changed; analyze it again");
    Ok(Analysis { source: source.to_owned(), source_format, report, stamp: before, encrypted, multipart })
}

impl Analysis {
    pub(crate) fn source_format(&self) -> ResolvedFormat { self.source_format }
    pub(crate) fn entry_count(&self) -> usize { self.report.entries.len() }

    pub(crate) fn assess(&self, target: ResolvedFormat) -> Assessment {
        let mut warnings = vec![
            ("Conversion rewrites every file using target defaults; output size and processing time may change", None),
            ("Original timestamps are not copied; output entries use staging or format-default timestamps", None),
            ("Permissions, ownership, ACLs, extended attributes and special flags are not copied", None),
            ("Checksums are recalculated; signatures, recovery records and PAR2/REV sidecars are not transferred", None),
        ];
        let mut blockers = Vec::new();
        if self.report.error.is_some() || self.report.limited {
            blockers.push("The source could not be fully analyzed; check its password and integrity");
        }
        if !FORMATS.contains(&target) || !target.capabilities().can_create {
            blockers.push("The target format cannot be created; RAR requires the rar command-line tool");
        }
        let mut names = BTreeSet::new();
        let mut total = 0u64;
        let unsafe_entries = self.report.entries.iter().any(|entry| {
            total = total.saturating_add(entry.size);
            entry.link || entry.link_target.is_some() || entry.error.is_some()
                || archive::safe_extraction_path(&entry.name).map_or(true, |name|
                    name.as_os_str().is_empty() || !names.insert(name.to_string_lossy().to_lowercase()))
        });
        if unsafe_entries {
            blockers.push("Unsafe paths, links, duplicate names or unreadable entries prevent conversion");
        }
        if self.report.entries.is_empty() || self.report.entries.len() > MAX_FILES
            || self.report.entries.iter().any(|entry| entry.size > MAX_FILE_BYTES) || total > MAX_TOTAL_BYTES {
            blockers.push("Conversion limits: 10000 entries, 128 MiB per file and 512 MiB total");
        }
        if target == ResolvedFormat::Ar && self.report.entries.iter().any(|entry|
            entry.directory || entry.name.contains('/') || entry.name.contains('\\')) {
            blockers.push("AR cannot preserve directories or nested paths; choose another target format");
        }
        let comments = self.report.entries.iter().filter(|entry| entry.comment.as_ref().is_some_and(|text| !text.is_empty())).count();
        if comments > 0 { warnings.push(("Entry comments will be discarded", Some(comments))); }
        if self.report.archive_comment.as_ref().is_some_and(|text| !text.is_empty()) {
            warnings.push(("The archive comment will be discarded", None));
        }
        let extras = self.report.entries.iter().filter(|entry| entry.extra_fields.as_ref().is_some_and(|text| !text.is_empty())).count();
        if extras > 0 { warnings.push(("Format-specific extra fields will be discarded", Some(extras))); }
        if self.encrypted {
            warnings.push(("The converted archive will NOT be encrypted; the source password is used only for reading", None));
        }
        if self.multipart {
            warnings.push(("Multipart boundaries are not preserved; conversion creates one output archive", None));
        }
        if matches!(self.source_format, ResolvedFormat::SevenZip | ResolvedFormat::Rar) {
            warnings.push(("Solid blocks, compression methods and header encryption are replaced by target defaults", None));
        }
        Assessment { warnings, blockers }
    }
}

pub(crate) fn convert(plan: &Analysis, target: ResolvedFormat, password: Option<&str>,
    destination: &Path, cancel: &AtomicBool) -> Result<()> {
    anyhow::ensure!(plan.assess(target).blockers.is_empty(), "Conversion is blocked by the source or target limitations");
    anyhow::ensure!(plan.stamp == stamp(&plan.source)?, "The source archive changed; analyze it again");
    anyhow::ensure!(std::fs::symlink_metadata(destination).is_err(), "The destination already exists. Choose another location.");
    anyhow::ensure!(ResolvedFormat::from_path(destination) == Some(target), "The file extension must match the selected format.");
    let parent = destination.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let package = tempfile::tempdir_in(parent)?;
    let mut total = 0u64;
    for entry in &plan.report.entries {
        crate::task_queue::checkpoint()?;
        anyhow::ensure!(!cancel.load(std::sync::atomic::Ordering::Relaxed), "Task cancelled");
        let relative = archive::safe_extraction_path(&entry.name)?;
        let output = package.path().join(relative);
        if entry.directory {
            std::fs::create_dir_all(&output)?;
            continue;
        }
        let allowance = (MAX_TOTAL_BYTES - total).min(MAX_FILE_BYTES);
        anyhow::ensure!(entry.size <= allowance, "Conversion safety limit reached");
        let bytes = archive::read_entry_range_with_budget(&plan.source, &entry.name, password, cancel,
            0, allowance as usize, MAX_TOTAL_BYTES * 2)?;
        anyhow::ensure!(bytes.len() as u64 == entry.size && bytes.len() as u64 <= allowance,
            "Source content failed conversion size validation");
        total += bytes.len() as u64;
        if let Some(parent) = output.parent() { std::fs::create_dir_all(parent)?; }
        File::options().create_new(true).write(true).open(output)?.write_all(&bytes)?;
    }
    let verified = inspection::inspect(&plan.source, password, InspectionMode::Integrity, cancel);
    anyhow::ensure!(verified.integrity == inspection::Integrity::Passed && !verified.limited && verified.error.is_none(),
        "The source failed integrity validation; conversion was not saved");
    anyhow::ensure!(plan.stamp == stamp(&plan.source)?, "The source archive changed; analyze it again");
    let mut sources = std::fs::read_dir(package.path())?.map(|entry| entry.map(|entry| entry.path())).collect::<std::io::Result<Vec<_>>>()?;
    sources.sort();
    let staged = tempfile::Builder::new().prefix("zarchive-conversion-")
        .suffix(&format!(".{}", target.extension())).tempfile_in(parent)?.into_temp_path();
    let levels = target.compression_levels();
    let compression = if levels.contains(&archive::CompressionLevel::Normal) { archive::CompressionLevel::Normal } else { levels[0] };
    archive::create_archive(&staged, &sources, archive::CreateOptions { format: target, compression, include_hidden: true, ..Default::default() })?;
    File::options().write(true).open(&staged)?.sync_all()?;
    crate::task_queue::checkpoint()?;
    anyhow::ensure!(!cancel.load(std::sync::atomic::Ordering::Relaxed), "Task cancelled");
    staged.persist_noclobber(destination).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_sample(root: &Path) -> PathBuf {
        let file = root.join("payload.txt");
        std::fs::write(&file, b"conversion payload").unwrap();
        let source = root.join("source.zip");
        archive::create_archive(&source, &[file], Default::default()).unwrap();
        source
    }

    #[test]
    fn converts_all_available_targets_and_reads_them_back() {
        let root = tempfile::tempdir().unwrap();
        let source = zip_sample(root.path());
        let cancel = AtomicBool::new(false);
        let plan = analyze(&source, None, &cancel).unwrap();
        for format in FORMATS.into_iter().filter(|format| *format != ResolvedFormat::Rar) {
            let destination = root.path().join(format!("converted.{}", format.extension()));
            convert(&plan, format, None, &destination, &cancel).unwrap_or_else(|error| panic!("{format:?}: {error:#}"));
            let entries = archive::list_archive(&destination).unwrap();
            assert_eq!(entries.len(), 1, "{format:?}");
            assert_eq!(entries[0].name, "payload.txt", "{format:?}");
            assert_eq!(archive::read_entry_prefix_with_limit(&destination, "payload.txt", None, &cancel, 100).unwrap(), b"conversion payload", "{format:?}");
            let back = analyze(&destination, None, &cancel).unwrap();
            let restored = root.path().join(format!("back-{}.zip", format.extension()));
            convert(&back, ResolvedFormat::Zip, None, &restored, &cancel).unwrap_or_else(|error| panic!("{format:?} -> ZIP: {error:#}"));
            assert_eq!(archive::read_entry_prefix_with_limit(&restored, "payload.txt", None, &cancel, 100).unwrap(), b"conversion payload");
        }
    }

    #[test]
    fn warns_about_metadata_and_blocks_unsafe_or_oversized_entries() {
        let root = tempfile::tempdir().unwrap();
        let source = zip_sample(root.path());
        let cancel = AtomicBool::new(false);
        let mut plan = analyze(&source, None, &cancel).unwrap();
        plan.report.archive_comment = Some("archive note".into());
        plan.report.entries[0].comment = Some("entry note".into());
        plan.report.entries[0].extra_fields = Some("extra".into());
        plan.encrypted = true;
        plan.multipart = true;
        let warnings = plan.assess(ResolvedFormat::Zip).warnings;
        for key in ["Entry comments will be discarded", "The archive comment will be discarded", "Format-specific extra fields will be discarded",
            "The converted archive will NOT be encrypted; the source password is used only for reading",
            "Multipart boundaries are not preserved; conversion creates one output archive"] {
            assert!(warnings.iter().any(|(warning, _)| *warning == key), "{key}");
        }
        let entry = plan.report.entries[0].clone();
        for name in ["../escape", "/absolute", "C:\\escape", ""] {
            plan.report.entries[0].name = name.into();
            assert!(!plan.assess(ResolvedFormat::Tar).blockers.is_empty(), "{name}");
        }
        plan.report.entries = vec![entry.clone(), entry.clone()];
        assert!(!plan.assess(ResolvedFormat::Zip).blockers.is_empty());
        plan.report.entries = vec![entry.clone()];
        plan.report.entries[0].link = true;
        assert!(!plan.assess(ResolvedFormat::Zip).blockers.is_empty());
        plan.report.entries = vec![entry];
        plan.report.entries[0].size = MAX_FILE_BYTES + 1;
        assert!(plan.assess(ResolvedFormat::Zip).blockers.iter().any(|key| key.starts_with("Conversion limits")));
    }

    #[test]
    fn rejects_changed_source_wrong_extension_and_cancellation_without_publishing() {
        let root = tempfile::tempdir().unwrap();
        let source = zip_sample(root.path());
        let cancel = AtomicBool::new(false);
        let plan = analyze(&source, None, &cancel).unwrap();
        let destination = root.path().join("output.tar");
        assert!(convert(&plan, ResolvedFormat::Zip, None, &destination, &cancel).is_err());
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(convert(&plan, ResolvedFormat::Tar, None, &destination, &cancel).is_err());
        assert!(!destination.exists());
        cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        std::fs::write(&source, b"changed archive").unwrap();
        assert!(convert(&plan, ResolvedFormat::Tar, None, &destination, &cancel).unwrap_err().to_string().contains("changed"));
        assert!(!destination.exists());
        assert!(!root.path().read_dir().unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with(".tmp")));
    }

    #[test]
    fn encrypted_input_warns_and_requires_password_but_output_is_unencrypted() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("secret.txt");
        std::fs::write(&file, b"secret bytes").unwrap();
        let source = root.path().join("secret.zip");
        archive::create_archive_with_password(&source, &[file], Default::default(), Some("password")).unwrap();
        let cancel = AtomicBool::new(false);
        let plan = analyze(&source, Some("password"), &cancel).unwrap();
        assert!(plan.assess(ResolvedFormat::Zip).warnings.iter().any(|(warning, _)| warning.contains("NOT be encrypted")));
        let destination = root.path().join("converted.zip");
        assert!(convert(&plan, ResolvedFormat::Zip, Some("wrong"), &destination, &cancel).is_err());
        assert!(!destination.exists());
        convert(&plan, ResolvedFormat::Zip, Some("password"), &destination, &cancel).unwrap();
        assert!(!archive::is_password_protected(&destination));
        assert_eq!(archive::read_entry_prefix_with_limit(&destination, "secret.txt", None, &cancel, 100).unwrap(), b"secret bytes");
    }

    #[test]
    fn corrupt_source_is_never_published() {
        use std::io::{Seek, SeekFrom};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("corrupt.zip");
        let mut writer = zip::ZipWriter::new(File::create(&source).unwrap());
        writer.start_file("file.txt", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        writer.write_all(b"original payload").unwrap();
        writer.finish().unwrap();
        let offset = zip::ZipArchive::new(File::open(&source).unwrap()).unwrap().by_index(0).unwrap().data_start().unwrap();
        let mut file = File::options().write(true).open(&source).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(b"X").unwrap();
        drop(file);
        let cancel = AtomicBool::new(false);
        let plan = analyze(&source, None, &cancel).unwrap();
        let destination = root.path().join("converted.tar");
        assert!(convert(&plan, ResolvedFormat::Tar, None, &destination, &cancel).is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn conversion_reads_files_larger_than_the_preview_io_budget() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("large.zip");
        let mut writer = zip::ZipWriter::new(File::create(&source).unwrap());
        writer.start_file("large.bin", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        let bytes = vec![37u8; 1024 * 1024];
        for _ in 0..65 { writer.write_all(&bytes).unwrap(); }
        writer.finish().unwrap();
        let cancel = AtomicBool::new(false);
        let plan = analyze(&source, None, &cancel).unwrap();
        let destination = root.path().join("large.tar");
        convert(&plan, ResolvedFormat::Tar, None, &destination, &cancel).unwrap();
        assert_eq!(archive::list_archive(&destination).unwrap()[0].size, 65 * 1024 * 1024);
    }

    #[test]
    fn converts_zip_to_tar_preserving_paths_bytes_and_empty_directories() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("folder");
        std::fs::create_dir_all(folder.join("empty")).unwrap();
        std::fs::write(folder.join(".hidden"), b"conversion bytes").unwrap();
        let source = root.path().join("source.zip");
        archive::create_archive(&source, &[folder], Default::default()).unwrap();
        let original = std::fs::read(&source).unwrap();
        let cancel = AtomicBool::new(false);
        let plan = analyze(&source, None, &cancel).unwrap();
        assert!(plan.assess(ResolvedFormat::Ar).blockers.iter().any(|text| text.starts_with("AR cannot")));
        assert!(plan.assess(ResolvedFormat::Tar).warnings.len() >= 4);
        let destination = root.path().join("converted.tar");
        convert(&plan, ResolvedFormat::Tar, None, &destination, &cancel).unwrap();
        let names = archive::list_archive(&destination).unwrap();
        assert!(names.iter().any(|entry| entry.name.trim_end_matches('/') == "folder/empty" && entry.is_directory));
        let mut file = archive::extract_entry(&destination, "folder/.hidden").unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
        assert_eq!(bytes, b"conversion bytes");
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert!(convert(&plan, ResolvedFormat::Tar, None, &destination, &cancel).is_err());
    }
}