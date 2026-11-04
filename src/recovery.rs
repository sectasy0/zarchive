//! Recovers bounded data into private staging without modifying the source.

mod engine;
#[cfg(test)]
use engine::recover;
use engine::recover_source;
pub(crate) use engine::recover_with_parity;

mod native_backend;
mod rar_backend;
mod salvage;
mod sevenz_backend;
mod source;
mod structured;
mod tar_backend;
mod ui;
mod zip_backend;
#[cfg(test)]
use sevenz_backend::seven_zip_range;
use source::{Source, SourceReader};
use tar_backend::{recover_tar_blocks, recover_tar_header};
pub(crate) use zip_backend::verify_copy as verify_zip_copy;

#[cfg(test)]
use crate::ArchiveManager;
use anyhow::{Context, Result};
#[cfg(test)]
use gpui::px;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

const MAX_FILES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_INPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_SCAN_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CANDIDATES: usize = 20_000;
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CARVE_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) use ui::{State, footer, view};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Recovered,
    Unverified,
    Partial,
    Candidate,
    NotRecovered,
    Skipped,
}

impl Status {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Recovered => "Recovered",
            Self::Unverified => "Recovered (unverified)",
            Self::Partial => "Partially recovered",
            Self::Candidate => "Signature candidate (unverified)",
            Self::NotRecovered => "Not recovered",
            Self::Skipped => "Skipped",
        }
    }
}

pub(crate) struct Entry {
    pub name: String,
    pub output_name: Option<String>,
    pub status: Status,
    pub bytes: u64,
    pub issue: Option<&'static str>,
    file: Option<PathBuf>,
}

pub(crate) struct Block {
    pub offset: u64,
    pub length: u64,
    pub issue: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reason {
    Header,
    Checksum,
    Truncated,
    MissingVolume,
    Password,
    WrongPassword,
    EncryptedData,
    Unsupported,
    SolidBlock,
    Limit,
    Unverified,
    Read,
    ParityTool,
    ParityData,
}

impl Reason {
    fn label(self) -> &'static str {
        match self {
            Self::Header => "Archive headers are damaged or unavailable",
            Self::Checksum => "The decoded data does not match its stored checksum",
            Self::Truncated => "The data stream ended before the expected file size",
            Self::MissingVolume => "A required archive volume is missing",
            Self::Password => "The archive requires the correct password",
            Self::WrongPassword => "The supplied password was rejected by the archive",
            Self::EncryptedData => "The encrypted stream is damaged or the password is incorrect",
            Self::Unsupported => "The compression method is not supported by recovery",
            Self::SolidBlock => "This file depends on a damaged solid compression block",
            Self::Limit => "Recovery stopped at a safety limit",
            Self::Unverified => "No independent content checksum was available for verification",
            Self::Read => "The decoder could not read the remaining data",
            Self::ParityTool => "The RAR parity engine is not installed or configured",
            Self::ParityData => {
                "Available parity data could not reconstruct this archive completely"
            }
        }
    }

    fn action(self) -> &'static str {
        match self {
            Self::Header => "Try Salvage Mode or another intact copy of the archive",
            Self::Checksum | Self::Truncated | Self::Read => {
                "Keep partial data separately and retry with an intact source"
            }
            Self::MissingVolume => "Place every original volume in the same folder and retry",
            Self::Password | Self::WrongPassword => "Enter the original archive password and retry",
            Self::EncryptedData => {
                "Check the password and try another intact copy of the encrypted data"
            }
            Self::Unsupported => "Try a recovery tool that supports this compression method",
            Self::SolidBlock => "Recover independent blocks or retry with an intact solid block",
            Self::Limit => "Recover a smaller archive or selected intact volumes",
            Self::Unverified => "Compare the recovered data with a trusted copy before using it",
            Self::ParityTool => "Install rar on PATH or configure ZARCHIVE_RAR_TOOL",
            Self::ParityData => {
                "Provide matching intact PAR2 or REV volumes, or a valid archive recovery record"
            }
        }
    }
}

struct Diagnostic {
    entry: Option<usize>,
    reason: Reason,
    expected_size: Option<u64>,
    actual_size: Option<u64>,
    expected_crc: Option<u32>,
    actual_crc: Option<u32>,
    offset: Option<u64>,
    length: Option<u64>,
}

pub(crate) struct Recovery {
    pub source: PathBuf,
    pub salvage: bool,
    pub entries: Vec<Entry>,
    pub blocks: Vec<Block>,
    pub notes: Vec<&'static str>,
    pub limited: bool,
    archive_format: Option<crate::archive::ResolvedFormat>,
    directory: tempfile::TempDir,
    remaining: u64,
    diagnostics: Vec<Diagnostic>,
    parity: Option<serde_json::Value>,
    data_range_source: &'static str,
}

impl Recovery {
    pub(crate) fn original_format(&self) -> Option<crate::archive::ResolvedFormat> {
        self.archive_format
    }

