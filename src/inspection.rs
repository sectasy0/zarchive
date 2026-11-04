//! Reads archive metadata, verifies contents, and reports bounded security findings.

use crate::archive::{self, PasswordFailure, ResolvedFormat};

mod model;
pub use model::{AuditOptions, EntryInfo, Finding, InspectionMode, Integrity, Report};
use model::AuditState;

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
    io::{Read, Seek, Write},
    path::Path,
    sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}},
};

const MAX_ENTRIES: usize = 20_000;
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FINDINGS: usize = 500;
const MAX_SEVENZIP_DICTIONARY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_NESTED_ARCHIVES: usize = 64;
const MAX_NESTED_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_NESTED_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
struct InspectionLimit;

impl std::fmt::Display for InspectionLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Inspection limit reached")
    }
}

impl std::error::Error for InspectionLimit {}

#[derive(Debug)]
struct ExpansionLimit { bomb: bool }

impl std::fmt::Display for ExpansionLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Archive expansion safety limit reached")
    }
}

impl std::error::Error for ExpansionLimit {}

struct ExpansionReader<'a, R> {
    inner: R,
    bytes: &'a std::cell::Cell<u64>,
    limit: u64,
    bomb: bool,
    stopped: Arc<AtomicU8>,
}

impl<R: Read> Read for ExpansionReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        crate::task_queue::checkpoint()?;
        if buffer.is_empty() { return Ok(0); }
        let remaining = self.limit.saturating_sub(self.bytes.get());
        if remaining == 0 {
            let mut probe = [0u8; 1];
            if self.inner.read(&mut probe)? == 0 { return Ok(0); }
            self.stopped.store(if self.bomb { 2 } else { 1 }, Ordering::Relaxed);
            return Err(std::io::Error::other(ExpansionLimit { bomb: self.bomb }));
        }
        let length = remaining.min(buffer.len() as u64) as usize;
        let count = self.inner.read(&mut buffer[..length])?;
        self.bytes.set(self.bytes.get().saturating_add(count as u64));
        Ok(count)
    }
}

/// Infers the expected format from the archive filename extension.
fn expected_format(path: &Path) -> Option<ResolvedFormat> {
    if crate::volumes::numbered_base(path).is_some()
        || archive::ResolvedFormat::from_path(path) == Some(ResolvedFormat::Rar)
    {
        return archive::ResolvedFormat::from_path(path);
    }
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
    inspect_with_options(path, password, mode, cancel, AuditOptions::default())
}

pub fn inspect_with_options(path: &Path, password: Option<&str>, mode: InspectionMode,
    cancel: &AtomicBool, options: AuditOptions) -> Report {
    inspect_with_state(path, password, mode, cancel, AuditState::new(options))
}

