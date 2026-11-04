use super::*;

#[derive(Clone, Copy, Debug)]
pub struct AuditOptions {
    pub max_depth: u8,
}

impl Default for AuditOptions {
    fn default() -> Self {
        Self { max_depth: 2 }
    }
}

#[derive(Clone, Copy)]
pub(super) struct AuditState {
    pub(super) options: AuditOptions,
    pub(super) depth: u8,
    pub(super) archives_left: usize,
    pub(super) bytes_left: u64,
}

impl AuditState {
    pub(super) fn new(options: AuditOptions) -> Self {
        Self {
            options: AuditOptions {
                max_depth: options.max_depth.min(8),
            },
            depth: 0,
            archives_left: MAX_NESTED_ARCHIVES,
            bytes_left: MAX_NESTED_TOTAL_BYTES,
        }
    }
}

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

    pub(super) fn max_entries(self) -> usize {
        if self.is_full_audit() {
            usize::MAX
        } else {
            MAX_ENTRIES
        }
    }

    pub(super) fn max_entry_bytes(self) -> u64 {
        if self.is_full_audit() {
            u64::MAX
        } else {
            MAX_ENTRY_BYTES
        }
    }

    pub(super) fn max_total_bytes(self) -> u64 {
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
    /// Last access timestamp from the filesystem, when available.
    pub accessed: Option<String>,
    /// Filesystem permission bits or a read-only summary.
    pub filesystem_permissions: Option<String>,
    /// Whether the archive file is read-only on the filesystem.
    pub filesystem_readonly: Option<bool>,
    /// Numeric filesystem owner identifier on Unix.
    pub filesystem_owner_id: Option<u32>,
    /// Numeric filesystem group identifier on Unix.
    pub filesystem_group_id: Option<u32>,
    /// Container-level comment, when the format provides one.
    pub archive_comment: Option<String>,
    /// SHA-256 of the archive file, when calculated.
    pub archive_sha256: Option<String>,
    /// Metadata and verification results for each inspected entry.
    pub entries: Vec<EntryInfo>,
    /// Findings produced by integrity and audit checks.
    pub findings: Vec<Finding>,
    pub(crate) content_signatures: BTreeMap<String, String>,
    /// Overall integrity outcome.
    pub integrity: Integrity,
    /// Whether any configured inspection limit was reached.
    pub limited: bool,
    /// Top-level error encountered while inspecting the archive.
    pub error: Option<String>,
    pub(super) full_audit: bool,
    pub(super) audit: AuditState,
    pub(super) audit_bytes_read: u64,
    pub(super) expansion_stopped: Arc<AtomicU8>,
}

impl Report {
    /// Serializes the report and active inspection limits to JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path, "detected_format": self.signature, "expected_format": self.expected.map(|format| format.label()),
            "archive_size": self.archive_size, "created": self.created, "modified": self.modified,
            "accessed": self.accessed, "filesystem_permissions": self.filesystem_permissions,
            "filesystem_readonly": self.filesystem_readonly, "filesystem_owner_id": self.filesystem_owner_id,
            "filesystem_group_id": self.filesystem_group_id, "archive_comment": self.archive_comment,
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
            "content_signatures": self.content_signatures,
            "full_audit": self.full_audit,
            "nested_audit": { "max_depth": self.audit.options.max_depth,
                "archives_inspected": MAX_NESTED_ARCHIVES - self.audit.archives_left,
                "bytes_consumed": MAX_NESTED_TOTAL_BYTES - self.audit.bytes_left },
            "limits": {
                "max_entries": (!self.full_audit).then_some(MAX_ENTRIES),
                "max_entry_bytes": (!self.full_audit).then_some(MAX_ENTRY_BYTES),
                "max_total_bytes": (!self.full_audit).then_some(MAX_TOTAL_BYTES),
                "max_findings": (!self.full_audit).then_some(MAX_FINDINGS),
                "max_sevenz_dictionary_bytes": MAX_SEVENZIP_DICTIONARY_BYTES,
                "max_nested_archives": MAX_NESTED_ARCHIVES,
                "max_nested_archive_bytes": MAX_NESTED_ARCHIVE_BYTES,
                "max_nested_total_bytes": MAX_NESTED_TOTAL_BYTES,
                "max_content_signatures": crate::security::MAX_SIGNATURES,
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
    pub(crate) fn finding(
        &mut self,
        kind: &'static str,
        entry: impl Into<String>,
        detail: impl Into<String>,
    ) {
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
