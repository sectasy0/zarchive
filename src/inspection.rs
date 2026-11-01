//! Reads archive metadata, verifies contents, and reports bounded security findings.

use crate::archive::{self, PasswordFailure, ResolvedFormat};

/// Wraps an input reader so cancellation is checked before each read and seek.
struct CancellableReader<'a, R> {
    inner: R,
    cancel: &'a AtomicBool,
}

impl<R: Read> Read for CancellableReader<'_, R> {
    /// Reads bytes unless the inspection has been cancelled.
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        crate::task_queue::checkpoint()?;
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("Inspection cancelled"));
        }
        let count = self.inner.read(buffer)?;
        crate::task_queue::report_bytes(count as u64);
        Ok(count)
    }
}

impl<R: Seek> Seek for CancellableReader<'_, R> {
    /// Seeks unless the inspection has been cancelled.
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        crate::task_queue::checkpoint()?;
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("Inspection cancelled"));
        }
        self.inner.seek(position)
    }
}
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Seek},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_ENTRIES: usize = 20_000;
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FINDINGS: usize = 500;
const MAX_SEVENZIP_DICTIONARY_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Selects metadata preview, content verification, or security auditing.
pub enum InspectionMode {
    /// Reads metadata without consuming entry contents.
    Preview,
    /// Reads entries to verify sizes, stored checksums, and content hashes.
    Integrity,
    /// Performs integrity checks and reports suspicious archive metadata and content.
    Audit,
    /// Performs an audit without entry-count, entry-size, or total-read limits.
    FullAudit,
}

impl InspectionMode {
    pub(crate) fn is_audit(self) -> bool {
        matches!(self, Self::Audit | Self::FullAudit)
    }

    pub(crate) fn is_full_audit(self) -> bool {
        self == Self::FullAudit
    }

    fn max_entries(self) -> usize {
        if self.is_full_audit() {
            usize::MAX
        } else {
            MAX_ENTRIES
        }
    }

    fn max_entry_bytes(self) -> u64 {
        if self.is_full_audit() {
            u64::MAX
        } else {
            MAX_ENTRY_BYTES
        }
    }

    fn max_total_bytes(self) -> u64 {
        if self.is_full_audit() {
            u64::MAX
        } else {
            MAX_TOTAL_BYTES
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Summarizes the outcome of an integrity check.
pub enum Integrity {
    /// No content verification was requested.
    NotTested,
    /// All inspected content passed validation.
    Passed,
    /// Corrupt or inconsistent content was detected.
    Failed,
    /// Limits or cancellation prevented a complete check.
    Incomplete,
    /// The archive requires a password that was not supplied.
    PasswordRequired,
}

#[derive(Clone, Debug)]
/// Metadata and computed validation results for one archive entry.
pub struct EntryInfo {
    /// Name stored in the archive.
    pub name: String,
    /// Whether this entry represents a directory.
    pub directory: bool,
    /// Uncompressed size reported or read from the archive.
    pub size: u64,
    /// Compressed size, when the format provides an attributable value.
    pub compressed_size: Option<u64>,
    /// Compression method or methods reported for this entry.
    pub method: String,
    /// Modification timestamp, when available.
    pub modified: Option<String>,
    /// Platform attributes, when available.
    pub attributes: Option<String>,
    /// Numeric owner identifier, when the format records one.
    pub owner_id: Option<u64>,
    /// Numeric group identifier, when the format records one.
    pub group_id: Option<u64>,
    /// Entry comment, when the format records one.
    pub comment: Option<String>,
    /// Encoded format-specific extra fields, when available.
    pub extra_fields: Option<String>,
    /// Link target, when the format records one.
    pub link_target: Option<String>,
    /// CRC stored in the archive, when present and meaningful.
    pub stored_crc: Option<u32>,
    /// CRC computed while reading the entry.
    pub computed_crc: Option<u32>,
    /// SHA-256 computed while reading the entry.
    pub sha256: Option<String>,
    /// Whether the entry is encrypted.
    pub encrypted: bool,
    /// Whether the entry is a symbolic link or similar link type.
    pub link: bool,
    /// Entry-specific read or validation failure.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// One actionable observation found during archive inspection.
pub struct Finding {
    /// Stable category name for this finding.
    pub kind: &'static str,
    /// Archive entry associated with the finding, if any.
    pub entry: String,
    /// Human-readable finding details.
    pub detail: String,
}

/// Complete metadata, integrity, and audit output for an archive inspection.
pub struct Report {
    /// Path to the inspected archive.
    pub path: std::path::PathBuf,
    /// Format identified from the archive signature.
    pub detected: Option<ResolvedFormat>,
    /// Detected format name or a recognized non-archive signature.
    pub signature: String,
    /// Format inferred from the file extension.
    pub expected: Option<ResolvedFormat>,
    /// Compressed size of the archive file.
    pub archive_size: u64,
    /// Archive creation timestamp, when available.
    pub created: Option<String>,
    /// Archive modification timestamp, when available.
    pub modified: Option<String>,
    /// SHA-256 of the archive file, when calculated.
    pub archive_sha256: Option<String>,
    /// Metadata and verification results for each inspected entry.
    pub entries: Vec<EntryInfo>,
    /// Findings produced by integrity and audit checks.
    pub findings: Vec<Finding>,
    /// Overall integrity outcome.
    pub integrity: Integrity,
    /// Whether any configured inspection limit was reached.
    pub limited: bool,
    /// Top-level error encountered while inspecting the archive.
    pub error: Option<String>,
    full_audit: bool,
}

impl Report {
    /// Serializes the report and active inspection limits to JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path, "detected_format": self.signature, "expected_format": self.expected.map(|format| format.label()),
            "archive_size": self.archive_size, "created": self.created, "modified": self.modified,
            "uncompressed_size": self.unpacked_size(), "compression_ratio": self.ratio(),
            "compression_level": null, "archive_sha256": self.archive_sha256,
            "integrity": format!("{:?}", self.integrity), "limited": self.limited, "error": self.error,
            "entries": self.entries.iter().map(|entry| serde_json::json!({
                "name": entry.name, "directory": entry.directory, "size": entry.size, "compressed_size": entry.compressed_size,
                "method": entry.method, "modified": entry.modified, "attributes": entry.attributes,
                "owner_id": entry.owner_id, "group_id": entry.group_id, "comment": entry.comment,
                "extra_fields": entry.extra_fields, "link_target": entry.link_target,
                "stored_crc32": entry.stored_crc.map(|crc| format!("{crc:08x}")), "computed_crc32": entry.computed_crc.map(|crc| format!("{crc:08x}")),
                "sha256": entry.sha256, "encrypted": entry.encrypted, "link": entry.link, "error": entry.error,
            })).collect::<Vec<_>>(),
            "findings": self.findings.iter().map(|finding| serde_json::json!({"kind": finding.kind, "entry": finding.entry, "detail": finding.detail})).collect::<Vec<_>>(),
            "full_audit": self.full_audit,
            "limits": {
                "max_entries": (!self.full_audit).then_some(MAX_ENTRIES),
                "max_entry_bytes": (!self.full_audit).then_some(MAX_ENTRY_BYTES),
                "max_total_bytes": (!self.full_audit).then_some(MAX_TOTAL_BYTES),
                "max_findings": (!self.full_audit).then_some(MAX_FINDINGS),
                "max_sevenz_dictionary_bytes": MAX_SEVENZIP_DICTIONARY_BYTES,
            },
        })
    }
    /// Sums reported uncompressed entry sizes without overflowing.
    pub fn unpacked_size(&self) -> u64 {
        self.entries
            .iter()
            .fold(0_u64, |size, entry| size.saturating_add(entry.size))
    }
    /// Returns the uncompressed-to-archive size ratio when the archive is nonempty.
    pub fn ratio(&self) -> Option<f64> {
        (self.archive_size > 0).then(|| self.unpacked_size() as f64 / self.archive_size as f64)
    }
    /// Adds a finding until the report's bounded finding limit is reached.
    fn finding(&mut self, kind: &'static str, entry: impl Into<String>, detail: impl Into<String>) {
        if self.full_audit || self.findings.len() < MAX_FINDINGS {
            self.findings.push(Finding {
                kind,
                entry: entry.into(),
                detail: detail.into(),
            });
        } else {
            self.limited = true;
        }
    }
}

/// Infers the expected format from the archive filename extension.
fn expected_format(path: &Path) -> Option<ResolvedFormat> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".tar.bz2") || name.ends_with(".tbz") || name.ends_with(".tbz2") {
        Some(ResolvedFormat::TarBz2)
    } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        Some(ResolvedFormat::TarXz)
    } else if name.ends_with(".tar.zst") {
        Some(ResolvedFormat::TarZstd)
    } else if name.ends_with(".tar.lz4") {
        Some(ResolvedFormat::TarLz4)
    } else if name.ends_with(".tar.lz") {
        Some(ResolvedFormat::TarLzip)
    } else if name.ends_with(".tar.br") {
        Some(ResolvedFormat::TarBrotli)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(ResolvedFormat::TarGz)
    } else if name.ends_with(".gz") {
        Some(ResolvedFormat::Gzip)
    } else if name.ends_with(".bz2") {
        Some(ResolvedFormat::Bzip2)
    } else if name.ends_with(".xz") {
        Some(ResolvedFormat::Xz)
    } else if name.ends_with(".zst") {
        Some(ResolvedFormat::Zstandard)
    } else if name.ends_with(".lz4") {
        Some(ResolvedFormat::Lz4)
    } else if name.ends_with(".lz") {
        Some(ResolvedFormat::Lzip)
    } else if name.ends_with(".br") {
        Some(ResolvedFormat::Brotli)
    } else if name.ends_with(".zip") {
        Some(ResolvedFormat::Zip)
    } else if name.ends_with(".tar") {
        Some(ResolvedFormat::Tar)
    } else if name.ends_with(".7z") {
        Some(ResolvedFormat::SevenZip)
    } else if name.ends_with(".rar") {
        Some(ResolvedFormat::Rar)
    } else if name.ends_with(".cpio") {
        Some(ResolvedFormat::Cpio)
    } else if name.ends_with(".ar") || name.ends_with(".a") {
        Some(ResolvedFormat::Ar)
    } else if name.ends_with(".pax") {
        Some(ResolvedFormat::Pax)
    } else if name.ends_with(".xar") {
        Some(ResolvedFormat::Xar)
    } else {
        None
    }
}