    pub(crate) fn export_format(
        &self,
        destination: &Path,
        format: crate::archive::ResolvedFormat,
    ) -> Result<()> {
        anyhow::ensure!(
            format.archive.is_some() && format.capabilities().can_create,
            "This format cannot be created"
        );
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let package = tempfile::tempdir_in(parent)?;
        let mut sources = Vec::new();
        let mut mappings = Vec::new();
        let flat = format == crate::archive::ResolvedFormat::Ar;
        let mut roots = std::collections::BTreeSet::new();
        for entry in &self.entries {
            crate::task_queue::checkpoint()?;
            let (Some(file), Some(name)) = (&entry.file, &entry.output_name) else {
                continue;
            };
            let name = if flat {
                name.replace('/', "-")
            } else {
                name.clone()
            };
            let relative = Path::new(&name);
            anyhow::ensure!(
                relative
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_))),
                "Unsafe recovery output name"
            );
            let path = package.path().join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut output = File::options().write(true).create_new(true).open(&path)?;
            crate::task_queue::copy(&mut File::open(file)?, &mut output)?;
            let root = relative
                .components()
                .next()
                .context("Unsafe recovery output name")?
                .as_os_str()
                .to_os_string();
            roots.insert(root);
            mappings.push(serde_json::json!({ "source_name": entry.name, "exported_name": name, "status": entry.status.label() }));
        }
        for root in roots {
            sources.push(package.path().join(root));
        }
        let report_path = package.path().join("recovery-report.json");
        let mut report = self.json();
        report["export"] = serde_json::json!({ "format": format.label(), "paths": mappings });
        serde_json::to_writer_pretty(File::create(&report_path)?, &report)?;
        sources.push(report_path);
        let staged = tempfile::Builder::new()
            .prefix("zarchive-repaired-")
            .suffix(&format!(".{}", format.extension()))
            .tempfile_in(parent)?
            .into_temp_path();
        let levels = format.compression_levels();
        let compression = if levels.contains(&crate::archive::CompressionLevel::Normal) {
            crate::archive::CompressionLevel::Normal
        } else {
            levels[0]
        };
        crate::archive::create_archive(
            &staged,
            &sources,
            crate::archive::CreateOptions {
                format,
                compression,
                ..Default::default()
            },
        )?;
        File::open(&staged)?.sync_all()?;
        crate::task_queue::checkpoint()?;
        staged
            .persist_noclobber(destination)
            .map_err(|error| error.error)?;
        Ok(())
    }

    fn row_count(&self) -> usize {
        self.entries.len()
            + self.blocks.len()
            + self
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.entry.is_none())
                .count()
    }

    fn complete_diagnostics(&mut self) {
        for index in 0..self.entries.len() {
            if self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.entry == Some(index))
            {
                continue;
            }
            let entry = &self.entries[index];
            let reason = match entry.status {
                Status::Recovered => continue,
                Status::Unverified | Status::Candidate => Reason::Unverified,
                Status::Skipped if entry.issue == Some("Recovery safety limit reached") => {
                    Reason::Limit
                }
                _ => match entry.issue {
                    Some("Password required or invalid") => Reason::Password,
                    Some("Unsupported recovery compression method") => Reason::Unsupported,
                    Some("Damaged archive header") => Reason::Header,
                    Some("Directories and links are not recovered") => continue,
                    _ => Reason::Read,
                },
            };
            self.diagnose(Some(index), reason, None, None);
        }
        for block_index in 0..self.blocks.len() {
            let block = &self.blocks[block_index];
            if block.issue == "Missing archive volume"
                && !self.diagnostics.iter().any(|diagnostic| {
                    diagnostic.reason == Reason::MissingVolume
                        && diagnostic.offset == Some(block.offset)
                })
            {
                self.diagnose(
                    None,
                    Reason::MissingVolume,
                    Some(block.offset),
                    Some(block.length),
                );
            }
        }
    }

    fn diagnose(
        &mut self,
        entry: Option<usize>,
        reason: Reason,
        offset: Option<u64>,
        length: Option<u64>,
    ) {
        if self.diagnostics.len() >= MAX_CANDIDATES {
            self.limited = true;
            return;
        }
        self.diagnostics.push(Diagnostic {
            entry,
            reason,
            expected_size: None,
            actual_size: None,
            expected_crc: None,
            actual_crc: None,
            offset,
            length,
        });
    }

    fn unavailable(&mut self, name: String, reason: Reason, offset: u64, length: u64) {
        let index = self.entries.len();
        self.skipped(
            name,
            if reason == Reason::Limit {
                Status::Skipped
            } else {
                Status::NotRecovered
            },
            reason.label(),
        );
        if self.entries.len() > index {
            self.diagnose(Some(index), reason, Some(offset), Some(length));
        }
    }

    fn new(source: &Path, salvage: bool) -> Result<Self> {
        Ok(Self {
            source: source.to_owned(),
            salvage,
            entries: Vec::new(),
            blocks: Vec::new(),
            notes: Vec::new(),
            limited: false,
            archive_format: archive_format_for_path(source),
            directory: tempfile::tempdir()?,
            remaining: MAX_OUTPUT_BYTES,
            diagnostics: Vec::new(),
            parity: None,
            data_range_source: "original_archive",
        })
    }

    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "source": self.source, "salvage_mode": self.salvage, "limited": self.limited,
            "export_format": self.archive_format.map(crate::archive::ResolvedFormat::label),
            "notes": self.notes,
            "parity_reconstruction": self.parity,
            "data_range_source": self.data_range_source,
            "entries": self.entries.iter().map(|entry| serde_json::json!({
                "name": entry.name, "output_name": entry.output_name,
                "status": entry.status.label(), "bytes": entry.bytes, "issue": entry.issue,
            })).collect::<Vec<_>>(),
            "suspect_blocks": self.blocks.iter().map(|block| serde_json::json!({
                "offset": block.offset, "length": block.length, "issue": block.issue,
            })).collect::<Vec<_>>(),
            "diagnostics": self.diagnostics.iter().map(|diagnostic| serde_json::json!({
                "entry_index": diagnostic.entry, "reason": diagnostic.reason.label(),
                "suggestion": diagnostic.reason.action(), "expected_bytes": diagnostic.expected_size,
                "recovered_bytes": diagnostic.actual_size, "expected_crc32": diagnostic.expected_crc.map(|crc| format!("{crc:08x}")),
                "actual_crc32": diagnostic.actual_crc.map(|crc| format!("{crc:08x}")),
                "offset": diagnostic.offset, "length": diagnostic.length,
            })).collect::<Vec<_>>(),
            "limits": { "files": MAX_FILES, "file_bytes": MAX_FILE_BYTES, "total_output_bytes": MAX_OUTPUT_BYTES,
                "input_bytes": MAX_INPUT_BYTES, "scan_bytes": MAX_SCAN_BYTES, "signature_candidates": MAX_CANDIDATES,
                "index_bytes": MAX_INDEX_BYTES, "carve_bytes": MAX_CARVE_BYTES },
        })
    }

    #[cfg(test)]
    pub(crate) fn export_report(&self, destination: &Path) -> Result<()> {
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut staged, &self.json())?;
        staged.as_file().sync_all()?;
        crate::task_queue::checkpoint()?;
        staged
            .persist_noclobber(destination)
            .map_err(|error| error.error)?;
        Ok(())
    }

    fn skipped(&mut self, name: String, status: Status, issue: &'static str) {
        if self.entries.len() >= MAX_FILES {
            self.limited = true;
            return;
        }
        self.entries.push(Entry {
            name,
            output_name: None,
            status,
            bytes: 0,
            issue: Some(issue),
            file: None,
        });
    }

    fn retain(
        &mut self,
        name: String,
        mut reader: impl Read,
        expected_size: Option<u64>,
        crc: Option<u32>,
        offset: u64,
        packed: u64,
    ) -> Result<()> {
        if self.entries.len() >= MAX_FILES {
            self.limited = true;
            return Ok(());
        }
        let allowance = self.remaining.min(MAX_FILE_BYTES);
        if allowance == 0
            || expected_size.is_some_and(|size| {
                size > allowance || (size > 10 * 1024 * 1024 && size / packed.max(1) > 1000)
            })
        {
            self.limited = true;
            self.skipped(name, Status::Skipped, "Recovery safety limit reached");
            return Ok(());
        }
        let index = self.entries.len() + 1;
        let path = self.directory.path().join(format!("{index:05}.data"));
        let mut output = File::create(&path)?;
        let mut hash = crc32fast::Hasher::new();
        let mut bytes = 0u64;
        let mut buffer = vec![0u8; 65536];
        let mut issue = None;
        let mut reason = None;
        loop {
            crate::task_queue::checkpoint()?;
            let length = allowance.saturating_sub(bytes).min(buffer.len() as u64) as usize;
            if length == 0 {
                let mut probe = [0u8; 1];
                match reader.read(&mut probe) {
                    Ok(0) => {}
                    _ => {
                        self.limited = true;
                        issue = Some("Recovery safety limit reached");
                        reason = Some(Reason::Limit);
                    }
                }
                break;
            }
            match reader.read(&mut buffer[..length]) {
                Ok(0) => break,
                Ok(count) => {
                    output.write_all(&buffer[..count])?;
                    hash.update(&buffer[..count]);
                    bytes += count as u64;
                    self.remaining -= count as u64;
                }
                Err(error) => {
                    issue = Some("Damaged or missing data");
                    reason = Some(
                        if matches!(
                            error
                                .get_ref()
                                .and_then(|error| error.downcast_ref::<crate::xar::EntryError>()),
                            Some(crate::xar::EntryError::Checksum)
                        ) {
                            Reason::Checksum
                        } else if error.kind() == io::ErrorKind::UnexpectedEof {
                            Reason::Truncated
                        } else {
                            Reason::Read
                        },
                    );
                    break;
                }
            }
        }
        crate::task_queue::checkpoint()?;
        let actual_crc = hash.finalize();
        if expected_size.is_some_and(|size| size != bytes)
            || crc.is_some_and(|crc| crc != actual_crc)
        {
            issue = issue.or(Some("Size or checksum could not be verified"));
            if reason != Some(Reason::Limit) {
                if expected_size.is_some_and(|size| size < bytes) {
                    reason = Some(Reason::Header);
                } else if expected_size.is_some_and(|size| size != bytes) {
                    reason = Some(Reason::Truncated);
                } else if crc.is_some_and(|crc| crc != actual_crc) {
                    reason = Some(Reason::Checksum);
                }
            }
        }
        let status = if issue.is_some() && bytes == 0 {
            Status::NotRecovered
        } else if issue.is_some() {
            Status::Partial
        } else if crc.is_some() {
            Status::Recovered
        } else {
            Status::Unverified
        };
        output.sync_all()?;
        drop(output);
        let file = if status == Status::NotRecovered {
            std::fs::remove_file(&path)?;
            None
        } else {
            Some(path)
        };
        let output_name = file.as_ref().map(|_| {
            format!(
                "{}/{index:05}-{}",
                match status {
                    Status::Recovered => "recovered",
                    Status::Partial => "partial",
                    Status::Candidate => "carved",
                    _ => "unverified",
                },
                safe_basename(&name)
            )
        });
        if issue.is_some() {
            self.blocks.push(Block {
                offset,
                length: packed,
                issue: "Suspect compressed data range",
            });
        }
        if let Some(reason) =
            reason.or_else(|| (status == Status::Unverified).then_some(Reason::Unverified))
        {
            let reason = if issue.is_some()
                && self.blocks.iter().any(|block| {
                    block.issue == "Missing archive volume"
                        && block.offset < offset.saturating_add(packed)
                        && block.offset.saturating_add(block.length) > offset
                }) {
                Reason::MissingVolume
            } else {
                reason
            };
            self.diagnostics.push(Diagnostic {
                entry: Some(self.entries.len()),
                reason,
                expected_size,
                actual_size: Some(bytes),
                expected_crc: crc,
                actual_crc: Some(actual_crc),
                offset: Some(offset),
                length: Some(packed),
            });
        }
        self.entries.push(Entry {
            name,
            output_name,
            status,
            bytes,
            issue,
            file,
        });
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn export_zip(&self, destination: &Path) -> Result<()> {
        anyhow::ensure!(
            !destination.exists(),
            "The destination already exists. Choose another location."
        );
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        let mut writer = zip::ZipWriter::new(staged.as_file_mut());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for entry in &self.entries {
            crate::task_queue::checkpoint()?;
            if let (Some(path), Some(name)) = (&entry.file, &entry.output_name) {
                writer.start_file(name, options)?;
                crate::task_queue::copy(&mut File::open(path)?, &mut writer)?;
            }
        }
        writer.start_file("recovery-report.json", options)?;
        serde_json::to_writer_pretty(&mut writer, &self.json())?;
        writer.finish()?;
        staged.as_file().sync_all()?;
        crate::task_queue::checkpoint()?;
        staged
            .persist_noclobber(destination)
            .map_err(|error| error.error)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn export_tar(&self, destination: &Path) -> Result<()> {
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        let mut writer = tar::Builder::new(staged.as_file_mut());
        for entry in &self.entries {
            crate::task_queue::checkpoint()?;
            if let (Some(path), Some(name)) = (&entry.file, &entry.output_name) {
                append_repaired_tar_entry(&mut writer, name, path)?;
            }
        }
        let mut report = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut report, &self.json())?;
        report.flush()?;
        append_repaired_tar_entry(&mut writer, "recovery-report.json", report.path())?;
        writer.finish()?;
        drop(writer);
        staged.as_file().sync_all()?;
        crate::task_queue::checkpoint()?;
        staged
            .persist_noclobber(destination)
            .map_err(|error| error.error)?;
        Ok(())
    }
}