fn inspect_with_state(path: &Path, password: Option<&str>, mode: InspectionMode,
    cancel: &AtomicBool, audit: AuditState) -> Report {
    let mut report = Report {
        path: path.to_path_buf(),
        detected: None,
        signature: "Unknown".into(),
        expected: expected_format(path),
        archive_size: 0,
        created: None,
        modified: None,
        accessed: None,
        filesystem_permissions: None,
        filesystem_readonly: None,
        filesystem_owner_id: None,
        filesystem_group_id: None,
        archive_comment: None,
        archive_sha256: None,
        entries: Vec::new(),
        findings: Vec::new(),
        content_signatures: BTreeMap::new(),
        integrity: Integrity::NotTested,
        limited: false,
        error: None,
        full_audit: mode.is_full_audit(),
        audit,
        audit_bytes_read: 0,
        expansion_stopped: Arc::new(AtomicU8::new(0)),
    };
    if let Err(error) = inspect_inner(&mut report, password, mode, cancel) {
        let expansion = error.chain().find_map(|source| source.downcast_ref::<ExpansionLimit>().map(|limit| limit.bomb))
            .or_else(|| match report.expansion_stopped.load(Ordering::Relaxed) {
                1 => Some(false), 2 => Some(true), _ => None,
            });
        if matches!(error.downcast_ref::<crate::xar::ReadError>(), Some(crate::xar::ReadError::Limit)) {
            report.limited = true;
        }
        if let Some(expansion) = expansion {
            report.limited = true;
            report.integrity = Integrity::Incomplete;
            report.finding(if expansion { "Archive bomb" } else { "Inspection limit reached" }, "",
                if expansion { "Extreme expansion was observed while reading; decompression was stopped" }
                else { "Some data was not fully checked" });
        } else if error.downcast_ref::<PasswordFailure>().is_some() {
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
        if mode.is_audit() && report.integrity == Integrity::Failed {
            report.finding("Parser anomaly", "", "Archive metadata or content was rejected by the parser");
        }
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
    let metadata = std::fs::metadata(&report.path)?;
    let file = archive::open_archive_reader(&report.path)?;
    report.archive_size = file.len();
    if report.audit.depth > 0 {
        if file.len() > MAX_NESTED_ARCHIVE_BYTES || file.len() > report.audit.bytes_left {
            report.limited = true;
            report.integrity = Integrity::Incomplete;
            report.finding("Nested audit limit", "", "Nested archive input exceeds the shared audit budget");
            return Ok(());
        }
        report.audit.bytes_left -= file.len();
    }
    if mode != InspectionMode::Preview {
        report.limited |= !crate::volumes::verify(&report.path, cancel, mode.max_total_bytes())?;
    }
    report.created = metadata.created().ok().map(format_system_time);
    report.modified = metadata.modified().ok().map(format_system_time);
    report.accessed = metadata.accessed().ok().map(format_system_time);
    report.filesystem_permissions = filesystem_permissions(&metadata);
    report.filesystem_readonly = Some(metadata.permissions().readonly());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        report.filesystem_owner_id = Some(metadata.uid());
        report.filesystem_group_id = Some(metadata.gid());
    }
    let mut file = CancellableReader {
        inner: file,
        cancel,
    };
    let mut header = Vec::with_capacity(crate::security::SNIFF_BYTES);
    file.by_ref().take(crate::security::SNIFF_BYTES as u64).read_to_end(&mut header)?;
    let count = header.len();
    report.detected = resolve_detected_format(detect_format(&header[..count]), report.expected);
    if report.detected.is_none()
        && matches!(report.expected, Some(ResolvedFormat::Tar | ResolvedFormat::Pax))
        && count >= 512
        && header[..512].iter().all(|byte| *byte == 0)
        && report.archive_size >= 1024
    {
        report.detected = report.expected;
    }
    if report.detected.is_none() && report.expected == Some(ResolvedFormat::Zip) {
        file.rewind()?;
        if zip::ZipArchive::new(&mut file).is_ok() { report.detected = Some(ResolvedFormat::Zip); }
    }
    if mode.is_audit() {
        let count = report.archive_size.min(crate::security::SNIFF_BYTES as u64);
        file.seek(std::io::SeekFrom::End(-(count as i64)))?;
        let mut tail = Vec::with_capacity(count as usize);
        file.by_ref().take(count).read_to_end(&mut tail)?;
        crate::security::content(report, "", &header, &tail);
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
        let mut buffer = vec![0_u8; 65536];
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
            let mut reader = archive::open_extended_archive(&report.path)?;
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
                if mode.is_audit() {
                    if let Some(target) = entry.hardlink() { crate::security::hard_link(report, &info.name, &target); }
                }
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
            let mut ranges: BTreeMap<u64, u64> = BTreeMap::new();
            report.archive_comment = (!archive.comment().is_empty())
                .then(|| String::from_utf8_lossy(archive.comment()).into_owned());
            for index in 0..archive.len() {
                if report.entries.len() >= mode.max_entries() {
                    report.limited = true;
                    break;
                }
                let raw = archive.by_index_raw(index)?;
                let mut range_anomaly = false;
                if mode.is_audit() {
                    if let Some(end) = raw.data_start().and_then(|start| start.checked_add(raw.compressed_size())) {
                        let start = raw.header_start();
                        range_anomaly = end > raw.central_header_start() || end > report.archive_size || end < start
                            || ranges.range(..=start).next_back().is_some_and(|(_, previous_end)| *previous_end > start)
                            || (end > start && ranges.range(start..end).next().is_some());
                        ranges.insert(start, end);
                    }
                }
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
                if range_anomaly {
                    report.limited = true;
                    report.finding("Parser anomaly", &info.name, "ZIP entry ranges overlap or point outside the archive data");
                }
                if mode != InspectionMode::Preview && !info.directory && !range_anomaly {
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
            let decoded_bytes = std::cell::Cell::new(0u64);
            let before_content = report.audit_bytes_read;
            let expansion_limit = report.archive_size.saturating_mul(1000).max(10 * 1024 * 1024);
            let byte_limit = if report.audit.depth > 0 {
                report.audit.bytes_left.min(MAX_NESTED_ARCHIVE_BYTES)
            } else { mode.max_total_bytes() };
            let limit = if mode.is_audit() { expansion_limit.min(byte_limit) } else { u64::MAX };
            let mut archive = tar::Archive::new(ExpansionReader { inner: reader,
                bytes: &decoded_bytes, limit, bomb: expansion_limit <= byte_limit,
                stopped: report.expansion_stopped.clone() });
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
                if mode.is_audit() && header.entry_type().is_hard_link() {
                    if let Some(target) = &info.link_target { crate::security::hard_link(report, &info.name, target); }
                }
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
                let mut buffer = vec![0_u8; 65536];
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
            if report.audit.depth > 0 {
                let overhead = decoded_bytes.get().saturating_sub(report.audit_bytes_read.saturating_sub(before_content));
                if overhead > report.audit.bytes_left { report.limited = true; }
                report.audit.bytes_left = report.audit.bytes_left.saturating_sub(overhead);
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
        report.integrity = if report.limited || cancel.load(Ordering::Relaxed) {
            Integrity::Incomplete
        } else if report.entries.iter().any(|entry| entry.error.is_some()) {
            Integrity::Failed
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

fn filesystem_permissions(metadata: &std::fs::Metadata) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(format!("{:04o}", metadata.permissions().mode() & 0o7777))
    }
    #[cfg(not(unix))]
    {
        Some(if metadata.permissions().readonly() {
            "Read-only".into()
        } else {
            "Read/write".into()
        })
    }
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
    let mut buffer = vec![0_u8; 65536];
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
    if mode.is_audit() && info.compressed_size.is_some_and(|packed| {
        info.size > 10 * 1024 * 1024 && info.size / packed.max(1) > 1000
    }) {
        report.limited = true;
        report.finding("Archive bomb", &info.name, "Extreme expansion ratio; decompression was stopped for safety");
        return;
    }
    if report.audit.depth > 0 && expected_size.is_some_and(|size| {
        size > MAX_NESTED_ARCHIVE_BYTES || size > report.audit.bytes_left
    }) {
        report.limited = true;
        report.finding("Nested audit limit", &info.name, "Nested entry exceeds the shared audit budget");
        return;
    }
    if expected_size.is_some_and(|size| size > mode.max_entry_bytes() || size > *budget) {
        report.limited = true;
        return;
    }
    let result = (|| -> Result<()> {
        let mut crc = crc32fast::Hasher::new();
        let mut sha = Sha256::new();
        let mut total = 0_u64;
        let mut prefix = Vec::new();
        let mut tail = Vec::new();
        let mut nested: Option<tempfile::NamedTempFile> = None;
        let mut nested_seen = false;
        let mut buffer = vec![0_u8; 65536];
        loop {
            anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Inspection cancelled");
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if mode.is_audit() {
                report.audit_bytes_read = report.audit_bytes_read.saturating_add(count as u64);
                let packed = info.compressed_size.unwrap_or(report.archive_size).max(1);
                if (total > 10 * 1024 * 1024 && total / packed > 1000)
                    || (report.audit_bytes_read > 100 * 1024 * 1024
                        && report.audit_bytes_read / report.archive_size.max(1) > 1000)
                {
                    report.limited = true;
                    report.finding("Archive bomb", &info.name, "Extreme expansion was observed while reading; decompression was stopped");
                    return Err(InspectionLimit.into());
                }
            }
            if total > mode.max_entry_bytes() || count as u64 > *budget {
                report.limited = true;
                return Err(InspectionLimit.into());
            }
            *budget -= count as u64;
            if report.audit.depth > 0 {
                if total > MAX_NESTED_ARCHIVE_BYTES || count as u64 > report.audit.bytes_left {
                    report.limited = true;
                    return Err(InspectionLimit.into());
                }
                report.audit.bytes_left -= count as u64;
            }
            crc.update(&buffer[..count]);
            sha.update(&buffer[..count]);
            let keep = (crate::security::SNIFF_BYTES - prefix.len()).min(count);
            prefix.extend_from_slice(&buffer[..keep]);
            if mode.is_audit() {
                if tail.len() + count > crate::security::SNIFF_BYTES {
                    tail.drain(..tail.len().min(tail.len() + count - crate::security::SNIFF_BYTES));
                }
                tail.extend_from_slice(&buffer[..count]);
                if let Some(output) = nested.as_mut() {
                    if total <= MAX_NESTED_ARCHIVE_BYTES {
                        output.write_all(&buffer[..count])?;
                    } else {
                        nested = None;
                        report.limited = true;
                        report.finding("Nested audit limit", &info.name, "Nested archive exceeds the staging limit");
                    }
                } else if !nested_seen && !info.link {
                    let expected = expected_format(Path::new(&info.name));
                    if let Some(format) = resolve_detected_format(detect_format(&prefix), expected).or(expected)
                        .filter(|format| format.capabilities().can_list)
                    {
                        nested_seen = true;
                        report.finding("Nested archive", &info.name, "Entry contains an archive or compressed stream");
                        if report.audit.options.max_depth > 0 {
                            if report.audit.depth >= report.audit.options.max_depth || report.audit.archives_left == 0
                                || expected_size.is_some_and(|size| size > MAX_NESTED_ARCHIVE_BYTES)
                                || total > prefix.len() as u64 || report.limited
                            {
                                report.limited = true;
                                report.finding("Nested audit limit", &info.name, "Configured depth, archive count, or staging budget was reached");
                            } else {
                                let mut output = tempfile::Builder::new().prefix("zarchive-audit-")
                                    .suffix(&format!(".{}", format.extension())).tempfile()?;
                                output.write_all(&prefix)?;
                                nested = Some(output);
                            }
                        }
                    }
                }
            }
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
        if info.link && info.link_target.is_none() && total <= 4096 {
            info.link_target = Some(String::from_utf8_lossy(&prefix).trim_end_matches('\0').to_owned());
        }
        if mode.is_audit() {
            crate::security::content(report, &info.name, &prefix, &tail);
            let detected = detect_format(&prefix);
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
            if let Some(mut nested) = nested {
                nested.flush()?;
                audit_nested(report, &info.name, nested.path(), cancel);
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        if error.downcast_ref::<InspectionLimit>().is_none() {
            info.error = Some(error.to_string());
        }
    }
}

fn audit_nested(report: &mut Report, name: &str, path: &Path, cancel: &AtomicBool) {
    if cancel.load(Ordering::Relaxed) { report.limited = true; return; }
    let mut state = report.audit;
    state.depth += 1;
    state.archives_left -= 1;
    let nested = inspect_with_state(path, None, InspectionMode::Audit, cancel, state);
    report.audit.archives_left = nested.audit.archives_left;
    report.audit.bytes_left = nested.audit.bytes_left;
    let entry_path = |entry: &str| if entry.is_empty() { name.to_owned() } else { format!("{name}!{entry}") };
    if nested.integrity == Integrity::PasswordRequired {
        report.limited = true;
        report.finding("Encrypted nested archive", name, "A nested archive requires a password and was not fully inspected");
    } else if nested.integrity == Integrity::Failed {
        report.limited = true;
        report.finding("Parser anomaly", name, "A nested archive could not be fully parsed or verified");
    }
    if nested.limited || nested.integrity == Integrity::Incomplete {
        report.limited = true;
        report.finding("Nested audit limit", name, "Nested inspection was incomplete");
    }
    for finding in nested.findings {
        report.finding(finding.kind, entry_path(&finding.entry), finding.detail);
    }
    for (entry, signature) in nested.content_signatures {
        if report.content_signatures.len() >= MAX_ENTRIES { report.limited = true; break; }
        report.content_signatures.insert(entry_path(&entry), signature);
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
        crate::security::metadata(report, &info);
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
    if mode.is_audit() {
        crate::security::structure(report);
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
pub(crate) mod tests {
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

    fn empty_zip() -> Vec<u8> {
        zip::ZipWriter::new(std::io::Cursor::new(Vec::new())).finish().unwrap().into_inner()
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer.start_file(*name, zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn security_audit_nested_archives_fit_platform_worker_stack() {
        std::thread::Builder::new().name("audit-small-stack".to_owned()).stack_size(512 * 1024)
            .spawn(|| {
                let root = tempfile::tempdir().unwrap();
                let path = root.path().join("nested.zip");
                let mut bytes = zip_bytes(&[("payload.txt", b"small worker stack regression")]);
                for index in 0..8 {
                    bytes = zip_bytes(&[(&format!("level-{index}.zip"), &bytes)]);
                }
                std::fs::write(&path, bytes).unwrap();
                for mode in [InspectionMode::Audit, InspectionMode::FullAudit] {
                    let report = inspect_with_options(&path, None, mode, &AtomicBool::new(false),
                        AuditOptions { max_depth: 8 });
                    assert_eq!(report.integrity, Integrity::Passed, "{:?}", report.error);
                    assert_eq!(report.json()["nested_audit"]["archives_inspected"], 8);
                }
            }).unwrap().join().unwrap();
    }

    #[test]
    fn security_audit_recursion_respects_depth_and_reports_nested_paths() {
        let root = tempfile::tempdir().unwrap();
        let leaf = zip_bytes(&[("invoice.pdf.exe", b"MZ payload"), ("../escape.txt", b"unsafe")]);
        let middle = zip_bytes(&[("inner.zip", &leaf)]);
        let path = root.path().join("outer.zip");
        std::fs::write(&path, zip_bytes(&[("middle.zip", &middle)])).unwrap();
        for depth in [0, 1, 2] {
            let report = inspect_with_options(&path, None, InspectionMode::Audit,
                &AtomicBool::new(false), AuditOptions { max_depth: depth });
            assert_eq!(report.integrity, if depth == 1 { Integrity::Incomplete } else { Integrity::Passed });
            assert_eq!(report.json()["nested_audit"]["archives_inspected"], depth);
            assert_eq!(report.findings.iter().any(|finding| finding.kind == "Double extension"
                && finding.entry == "middle.zip!inner.zip!invoice.pdf.exe"), depth == 2);
            if depth == 2 {
                assert!(report.findings.iter().any(|finding| finding.kind == "Unsafe path"
                    && finding.entry == "middle.zip!inner.zip!../escape.txt"));
                assert!(report.json()["content_signatures"].as_object().unwrap().contains_key("middle.zip!inner.zip!invoice.pdf.exe"));
            }
        }
        assert!(!root.path().join("escape.txt").exists());
    }

    #[test]
    fn security_audit_marks_encrypted_and_corrupt_nested_archives_incomplete() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.txt");
        std::fs::write(&source, b"private nested data").unwrap();
        let nested = root.path().join("protected.zip");
        archive::create_archive_with_password(&nested, &[source], archive::CreateOptions::default(), Some("nested-password")).unwrap();
        let encrypted = std::fs::read(nested).unwrap();
        for (bytes, kind) in [(encrypted.as_slice(), "Encrypted nested archive"), (b"PK\x05\x06\0\0".as_slice(), "Parser anomaly")] {
            let path = root.path().join("outer.zip");
            std::fs::write(&path, zip_bytes(&[("inner.zip", bytes)])).unwrap();
            let report = inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(false));
            assert_eq!(report.integrity, Integrity::Incomplete);
            assert!(report.findings.iter().any(|finding| finding.kind == kind), "{kind}");
            assert!(!report.json().to_string().contains("nested-password"));
            assert_eq!(inspect(&path, None, InspectionMode::Integrity, &AtomicBool::new(false)).integrity, Integrity::Passed);
        }
    }

    #[test]
    fn security_audit_limits_archive_counts_and_shared_nested_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outer.zip");
        let empty = empty_zip();
        let names = (0..=MAX_NESTED_ARCHIVES).map(|index| format!("nested-{index}.zip")).collect::<Vec<_>>();
        let entries = names.iter().map(|name| (name.as_str(), empty.as_slice())).collect::<Vec<_>>();
        std::fs::write(&path, zip_bytes(&entries)).unwrap();
        let report = inspect(&path, None, InspectionMode::FullAudit, &AtomicBool::new(false));
        assert_eq!(report.integrity, Integrity::Incomplete);
        assert_eq!(report.json()["nested_audit"]["archives_inspected"], MAX_NESTED_ARCHIVES);
        assert!(report.findings.iter().any(|finding| finding.kind == "Nested audit limit"));
        let mut state = AuditState::new(AuditOptions::default());
        state.depth = 1;
        state.bytes_left = 1;
        let report = inspect_with_state(&path, None, InspectionMode::Audit, &AtomicBool::new(false), state);
        assert_eq!(report.integrity, Integrity::Incomplete);
    }

    #[test]
    fn security_audit_stops_zip_and_stream_expansion_bombs() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bomb.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer.start_file("zeros.txt", zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated).compression_level(Some(9))).unwrap();
        let block = [0u8; 65536];
        for _ in 0..192 { writer.write_all(&block).unwrap(); }
        writer.finish().unwrap();
        let stream = root.path().join("bomb.gz");
        let mut writer = flate2::write::GzEncoder::new(File::create(&stream).unwrap(), flate2::Compression::best());
        for _ in 0..192 { writer.write_all(&block).unwrap(); }
        writer.finish().unwrap();
        let tar_stream = root.path().join("bomb.tar.gz");
        let writer = flate2::write::GzEncoder::new(File::create(&tar_stream).unwrap(), flate2::Compression::best());
        let mut builder = tar::Builder::new(writer);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_link_name("inside").unwrap();
        header.set_size(12 * 1024 * 1024);
        header.set_mode(0o777);
        header.set_cksum();
        builder.append_data(&mut header, "skip-link", std::io::repeat(0).take(12 * 1024 * 1024)).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        for path in [&path, &stream, &tar_stream] {
            for mode in [InspectionMode::Audit, InspectionMode::FullAudit] {
                let report = inspect(path, None, mode, &AtomicBool::new(false));
                assert_eq!(report.integrity, Integrity::Incomplete);
                assert!(report.findings.iter().any(|finding| finding.kind == "Archive bomb"));
                assert!(!report.findings.iter().any(|finding| finding.kind == "Damaged entry"));
            }
        }
    }

    #[test]
    fn security_audit_detects_zip_symlinks_ads_and_polyglot_containers() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("links.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer.add_symlink("unsafe-link", "../../outside", zip::write::SimpleFileOptions::default()).unwrap();
        writer.start_file("notes.txt:hidden.exe", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(b"ADS contents").unwrap();
        writer.finish().unwrap();
        let report = inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(false));
        assert!(report.findings.iter().any(|finding| finding.kind == "Suspicious link"));
        assert!(report.findings.iter().any(|finding| finding.kind == "Alternate data stream"));
        assert_eq!(report.entries[0].link_target.as_deref(), Some("../../outside"));
        let polyglot = root.path().join("polyglot.zip");
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&zip_bytes(&[("payload.txt", b"plain payload")]));
        std::fs::write(&polyglot, bytes).unwrap();
        let report = inspect(&polyglot, None, InspectionMode::Audit, &AtomicBool::new(false));
        assert_eq!(report.integrity, Integrity::Passed, "{:?}", report.error);
        assert!(report.findings.iter().any(|finding| finding.kind == "Polyglot file"));
        assert!(!root.path().join("outside").exists());
    }

    #[test]
    fn security_audit_detects_hardlinks_and_link_cycles() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("links.tar");
        let mut builder = tar::Builder::new(File::create(&path).unwrap());
        for (name, target, kind) in [("dir/hard", "../outside", tar::EntryType::Link),
            ("first", "second", tar::EntryType::Symlink), ("second", "first", tar::EntryType::Symlink)] {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            builder.append_link(&mut header, name, target).unwrap();
        }
        builder.finish().unwrap();
        let report = inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(false));
        assert_eq!(report.integrity, Integrity::Passed);
        assert!(report.findings.iter().any(|finding| finding.kind == "Suspicious link" && finding.entry == "dir/hard"));
        assert!(report.findings.iter().any(|finding| finding.kind == "Suspicious link" && finding.detail.contains("cycle")));
    }

    #[test]
    fn security_audit_detects_aliased_zip_ranges() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("overlap.zip");
        let mut bytes = zip_bytes(&[("first.txt", b"same"), ("second.txt", b"same")]);
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let first = archive.by_index_raw(0).unwrap().header_start();
        let second = archive.by_index_raw(1).unwrap().central_header_start() as usize;
        drop(archive);
        bytes[second + 42..second + 46].copy_from_slice(&(first as u32).to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let report = inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(false));
        assert!(report.findings.iter().any(|finding| finding.kind == "Parser anomaly"));
        assert_ne!(report.integrity, Integrity::Passed);
    }

    #[test]
    fn security_audit_accepts_ordinary_images_and_records_magic_types() {
        let root = tempfile::tempdir().unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let path = root.path().join("ordinary.zip");
        std::fs::write(&path, zip_bytes(&[("photo.png", png.get_ref()), ("notes.txt", b"ordinary text")])).unwrap();
        let report = inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(false));
        assert_eq!(report.integrity, Integrity::Passed);
        assert_eq!(report.json()["content_signatures"]["photo.png"], "image/png");
        assert!(!report.findings.iter().any(|finding| matches!(finding.kind,
            "Polyglot file" | "Suspicious executable" | "Format mismatch" | "Parser anomaly")));
    }

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
        let nested = empty_zip();
        for (name, bytes) in [
            ("first.txt", b"duplicate".as_slice()),
            ("second.txt", b"duplicate"),
            ("FIRST.TXT", b"other"),
            ("nested.zip", nested.as_slice()),
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
        assert!(report.filesystem_permissions.is_some());
        assert_eq!(report.filesystem_readonly, Some(false));
        assert!(report.accessed.is_some());
        #[cfg(unix)]
        {
            assert!(report.filesystem_owner_id.is_some());
            assert!(report.filesystem_group_id.is_some());
        }
        let json = report.json();
        assert_eq!(json["detected_format"], "ZIP");
        assert!(report.modified.is_some());
        assert!(json["modified"].as_str().is_some());
        assert!(json["filesystem_permissions"].as_str().is_some());
        assert_eq!(json["filesystem_readonly"], false);
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

    fn write_corrupt_entry_sample(path: &Path) {
        let payload = b"corrupt this stored payload";
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        zip.start_file(
            "integrity/notes.txt",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(payload).unwrap();
        zip.finish().unwrap();
        let mut bytes = std::fs::read(path).unwrap();
        let offset = bytes
            .windows(payload.len())
            .position(|window| window == payload)
            .unwrap();
        bytes[offset] ^= 1;
        std::fs::write(path, bytes).unwrap();
    }

    pub(crate) fn write_audit_sample(path: &Path) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let nested = empty_zip();
        for (name, contents) in [
            ("README.txt", b"Audit findings sample".as_slice()),
            ("copies/first.txt", b"duplicate payload"),
            ("copies/second.txt", b"duplicate payload"),
            ("SAME.txt", b"first name"),
            ("same.TXT", b"second name"),
            ("nested.zip", nested.as_slice()),
            ("image.txt", b"\x89PNG\r\n\x1a\n"),
            ("../escape.txt", b"unsafe path"),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(contents).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    /// Confirms the corrupted-entry fixture fails CRC verification.
    fn bundled_corrupt_entry_sample_fails_crc_verification() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("corrupt-entry.zip");
        write_corrupt_entry_sample(&path);
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
    /// Confirms the audit fixture produces multiple finding categories.
    fn bundled_audit_sample_covers_multiple_finding_kinds() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("audit-findings.zip");
        write_audit_sample(&path);
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

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("audit-findings.zip");
        write_audit_sample(&path);
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
        use std::fs;

        let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        fs::create_dir_all(&samples).unwrap();

        let damaged = samples.join("13-corrupt-entry.zip");
        if !damaged.exists() {
            write_corrupt_entry_sample(&damaged);
        }

        let audit = samples.join("14-audit-findings.zip");
        if !audit.exists() {
            write_audit_sample(&audit);
        }

        assert!(damaged.exists());
        assert!(audit.exists());
    }

    fn store_security_example(path: &Path, bytes: &[u8]) {
        if path.exists() { return; }
        let mut staged = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
        staged.write_all(bytes).unwrap();
        staged.persist_noclobber(path).unwrap();
    }

    fn security_examples(directory: &Path) -> Vec<Report> {
        std::fs::create_dir_all(directory).unwrap();
        let mut image = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1).write_to(&mut image, image::ImageFormat::Png).unwrap();
        let leaf = zip_bytes(&[("nested-note.pdf.exe", b"Harmless text fixture, not an executable.")]);
        let nested = zip_bytes(&[("level-two.zip", &leaf)]);
        let mut polyglot = b"GIF89a".to_vec();
        polyglot.extend_from_slice(&zip_bytes(&[("embedded.txt", b"Harmless embedded ZIP content.")]));
        let mut links = tar::Builder::new(Vec::new());
        for (name, target, kind) in [
            ("outside-symlink", "../../outside", tar::EntryType::Symlink),
            ("dir/outside-hardlink", "../outside", tar::EntryType::Link),
            ("cycle-a", "cycle-b", tar::EntryType::Symlink),
            ("cycle-b", "cycle-a", tar::EntryType::Symlink),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            links.append_link(&mut header, name, target).unwrap();
        }
        let links = links.into_inner().unwrap();
        let main = directory.join("35-security-audit.zip");
        store_security_example(&main, &zip_bytes(&[
            ("README.txt", b"Harmless audit fixtures. Run Audit archive, not extraction. Executable names contain text only; links exist only in the TAR metadata. Expected: executable, double-extension, ADS, signature mismatch, polyglot, link and structure warnings. Use nested audit depth 2 to inspect both ZIP levels."),
            ("documents/invoice.pdf.exe", b"Harmless text fixture. No executable code."),
            ("signatures/image.txt", image.get_ref()),
            ("polyglot/image-with-zip.gif", &polyglot),
            ("streams/notes.txt:review.txt", b"Harmless NTFS ADS filename fixture."),
            ("structures/CON.txt", b"Reserved Windows filename fixture."),
            ("structures/conflict", b"A regular file used as a parent path."),
            ("structures/conflict/child.txt", b"File/directory collision fixture."),
            ("links/suspicious-links.tar", &links),
            ("nested/level-one.zip", &nested),
        ]));
        let expansion = directory.join("36-bounded-expansion.zip");
        if !expansion.exists() {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            writer.start_file("zeros.txt", zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated).compression_level(Some(9))).unwrap();
            let block = [0u8; 65536];
            for _ in 0..192 { writer.write_all(&block).unwrap(); }
            store_security_example(&expansion, &writer.finish().unwrap().into_inner());
        }
        let parser = directory.join("37-parser-anomaly.zip");
        let mut bytes = zip_bytes(&[("first.txt", b"same"), ("second.txt", b"same")]);
        let mut reader = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let first = reader.by_index_raw(0).unwrap().header_start();
        let second = reader.by_index_raw(1).unwrap().central_header_start() as usize;
        drop(reader);
        bytes[second + 42..second + 46].copy_from_slice(&(first as u32).to_le_bytes());
        store_security_example(&parser, &bytes);

        let cancel = AtomicBool::new(false);
        let reports = [&main, &expansion, &parser].map(|path| inspect_with_options(
            path, None, InspectionMode::Audit, &cancel, AuditOptions { max_depth: 2 }));
        assert_eq!(reports[0].integrity, Integrity::Passed, "{:?}", reports[0].error);
        for kind in ["Suspicious executable", "Double extension", "Alternate data stream",
            "Polyglot file", "Format mismatch", "Suspicious link", "Suspicious structure", "Nested archive"] {
            assert!(reports[0].findings.iter().any(|finding| finding.kind == kind), "missing {kind}");
        }
        assert!(reports[0].findings.iter().any(|finding| finding.entry ==
            "nested/level-one.zip!level-two.zip!nested-note.pdf.exe" && finding.kind == "Double extension"));
        assert_eq!(reports[1].integrity, Integrity::Incomplete);
        assert!(reports[1].findings.iter().any(|finding| finding.kind == "Archive bomb"));
        assert_ne!(reports[2].integrity, Integrity::Passed);
        assert!(reports[2].findings.iter().any(|finding| finding.kind == "Parser anomaly"));
        reports.into_iter().collect()
    }

    #[test]
    fn security_examples_trigger_expected_findings_without_local_samples() {
        let root = tempfile::tempdir().unwrap();
        let reports = security_examples(root.path());
        assert_eq!(reports.len(), 3);
        assert!(!root.path().join("outside").exists());
        let original = std::fs::read(&reports[0].path).unwrap();
        security_examples(root.path());
        assert_eq!(std::fs::read(&reports[0].path).unwrap(), original);
    }

    #[test]
    #[ignore = "generates harmless manual security-audit examples"]
    fn generate_security_audit_samples() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        for report in security_examples(&directory) {
            println!("{}: {:?}", report.path.display(), report.integrity);
            let kinds = report.findings.iter().map(|finding| finding.kind).collect::<BTreeSet<_>>();
            println!("  Findings: {}", kinds.into_iter().collect::<Vec<_>>().join(", "));
        }
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