/// Identifies a supported archive format from its leading signature bytes.
pub fn detect_format(header: &[u8]) -> Option<ResolvedFormat> {
    if header.starts_with(b"!<arch>\n") {
        Some(ResolvedFormat::Ar)
    } else if header.starts_with(b"xar!") {
        Some(ResolvedFormat::Xar)
    } else if header.starts_with(b"070701") || header.starts_with(b"070702")
        || header.starts_with(b"070707") || header.starts_with(&[0x71, 0xc7])
        || header.starts_with(&[0xc7, 0x71]) {
        Some(ResolvedFormat::Cpio)
    } else if header.starts_with(b"PK\x03\x04")
        || header.starts_with(b"PK\x05\x06")
        || header.starts_with(b"PK\x07\x08")
    {
        Some(ResolvedFormat::Zip)
    } else if header.starts_with(&[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]) {
        Some(ResolvedFormat::SevenZip)
    } else if header.starts_with(b"Rar!\x1a\x07\x00") || header.starts_with(b"Rar!\x1a\x07\x01\x00")
    {
        Some(ResolvedFormat::Rar)
    } else if header.starts_with(&[0x1f, 0x8b]) {
        Some(ResolvedFormat::Gzip)
    } else if header.starts_with(b"BZh") {
        Some(ResolvedFormat::Bzip2)
    } else if header.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Some(ResolvedFormat::Xz)
    } else if header.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Some(ResolvedFormat::Zstandard)
    } else if header.starts_with(&[0x04, 0x22, 0x4d, 0x18]) {
        Some(ResolvedFormat::Lz4)
    } else if header.starts_with(b"LZIP") {
        Some(ResolvedFormat::Lzip)
    } else if header.len() >= 512
        && (header.get(257..262) == Some(b"ustar") || {
            let expected = std::str::from_utf8(&header[148..156])
                .ok()
                .and_then(|text| u64::from_str_radix(text.trim_matches(['\0', ' ']), 8).ok());
            let actual: u64 = header[..512]
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    if (148..156).contains(&index) {
                        32
                    } else {
                        *value as u64
                    }
                })
                .sum();
            expected == Some(actual) && actual > 256
        })
    {
        Some(ResolvedFormat::Tar)
    } else {
        None
    }
}

/// Uses the filename to distinguish a compressed TAR stream from a standalone stream.
fn resolve_detected_format(
    detected: Option<ResolvedFormat>,
    expected: Option<ResolvedFormat>,
) -> Option<ResolvedFormat> {
    match (detected, expected) {
        (Some(ResolvedFormat::Tar), Some(ResolvedFormat::Pax)) => expected,
        (Some(ResolvedFormat::Gzip), Some(ResolvedFormat::TarGz)) => expected,
        (Some(ResolvedFormat::Bzip2), Some(ResolvedFormat::TarBz2)) => expected,
        (Some(ResolvedFormat::Xz), Some(ResolvedFormat::TarXz)) => expected,
        (Some(ResolvedFormat::Zstandard), Some(ResolvedFormat::TarZstd)) => expected,
        (Some(ResolvedFormat::Lz4), Some(ResolvedFormat::TarLz4)) => expected,
        (Some(ResolvedFormat::Lzip), Some(ResolvedFormat::TarLzip)) => expected,
        (None, Some(ResolvedFormat::TarBrotli | ResolvedFormat::Brotli)) => expected,
        (detected, _) => detected,
    }
}