fn archive_format_for_path(path: &Path) -> Option<crate::archive::ResolvedFormat> {
    let format = if crate::parity::is_split_zip(path) {
        crate::archive::ResolvedFormat::Zip
    } else {
        crate::archive::ResolvedFormat::from_path(path)?
    };
    let format = if format.is_single_compressed() {
        crate::archive::ResolvedFormat::tar(format.compression)
    } else {
        format
    };
    (format.archive.is_some() && format.capabilities().can_create).then_some(format)
}

#[cfg(test)]
fn append_repaired_tar_entry<W: Write>(
    writer: &mut tar::Builder<W>,
    name: &str,
    path: &Path,
) -> Result<()> {
    let file = File::open(path)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(file.metadata()?.len());
    header.set_mode(0o600);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    writer.append_data(&mut header, name, crate::task_queue::TaskReader(file))?;
    Ok(())
}

fn safe_basename(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or("file");
    let name: String = name
        .chars()
        .take(100)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name == "." || name == ".." {
        "file".into()
    } else {
        name
    }
}

fn mark_partial(recovery: &mut Recovery, index: usize, reason: Reason) {
    let entry = &mut recovery.entries[index];
    if entry.file.is_none() || entry.status == Status::Recovered {
        return;
    }
    entry.status = Status::Partial;
    entry.issue = Some(reason.label());
    if let Some(name) = &entry.output_name {
        entry.output_name = Some(name.replacen("unverified/", "partial/", 1));
    }
    recovery.diagnose(Some(index), reason, None, None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::test::TestWindowExt;

    pub(super) fn complete(cx: &mut gpui::VisualTestContext, view: &gpui::Entity<ArchiveManager>) {
        for _ in 0..8 {
            cx.run_until_parked();
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            if cx.read(|cx| view.read(cx).recovery.task.is_none()) {
                break;
            }
        }
    }

    #[gpui::test]
    fn recovery_results_wrap_long_names_statuses_and_issues_without_overlap(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.zip");
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.language = crate::i18n::Language::Polish;
            let mut report = Recovery::new(&source, true).unwrap();
            report.entries.push(Entry {
                name: format!("{}.pdf", "very_long_unbroken_filename_".repeat(16)),
                output_name: None,
                status: Status::Candidate,
                bytes: 12345,
                issue: Some("Signature found; file boundaries are not guaranteed"),
                file: None,
            });
            report.entries.push(Entry {
                name: "next-file.txt".to_owned(),
                output_name: None,
                status: Status::Recovered,
                bytes: 12,
                issue: None,
                file: None,
            });
            manager.recovery.source = Some(source.clone());
            manager.recovery.results.reset(2);
            manager.recovery.result = Some(Arc::new(report));
            manager.show_modal(crate::Modal::Recovery, window, cx);
            manager
        });
        for viewport in [
            gpui::size(px(1100.0), px(900.0)),
            gpui::size(px(560.0), px(700.0)),
        ] {
            cx.simulate_resize(viewport);
            complete(cx, &view);
            let row = cx.debug_bounds("recovery-result-0").unwrap();
            let name = cx.debug_bounds("recovery-result-name-0").unwrap();
            let status = cx.debug_bounds("recovery-result-status-0").unwrap();
            let issue = cx.debug_bounds("recovery-result-issue-0").unwrap();
            assert!(
                name.size.height > px(28.0),
                "long names must wrap rather than truncate"
            );
            assert!(row.size.height > px(64.0));
            assert!(name.bottom() <= status.origin.y);
            assert!(status.bottom() <= issue.origin.y);
            assert!(issue.bottom() <= row.bottom());
            assert!(name.right() <= row.right());
            assert!(issue.right() <= row.right());
            if let Some(next) = cx.debug_bounds("recovery-result-1") {
                assert!(row.bottom() <= next.origin.y);
            }
            let panel = cx.debug_bounds("dialog-0").unwrap();
            let actions = cx.debug_bounds("recovery-actions").unwrap();
            assert!(actions.bottom() <= panel.bottom());
            assert!(panel.bottom() <= viewport.height);
        }
    }

    #[gpui::test]
    fn recovery_dialog_runs_salvage_exports_and_cleans_private_staging(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("broken.zip");
        let mut writer = zip::ZipWriter::new(File::create(&source).unwrap());
        writer
            .start_file("notes.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"recovery UI fixture").unwrap();
        writer.finish().unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&source).unwrap()).unwrap();
        let directory = archive.by_index_raw(0).unwrap().central_header_start();
        drop(archive);
        File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_len(directory)
            .unwrap();
        let before = std::fs::read(&source).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(root.path().to_path_buf());
            manager.entries = crate::browser::read_directory(root.path()).unwrap();
            manager.selected_entry = Some(source.to_string_lossy().into_owned());
            manager
        });
        view.update(cx, |manager, cx| {
            manager.selected_entry = Some(source.to_string_lossy().into_owned());
            cx.notify();
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).inspection_target().map(|target| target.0),
                Some(source.clone())
            )
        });
        cx.update(|window, cx| window.click("toolbar-inspect-dropdown", cx));
        cx.run_until_parked();
        let bounds = cx.update(|window, _| {
            window
                .within("popup-menu")
                .find_by_label("Archive Repair & Recovery")
                .bounds()
        });
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.read(|cx| assert!(view.read(cx).modal == Some(crate::Modal::Recovery)));
        for _ in 0..3 {
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        }
        assert!(cx.debug_bounds("recovery-content").is_some());
        cx.read(|cx| assert_eq!(view.read(cx).recovery.source.as_ref(), Some(&source)));
        let bounds = cx.debug_bounds("recovery-salvage").unwrap();
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        let bounds = cx.debug_bounds("start-recovery").unwrap();
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        complete(cx, &view);
        let staging = cx.read(|cx| {
            let manager = view.read(cx);
            assert!(
                manager.recovery.error.is_none(),
                "{:?}",
                manager.recovery.error
            );
            let result = manager.recovery.result.as_ref().unwrap();
            assert!(result.salvage);
            assert_eq!(result.entries[0].status, Status::Recovered);
            result.directory.path().to_owned()
        });
        let destination = root.path().join("recovered.zip");
        view.update(cx, |manager, cx| {
            manager.export_recovered_archive(
                manager.recovery.result.clone().unwrap(),
                destination.clone(),
                cx,
            );
        });
        complete(cx, &view);
        assert!(
            zip::ZipArchive::new(File::open(&destination).unwrap())
                .unwrap()
                .by_name("recovery-report.json")
                .is_ok()
        );
        for viewport in [
            gpui::size(px(1100.0), px(720.0)),
            gpui::size(px(560.0), px(440.0)),
        ] {
            cx.simulate_resize(viewport);
            cx.run_until_parked();
            let panel = cx.debug_bounds("dialog-0").unwrap();
            let actions = cx.debug_bounds("recovery-actions").unwrap();
            assert!(actions.bottom() <= panel.bottom());
            assert!(panel.bottom() <= viewport.height);
        }
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.modal.is_none());
            assert!(manager.recovery.result.is_none());
            assert!(manager.recovery.password.text.is_empty());
        });
        assert!(!staging.exists());
        assert_eq!(std::fs::read(source).unwrap(), before);
    }

    #[test]
    fn recovery_preserves_original_and_exports_verified_files_with_a_report() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.zip");
        let mut writer = zip::ZipWriter::new(File::create(&source).unwrap());
        writer
            .start_file("../unsafe.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"recoverable data").unwrap();
        writer.finish().unwrap();
        let before = std::fs::read(&source).unwrap();
        let result = recover(&source, None, false).unwrap();
        assert_eq!(result.entries[0].status, Status::Recovered);
        let output = root.path().join("recovered.zip");
        result.export_zip(&output).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&output).unwrap()).unwrap();
        assert!(archive.by_name("recovery-report.json").is_ok());
        let mut data = String::new();
        archive
            .by_name("recovered/00001-unsafe.txt")
            .unwrap()
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "recoverable data");
        assert!(result.export_zip(&source).is_err());
        assert!(result.export_zip(&output).is_err());
        let report = root.path().join("report.json");
        result.export_report(&report).unwrap();
        assert!(result.export_report(&report).is_err());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(report).unwrap()).unwrap()["entries"]
                [0]["status"],
            "Recovered"
        );
        assert_eq!(std::fs::read(source).unwrap(), before);
        assert!(!root.path().join("unsafe.txt").exists());
    }

    #[test]
    fn recovery_keeps_crc_damaged_stored_data_separate_from_verified_files() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("damaged.zip");
        let mut writer = zip::ZipWriter::new(File::create(&source).unwrap());
        for name in ["good.txt", "bad.txt"] {
            writer
                .start_file(
                    name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(name.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        let mut reader = zip::ZipArchive::new(File::open(&source).unwrap()).unwrap();
        let offset = reader.by_name("bad.txt").unwrap().data_start().unwrap() as usize;
        drop(reader);
        let mut bytes = std::fs::read(&source).unwrap();
        bytes[offset] ^= 1;
        std::fs::write(&source, bytes).unwrap();
        let result = recover(&source, None, true).unwrap();
        assert_eq!(result.entries[0].status, Status::Recovered);
        assert_eq!(result.entries[1].status, Status::Partial);
        assert_eq!(result.entries[1].bytes, 7);
        assert_eq!(result.blocks.len(), 1);
        let cause = result
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.entry == Some(1))
            .unwrap();
        assert_eq!(cause.reason, Reason::Checksum);
        assert_ne!(cause.expected_crc, cause.actual_crc);
    }

    #[test]
    fn recovery_rebuilds_missing_zip_directory_from_local_headers() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("truncated.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        for name in ["first.txt", "second.txt"] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(name.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let directory = archive.by_index_raw(0).unwrap().central_header_start();
        drop(archive);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(directory)
            .unwrap();
        let result = recover(&path, None, true).unwrap();
        assert_eq!(
            result
                .entries
                .iter()
                .filter(|entry| entry.status == Status::Recovered)
                .count(),
            2
        );
        let output = root.path().join("fixed.zip");
        result.export_zip(&output).unwrap();
        assert_eq!(
            zip::ZipArchive::new(File::open(output).unwrap())
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn recovery_uses_available_volumes_without_collapsing_missing_offsets() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.txt");
        std::fs::write(&source, b"volume payload".repeat(500)).unwrap();
        let base = root.path().join("split.zip");
        crate::archive::create_archive(
            &base,
            &[source],
            crate::archive::CreateOptions {
                compression: crate::archive::CompressionLevel::Stored,
                volume_size: Some(1000),
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::remove_file(crate::volumes::part_path(&base, 3)).unwrap();
        let path = crate::volumes::part_path(&base, 1);
        let result = recover(&path, None, true).unwrap();
        assert!(
            result
                .blocks
                .iter()
                .any(|block| block.issue == "Missing archive volume" && block.offset == 2000)
        );
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.status == Status::Partial)
        );
        assert!(
            !result
                .entries
                .iter()
                .any(|entry| entry.status == Status::Recovered)
        );
    }

    #[test]
    fn recovery_exports_valid_repaired_tar_without_overwriting_the_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("damaged.tar");
        let mut builder = tar::Builder::new(File::create(&source).unwrap());
        let mut header = tar::Header::new_ustar();
        header.set_size(7);
        header.set_mode(0o777);
        header.set_cksum();
        builder
            .append_data(&mut header, "notes.txt", b"payload".as_slice())
            .unwrap();
        builder.finish().unwrap();
        let mut damaged = std::fs::read(&source).unwrap();
        damaged[148..156].fill(b'0');
        std::fs::write(&source, &damaged).unwrap();
        let recovery = recover(&source, None, true).unwrap();
        let destination = root.path().join("repaired.tar");
        recovery.export_tar(&destination).unwrap();
        let mut archive = tar::Archive::new(File::open(&destination).unwrap());
        let mut files = std::collections::BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            assert!(entry.header().entry_type().is_file());
            assert_eq!(entry.header().mode().unwrap(), 0o600);
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            files.insert(name, bytes);
        }
        assert_eq!(files["unverified/00001-notes.txt"], b"payload");
        assert!(
            serde_json::from_slice::<serde_json::Value>(&files["recovery-report.json"]).is_ok()
        );
        assert!(recovery.export_tar(&source).is_err());
        assert!(recovery.export_tar(&destination).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), damaged);
    }

    #[test]
    fn recovery_tar_resynchronizes_old_and_modern_headers_after_corruption() {
        let root = tempfile::tempdir().unwrap();
        for (index, template) in [
            tar::Header::new_old(),
            tar::Header::new_ustar(),
            tar::Header::new_gnu(),
        ]
        .into_iter()
        .enumerate()
        {
            let source = root.path().join(format!("damaged-{index}.tar"));
            let mut builder = tar::Builder::new(File::create(&source).unwrap());
            for (name, data) in [
                ("first.txt", b"first".as_slice()),
                ("broken.txt", b"middle"),
                ("last.txt", b"lastpayload"),
            ] {
                let mut header = template.clone();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, data).unwrap();
            }
            builder.finish().unwrap();
            let mut damaged = std::fs::read(&source).unwrap();
            damaged[148..156].fill(b'x');
            damaged[1024..1536].fill(b'Z');
            damaged.truncate(2560 + 5);
            std::fs::write(&source, &damaged).unwrap();
            let result = recover(&source, None, false).unwrap();
            let first = result
                .entries
                .iter()
                .find(|entry| entry.name == "first.txt")
                .unwrap();
            let last = result
                .entries
                .iter()
                .find(|entry| entry.name == "last.txt")
                .unwrap();
            assert_eq!(first.status, Status::Unverified);
            assert_eq!(
                std::fs::read(first.file.as_ref().unwrap()).unwrap(),
                b"first"
            );
            assert_eq!(last.status, Status::Partial);
            assert_eq!(
                std::fs::read(last.file.as_ref().unwrap()).unwrap(),
                b"lastp"
            );
            assert!(
                result
                    .blocks
                    .iter()
                    .any(|block| block.issue == "Damaged archive header" && block.offset == 1024)
            );
            result
                .export_tar(&root.path().join(format!("fixed-{index}.tar")))
                .unwrap();
            assert_eq!(std::fs::read(&source).unwrap(), damaged);
        }
    }

    #[test]
    fn recovery_tar_preserves_gnu_long_names_when_the_file_header_is_damaged() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("long-name.tar");
        let name = format!("folder/{}/notes.txt", "long-name-".repeat(16));
        let mut builder = tar::Builder::new(File::create(&source).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_size(7);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, &name, b"payload".as_slice())
            .unwrap();
        builder.finish().unwrap();
        let mut bytes = std::fs::read(&source).unwrap();
        bytes[1024 + 148..1024 + 156].fill(b'x');
        std::fs::write(&source, bytes).unwrap();
        let result = recover(&source, None, true).unwrap();
        let entry = result
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap();
        assert_eq!(
            std::fs::read(entry.file.as_ref().unwrap()).unwrap(),
            b"payload"
        );
        assert!(
            result
                .blocks
                .iter()
                .any(|block| block.issue == "TAR header checksum reconstructed")
        );
    }

    #[gpui::test]
    fn recovery_tar_export_runs_in_background_from_recovery_dialog(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("damaged.tar");
        let mut builder = tar::Builder::new(File::create(&source).unwrap());
        let mut header = tar::Header::new_old();
        header.set_size(7);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "notes.txt", b"payload".as_slice())
            .unwrap();
        builder.finish().unwrap();
        let mut before = std::fs::read(&source).unwrap();
        before[148..156].fill(b'x');
        std::fs::write(&source, &before).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.recovery.source = Some(source.clone());
            manager.show_modal(crate::Modal::Recovery, window, cx);
            manager
        });
        view.update(cx, |manager, cx| manager.start_recovery(cx));
        complete(cx, &view);
        assert!(cx.debug_bounds("export-recovery").is_some());
        let destination = root.path().join("fixed.tar");
        view.update(cx, |manager, cx| {
            let result = manager.recovery.result.clone().unwrap();
            assert!(result.entries.iter().any(|entry| entry.name == "notes.txt"));
            manager.export_recovered_archive(result, destination.clone(), cx);
        });
        complete(cx, &view);
        cx.read(|cx| assert!(view.read(cx).recovery.error.is_none()));
        let mut archive = tar::Archive::new(File::open(&destination).unwrap());
        let mut names = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            names.push(entry.path().unwrap().to_string_lossy().into_owned());
            std::io::copy(&mut entry, &mut std::io::sink()).unwrap();
        }
        assert!(names.iter().any(|name| name.ends_with("notes.txt")));
        assert!(names.iter().any(|name| name == "recovery-report.json"));
        assert_eq!(std::fs::read(source).unwrap(), before);
    }

    #[test]
    #[ignore = "generates a damaged TAR and its repaired copy for manual testing"]
    fn generate_tar_repair_sample() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("38-tar-repair.tar");
        let output = directory.join("38-tar-repaired.tar");
        if !source.exists() {
            let mut builder = tar::Builder::new(std::io::Cursor::new(Vec::new()));
            for (name, data) in [
                (
                    "first.txt",
                    b"Intact payload with a damaged checksum.".as_slice(),
                ),
                ("broken.txt", b"This entry has a destroyed header."),
                ("last.txt", b"Partially retained last file payload."),
            ] {
                let mut header = tar::Header::new_old();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, data).unwrap();
            }
            let mut bytes = builder.into_inner().unwrap().into_inner();
            bytes[148..156].fill(b'x');
            bytes[1024..1536].fill(b'Z');
            bytes.truncate(2560 + 5);
            let mut staged = tempfile::NamedTempFile::new_in(&directory).unwrap();
            staged.write_all(&bytes).unwrap();
            staged.persist_noclobber(&source).unwrap();
        }
        let original = std::fs::read(&source).unwrap();
        let result = recover(&source, None, false).unwrap();
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.name == "first.txt" && entry.status == Status::Unverified)
        );
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.name == "last.txt" && entry.status == Status::Partial)
        );
        assert!(
            result
                .blocks
                .iter()
                .any(|block| block.issue == "TAR header checksum reconstructed")
        );
        if !output.exists() {
            result.export_tar(&output).unwrap();
        }
        let entries = crate::archive::list_archive(&output).unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == "recovery-report.json")
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.name.ends_with("first.txt"))
        );
        assert!(entries.iter().any(|entry| entry.name.ends_with("last.txt")));
        assert_eq!(std::fs::read(&source).unwrap(), original);
        println!("Damaged TAR: {}", source.display());
        println!("Repaired TAR: {}", output.display());
        for entry in &result.entries {
            println!(
                "{}: {} ({} bytes)",
                entry.name,
                entry.status.label(),
                entry.bytes
            );
        }
    }

    #[test]
    fn recovery_salvages_tar_with_damaged_header_checksum() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("damaged.tar");
        let mut builder = tar::Builder::new(File::create(&path).unwrap());
        let mut header = tar::Header::new_ustar();
        header.set_size(7);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "notes.txt", b"payload".as_slice())
            .unwrap();
        builder.finish().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[148..156].fill(b'0');
        std::fs::write(&path, bytes).unwrap();
        let result = recover(&path, None, true).unwrap();
        assert!(
            result
                .blocks
                .iter()
                .any(|block| block.issue == "TAR header checksum reconstructed")
        );
        let entry = result
            .entries
            .iter()
            .find(|entry| entry.name == "notes.txt")
            .unwrap();
        assert_eq!(entry.status, Status::Unverified);
        assert_eq!(
            std::fs::read(entry.file.as_ref().unwrap()).unwrap(),
            b"payload"
        );
    }

    #[test]
    fn recovery_salvage_finds_embedded_image_without_an_archive_directory() {
        let root = tempfile::tempdir().unwrap();
        let mut image = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut image, image::ImageFormat::Png)
            .unwrap();
        let path = root.path().join("unreadable.bin");
        let mut bytes = b"damaged prefix with no archive header".to_vec();
        bytes.extend_from_slice(image.get_ref());
        std::fs::write(&path, &bytes).unwrap();
        assert!(recover(&path, None, false).unwrap().entries.is_empty());
        let result = recover(&path, None, true).unwrap();
        let entry = result
            .entries
            .iter()
            .find(|entry| entry.status == Status::Candidate)
            .unwrap();
        assert_eq!(
            std::fs::read(entry.file.as_ref().unwrap()).unwrap(),
            *image.get_ref()
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn recovery_7z_password_failures_and_missing_volumes_have_specific_diagnostics() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("private.txt");
        std::fs::write(&data, b"private recovery payload".repeat(16)).unwrap();
        for headers in [false, true] {
            let path = root.path().join(format!("encrypted-{headers}.7z"));
            crate::archive::create_archive_with_password(
                &path,
                std::slice::from_ref(&data),
                crate::archive::CreateOptions {
                    format: crate::archive::ResolvedFormat::SevenZip,
                    encrypt_headers: headers,
                    dictionary_mib: 1,
                    ..Default::default()
                },
                Some("test-recovery-secret"),
            )
            .unwrap();
            let missing = recover(&path, None, false).unwrap();
            assert!(
                missing
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.reason == Reason::Password)
            );
            let wrong = recover(&path, Some("wrong-secret"), false).unwrap();
            assert!(
                !wrong
                    .entries
                    .iter()
                    .any(|entry| entry.status == Status::Recovered)
            );
            assert!(!wrong.json().to_string().contains("wrong-secret"));
            let valid = recover(&path, Some("test-recovery-secret"), false).unwrap();
            assert!(valid.entries.iter().any(|entry| entry.name == "private.txt"
                && entry.file.is_some()
                && matches!(entry.status, Status::Recovered | Status::Unverified)));
            assert!(!valid.json().to_string().contains("test-recovery-secret"));
        }
        let split = root.path().join("split.7z");
        crate::archive::create_archive(
            &split,
            &[data],
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::SevenZip,
                volume_size: Some(31),
                dictionary_mib: 1,
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::remove_file(crate::volumes::part_path(&split, 2)).unwrap();
        let result = recover(&crate::volumes::part_path(&split, 1), None, false).unwrap();
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.reason == Reason::MissingVolume)
        );
    }

    #[test]
    fn recovery_7z_rejects_oversized_header_before_decoding_metadata() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("oversized.7z");
        let mut header = [0u8; 32];
        header[..6].copy_from_slice(b"7z\xbc\xaf\x27\x1c");
        header[20..28].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&path, header).unwrap();
        let result = recover(&path, None, false).unwrap();
        assert!(result.limited);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.reason == Reason::Limit)
        );
    }

    #[gpui::test]
    fn recovery_parity_option_is_available_for_rar_and_recorded_in_results(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.rar");
        let bytes = crate::rar_recovery::stored_fixture(&[("file.txt", b"parity option fixture")]);
        std::fs::write(&path, &bytes).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.recovery.source = Some(path.clone());
            manager.show_modal(crate::Modal::Recovery, window, cx);
            manager
        });
        for _ in 0..3 {
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        }
        let bounds = cx.debug_bounds("recovery-parity").unwrap();
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.read(|cx| assert!(view.read(cx).recovery.parity));
        view.update(cx, |manager, cx| manager.start_recovery(cx));
        complete(cx, &view);
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(
                manager.recovery.error.is_none(),
                "{:?}",
                manager.recovery.error
            );
            let result = manager.recovery.result.as_ref().unwrap();
            assert!(result.json()["parity_reconstruction"].is_object());
            assert_eq!(result.source, path);
            assert!(result.entries.iter().any(|entry| entry.file.is_some()));
        });
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        view.update(cx, |manager, cx| {
            manager.recovery.source = Some(root.path().join("source.7z"));
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("recovery-parity").is_none());
    }

    #[gpui::test]
    fn recovery_interface_shows_specific_causes_for_rar_and_7z(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let rar = root.path().join("damaged.rar");
        let mut bytes =
            crate::rar_recovery::stored_fixture(&[("bad.txt", b"original RAR content")]);
        let offset = memchr::memmem::find(&bytes, b"original RAR content").unwrap();
        bytes[offset] ^= 1;
        std::fs::write(&rar, bytes).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.language = crate::i18n::Language::Polish;
            manager.recovery.source = Some(rar.clone());
            manager.show_modal(crate::Modal::Recovery, window, cx);
            manager
        });
        view.update(cx, |manager, cx| manager.start_recovery(cx));
        complete(cx, &view);
        assert!(cx.debug_bounds("recovery-cause-0").is_some());
        cx.read(|cx| {
            let manager = view.read(cx);
            let report = manager.recovery.result.as_ref().unwrap();
            assert_eq!(report.entries[0].status, Status::Partial);
            assert!(
                report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.reason == Reason::Checksum)
            );
        });
        let seven = root.path().join("unreadable.7z");
        std::fs::write(&seven, b"7z\xbc\xaf\x27\x1c truncated header").unwrap();
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.clear_recovery(window, cx);
                manager.recovery.source = Some(seven.clone());
                manager.start_recovery(cx);
            })
        });
        complete(cx, &view);
        assert!(cx.debug_bounds("recovery-cause-archive").is_some());
        cx.read(|cx| {
            let manager = view.read(cx);
            let report = manager.recovery.result.as_ref().unwrap();
            assert!(
                report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.entry.is_none())
            );
        });
    }

    #[test]
    fn recovery_rar_recovers_after_crc_failure_with_specific_causes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("damaged.rar");
        let mut bytes = crate::rar_recovery::stored_fixture(&[
            ("first.txt", b"first payload"),
            ("bad.txt", b"damaged payload"),
            ("last.txt", b"last payload"),
        ]);
        let damaged = memchr::memmem::find(&bytes, b"damaged payload").unwrap();
        bytes[damaged] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let result = recover(&path, None, false).unwrap();
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.entries[0].status, Status::Recovered);
        assert_eq!(result.entries[1].status, Status::Partial);
        assert_eq!(result.entries[2].status, Status::Recovered);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.entry == Some(1)
                    && diagnostic.reason == Reason::Checksum)
        );
        let output = root.path().join("recovered.zip");
        result.export_zip(&output).unwrap();
        let mut zip = zip::ZipArchive::new(File::open(output).unwrap()).unwrap();
        assert!(zip.by_name("partial/00002-bad.txt").is_ok());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn recovery_rar_explains_damaged_headers_without_claiming_password_failure() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("damaged-header.rar");
        let mut bytes = crate::rar_recovery::stored_fixture(&[("file.txt", b"data")]);
        bytes[20] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        let result = recover(&path, None, true).unwrap();
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.reason == Reason::Header)
        );
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.reason == Reason::Password)
        );
    }

    #[test]
    fn recovery_7z_continues_after_corrupt_independent_block() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("independent.7z");
        let mut writer = sevenz_rust::SevenZWriter::create(&path).unwrap();
        writer.set_content_methods(vec![sevenz_rust::SevenZMethodConfiguration::new(
            sevenz_rust::SevenZMethod::LZMA2,
        )]);
        for name in ["first.txt", "middle.txt", "last.txt"] {
            let source = root.path().join(name);
            std::fs::write(&source, format!("payload for {name}")).unwrap();
            writer
                .push_archive_entry(
                    sevenz_rust::SevenZArchiveEntry::from_path(&source, name.to_owned()),
                    Some(File::open(&source).unwrap()),
                )
                .unwrap();
        }
        writer.finish().unwrap();
        let reader =
            sevenz_rust::SevenZReader::open(&path, sevenz_rust::Password::empty()).unwrap();
        assert_eq!(reader.archive().folders.len(), 3);
        let (offset, _) = seven_zip_range(reader.archive(), 1).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[offset as usize + 3] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let result = recover(&path, None, false).unwrap();
        for name in ["first.txt", "last.txt"] {
            let entry = result
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap();
            assert_eq!(entry.status, Status::Recovered, "{name}");
            assert_eq!(
                std::fs::read_to_string(entry.file.as_ref().unwrap()).unwrap(),
                format!("payload for {name}")
            );
        }
        let middle = result
            .entries
            .iter()
            .position(|entry| entry.name == "middle.txt")
            .unwrap();
        assert!(matches!(
            result.entries[middle].status,
            Status::Partial | Status::NotRecovered
        ));
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.entry == Some(middle)
                    && matches!(
                        diagnostic.reason,
                        Reason::Checksum | Reason::Read | Reason::Truncated
                    ))
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn recovery_7z_reports_files_blocked_by_solid_stream_corruption() {
        let root = tempfile::tempdir().unwrap();
        let paths = [
            root.path().join("first.txt"),
            root.path().join("second.txt"),
        ];
        for path in &paths {
            std::fs::write(path, b"solid recovery fixture payload".repeat(32)).unwrap();
        }
        let path = root.path().join("solid.7z");
        crate::archive::create_archive(
            &path,
            &paths,
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::SevenZip,
                solid: true,
                dictionary_mib: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let reader =
            sevenz_rust::SevenZReader::open(&path, sevenz_rust::Password::empty()).unwrap();
        let (offset, _) = seven_zip_range(reader.archive(), 0).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[offset as usize] = 0x03;
        std::fs::write(&path, bytes).unwrap();
        let result = recover(&path, None, false).unwrap();
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.name == "second.txt" && entry.status == Status::NotRecovered),
            "{:?}",
            result
                .entries
                .iter()
                .map(|entry| (&entry.name, entry.status))
                .collect::<Vec<_>>()
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.reason == Reason::SolidBlock)
        );
    }

    #[test]
    fn recovery_7z_repairs_only_start_crc_when_next_header_is_intact() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("notes.txt");
        std::fs::write(&data, b"header recovery payload").unwrap();
        let path = root.path().join("header.7z");
        crate::archive::create_archive(
            &path,
            &[data],
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::SevenZip,
                ..Default::default()
            },
        )
        .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[8] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let result = recover(&path, None, false).unwrap();
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.name == "notes.txt" && entry.status == Status::Recovered)
        );
        assert!(
            result
                .notes
                .iter()
                .any(|note| note.starts_with("7z start header checksum reconstructed"))
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&path, bytes).unwrap();
        let result = recover(&path, None, false).unwrap();
        assert!(
            !result
                .notes
                .iter()
                .any(|note| note.starts_with("7z start header checksum reconstructed"))
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.reason == Reason::Header)
        );
    }

    #[test]
    fn recovery_reads_native_7z_without_claiming_unchecked_data_is_verified() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.txt");
        std::fs::write(&source, b"native recovery").unwrap();
        let path = root.path().join("source.7z");
        crate::archive::create_archive(
            &path,
            &[source],
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::SevenZip,
                ..Default::default()
            },
        )
        .unwrap();
        let result = recover(&path, None, false).unwrap();
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.name == "payload.txt" && entry.bytes == 15)
        );
    }

    #[test]
    fn recovery_bounds_untrusted_zip_directory_before_allocating_metadata() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("metadata.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer
            .start_file("file.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"metadata recovery").unwrap();
        writer.finish().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let end = memchr::memmem::rfind(&bytes, b"PK\x05\x06").unwrap();
        bytes[end + 8..end + 12].copy_from_slice(&[0xff; 4]);
        bytes[end + 12..end + 16].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let result = recover(&path, None, true).unwrap();
        assert!(result.limited);
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.name == "file.txt" && entry.status == Status::Recovered)
        );
    }

    #[test]
    fn recovery_reader_enforces_metadata_phase_budget() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input.bin");
        std::fs::write(&path, b"bounded metadata input").unwrap();
        let mut recovery = Recovery::new(&path, false).unwrap();
        let source = Source::open(&path, &mut recovery).unwrap();
        source.metadata_remaining.store(3, Ordering::Relaxed);
        let mut reader = source.reader();
        let mut bytes = [0u8; 3];
        reader.read_exact(&mut bytes).unwrap();
        assert!(reader.read(&mut bytes).is_err());
        source.metadata_remaining.store(u64::MAX, Ordering::Relaxed);
        assert!(reader.read(&mut bytes).unwrap() > 0);
    }

    #[gpui::test]
    fn recovery_closed_before_completion_does_not_restore_stale_results(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.zip");
        let data = root.path().join("notes.txt");
        std::fs::write(&data, b"cancel recovery fixture").unwrap();
        crate::archive::create_zip(&source, &[data]).unwrap();
        let before = std::fs::read(&source).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.recovery.source = Some(source.clone());
            manager.show_modal(crate::Modal::Recovery, window, cx);
            manager
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.start_recovery(cx);
                manager.close_modal(window, cx);
            })
        });
        complete(cx, &view);
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.modal.is_none());
            assert!(manager.recovery.result.is_none());
            assert!(manager.recovery.task.is_none());
        });
        assert_eq!(std::fs::read(source).unwrap(), before);
    }

    #[test]
    fn recovery_runs_on_platform_worker_stack() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("notes.txt");
        std::fs::write(&data, b"small-stack recovery fixture").unwrap();
        let mut paths = Vec::new();
        for format in [
            crate::archive::ResolvedFormat::Zip,
            crate::archive::ResolvedFormat::SevenZip,
            crate::archive::ResolvedFormat::Tar,
        ] {
            let path = root.path().join(format!("source.{}", format.extension()));
            crate::archive::create_archive(
                &path,
                std::slice::from_ref(&data),
                crate::archive::CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    ..Default::default()
                },
            )
            .unwrap();
            paths.push(path);
        }
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                for path in paths {
                    let result = recover(&path, None, true).unwrap();
                    assert!(result.entries.iter().any(|entry| entry.bytes == 28));
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