/// Inspects an archive and converts failures into report state and findings.
pub fn inspect(
    path: &Path,
    password: Option<&str>,
    mode: InspectionMode,
    cancel: &AtomicBool,
) -> Report {
    let mut report = Report {
        path: path.to_path_buf(),
        detected: None,
        signature: "Unknown".into(),
        expected: expected_format(path),
        archive_size: 0,
        created: None,
        modified: None,
        archive_sha256: None,
        entries: Vec::new(),
        findings: Vec::new(),
        integrity: Integrity::NotTested,
        limited: false,
        error: None,
        full_audit: mode.is_full_audit(),
    };
    if let Err(error) = inspect_inner(&mut report, password, mode, cancel) {
        if matches!(error.downcast_ref::<crate::xar::ReadError>(), Some(crate::xar::ReadError::Limit)) {
            report.limited = true;
        }
        if error.downcast_ref::<PasswordFailure>().is_some() {
            report.integrity = Integrity::PasswordRequired;
        } else if matches!(
            error.downcast_ref::<sevenz_rust::Error>(),
            Some(sevenz_rust::Error::UnsupportedCompressionMethod(_))
        ) || matches!(error.downcast_ref::<crate::xar::ReadError>(), Some(crate::xar::ReadError::Unsupported(_))) {
            report.integrity = Integrity::Incomplete;
            report.finding("Unsupported compression method", "", error.to_string());
        } else if report.limited || cancel.load(Ordering::Relaxed) {
            report.integrity = Integrity::Incomplete;
        } else {
            report.integrity = Integrity::Failed;
            report.finding("Damaged archive", "", error.to_string());
        }
        report.error = Some(format!("{error:#}"));
    }
    report
}

/// Reads archive metadata and, depending on mode, verifies contents and audit rules.
fn inspect_inner(
    report: &mut Report,
    password: Option<&str>,
    mode: InspectionMode,
    cancel: &AtomicBool,
) -> Result<()> {
    let file = File::open(&report.path)?;
    let metadata = file.metadata()?;
    report.archive_size = metadata.len();
    report.created = metadata.created().ok().map(format_system_time);
    report.modified = metadata.modified().ok().map(format_system_time);
    let mut file = CancellableReader {
        inner: file,
        cancel,
    };
    let mut header = [0_u8; 512];
    let count = file.read(&mut header)?;
    report.detected = resolve_detected_format(detect_format(&header[..count]), report.expected);
    if report.detected.is_none()
        && matches!(report.expected, Some(ResolvedFormat::Tar | ResolvedFormat::Pax))
        && count == 512
        && header.iter().all(|byte| *byte == 0)
        && report.archive_size >= 1024
    {
        report.detected = report.expected;
    }
    report.signature = report
        .detected
        .map(|format| format.label())
        .unwrap_or_else(|| {
            let bytes = &header[..count];
            if bytes.starts_with(b"%PDF-") {
                "PDF"
            } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                "PNG"
            } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
                "JPEG"
            } else if bytes.starts_with(b"Rar!") {
                "RAR"
            } else if bytes.starts_with(b"MZ") {
                "PE"
            } else if bytes.starts_with(b"\x7fELF") {
                "ELF"
            } else {
                "Unknown"
            }
        })
        .to_owned();
    if report.detected != report.expected {
        report.finding(
            "Format mismatch",
            "",
            "File signature does not match its extension",
        );
    }
    let format = report.detected.context("Unrecognized archive signature")?;
    anyhow::ensure!(
        format.capabilities().can_test,
        "This format does not support archive inspection"
    );
    file.rewind()?;
    if mode != InspectionMode::Preview && report.archive_size <= mode.max_total_bytes() {
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 65536];
        loop {
            anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Inspection cancelled");
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        report.archive_sha256 = Some(format!("{:x}", hash.finalize()));
        file.rewind()?;
    }
    let mut budget = mode.max_total_bytes();
    match format {
        ResolvedFormat::Cpio | ResolvedFormat::Ar | ResolvedFormat::Pax => {
            let mut reader = libarchive2::ReadArchive::open(&report.path)?;
            loop {
                anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Inspection cancelled");
                if report.entries.len() >= mode.max_entries() { report.limited = true; break; }
                let Some(entry) = reader.next_entry()? else { break; };
                let regular = entry.file_type() == libarchive2::FileType::RegularFile;
                let mut info = EntryInfo {
                    name: entry.pathname().context("Archive entry has no filename")?,
                    directory: entry.file_type() == libarchive2::FileType::Directory,
                    size: u64::try_from(entry.size()).context("Invalid archive entry size")?,
                    compressed_size: Some(u64::try_from(entry.size())?),
                    method: "Stored".into(),
                    modified: entry.mtime().map(format_system_time),
                    attributes: Some(format!("{:o}", entry.mode())),
                    owner_id: None, group_id: None, comment: None, extra_fields: None,
                    link_target: entry.symlink().or_else(|| entry.hardlink()),
                    stored_crc: None, computed_crc: None, sha256: None,
                    encrypted: entry.is_encrypted(),
                    link: entry.symlink().is_some() || entry.hardlink().is_some()
                        || (!regular && entry.file_type() != libarchive2::FileType::Directory),
                    error: None,
                };
                if info.size > mode.max_entry_bytes() || info.size > budget { report.limited = true; }
                if !report.limited && regular && !info.link {
                    if mode == InspectionMode::Preview { budget -= info.size; }
                    else {
                        let expected = info.size;
                        read_entry(CancellableReader { inner: archive::ExtendedEntryReader(&mut reader), cancel }, &mut info, Some(expected), report, mode, cancel, &mut budget);
                    }
                }
                report.entries.push(info);
                if report.limited { break; }
            }
        }
        ResolvedFormat::Xar => {
            let mut reader = crate::xar::XarReader::new(file)?;
            for entry in reader.entries.clone() {
                anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Inspection cancelled");
                if report.entries.len() >= mode.max_entries() { report.limited = true; break; }
                let mut info = EntryInfo {
                    name: entry.path.clone(),
                    directory: entry.file_type == xara::XarFileType::Directory,
                    size: entry.data.as_ref().map_or(0, |data| data.size),
                    compressed_size: entry.data.as_ref().map(|data| data.length),
                    method: entry.data.as_ref().map_or_else(|| "Stored".into(), |data| data.encoding.clone()),
                    modified: None, attributes: None, stored_crc: None, computed_crc: None,
                    owner_id: None, group_id: None, comment: None, extra_fields: None,
                    link_target: entry.link.clone(),
                    sha256: None, encrypted: false,
                    link: entry.file_type == xara::XarFileType::Symlink || entry.link.is_some(),
                    error: None,
                };
                if info.size > mode.max_entry_bytes() || info.size > budget { report.limited = true; }
                if !report.limited && !info.directory && !info.link {
                    if mode == InspectionMode::Preview { budget -= info.size; }
                    else {
                        let expected = info.size;
                        read_entry(reader.entry_reader(&entry)?, &mut info, Some(expected), report, mode, cancel, &mut budget);
                    }
                }
                report.entries.push(info);
                if report.limited { break; }
            }
        }
        ResolvedFormat::Zip => {
            let mut archive = zip::ZipArchive::new(file)?;
            for index in 0..archive.len() {
                if report.entries.len() >= mode.max_entries() {
                    report.limited = true;
                    break;
                }
                let raw = archive.by_index_raw(index)?;
                let supported = matches!(
                    raw.compression(),
                    zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
                );
                let mut info = EntryInfo {
                    name: raw.name()?.into_owned(),
                    directory: raw.is_dir(),
                    size: raw.size(),
                    compressed_size: Some(raw.compressed_size()),
                    method: format!("{:?}", raw.compression()),
                    modified: raw.last_modified().map(|value| value.to_string()),
                    attributes: raw.unix_mode().map(|mode| format!("{mode:o}")),
                    owner_id: None,
                    group_id: None,
                    comment: (!raw.comment().is_empty()).then(|| raw.comment().to_owned()),
                    extra_fields: raw.extra_data().map(|bytes| {
                        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
                    }),
                    link_target: None,
                    stored_crc: (!raw.encrypted() || raw.crc32() != 0).then_some(raw.crc32()),
                    computed_crc: None,
                    sha256: None,
                    encrypted: raw.encrypted(),
                    link: raw
                        .unix_mode()
                        .is_some_and(|mode| mode & 0o170000 == 0o120000),
                    error: None,
                };
                drop(raw);
                if mode != InspectionMode::Preview && !info.directory {
                    if !supported {
                        report.limited = true;
                        report.finding("Unsupported compression method", &info.name, &info.method);
                    } else if info.encrypted && password.is_none() {
                        report.integrity = Integrity::PasswordRequired;
                    } else {
                        let result = if let Some(password) = password {
                            archive.by_index_decrypt(index, password.as_bytes())
                        } else {
                            archive.by_index(index)
                        };
                        match result {
                            Ok(mut reader) => {
                                let expected_size = info.size;
                                read_entry(
                                    &mut reader,
                                    &mut info,
                                    Some(expected_size),
                                    report,
                                    mode,
                                    cancel,
                                    &mut budget,
                                )
                            }
                            Err(error) => info.error = Some(error.to_string()),
                        }
                    }
                }
                report.entries.push(info);
            }
        }
        ResolvedFormat::Gzip
        | ResolvedFormat::Bzip2
        | ResolvedFormat::Xz
        | ResolvedFormat::Zstandard
        | ResolvedFormat::Lz4
        | ResolvedFormat::Lzip
        | ResolvedFormat::Brotli => {
            let name = archive::single_compressed_entry_name(&report.path)?;
            let mut info = EntryInfo {
                name,
                directory: false,
                size: 0,
                compressed_size: Some(report.archive_size),
                method: format.label().to_owned(),
                modified: None,
                attributes: None,
                owner_id: None,
                group_id: None,
                comment: None,
                extra_fields: None,
                link_target: None,
                stored_crc: None,
                computed_crc: None,
                sha256: None,
                encrypted: false,
                link: false,
                error: None,
            };
            let reader = archive::decode_archive_stream(file, format)?;
            if mode == InspectionMode::Preview {
                measure_entry(reader, &mut info, report, cancel, &mut budget)?;
            } else {
                read_entry(reader, &mut info, None, report, mode, cancel, &mut budget);
            }
            report.entries.push(info);
        }
        ResolvedFormat::Tar
        | ResolvedFormat::TarGz
        | ResolvedFormat::TarBz2
        | ResolvedFormat::TarXz
        | ResolvedFormat::TarZstd
        | ResolvedFormat::TarLz4
        | ResolvedFormat::TarLzip
        | ResolvedFormat::TarBrotli => {
            let reader = archive::decode_archive_stream(file, format)?;
            let mut archive = tar::Archive::new(reader);
            for entry in archive.entries()? {
                if report.entries.len() >= mode.max_entries() {
                    report.limited = true;
                    break;
                }
                let mut entry = entry?;
                let header = entry.header();
                let mut info = EntryInfo {
                    name: entry.path()?.to_string_lossy().into_owned(),
                    directory: header.entry_type().is_dir(),
                    size: header.size()?,
                    compressed_size: (format == ResolvedFormat::Tar).then_some(header.size()?),
                    method: match format {
                        ResolvedFormat::Tar => "Stored",
                        ResolvedFormat::TarGz => "GZIP",
                        ResolvedFormat::TarBz2 => "BZIP2",
                        ResolvedFormat::TarXz => "XZ",
                        ResolvedFormat::TarZstd => "Zstandard",
                        ResolvedFormat::TarLz4 => "LZ4",
                        ResolvedFormat::TarLzip => "LZIP",
                        ResolvedFormat::TarBrotli => "Brotli",
                        _ => unreachable!("only TAR formats reach this match"),
                    }
                    .into(),
                    modified: header.mtime().ok().map(|value| format!("Unix {value}")),
                    attributes: header.mode().ok().map(|mode| format!("{mode:o}")),
                    owner_id: header.uid().ok(),
                    group_id: header.gid().ok(),
                    comment: None,
                    extra_fields: None,
                    link_target: header.link_name().ok().flatten().map(|path| path.to_string_lossy().into_owned()),
                    stored_crc: None,
                    computed_crc: None,
                    sha256: None,
                    encrypted: false,
                    link: header.entry_type().is_symlink() || header.entry_type().is_hard_link(),
                    error: None,
                };
                if mode != InspectionMode::Preview && header.entry_type().is_file() {
                    let expected_size = info.size;
                    read_entry(
                        &mut entry,
                        &mut info,
                        Some(expected_size),
                        report,
                        mode,
                        cancel,
                        &mut budget,
                    );
                }
                report.entries.push(info);
                if mode == InspectionMode::Preview {
                    let size = report.entries.last().unwrap().size;
                    if size > mode.max_entry_bytes() || size > budget {
                        report.limited = true;
                    } else {
                        budget -= size;
                    }
                }
                if report.limited {
                    break;
                }
            }
            if mode != InspectionMode::Preview && !report.limited {
                let mut reader = archive.into_inner();
                let mut buffer = [0_u8; 65536];
                loop {
                    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Inspection cancelled");
                    let count = reader.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    if count as u64 > budget {
                        report.limited = true;
                        break;
                    }
                    budget -= count as u64;
                }
            }
        }
        ResolvedFormat::SevenZip => {
            let length = report.archive_size;
            let mut archive = sevenz_rust::SevenZReader::new(
                file,
                length,
                password
                    .map(sevenz_rust::Password::from)
                    .unwrap_or_default(),
            )
            .map_err(|error| {
                if matches!(error, sevenz_rust::Error::PasswordRequired) {
                    anyhow::Error::from(PasswordFailure::Required)
                } else {
                    error.into()
                }
            })?;
            let methods = archive
                .archive()
                .folders
                .iter()
                .map(|folder| {
                    folder
                        .coders
                        .iter()
                        .map(|coder| match coder.decompression_method_id() {
                            [0x21] => "LZMA2".to_owned(),
                            [3, 1, 1] => "LZMA".to_owned(),
                            [0] => "Stored".to_owned(),
                            [6, 0xf1, 7, 1] => "AES-256".to_owned(),
                            method => format!("{method:02x?}"),
                        })
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .collect::<Vec<_>>();
            let encrypted = archive
                .archive()
                .folders
                .iter()
                .flat_map(|folder| &folder.coders)
                .any(|coder| coder.decompression_method_id() == [6, 0xf1, 7, 1]);
            let oversized_dictionary = archive
                .archive()
                .folders
                .iter()
                .flat_map(|folder| &folder.coders)
                .any(|coder| {
                    let dictionary = match coder.decompression_method_id() {
                        [0x21] => coder.properties.first().map(|property| {
                            if *property >= 40 {
                                u64::MAX
                            } else {
                                (2_u64 | (*property as u64 & 1)) << (*property as u32 / 2 + 11)
                            }
                        }),
                        [3, 1, 1] if coder.properties.len() >= 5 => Some(u32::from_le_bytes(
                            coder.properties[1..5].try_into().unwrap(),
                        )
                            as u64),
                        _ => None,
                    };
                    dictionary.is_some_and(|size| size > 128 * 1024 * 1024)
                });
            if oversized_dictionary {
                report.limited = true;
                report.finding(
                    "Suspicious size",
                    "",
                    "Compression dictionary exceeds the inspection memory limit",
                );
            }
            for (index, entry) in archive.archive().files.iter().enumerate() {
                if report.entries.len() >= mode.max_entries() {
                    report.limited = true;
                    break;
                }
                let folder = archive
                    .archive()
                    .stream_map
                    .file_folder_index
                    .get(index)
                    .copied()
                    .flatten();
                let block = folder.and_then(|index| archive.archive().folders.get(index));
                report.entries.push(EntryInfo {
                    name: entry.name.clone(),
                    directory: entry.is_directory,
                    size: entry.size,
                    compressed_size: (entry.compressed_size > 0
                        && block.is_none_or(|block| block.num_unpack_sub_streams <= 1))
                    .then_some(entry.compressed_size),
                    method: folder
                        .and_then(|index| methods.get(index))
                        .cloned()
                        .unwrap_or_else(|| "Stored".into()),
                    modified: entry
                        .has_last_modified_date
                        .then(|| entry.last_modified_date.to_string()),
                    attributes: entry
                        .has_windows_attributes
                        .then(|| format!("{:x}", entry.windows_attributes)),
                    owner_id: None,
                    group_id: None,
                    comment: None,
                    extra_fields: None,
                    link_target: None,
                    stored_crc: entry.has_crc.then_some(entry.crc as u32),
                    computed_crc: None,
                    sha256: None,
                    encrypted: block.is_some_and(|block| {
                        block
                            .coders
                            .iter()
                            .any(|coder| coder.decompression_method_id() == [6, 0xf1, 7, 1])
                    }),
                    link: entry.has_windows_attributes && entry.windows_attributes & 0x400 != 0,
                    error: None,
                });
            }
            if mode != InspectionMode::Preview && !report.limited {
                if encrypted && password.is_none() {
                    report.integrity = Integrity::PasswordRequired;
                } else {
                    let mut metadata = std::mem::take(&mut report.entries);
                    archive.for_each_entries(|entry, reader| {
                        if report.entries.len() >= mode.max_entries() {
                            report.limited = true;
                            return Ok(false);
                        }
                        let Some(index) =
                            metadata.iter().position(|info| info.name == entry.name())
                        else {
                            return Err(sevenz_rust::Error::other("Entry metadata is missing"));
                        };
                        let mut info = metadata.remove(index);
                        if !info.directory {
                            let expected_size = info.size;
                            read_entry(
                                reader,
                                &mut info,
                                Some(expected_size),
                                report,
                                mode,
                                cancel,
                                &mut budget,
                            );
                        }
                        report.entries.push(info);
                        Ok(!report.limited && !cancel.load(Ordering::Relaxed))
                    })?;
                }
            }
        }
        ResolvedFormat::Rar => inspect_rar(report, password, mode, cancel, &mut budget)?,
        _ => anyhow::bail!("Unsupported resolved format for inspection"),
    }
    audit_metadata(report, mode);
    if mode != InspectionMode::Preview && report.integrity != Integrity::PasswordRequired {
        report.integrity = if report.entries.iter().any(|entry| entry.error.is_some()) {
            Integrity::Failed
        } else if report.limited || cancel.load(Ordering::Relaxed) {
            Integrity::Incomplete
        } else {
            Integrity::Passed
        };
    }
    Ok(())
}

/// Inspects RAR entries through the external reader and a bounded scratch directory.
fn inspect_rar(
    report: &mut Report,
    password: Option<&str>,
    mode: InspectionMode,
    cancel: &AtomicBool,
    budget: &mut u64,
) -> Result<()> {
    let path = report.path.clone();
    let mut archive = archive::rar_archive(&path, password)
        .open_for_processing()
        .map_err(archive::map_rar_error)?;
    let scratch = tempfile::tempdir()?;
    let mut index = 0_usize;
    loop {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Inspection cancelled");
        if report.entries.len() >= mode.max_entries() {
            report.limited = true;
            break;
        }
        let Some(entry) = archive.read_header().map_err(archive::map_rar_error)? else {
            break;
        };
        let header = entry.entry();
        let name = header.filename.to_string_lossy().replace('\\', "/");
        let directory = header.is_directory();
        let encrypted = header.is_encrypted();
        let file_attr = header.file_attr;
        let size = header.unpacked_size;
        let mut info = EntryInfo {
            name,
            directory,
            size,
            compressed_size: None,
            method: format!("RAR method {}", header.method),
            modified: None,
            attributes: Some(format!("{file_attr:o}")),
            owner_id: None,
            group_id: None,
            comment: None,
            extra_fields: None,
            link_target: None,
            stored_crc: Some(header.file_crc),
            computed_crc: None,
            sha256: None,
            encrypted,
            link: file_attr & 0x400 != 0 || file_attr & 0o170000 == 0o120000,
            error: None,
        };
        if mode != InspectionMode::Preview && encrypted && password.is_none() {
            report.integrity = Integrity::PasswordRequired;
            archive = entry.skip().map_err(archive::map_rar_error)?;
        } else if mode != InspectionMode::Preview && !directory {
            if size > mode.max_entry_bytes() || size > *budget {
                report.limited = true;
                report.entries.push(info);
                break;
            }
            let extracted_path = scratch.path().join(format!("entry-{index}"));
            archive = entry
                .extract_to(&extracted_path)
                .map_err(archive::map_rar_error)?;
            read_entry(
                File::open(extracted_path)?,
                &mut info,
                Some(size),
                report,
                mode,
                cancel,
                budget,
            );
        } else {
            if mode == InspectionMode::Preview && !directory {
                if size > mode.max_entry_bytes() || size > *budget {
                    report.limited = true;
                } else {
                    *budget -= size;
                }
            }
            archive = entry.skip().map_err(archive::map_rar_error)?;
        }
        report.entries.push(info);
        index += 1;
        if report.limited {
            break;
        }
    }
    Ok(())
}

/// Formats a system timestamp in a stable UTC representation.
fn format_system_time(time: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time)
        .format("%Y-%m-%d %H:%M UTC")
        .to_string()
}

/// Counts a preview stream without computing integrity hashes, respecting inspection limits.
fn measure_entry(
    mut reader: impl Read,
    info: &mut EntryInfo,
    report: &mut Report,
    cancel: &AtomicBool,
    budget: &mut u64,
) -> Result<()> {
    let mut total = 0_u64;
    let mut buffer = [0_u8; 65536];
    loop {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Inspection cancelled");
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let count = count as u64;
        if total.saturating_add(count) > MAX_ENTRY_BYTES || count > *budget {
            report.limited = true;
            break;
        }
        total += count;
        *budget -= count;
    }
    info.size = total;
    Ok(())
}

/// Reads one entry within size and cancellation limits, computing checksums and audit findings.
fn read_entry(
    mut reader: impl Read,
    info: &mut EntryInfo,
    expected_size: Option<u64>,
    report: &mut Report,
    mode: InspectionMode,
    cancel: &AtomicBool,
    budget: &mut u64,
) {
    if expected_size.is_some_and(|size| size > mode.max_entry_bytes() || size > *budget) {
        report.limited = true;
        return;
    }
    let result = (|| -> Result<()> {
        let mut crc = crc32fast::Hasher::new();
        let mut sha = Sha256::new();
        let mut total = 0_u64;
        let mut prefix = Vec::new();
        let mut buffer = [0_u8; 65536];
        loop {
            anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Inspection cancelled");
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > mode.max_entry_bytes() || count as u64 > *budget {
                report.limited = true;
                anyhow::bail!("Inspection byte limit reached");
            }
            *budget -= count as u64;
            crc.update(&buffer[..count]);
            sha.update(&buffer[..count]);
            let keep = (512 - prefix.len()).min(count);
            prefix.extend_from_slice(&buffer[..keep]);
        }
        info.computed_crc = Some(crc.finalize());
        anyhow::ensure!(
            expected_size.is_none_or(|size| total == size),
            "Uncompressed size mismatch"
        );
        anyhow::ensure!(
            info.stored_crc
                .is_none_or(|stored| Some(stored) == info.computed_crc),
            "CRC32 mismatch"
        );
        info.sha256 = Some(format!("{:x}", sha.finalize()));
        info.size = total;
        if mode.is_audit() {
            let detected = detect_format(&prefix);
            if detected.is_some() || expected_format(Path::new(&info.name)).is_some() {
                report.finding(
                    "Nested archive",
                    &info.name,
                    "Nested archives are reported, not recursively expanded",
                );
            }
            if let Some(detected) = detected {
                if expected_format(Path::new(&info.name)) != Some(detected) {
                    report.finding("Format mismatch", &info.name, detected.label());
                }
            } else if expected_format(Path::new(&info.name)).is_some() {
                report.finding(
                    "Format mismatch",
                    &info.name,
                    "Entry does not have an archive signature",
                );
            }
            let extension = Path::new(&info.name)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let expected = if prefix.starts_with(b"%PDF-") {
                Some(&["pdf"][..])
            } else if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
                Some(&["png"][..])
            } else if prefix.starts_with(&[0xff, 0xd8, 0xff]) {
                Some(&["jpg", "jpeg"][..])
            } else if prefix.starts_with(b"MZ") {
                Some(&["exe", "dll", "sys", "scr"][..])
            } else {
                None
            };
            if expected.is_some_and(|extensions| !extensions.contains(&extension.as_str())) {
                report.finding(
                    "Format mismatch",
                    &info.name,
                    "Detected content signature does not match the filename",
                );
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        info.error = Some(error.to_string());
    }
}

/// Checks entry names, links, sizes, duplicate names, and duplicate content.
fn audit_metadata(report: &mut Report, mode: InspectionMode) {
    let mut names = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for info in report.entries.clone() {
        if let Some(error) = &info.error {
            report.finding("Damaged entry", &info.name, error);
        }
        if !mode.is_audit() {
            continue;
        }
        let normalized = info.name.replace('\\', "/");
        if normalized.starts_with('/')
            || normalized.contains(':')
            || normalized.split('/').any(|part| part == "..")
        {
            report.finding(
                "Unsafe path",
                &info.name,
                "Entry could escape an extraction folder",
            );
        }
        if info.link {
            report.finding(
                "Link entry",
                &info.name,
                "Symbolic links and reparse points require review",
            );
        }
        if info.size >= 128 * 1024 * 1024
            || info.compressed_size.is_some_and(|packed| {
                packed > 0 && info.size / packed > 1000 && info.size > 10 * 1024 * 1024
            })
        {
            report.finding(
                "Suspicious size",
                &info.name,
                "Large entry or unusually high compression ratio",
            );
        }
        if let Some(previous) = names.insert(
            normalized.trim_end_matches('/').to_lowercase(),
            info.name.clone(),
        ) {
            report.finding("Duplicate name", &info.name, previous);
        }
        if !info.directory && info.error.is_none() {
            if let Some(hash) = info.sha256 {
                if let Some(previous) = hashes.insert(hash, info.name.clone()) {
                    report.finding("Duplicate content", &info.name, previous);
                }
            }
        }
    }
    if report.limited {
        report.finding(
            "Inspection limit reached",
            "",
            "Some data was not fully checked",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Recognizes both supported RAR signature versions and filename extensions.
    fn detects_rar4_and_rar5_signatures() {
        assert_eq!(
            detect_format(b"Rar!\x1a\x07\x00"),
            Some(ResolvedFormat::Rar)
        );
        assert_eq!(
            detect_format(b"Rar!\x1a\x07\x01\x00"),
            Some(ResolvedFormat::Rar)
        );
        assert_eq!(
            expected_format(Path::new("backup.rar")),
            Some(ResolvedFormat::Rar)
        );
        assert_eq!(detect_format(b"BZh9"), Some(ResolvedFormat::Bzip2));
        assert_eq!(
            detect_format(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]),
            Some(ResolvedFormat::Xz)
        );
        assert_eq!(
            detect_format(&[0x28, 0xb5, 0x2f, 0xfd]),
            Some(ResolvedFormat::Zstandard)
        );
        assert_eq!(
            detect_format(&[0x04, 0x22, 0x4d, 0x18]),
            Some(ResolvedFormat::Lz4)
        );
        assert_eq!(detect_format(b"LZIP\x01"), Some(ResolvedFormat::Lzip));
        assert_eq!(
            expected_format(Path::new("backup.tbz")),
            Some(ResolvedFormat::TarBz2)
        );
        assert_eq!(
            expected_format(Path::new("backup.tbz2")),
            Some(ResolvedFormat::TarBz2)
        );
        assert_eq!(
            expected_format(Path::new("backup.txz")),
            Some(ResolvedFormat::TarXz)
        );
        assert_eq!(
            expected_format(Path::new("backup.tar.br")),
            Some(ResolvedFormat::TarBrotli)
        );
    }
    use std::io::Write;

    #[test]
    /// Stops stream reads and seeks as soon as cancellation is requested.
    fn cancelled_reader_stops_stream_reads_and_seeks() {
        let cancel = AtomicBool::new(false);
        let mut reader = CancellableReader {
            inner: std::io::Cursor::new(vec![1_u8; 128]),
            cancel: &cancel,
        };
        let mut buffer = [0_u8; 8];
        assert_eq!(reader.read(&mut buffer).unwrap(), 8);
        cancel.store(true, Ordering::Relaxed);
        assert!(reader.read(&mut buffer).is_err());
        assert!(reader.rewind().is_err());
    }

    #[test]
    /// Detects CRC errors, duplicate content/names, nested archives, and unsafe paths.
    fn audit_checks_crc_duplicates_nested_archives_and_mismatched_names() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("data.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in [
            ("first.txt", b"duplicate".as_slice()),
            ("second.txt", b"duplicate"),
            ("FIRST.TXT", b"other"),
            ("nested.zip", b"PK\x05\x06\0\0"),
            ("image.txt", b"\x89PNG\r\n\x1a\n"),
            ("../escape.txt", b"unsafe"),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        let report = inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(false));
        assert_eq!(report.integrity, Integrity::Passed);
        for kind in [
            "Duplicate name",
            "Duplicate content",
            "Nested archive",
            "Format mismatch",
            "Unsafe path",
        ] {
            assert!(
                report.findings.iter().any(|finding| finding.kind == kind),
                "missing {kind}"
            );
        }
        assert!(
            report
                .entries
                .iter()
                .all(|entry| entry.computed_crc == entry.stored_crc)
        );
        assert!(report.archive_sha256.is_some());
        let json = report.json();
        assert_eq!(json["detected_format"], "ZIP");
        assert!(report.modified.is_some());
        assert!(json["modified"].as_str().is_some());
        assert!(json["entries"][0]["sha256"].as_str().is_some());
        assert!(json["compression_level"].is_null());
        let renamed = root.path().join("data.tar");
        std::fs::rename(&path, &renamed).unwrap();
        let report = inspect(
            &renamed,
            None,
            InspectionMode::Preview,
            &AtomicBool::new(false),
        );
        assert_eq!(report.detected, Some(ResolvedFormat::Zip));
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == "Format mismatch")
        );
    }

    #[test]
    /// Reports corrupted ZIP payload data as an integrity failure.
    fn integrity_detects_corrupted_zip_data() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("damaged.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file(
            "notes.txt",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"unique test payload").unwrap();
        zip.finish().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let offset = bytes
            .windows(19)
            .position(|window| window == b"unique test payload")
            .unwrap();
        bytes[offset] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        let report = inspect(
            &path,
            None,
            InspectionMode::Integrity,
            &AtomicBool::new(false),
        );
        assert_eq!(report.integrity, Integrity::Failed);
        assert!(report.entries[0].error.is_some());
    }

    #[test]
    /// Confirms the bundled corrupted-entry fixture fails CRC verification.
    fn bundled_corrupt_entry_sample_fails_crc_verification() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/13-corrupt-entry.zip");
        let report = inspect(
            &path,
            None,
            InspectionMode::Integrity,
            &AtomicBool::new(false),
        );
        assert_eq!(report.integrity, Integrity::Failed);
        assert!(report.entries.iter().any(|entry| entry.error.is_some()));
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == "Damaged entry")
        );
    }

    #[test]
    /// Confirms the bundled audit fixture produces multiple finding categories.
    fn bundled_audit_sample_covers_multiple_finding_kinds() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/14-audit-findings.zip");
        for mode in [InspectionMode::Audit, InspectionMode::FullAudit] {
            let report = inspect(&path, None, mode, &AtomicBool::new(false));
            assert_eq!(report.integrity, Integrity::Passed);
            assert_eq!(report.full_audit, mode.is_full_audit());
            for kind in [
                "Unsafe path",
                "Duplicate name",
                "Duplicate content",
                "Nested archive",
                "Format mismatch",
            ] {
                assert!(
                    report.findings.iter().any(|finding| finding.kind == kind),
                    "missing {kind}"
                );
            }
            assert_eq!(
                report.json()["limits"]["max_entries"].is_null(),
                mode.is_full_audit()
            );
        }
    }

    #[test]
    fn full_audit_removes_scan_and_finding_limits() {
        assert_eq!(InspectionMode::Audit.max_entries(), MAX_ENTRIES);
        assert_eq!(InspectionMode::Audit.max_entry_bytes(), MAX_ENTRY_BYTES);
        assert_eq!(InspectionMode::Audit.max_total_bytes(), MAX_TOTAL_BYTES);
        assert_eq!(InspectionMode::FullAudit.max_entries(), usize::MAX);
        assert_eq!(InspectionMode::FullAudit.max_entry_bytes(), u64::MAX);
        assert_eq!(InspectionMode::FullAudit.max_total_bytes(), u64::MAX);

        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/14-audit-findings.zip");
        let mut bounded = inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(false));
        let mut full = inspect(&path, None, InspectionMode::FullAudit, &AtomicBool::new(false));
        bounded.findings.clear();
        full.findings.clear();
        for _ in 0..=MAX_FINDINGS {
            bounded.finding("Test", "", "finding");
            full.finding("Test", "", "finding");
        }
        assert_eq!(bounded.findings.len(), MAX_FINDINGS);
        assert!(bounded.limited);
        assert_eq!(full.findings.len(), MAX_FINDINGS + 1);
        assert!(!full.limited);
    }

    #[test]
    #[ignore = "generates manual integrity and audit sample archives"]
    /// Regenerates sample archives used by manual integrity and audit checks.
    fn generate_integrity_and_audit_samples() {
        use std::fs::{self, File};

        let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        fs::create_dir_all(&samples).unwrap();

        let damaged = samples.join("13-corrupt-entry.zip");
        if !damaged.exists() {
            let payload = b"corrupt this stored payload";
            let mut zip = zip::ZipWriter::new(File::create(&damaged).unwrap());
            zip.start_file(
                "integrity/notes.txt",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            zip.write_all(payload).unwrap();
            zip.finish().unwrap();
            let mut bytes = fs::read(&damaged).unwrap();
            let offset = bytes
                .windows(payload.len())
                .position(|window| window == payload)
                .unwrap();
            bytes[offset] ^= 1;
            fs::write(&damaged, bytes).unwrap();
        }

        let audit = samples.join("14-audit-findings.zip");
        if !audit.exists() {
            let mut zip = zip::ZipWriter::new(File::create(&audit).unwrap());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, contents) in [
                ("README.txt", b"Audit findings sample".as_slice()),
                ("copies/first.txt", b"duplicate payload"),
                ("copies/second.txt", b"duplicate payload"),
                ("SAME.txt", b"first name"),
                ("same.TXT", b"second name"),
                ("nested.zip", b"PK\x05\x06\0\0"),
                ("image.txt", b"\x89PNG\r\n\x1a\n"),
                ("../escape.txt", b"unsafe path"),
            ] {
                zip.start_file(name, options).unwrap();
                zip.write_all(contents).unwrap();
            }
            zip.finish().unwrap();
        }

        assert!(damaged.exists());
        assert!(audit.exists());
    }

    #[test]
    /// Exercises inspection across formats, encrypted archives, and solid 7z entries.
    fn inspection_reads_every_format_including_encrypted_solid_7z() {
        use crate::archive::{self, CreateOptions};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("empty")).unwrap();
        std::fs::write(source.join("one.txt"), b"first contents").unwrap();
        std::fs::write(source.join("two.txt"), b"second contents").unwrap();
        for format in ResolvedFormat::ALL
            .into_iter()
            .filter(|format| !format.is_single_compressed() && *format != ResolvedFormat::Ar)
        {
            let path = root.path().join(format!("archive.{}", format.extension()));
            let password = format.supports_password().then_some("inspect-password");
            archive::create_archive_with_password(
                &path,
                &[source.clone()],
                CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    solid: true,
                    dictionary_mib: 1,
                    ..CreateOptions::default()
                },
                password,
            )
            .unwrap();
            let cancel = AtomicBool::new(false);
            let report = inspect(&path, password, InspectionMode::Preview, &cancel);
            assert!(report.error.is_none(), "{format:?}: {:?}", report.error);
            assert_eq!(report.entries.len(), 4);
            assert_eq!(report.integrity, Integrity::NotTested);
            let report = inspect(&path, password, InspectionMode::Integrity, &cancel);
            assert_eq!(
                report.integrity,
                Integrity::Passed,
                "{format:?}: {:?}",
                report.error
            );
            assert!(
                report
                    .entries
                    .iter()
                    .filter(|entry| !entry.directory)
                    .all(|entry| entry.computed_crc.is_some() && entry.sha256.is_some())
            );
            if password.is_some() {
                let report = inspect(&path, None, InspectionMode::Integrity, &cancel);
                assert_eq!(report.integrity, Integrity::PasswordRequired);
            }
        }
    }

    #[test]
    fn extended_archive_inspection_supports_all_modes() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        std::fs::write(&source, b"archive inspection contents").unwrap();
        for format in [ResolvedFormat::Cpio, ResolvedFormat::Ar, ResolvedFormat::Pax, ResolvedFormat::Xar] {
            let path = root.path().join(format!("archive.{}", format.extension()));
            crate::archive::create_archive(&path, std::slice::from_ref(&source), crate::archive::CreateOptions {
                format, compression: format.compression_levels()[0], ..crate::archive::CreateOptions::default()
            }).unwrap();
            for mode in [
                InspectionMode::Preview,
                InspectionMode::Integrity,
                InspectionMode::Audit,
                InspectionMode::FullAudit,
            ] {
                let report = inspect(&path, None, mode, &AtomicBool::new(false));
                assert_eq!(report.detected, Some(format), "{format:?}");
                assert!(report.error.is_none(), "{format:?}: {:?}", report.error);
                assert_eq!(report.entries.len(), 1);
                assert_eq!(report.entries[0].name, "notes.txt");
                if mode == InspectionMode::Preview {
                    assert_eq!(report.integrity, Integrity::NotTested);
                } else {
                    assert_eq!(report.integrity, Integrity::Passed, "{format:?}");
                    assert!(report.entries[0].sha256.is_some());
                }
            }
            let cancelled = inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(true));
            assert_eq!(cancelled.integrity, Integrity::Incomplete);
        }
    }

    #[test]
    /// Inspects standalone compressed files as one entry and verifies their decompressed content.
    fn inspection_reads_standalone_compressed_files() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        std::fs::write(&source, b"standalone inspection contents").unwrap();
        for format in [
            ResolvedFormat::Gzip,
            ResolvedFormat::Bzip2,
            ResolvedFormat::Xz,
            ResolvedFormat::Zstandard,
            ResolvedFormat::Lz4,
            ResolvedFormat::Lzip,
            ResolvedFormat::Brotli,
        ] {
            let path = root
                .path()
                .join(format!("notes.txt.{}", format.extension()));
            crate::archive::create_archive(
                &path,
                std::slice::from_ref(&source),
                crate::archive::CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    ..crate::archive::CreateOptions::default()
                },
            )
            .unwrap();
            let preview = inspect(
                &path,
                None,
                InspectionMode::Preview,
                &AtomicBool::new(false),
            );
            assert_eq!(preview.detected, Some(format));
            assert_eq!(preview.entries.len(), 1);
            assert_eq!(preview.entries[0].name, "notes.txt");
            assert_eq!(
                preview.entries[0].size,
                b"standalone inspection contents".len() as u64
            );
            assert_eq!(preview.integrity, Integrity::NotTested);

            for mode in [InspectionMode::Integrity, InspectionMode::Audit] {
                let verified = inspect(&path, None, mode, &AtomicBool::new(false));
                assert_eq!(verified.integrity, Integrity::Passed, "{format:?}: {mode:?}");
                assert!(verified.entries[0].sha256.is_some());
            }
        }
    }

    #[test]
    /// Verifies gzip footer checks and incomplete outcomes for cancellation or limits.
    fn verifies_gzip_footer_and_reports_cancelled_or_bounded_checks() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let path = root.path().join("archive.tar.gz");
        std::fs::write(&source, b"contents").unwrap();
        crate::archive::create_archive(
            &path,
            &[source.clone()],
            crate::archive::CreateOptions {
                format: ResolvedFormat::TarGz,
                ..crate::archive::CreateOptions::default()
            },
        )
        .unwrap();
        let cancelled = inspect(
            &path,
            None,
            InspectionMode::Integrity,
            &AtomicBool::new(true),
        );
        assert_eq!(cancelled.integrity, Integrity::Incomplete);
        let mut bytes = std::fs::read(&path).unwrap();
        let crc = bytes.len() - 8;
        bytes[crc] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(
            inspect(
                &path,
                None,
                InspectionMode::Integrity,
                &AtomicBool::new(false)
            )
            .integrity,
            Integrity::Failed
        );
        let path = root.path().join("large.zip");
        crate::archive::create_archive(&path, &[source], crate::archive::CreateOptions::default())
            .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .unwrap();
        bytes[central + 24..central + 28].copy_from_slice(&(300_u32 * 1024 * 1024).to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let report = inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(false));
        assert_eq!(report.integrity, Integrity::Incomplete);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == "Suspicious size")
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == "Inspection limit reached")
        );
    }
}
