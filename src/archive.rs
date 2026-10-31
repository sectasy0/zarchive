//! Lists, creates, inspects, and safely extracts supported archive formats.

use std::{
    collections::BTreeSet,
    fs::File,
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use sevenz_rust::{Password, SevenZReader};
use tar::Archive as TarArchive;
use tempfile::{Builder, NamedTempFile};
use zip::ZipArchive;

#[derive(Clone)]
/// One archive entry or filesystem item in the shared browser representation.
pub struct ArchiveEntry {
    /// Name shown in the archive browser.
    pub name: String,
    /// Original filesystem or archive-relative path, when available.
    pub source_path: Option<String>,
    /// Uncompressed size in bytes.
    pub size: u64,
    pub modified: Option<chrono::NaiveDate>,
    /// Whether this entry represents a directory.
    pub is_directory: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// A supported archive container, independent of any outer compression stream.
pub enum ArchiveFormat {
    /// ZIP archive.
    #[default]
    Zip,
    /// TAR archive.
    Tar,
    /// 7z archive.
    SevenZip,
    /// RAR archive, supported for reading only.
    Rar,
    /// CPIO archive.
    Cpio,
    /// Unix AR archive.
    Ar,
    /// POSIX PAX archive.
    Pax,
    /// XAR archive.
    Xar,
}

impl ArchiveFormat {
    /// Archive container types currently recognized by the application.
    #[allow(dead_code)]
    pub const ALL: [Self; 8] = [
        Self::Zip,
        Self::Tar,
        Self::SevenZip,
        Self::Rar,
        Self::Cpio,
        Self::Ar,
        Self::Pax,
        Self::Xar,
    ];

    /// Returns the display name for this archive container.
    pub fn label(self) -> &'static str {
        match self {
            Self::Zip => "ZIP",
            Self::Tar => "TAR",
            Self::SevenZip => "7z",
            Self::Rar => "RAR",
            Self::Cpio => "CPIO",
            Self::Ar => "AR",
            Self::Pax => "PAX",
            Self::Xar => "XAR",
        }
    }

    /// Reports whether this container can be created with the current writer set.
    fn can_create(self) -> bool {
        !matches!(self, Self::Rar)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// A stream codec that may wrap a TAR archive or compress one standalone file.
pub enum CompressionFormat {
    /// No outer compression stream.
    #[default]
    None,
    /// GZIP stream.
    Gzip,
    /// BZIP2 stream.
    Bzip2,
    /// XZ stream.
    Xz,
    /// Zstandard stream.
    Zstandard,
    /// LZ4 frame stream.
    Lz4,
    /// LZIP stream.
    Lzip,
    /// Brotli stream.
    Brotli,
}

impl CompressionFormat {
    /// Codec choices available for wrapping a TAR archive.
    pub const ALL: [Self; 8] = [
        Self::None,
        Self::Gzip,
        Self::Bzip2,
        Self::Xz,
        Self::Zstandard,
        Self::Lz4,
        Self::Lzip,
        Self::Brotli,
    ];

    /// Returns the localized method label key used by the creation UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No compression",
            Self::Gzip => "GZIP compression",
            Self::Bzip2 => "BZIP2 compression",
            Self::Xz => "XZ compression",
            Self::Zstandard => "Zstandard compression",
            Self::Lz4 => "LZ4 compression",
            Self::Lzip => "LZIP compression",
            Self::Brotli => "Brotli compression",
        }
    }

    /// Returns the canonical suffix for a standalone stream or compressed TAR.
    fn extension(self) -> &'static str {
        match self {
            Self::None => "tar",
            Self::Gzip => "gz",
            Self::Bzip2 => "bz2",
            Self::Xz => "xz",
            Self::Zstandard => "zst",
            Self::Lz4 => "lz4",
            Self::Lzip => "lz",
            Self::Brotli => "br",
        }
    }

    fn tar_extension(self) -> &'static str {
        match self {
            Self::None => "tar",
            Self::Gzip => "tar.gz",
            Self::Bzip2 => "tar.bz2",
            Self::Xz => "tar.xz",
            Self::Zstandard => "tar.zst",
            Self::Lz4 => "tar.lz4",
            Self::Lzip => "tar.lz",
            Self::Brotli => "tar.br",
        }
    }

    fn supports_standalone(self) -> bool {
        self != Self::None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A disk-image or virtual-disk container format, reserved for future support.
pub enum ContainerFormat {
    /// ISO optical-disc image.
    Iso,
    /// Apple disk image.
    Dmg,
    /// QEMU copy-on-write disk image.
    Qcow2,
    /// Hyper-V virtual hard disk.
    Vhdx,
}

impl ContainerFormat {
    /// Returns the display label for this disk-image type.
    pub fn label(self) -> &'static str {
        match self {
            Self::Iso => "ISO",
            Self::Dmg => "DMG",
            Self::Qcow2 => "QCOW2",
            Self::Vhdx => "VHDX",
        }
    }

    /// Returns the canonical filename suffix for this container.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Iso => "iso",
            Self::Dmg => "dmg",
            Self::Qcow2 => "qcow2",
            Self::Vhdx => "vhdx",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Operations currently implemented for a resolved file format.
pub struct FormatCapabilities {
    /// Whether archive metadata can be listed.
    pub can_list: bool,
    /// Whether contents can be extracted.
    pub can_extract: bool,
    /// Whether new files can be created in this format.
    pub can_create: bool,
    /// Whether existing contents can be modified.
    pub can_modify: bool,
    /// Whether integrity/content checks are available.
    pub can_test: bool,
}

impl FormatCapabilities {
    const READ_ONLY_ARCHIVE: Self = Self {
        can_list: true,
        can_extract: true,
        can_create: false,
        can_modify: false,
        can_test: true,
    };

    const CREATEABLE_ARCHIVE: Self = Self {
        can_list: true,
        can_extract: true,
        can_create: true,
        can_modify: false,
        can_test: true,
    };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// A resolved file type composed from a container, compression stream, or disk image.
pub struct ResolvedFormat {
    /// Archive container, when the file contains an archive.
    pub archive: Option<ArchiveFormat>,
    /// Outer compression stream, if any.
    pub compression: CompressionFormat,
    /// Disk-image container, reserved for future readers.
    pub container: Option<ContainerFormat>,
}

#[allow(non_upper_case_globals)]
impl ResolvedFormat {
    /// Canonical resolved forms used by dispatchers and tests.
    pub const Zip: Self = Self::archive(ArchiveFormat::Zip, CompressionFormat::None);
    pub const Tar: Self = Self::archive(ArchiveFormat::Tar, CompressionFormat::None);
    pub const TarGz: Self = Self::archive(ArchiveFormat::Tar, CompressionFormat::Gzip);
    pub const TarBz2: Self = Self::archive(ArchiveFormat::Tar, CompressionFormat::Bzip2);
    pub const TarXz: Self = Self::archive(ArchiveFormat::Tar, CompressionFormat::Xz);
    pub const TarZstd: Self = Self::archive(ArchiveFormat::Tar, CompressionFormat::Zstandard);
    pub const TarLz4: Self = Self::archive(ArchiveFormat::Tar, CompressionFormat::Lz4);
    pub const TarLzip: Self = Self::archive(ArchiveFormat::Tar, CompressionFormat::Lzip);
    pub const TarBrotli: Self = Self::archive(ArchiveFormat::Tar, CompressionFormat::Brotli);
    pub const Gzip: Self = Self::standalone(CompressionFormat::Gzip);
    pub const Bzip2: Self = Self::standalone(CompressionFormat::Bzip2);
    pub const Xz: Self = Self::standalone(CompressionFormat::Xz);
    pub const Zstandard: Self = Self::standalone(CompressionFormat::Zstandard);
    pub const Lz4: Self = Self::standalone(CompressionFormat::Lz4);
    pub const Lzip: Self = Self::standalone(CompressionFormat::Lzip);
    pub const Brotli: Self = Self::standalone(CompressionFormat::Brotli);
    pub const SevenZip: Self = Self::archive(ArchiveFormat::SevenZip, CompressionFormat::None);
    pub const Rar: Self = Self::archive(ArchiveFormat::Rar, CompressionFormat::None);
    pub const Cpio: Self = Self::archive(ArchiveFormat::Cpio, CompressionFormat::None);
    pub const Ar: Self = Self::archive(ArchiveFormat::Ar, CompressionFormat::None);
    pub const Pax: Self = Self::archive(ArchiveFormat::Pax, CompressionFormat::None);
    pub const Xar: Self = Self::archive(ArchiveFormat::Xar, CompressionFormat::None);
    pub const Iso: Self = Self::disk_image(ContainerFormat::Iso);
    pub const Dmg: Self = Self::disk_image(ContainerFormat::Dmg);
    pub const Qcow2: Self = Self::disk_image(ContainerFormat::Qcow2);
    pub const Vhdx: Self = Self::disk_image(ContainerFormat::Vhdx);

    /// Concrete readable/writable formats covered by round-trip tests.
    #[cfg(test)]
    pub const ALL: [Self; 21] = [
        Self::Zip,
        Self::Tar,
        Self::TarGz,
        Self::TarBz2,
        Self::TarXz,
        Self::TarZstd,
        Self::TarLz4,
        Self::TarLzip,
        Self::TarBrotli,
        Self::Gzip,
        Self::Bzip2,
        Self::Xz,
        Self::Zstandard,
        Self::Lz4,
        Self::Lzip,
        Self::Brotli,
        Self::Cpio,
        Self::Ar,
        Self::Pax,
        Self::Xar,
        Self::SevenZip,
    ];

    /// Logical output choices presented by the creation form.
    pub const CREATION_FORMATS: [Self; 14] = [
        Self::Zip,
        Self::Tar,
        Self::Cpio,
        Self::Ar,
        Self::Pax,
        Self::Xar,
        Self::SevenZip,
        Self::Gzip,
        Self::Bzip2,
        Self::Xz,
        Self::Zstandard,
        Self::Lz4,
        Self::Lzip,
        Self::Brotli,
    ];

    const fn archive(archive: ArchiveFormat, compression: CompressionFormat) -> Self {
        Self {
            archive: Some(archive),
            compression,
            container: None,
        }
    }

    /// Builds a TAR container with the selected compression codec.
    pub(crate) const fn tar(compression: CompressionFormat) -> Self {
        Self::archive(ArchiveFormat::Tar, compression)
    }

    const fn standalone(compression: CompressionFormat) -> Self {
        Self {
            archive: None,
            compression,
            container: None,
        }
    }

    const fn disk_image(container: ContainerFormat) -> Self {
        Self {
            archive: None,
            compression: CompressionFormat::None,
            container: Some(container),
        }
    }

    /// Reports whether the resolved archive type supports password encryption.
    pub fn supports_password(self) -> bool {
        self.container.is_none()
            && self.compression == CompressionFormat::None
            && matches!(
                self.archive,
                Some(ArchiveFormat::Zip | ArchiveFormat::SevenZip)
            )
    }

    /// Returns supported operations for this exact archive, codec, or image combination.
    pub fn capabilities(self) -> FormatCapabilities {
        match (self.archive, self.compression, self.container) {
            (Some(archive), codec, None)
                if archive.can_create()
                    && (archive == ArchiveFormat::Tar || codec == CompressionFormat::None) =>
            {
                FormatCapabilities {
                    can_modify: true,
                    ..FormatCapabilities::CREATEABLE_ARCHIVE
                }
            }
            (Some(ArchiveFormat::Rar), CompressionFormat::None, None) => FormatCapabilities {
                can_modify: rar_program().is_some(),
                ..FormatCapabilities::READ_ONLY_ARCHIVE
            },
            (None, codec, None) if codec.supports_standalone() => {
                FormatCapabilities::CREATEABLE_ARCHIVE
            }
            _ => FormatCapabilities::default(),
        }
    }

    /// Returns the display label for this resolved file format.
    pub fn label(self) -> &'static str {
        if let Some(container) = self.container {
            return container.label();
        }
        match self.archive {
            Some(ArchiveFormat::Tar) => match self.compression {
                CompressionFormat::None => "TAR",
                CompressionFormat::Gzip => "TAR.GZ",
                CompressionFormat::Bzip2 => "TAR.BZ2",
                CompressionFormat::Xz => "TAR.XZ",
                CompressionFormat::Zstandard => "TAR.ZST",
                CompressionFormat::Lz4 => "TAR.LZ4",
                CompressionFormat::Lzip => "TAR.LZ",
                CompressionFormat::Brotli => "TAR.BR",
            },
            Some(archive) => archive.label(),
            None => match self.compression {
                CompressionFormat::None => "Unknown",
                CompressionFormat::Gzip => "GZIP",
                CompressionFormat::Bzip2 => "BZIP2",
                CompressionFormat::Xz => "XZ",
                CompressionFormat::Zstandard => "Zstandard",
                CompressionFormat::Lz4 => "LZ4",
                CompressionFormat::Lzip => "LZIP",
                CompressionFormat::Brotli => "Brotli",
            },
        }
    }

    /// Returns the canonical suffix for this resolved file format.
    pub fn extension(self) -> &'static str {
        if let Some(container) = self.container {
            return container.extension();
        }
        match self.archive {
            Some(ArchiveFormat::Tar) => self.compression.tar_extension(),
            Some(ArchiveFormat::Zip) => "zip",
            Some(ArchiveFormat::SevenZip) => "7z",
            Some(ArchiveFormat::Rar) => "rar",
            Some(ArchiveFormat::Cpio) => "cpio",
            Some(ArchiveFormat::Ar) => "ar",
            Some(ArchiveFormat::Pax) => "pax",
            Some(ArchiveFormat::Xar) => "xar",
            None => self.compression.extension(),
        }
    }

    /// Returns compression levels supported by this archive/codec combination.
    pub fn compression_levels(self) -> &'static [CompressionLevel] {
        use CompressionLevel::*;
        match (self.archive, self.compression, self.container) {
            (Some(ArchiveFormat::Tar), CompressionFormat::None, None) => &[Stored],
            (Some(ArchiveFormat::Tar) | None, CompressionFormat::Lz4, None) => &[Fast],
            (Some(ArchiveFormat::Tar), _, None) => &[Fast, Normal, Best],
            (None, codec, None) if codec.supports_standalone() => &[Fast, Normal, Best],
            (Some(ArchiveFormat::SevenZip), CompressionFormat::None, None) => &[Fast, Normal, Best],
            (Some(ArchiveFormat::Rar), CompressionFormat::None, None) => &[Stored],
            (
                Some(ArchiveFormat::Cpio | ArchiveFormat::Ar | ArchiveFormat::Pax),
                CompressionFormat::None,
                None,
            ) => &[Stored],
            (Some(ArchiveFormat::Xar), CompressionFormat::None, None) => &[Normal],
            (Some(ArchiveFormat::Zip), CompressionFormat::None, None) => {
                &[Stored, Fast, Normal, Best]
            }
            _ => &[],
        }
    }

    /// Reports whether this resolved format wraps a TAR archive.
    pub(crate) fn is_tar(self) -> bool {
        self.archive == Some(ArchiveFormat::Tar) && self.container.is_none()
    }

    pub(crate) fn is_extended_archive(self) -> bool {
        matches!(self, Self::Cpio | Self::Ar | Self::Pax | Self::Xar)
    }

    /// Reports whether this format compresses one file without an archive container.
    pub(crate) fn is_single_compressed(self) -> bool {
        self.archive.is_none() && self.container.is_none() && self.compression.supports_standalone()
    }

    /// Resolves a supported extension to its archive, compression, or disk-image layers.
    pub(crate) fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".tar.bz2") || name.ends_with(".tbz") || name.ends_with(".tbz2") {
            Some(Self::TarBz2)
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Some(Self::TarXz)
        } else if name.ends_with(".tar.zst") {
            Some(Self::TarZstd)
        } else if name.ends_with(".tar.lz4") {
            Some(Self::TarLz4)
        } else if name.ends_with(".tar.lz") {
            Some(Self::TarLzip)
        } else if name.ends_with(".tar.br") {
            Some(Self::TarBrotli)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if name.ends_with(".tar") {
            Some(Self::Tar)
        } else if name.ends_with(".gz") {
            Some(Self::Gzip)
        } else if name.ends_with(".bz2") {
            Some(Self::Bzip2)
        } else if name.ends_with(".xz") {
            Some(Self::Xz)
        } else if name.ends_with(".zst") {
            Some(Self::Zstandard)
        } else if name.ends_with(".lz4") {
            Some(Self::Lz4)
        } else if name.ends_with(".lz") {
            Some(Self::Lzip)
        } else if name.ends_with(".br") {
            Some(Self::Brotli)
        } else if name.ends_with(".zip") {
            Some(Self::Zip)
        } else if name.ends_with(".7z") {
            Some(Self::SevenZip)
        } else if name.ends_with(".rar") {
            Some(Self::Rar)
        } else if name.ends_with(".cpio") {
            Some(Self::Cpio)
        } else if name.ends_with(".ar") || name.ends_with(".a") {
            Some(Self::Ar)
        } else if name.ends_with(".pax") {
            Some(Self::Pax)
        } else if name.ends_with(".xar") {
            Some(Self::Xar)
        } else if name.ends_with(".iso") {
            Some(Self::Iso)
        } else if name.ends_with(".dmg") {
            Some(Self::Dmg)
        } else if name.ends_with(".qcow2") {
            Some(Self::Qcow2)
        } else if name.ends_with(".vhdx") {
            Some(Self::Vhdx)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// User-facing compression presets mapped to format-specific encoder settings.
pub enum CompressionLevel {
    /// Stores file contents without compression.
    Stored,
    /// Favors faster encoding over compression ratio.
    Fast,
    /// Balances compression speed and output size.
    #[default]
    Normal,
    /// Favors the smallest output over encoding speed.
    Best,
}

impl CompressionLevel {
    /// Returns the display label used by the interface.
    pub fn label(self) -> &'static str {
        match self {
            Self::Stored => "No compression",
            Self::Fast => "Fast",
            Self::Normal => "Normal",
            Self::Best => "Best compression",
        }
    }

    /// Maps this preset to the numeric level used by compatible encoders.
    fn level(self) -> u32 {
        match self {
            Self::Stored => 0,
            Self::Fast => 1,
            Self::Normal => 6,
            Self::Best => 9,
        }
    }
}

#[derive(Clone, Copy, Debug)]
/// Options controlling archive format, compression, filtering, and encryption.
pub struct CreateOptions {
    /// Output archive format.
    pub format: ResolvedFormat,
    /// Requested compression preset.
    pub compression: CompressionLevel,
    /// Whether dot-prefixed source files are included.
    pub include_hidden: bool,
    /// Whether ZIP64 extensions are enabled for ZIP output.
    pub zip64: bool,
    /// Whether compatible 7z entries are grouped into solid blocks.
    pub solid: bool,
    /// 7z compression dictionary size in mebibytes.
    pub dictionary_mib: u32,
    /// Whether the 7z archive header is encrypted with the content.
    pub encrypt_headers: bool,
    /// Whether TAR metadata is normalized for reproducible output.
    pub deterministic: bool,
}

impl Default for CreateOptions {
    /// Returns the standard ZIP creation settings.
    fn default() -> Self {
        Self {
            format: ResolvedFormat::Zip,
            compression: CompressionLevel::Normal,
            include_hidden: true,
            zip64: false,
            solid: false,
            dictionary_mib: 8,
            encrypt_headers: true,
            deterministic: false,
        }
    }
}

impl CreateOptions {
    /// Resolves the selected type and TAR method to a concrete output format.
    pub(crate) fn output_format(self) -> ResolvedFormat {
        self.format
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Distinguishes missing credentials from an invalid password.
pub enum PasswordFailure {
    /// The archive requires a password that was not provided.
    Required,
    /// The supplied password is invalid or the encrypted data is damaged.
    Incorrect,
}

impl std::fmt::Display for PasswordFailure {
    /// Writes a generic user-safe description of the password failure.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Required => "Password required",
            Self::Incorrect => "Incorrect password or damaged archive",
        })
    }
}

impl std::error::Error for PasswordFailure {}

/// Normalizes 7z password-related errors into the archive password error type.
fn seven_zip_error(error: sevenz_rust::Error, password: Option<&str>) -> anyhow::Error {
    match error {
        sevenz_rust::Error::PasswordRequired => PasswordFailure::Required.into(),
        sevenz_rust::Error::MaybeBadPassword(_) => PasswordFailure::Incorrect.into(),
        sevenz_rust::Error::ChecksumVerificationFailed
        | sevenz_rust::Error::BadTerminatedheader(_)
            if password.is_some() =>
        {
            PasswordFailure::Incorrect.into()
        }
        error => error.into(),
    }
}

/// Reports whether the path has a supported archive filename extension.
pub fn is_supported_archive(path: &Path) -> bool {
    ResolvedFormat::from_path(path).is_some_and(|format| format.capabilities().can_list)
}

/// Reports whether the resolved file format supports content extraction.
pub fn can_extract_archive(path: &Path) -> bool {
    ResolvedFormat::from_path(path).is_some_and(|format| format.capabilities().can_extract)
}

/// Lists archive entries without supplying a password.
pub fn list_archive(path: &Path) -> Result<Vec<ArchiveEntry>> {
    list_archive_with_password(path, None)
}

/// Lists entries from a supported archive, optionally using a password.
pub fn list_archive_with_password(
    path: &Path,
    password: Option<&str>,
) -> Result<Vec<ArchiveEntry>> {
    let format = ResolvedFormat::from_path(path)
        .with_context(|| format!("Unsupported archive format: {}", path.display()))?;
    anyhow::ensure!(
        format.capabilities().can_list,
        "This format cannot be listed"
    );

    if format.is_tar() {
        return list_tar(open_tar_reader(path, format)?, path);
    }
    if format.is_single_compressed() {
        return list_single_compressed(path, format);
    }
    if format.is_extended_archive() {
        return list_extended_archive(path);
    }
    match format {
        ResolvedFormat::Zip => list_zip(path, password),
        ResolvedFormat::SevenZip => list_seven_zip(path, password),
        ResolvedFormat::Rar => list_rar(path, password),
        _ => unreachable!("TAR formats are handled before this match"),
    }
}

/// Opens a compressed TAR stream through the decoder selected by its filename format.
fn open_tar_reader(path: &Path, format: ResolvedFormat) -> Result<Box<dyn Read>> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    decode_archive_stream(file, format)
}

pub(crate) struct ExtendedEntryReader<'a, 'b>(pub(crate) &'a mut libarchive2::ReadArchive<'b>);

impl Read for ExtendedEntryReader<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read_data(buffer).map_err(io::Error::other)
    }
}

fn list_extended_archive(path: &Path) -> Result<Vec<ArchiveEntry>> {
    if ResolvedFormat::from_path(path) == Some(ResolvedFormat::Xar) {
        let reader = crate::xar::XarReader::new(File::open(path)?)?;
        return Ok(reader
            .entries
            .iter()
            .map(|entry| ArchiveEntry {
                name: entry.path.clone(),
                source_path: Some(entry.path.clone()),
                size: entry.data.as_ref().map_or(0, |data| data.size),
                modified: None,
                is_directory: entry.file_type == xara::XarFileType::Directory,
            })
            .collect());
    }
    let mut reader = libarchive2::ReadArchive::open(path)?;
    let mut entries = Vec::new();
    while let Some(entry) = reader.next_entry()? {
        crate::task_queue::checkpoint()?;
        let name = entry.pathname().context("Archive entry has no filename")?;
        entries.push(ArchiveEntry {
            name: name.clone(),
            source_path: Some(name),
            size: u64::try_from(entry.size()).context("Invalid archive entry size")?,
            modified: entry
                .mtime()
                .map(|time| chrono::DateTime::<chrono::Local>::from(time).date_naive()),
            is_directory: entry.file_type() == libarchive2::FileType::Directory,
        });
    }
    Ok(entries)
}

fn validate_extended_entry(entry: &libarchive2::Entry<'_>) -> Result<()> {
    anyhow::ensure!(
        matches!(
            entry.file_type(),
            libarchive2::FileType::RegularFile | libarchive2::FileType::Directory
        ) && entry.symlink().is_none()
            && entry.hardlink().is_none(),
        "Only regular files and directories are extracted"
    );
    Ok(())
}

fn extract_extended_entry(path: &Path, name: &str, output: &mut impl Write) -> Result<()> {
    if ResolvedFormat::from_path(path) == Some(ResolvedFormat::Xar) {
        let mut reader = crate::xar::XarReader::new(File::open(path)?)?;
        let entry = reader
            .entries
            .iter()
            .find(|entry| entry.path == name)
            .cloned()
            .context("Archive entry not found")?;
        anyhow::ensure!(entry.link.is_none(), "Links in archives are not extracted");
        crate::task_queue::copy(&mut reader.entry_reader(&entry)?, output)?;
        return Ok(());
    }
    let mut reader = libarchive2::ReadArchive::open(path)?;
    while let Some(entry) = reader.next_entry()? {
        if entry.pathname().as_deref() != Some(name) {
            continue;
        }
        validate_extended_entry(&entry)?;
        anyhow::ensure!(
            entry.file_type() == libarchive2::FileType::RegularFile,
            "Cannot open a directory as a document"
        );
        crate::task_queue::copy(&mut ExtendedEntryReader(&mut reader), output)?;
        return Ok(());
    }
    anyhow::bail!("Archive entry not found: {name}")
}

fn extract_extended_tree(path: &Path, destination: &Path, selection: Option<&Path>) -> Result<()> {
    if ResolvedFormat::from_path(path) == Some(ResolvedFormat::Xar) {
        let mut reader = crate::xar::XarReader::new(File::open(path)?)?;
        for entry in reader.entries.clone() {
            anyhow::ensure!(
                entry.file_type != xara::XarFileType::Symlink && entry.link.is_none(),
                "Links in archives are not extracted"
            );
            let directory = entry.file_type == xara::XarFileType::Directory;
            if directory {
                write_extracted_entry(destination, &entry.path, true, io::empty(), selection)?;
            } else {
                write_extracted_entry(
                    destination,
                    &entry.path,
                    false,
                    reader.entry_reader(&entry)?,
                    selection,
                )?;
            }
        }
        return Ok(());
    }
    let mut reader = libarchive2::ReadArchive::open(path)?;
    while let Some(entry) = reader.next_entry()? {
        validate_extended_entry(&entry)?;
        let name = entry.pathname().context("Archive entry has no filename")?;
        let is_directory = entry.file_type() == libarchive2::FileType::Directory;
        write_extracted_entry(
            destination,
            &name,
            is_directory,
            ExtendedEntryReader(&mut reader),
            selection,
        )?;
    }
    Ok(())
}

/// Lists the single decompressed file represented by a standalone codec stream.
fn list_single_compressed(path: &Path, format: ResolvedFormat) -> Result<Vec<ArchiveEntry>> {
    let name = single_compressed_entry_name(path)?;
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut reader = decode_archive_stream(file, format)?;
    let size = crate::task_queue::copy(&mut reader, &mut io::sink())?;
    Ok(vec![ArchiveEntry {
        name: name.clone(),
        source_path: Some(name),
        size,
        is_directory: false,
        modified: None,
    }])
}

/// Removes a standalone compression suffix to recover the decompressed filename.
pub(crate) fn single_compressed_entry_name(path: &Path) -> Result<String> {
    let format = ResolvedFormat::from_path(path).context("Unsupported compressed file format")?;
    anyhow::ensure!(
        format.capabilities().can_extract && format.is_single_compressed(),
        "Expected a standalone compressed file"
    );
    let filename = path
        .file_name()
        .context("Compressed file has no filename")?
        .to_string_lossy();
    let suffix = format!(".{}", format.extension());
    let lower = filename.to_ascii_lowercase();
    let stem = if lower.ends_with(&suffix) {
        &filename[..filename.len() - suffix.len()]
    } else {
        filename.as_ref()
    };
    if stem.is_empty() || stem == "." || stem == ".." {
        Ok("file".to_owned())
    } else {
        Ok(stem.to_owned())
    }
}

/// Reports whether a path names a standalone compressed file rather than an archive container.
pub(crate) fn is_single_compressed_archive(path: &Path) -> bool {
    ResolvedFormat::from_path(path).is_some_and(ResolvedFormat::is_single_compressed)
}

/// Decompresses one standalone stream to a new file without overwriting existing data.
pub(crate) fn extract_single_compressed_to(path: &Path, destination: &Path) -> Result<()> {
    let format = ResolvedFormat::from_path(path).context("Unsupported compressed file format")?;
    anyhow::ensure!(
        format.is_single_compressed(),
        "Expected a standalone compressed file"
    );
    anyhow::ensure!(
        std::fs::symlink_metadata(destination).is_err(),
        "The destination already exists"
    );
    let name = single_compressed_entry_name(path)?;
    let mut extracted = extract_entry_with_password(path, &name, None)?;
    let parent = destination
        .parent()
        .context("The destination has no parent directory")?;
    let mut staged = NamedTempFile::new_in(parent)?;
    crate::task_queue::copy(extracted.as_file_mut(), staged.as_file_mut())?;
    staged
        .persist_noclobber(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

/// Wraps an input stream in the decompressor associated with an archive format.
pub(crate) fn decode_archive_stream<'a>(
    reader: impl Read + 'a,
    format: ResolvedFormat,
) -> Result<Box<dyn Read + 'a>> {
    let decoded: Box<dyn Read + 'a> = match format {
        ResolvedFormat::Tar => Box::new(reader),
        ResolvedFormat::TarGz | ResolvedFormat::Gzip => Box::new(GzDecoder::new(reader)),
        ResolvedFormat::TarBz2 | ResolvedFormat::Bzip2 => {
            Box::new(bzip2::read::BzDecoder::new(reader))
        }
        ResolvedFormat::TarXz | ResolvedFormat::Xz => {
            Box::new(lzma_rust2::XzReader::new(reader, true))
        }
        ResolvedFormat::TarZstd | ResolvedFormat::Zstandard => {
            Box::new(zstd::stream::read::Decoder::new(reader)?)
        }
        ResolvedFormat::TarLz4 | ResolvedFormat::Lz4 => {
            Box::new(lz4_flex::frame::FrameDecoder::new(reader))
        }
        ResolvedFormat::TarLzip | ResolvedFormat::Lzip => {
            Box::new(lzma_rust2::LzipReader::new(reader))
        }
        ResolvedFormat::TarBrotli | ResolvedFormat::Brotli => {
            Box::new(brotli::Decompressor::new(reader, 4096))
        }
        _ => anyhow::bail!("Unsupported TAR compression format"),
    };
    Ok(decoded)
}

#[cfg(test)]
/// Creates a ZIP archive using the default creation options.
pub fn create_zip(path: &Path, source_paths: &[PathBuf]) -> Result<()> {
    create_archive(path, source_paths, CreateOptions::default())
}

/// Creates an unencrypted archive using the supplied options.
pub fn create_archive(path: &Path, source_paths: &[PathBuf], options: CreateOptions) -> Result<()> {
    create_archive_with_password(path, source_paths, options, None)
}

/// Validates inputs, writes to a staged file, then publishes the completed archive.
pub fn create_archive_with_password(
    path: &Path,
    source_paths: &[PathBuf],
    options: CreateOptions,
    password: Option<&str>,
) -> Result<()> {
    create_archive_contents(path, source_paths, options, password, false)
}

fn create_archive_contents(
    path: &Path,
    source_paths: &[PathBuf],
    options: CreateOptions,
    password: Option<&str>,
    allow_empty: bool,
) -> Result<()> {
    let format = options.output_format();
    anyhow::ensure!(
        format.capabilities().can_create,
        "This format cannot be created"
    );
    anyhow::ensure!(
        password.is_none() || format.supports_password(),
        "This format does not support encryption"
    );
    anyhow::ensure!(
        format != ResolvedFormat::Rar,
        "RAR archives cannot be created"
    );
    anyhow::ensure!(
        password.is_none_or(|password| !password.is_empty()),
        "The password cannot be empty"
    );
    anyhow::ensure!(
        format != ResolvedFormat::SevenZip || [1, 8, 32, 64].contains(&options.dictionary_mib),
        "Unsupported dictionary size"
    );
    if source_paths.is_empty() && !allow_empty {
        anyhow::bail!("Select at least one file or folder to create an archive");
    }
    anyhow::ensure!(
        options
            .output_format()
            .compression_levels()
            .contains(&options.compression),
        "Unsupported compression level for {}",
        format.label()
    );
    anyhow::ensure!(
        ResolvedFormat::from_path(path) == Some(format),
        "The output extension must be .{}",
        format.extension()
    );
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(parent).context("The output directory is unavailable")?;
    let output_path = if path.exists() {
        std::fs::canonicalize(path)?
    } else {
        parent.join(
            path.file_name()
                .context("The output path has no file name")?,
        )
    };
    if format.is_single_compressed() {
        anyhow::ensure!(
            source_paths.len() == 1,
            "Standalone compression requires exactly one file"
        );
        let source = std::fs::canonicalize(&source_paths[0])?;
        anyhow::ensure!(
            source.is_file(),
            "Standalone compression requires a regular file"
        );
    }
    let mut entry_names = BTreeSet::new();
    let mut entries = Vec::new();
    for source_path in source_paths {
        crate::task_queue::checkpoint()?;
        let source_path = std::fs::canonicalize(source_path)
            .with_context(|| format!("Failed to resolve {}", source_path.display()))?;
        anyhow::ensure!(
            source_path != output_path,
            "The archive output cannot overwrite a selected source file"
        );
        anyhow::ensure!(
            !source_path.is_dir() || !output_path.starts_with(&source_path),
            "The archive output cannot be inside a selected folder"
        );
        let entry_name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .context("Selected source path has no valid UTF-8 file name")?
            .to_owned();
        collect_archive_sources(
            &source_path,
            entry_name,
            options.include_hidden,
            &mut entry_names,
            &mut entries,
        )?;
    }
    anyhow::ensure!(
        allow_empty || !entries.is_empty(),
        "No supported files or folders were selected"
    );
    let total_bytes = entries.iter().filter(|(_, _, directory)| !directory).try_fold(0_u64, |total, (source, _, _)| {
        Ok::<_, io::Error>(total.saturating_add(source.metadata()?.len()))
    })?;
    crate::task_queue::report_byte_total(total_bytes);
    let mut output =
        NamedTempFile::new_in(&parent).context("Failed to create temporary archive")?;
    match format {
        ResolvedFormat::Zip => {
            let mut archive = zip::ZipWriter::new(output.as_file_mut());
            let mut file_options = zip::write::SimpleFileOptions::default()
                .compression_method(if options.compression == CompressionLevel::Stored {
                    zip::CompressionMethod::Stored
                } else {
                    zip::CompressionMethod::Deflated
                })
                .compression_level(if options.compression == CompressionLevel::Stored {
                    None
                } else {
                    Some(options.compression.level() as i64)
                })
                .large_file(options.zip64);
            if let Some(password) = password {
                file_options = file_options.with_aes_encryption(zip::AesMode::Aes256, password);
            }
            for (source, name, is_directory) in &entries {
                crate::task_queue::checkpoint()?;
                if *is_directory {
                    archive.add_directory(format!("{name}/"), file_options)?;
                } else {
                    archive.start_file(
                        name,
                        file_options.large_file(
                            options.zip64 || source.metadata()?.len() >= u32::MAX as u64,
                        ),
                    )?;
                    crate::task_queue::copy(&mut File::open(source)?, &mut archive)?;
                }
            }
            archive.finish()?;
        }
        ResolvedFormat::Tar => write_tar(output.as_file_mut(), &entries, options.deterministic)?,
        ResolvedFormat::Cpio | ResolvedFormat::Ar | ResolvedFormat::Pax | ResolvedFormat::Xar => {
            write_extended_archive(output.path(), &entries, format)?;
        }
        ResolvedFormat::TarGz
        | ResolvedFormat::TarBz2
        | ResolvedFormat::TarXz
        | ResolvedFormat::TarZstd
        | ResolvedFormat::TarLz4
        | ResolvedFormat::TarLzip
        | ResolvedFormat::TarBrotli => {
            write_compressed_tar(output.as_file_mut(), &entries, format, options)?;
        }
        ResolvedFormat::Gzip
        | ResolvedFormat::Bzip2
        | ResolvedFormat::Xz
        | ResolvedFormat::Zstandard
        | ResolvedFormat::Lz4
        | ResolvedFormat::Lzip
        | ResolvedFormat::Brotli => {
            let (source, _, is_directory) = entries
                .first()
                .context("Standalone compression requires one source file")?;
            anyhow::ensure!(
                !is_directory,
                "Standalone compression requires a regular file"
            );
            write_compressed_file(
                output.as_file_mut(),
                source,
                format,
                options.compression.level(),
            )?;
        }
        ResolvedFormat::SevenZip => {
            let mut archive = sevenz_rust::SevenZWriter::new(output.as_file_mut())?;
            let preset = match options.compression {
                CompressionLevel::Fast => 1,
                CompressionLevel::Normal => 5,
                CompressionLevel::Best => 7,
                CompressionLevel::Stored => unreachable!(),
            };
            let mut lzma = sevenz_rust::lzma::LZMA2Options::with_preset(preset);
            lzma.dict_size = options.dictionary_mib * 1024 * 1024;
            let mut methods = Vec::new();
            if let Some(password) = password {
                methods.push(sevenz_rust::AesEncoderOptions::new(Password::from(password)).into());
            }
            methods.push(lzma.into());
            archive.set_content_methods(methods);
            archive.set_encrypt_header(password.is_some() && options.encrypt_headers);
            let mut solid_entries = Vec::new();
            let mut readers = Vec::new();
            let mut solid_size = 0_u64;
            for (source, name, is_directory) in &entries {
                let entry = sevenz_rust::SevenZArchiveEntry::from_path(source, name.clone());
                let file_size = source.metadata()?.len();
                if options.solid && !is_directory && file_size > 0 && file_size < u32::MAX as u64 {
                    if readers.len() >= 128 || solid_size + file_size > u32::MAX as u64 {
                        archive.push_archive_entries(
                            std::mem::take(&mut solid_entries),
                            sevenz_rust::SeqReader::new(std::mem::take(&mut readers)),
                        )?;
                        solid_size = 0;
                    }
                    solid_entries.push(entry);
                    readers.push(sevenz_rust::SourceReader::new(crate::task_queue::TaskReader(File::open(source)?)));
                    solid_size += file_size;
                    continue;
                }
                let reader = if *is_directory {
                    None
                } else {
                    Some(crate::task_queue::TaskReader(File::open(source)?))
                };
                archive.push_archive_entry(entry, reader)?;
            }
            if !solid_entries.is_empty() {
                archive
                    .push_archive_entries(solid_entries, sevenz_rust::SeqReader::new(readers))?;
            }
            archive.finish()?;
        }
        ResolvedFormat::Rar => anyhow::bail!("RAR archives cannot be created"),
        _ => anyhow::bail!("Unsupported resolved format for archive creation"),
    }
    output.as_file().sync_all()?;
    crate::task_queue::checkpoint()?;
    output
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to save archive {}", path.display()))?;
    Ok(())
}

fn rar_program() -> Option<&'static Path> {
    static PROGRAM: std::sync::LazyLock<Option<PathBuf>> = std::sync::LazyLock::new(|| {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join(if cfg!(windows) { "rar.exe" } else { "rar" }))
                .find(|path| path.is_file())
        })
    });
    PROGRAM.as_deref()
}

pub(crate) fn can_add_to_archive(path: &Path) -> bool {
    ResolvedFormat::from_path(path).is_some_and(|format| format.capabilities().can_modify)
}

#[cfg(test)]
pub(crate) fn add_to_archive(path: &Path, sources: &[PathBuf], folder: &str) -> Result<()> {
    add_to_archive_with_password(path, sources, folder, None)
}

pub(crate) fn add_to_archive_with_password(
    path: &Path,
    sources: &[PathBuf],
    folder: &str,
    password: Option<&str>,
) -> Result<()> {
    let format = ResolvedFormat::from_path(path).context("Unsupported archive format")?;
    anyhow::ensure!(
        format.archive.is_some(),
        "A single compressed stream cannot contain additional files"
    );
    if format == ResolvedFormat::Rar {
        anyhow::ensure!(
            rar_program().is_some(),
            "Install the rar command-line tool to modify RAR archives"
        );
    }
    anyhow::ensure!(
        format.capabilities().can_modify,
        "This archive format cannot be modified"
    );
    if format != ResolvedFormat::Zip {
        return rebuild_archive_with_additions(path, sources, folder, password, format);
    }
    anyhow::ensure!(
        !sources.is_empty() && sources.len() <= 256,
        "No supported files or folders were selected"
    );
    let path = std::fs::canonicalize(path)?;
    let original = path.metadata()?;
    let prefix = if folder.is_empty() {
        String::new()
    } else {
        format!(
            "{}/",
            safe_extraction_path(folder)?
                .to_string_lossy()
                .replace('\\', "/")
        )
    };
    let mut existing = ZipArchive::new(File::open(&path)?)?;
    let mut names = BTreeSet::new();
    for index in 0..existing.len() {
        let entry = existing.by_index_raw(index)?;
        let name = safe_extraction_path(&entry.name()?)?
            .to_string_lossy()
            .replace('\\', "/");
        anyhow::ensure!(
            names.insert(name.trim_end_matches('/').to_lowercase()),
            "The archive contains duplicate names and cannot be rebuilt safely"
        );
    }
    let encrypted = (0..existing.len()).try_fold(false, |encrypted, index| {
        existing
            .by_index_raw(index)
            .map(|entry| encrypted || entry.encrypted())
    })?;
    if encrypted {
        let password = password.ok_or(PasswordFailure::Required)?;
        for index in 0..existing.len() {
            let mut entry = existing
                .by_index_decrypt(index, password.as_bytes())
                .map_err(|_| PasswordFailure::Incorrect)?;
            crate::task_queue::copy(&mut entry, &mut io::sink()).map_err(|_| PasswordFailure::Incorrect)?;
        }
    }
    drop(existing);
    let mut entries = Vec::new();
    for source in sources {
        let source = std::fs::canonicalize(source)?;
        anyhow::ensure!(
            source != path && (!source.is_dir() || !path.starts_with(&source)),
            "An archive cannot contain itself"
        );
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .context("A source file name is not valid UTF-8")?;
        collect_archive_sources(
            &source,
            format!("{prefix}{name}"),
            true,
            &mut names,
            &mut entries,
        )?;
    }
    anyhow::ensure!(
        !entries.is_empty(),
        "No supported files or folders were selected"
    );
    let mut output = NamedTempFile::new_in(
        path.parent()
            .context("The destination has no parent directory")?,
    )?;
    crate::task_queue::copy(&mut File::open(&path)?, output.as_file_mut())?;
    let mut writer = zip::ZipWriter::new_append(output.as_file_mut())?;
    let mut options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    if encrypted {
        options = options.with_aes_encryption(zip::AesMode::Aes256, password.unwrap());
    }
    for (source, name, directory) in entries {
        if directory {
            writer.add_directory(format!("{name}/"), options)?;
        } else {
            writer.start_file(
                name,
                options.large_file(source.metadata()?.len() >= u32::MAX as u64),
            )?;
            crate::task_queue::copy(&mut File::open(source)?, &mut writer)?;
        }
    }
    writer.finish()?;
    let current = path.metadata()?;
    anyhow::ensure!(
        original.len() == current.len() && original.modified()? == current.modified()?,
        "The archive changed while files were being added"
    );
    output.as_file().set_permissions(original.permissions())?;
    output.as_file().sync_all()?;
    output.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn remove_archive_entry(path: &Path, name: &str, password: Option<&str>) -> Result<()> {
    let format = ResolvedFormat::from_path(path).context("Unsupported archive format")?;
    anyhow::ensure!(
        format.capabilities().can_modify,
        "This archive format cannot be modified"
    );
    let selected = safe_extraction_path(name)?;
    if format != ResolvedFormat::Zip {
        return rebuild_archive_with_changes(path, &[], "", password, format, Some(&selected));
    }
    let path = std::fs::canonicalize(path)?;
    let original = path.metadata()?;
    let mut existing = ZipArchive::new(File::open(&path)?)?;
    anyhow::ensure!(
        !existing.metadata().has_duplicate_names(),
        "The archive contains duplicate names and cannot be rebuilt safely"
    );
    let legacy_encryption = (0..existing.len()).try_fold(false, |legacy, index| {
        let entry = existing.by_index_raw(index)?;
        Ok::<_, zip::result::ZipError>(
            legacy
                || (entry.encrypted()
                    && !entry
                        .extra_data_fields()
                        .any(|field| matches!(field, zip::ExtraField::AeXEncryption(_)))),
        )
    })?;
    if legacy_encryption {
        drop(existing);
        return rebuild_archive_with_changes(&path, &[], "", password, format, Some(&selected));
    }
    let mut output = NamedTempFile::new_in(
        path.parent()
            .context("The destination has no parent directory")?,
    )?;
    let mut writer = zip::ZipWriter::new(output.as_file_mut());
    writer.set_raw_comment(existing.comment().to_vec().into_boxed_slice())?;
    let mut removed = false;
    for index in 0..existing.len() {
        let entry = existing.by_index_raw(index)?;
        let entry_path = safe_extraction_path(&entry.name()?)?;
        if entry_path == selected || entry_path.starts_with(&selected) {
            removed = true;
        } else {
            writer.raw_copy_file(entry)?;
        }
    }
    anyhow::ensure!(removed, "Archive entry not found");
    writer.finish()?;
    drop(existing);
    let current = path.metadata()?;
    anyhow::ensure!(
        original.len() == current.len() && original.modified()? == current.modified()?,
        "The archive changed while files were being added"
    );
    output.as_file().set_permissions(original.permissions())?;
    output.as_file().sync_all()?;
    output.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

fn rebuild_archive_with_additions(
    path: &Path,
    sources: &[PathBuf],
    folder: &str,
    password: Option<&str>,
    format: ResolvedFormat,
) -> Result<()> {
    rebuild_archive_with_changes(path, sources, folder, password, format, None)
}

fn rebuild_archive_with_changes(
    path: &Path,
    sources: &[PathBuf],
    folder: &str,
    password: Option<&str>,
    format: ResolvedFormat,
    remove: Option<&Path>,
) -> Result<()> {
    anyhow::ensure!(
        remove.is_some() || (!sources.is_empty() && sources.len() <= 256),
        "No supported files or folders were selected"
    );
    anyhow::ensure!(
        format != ResolvedFormat::Ar
            || (folder.is_empty() && sources.iter().all(|source| source.is_file())),
        "AR archives support files only. Select individual files instead of folders."
    );
    let path = std::fs::canonicalize(path)?;
    let original = path.metadata()?;
    let existing = list_archive_with_password(&path, password)?;
    if let Some(selected) = remove {
        anyhow::ensure!(
            existing
                .iter()
                .any(|entry| safe_extraction_path(&entry.name)
                    .is_ok_and(|name| name == selected || name.starts_with(selected))),
            "Archive entry not found"
        );
    }
    let mut names = BTreeSet::new();
    for entry in &existing {
        let name = safe_extraction_path(&entry.name)?
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_lowercase();
        anyhow::ensure!(
            names.insert(name),
            "The archive contains duplicate names and cannot be rebuilt safely"
        );
    }
    let prefix = if folder.is_empty() {
        PathBuf::new()
    } else {
        safe_extraction_path(folder)?
    };
    let mut additions = Vec::new();
    for source in sources {
        let source = std::fs::canonicalize(source)?;
        anyhow::ensure!(
            source != path && (!source.is_dir() || !path.starts_with(&source)),
            "An archive cannot contain itself"
        );
        let name = prefix.join(source.file_name().context("Invalid archive filename")?);
        collect_archive_sources(
            &source,
            name.to_string_lossy().replace('\\', "/"),
            true,
            &mut names,
            &mut additions,
        )?;
    }
    anyhow::ensure!(
        remove.is_some() || !additions.is_empty(),
        "No supported files or folders were selected"
    );
    let parent = path
        .parent()
        .context("The destination has no parent directory")?;
    let staging = tempfile::tempdir_in(parent)?;
    let contents = staging.path().join("contents");
    if format == ResolvedFormat::Rar {
        std::fs::create_dir(&contents)?;
        let archive = rar_archive(&path, password)
            .open_for_listing()
            .map_err(map_rar_error)?;
        for entry in archive {
            anyhow::ensure!(
                !entry.map_err(map_rar_error)?.is_encrypted(),
                "Adding files to encrypted archives is not supported"
            );
        }
    } else {
        extract_archive_to(&path, &contents, password)?;
    }
    if format != ResolvedFormat::Rar {
        if let Some(selected) = remove {
            let destination = contents.join(selected);
            if destination.is_dir() {
                std::fs::remove_dir_all(destination)?;
            } else {
                std::fs::remove_file(destination)?;
            }
        }
    }
    for (source, name, directory) in &additions {
        let destination = contents.join(safe_extraction_path(name)?);
        if *directory {
            std::fs::create_dir_all(&destination)?;
        } else {
            std::fs::create_dir_all(destination.parent().context("Missing entry parent")?)?;
            std::fs::copy(source, &destination)?;
            if let Ok(modified) = source.metadata()?.modified() {
                File::options()
                    .write(true)
                    .open(destination)?
                    .set_times(std::fs::FileTimes::new().set_modified(modified))?;
            }
        }
    }
    let rebuilt = staging
        .path()
        .join(format!("archive.{}", format.extension()));
    if format == ResolvedFormat::Rar {
        std::fs::copy(&path, &rebuilt)?;
        let arguments: Vec<PathBuf> = if let Some(selected) = remove {
            existing
                .iter()
                .filter(|entry| {
                    safe_extraction_path(&entry.name)
                        .is_ok_and(|name| name == selected || name.starts_with(selected))
                })
                .map(|entry| {
                    anyhow::ensure!(
                        !entry.name.contains(['*', '?', '[', ']']),
                        "This archive format cannot be modified"
                    );
                    Ok(PathBuf::from(&entry.name))
                })
                .collect::<Result<_>>()?
        } else {
            sources
                .iter()
                .map(|source| {
                    Ok(Path::new(".").join(&prefix).join(
                        std::fs::canonicalize(source)?
                            .file_name()
                            .context("Invalid archive filename")?,
                    ))
                })
                .collect::<Result<_>>()?
        };
        let result = std::process::Command::new(rar_program().unwrap())
            .current_dir(&contents)
            .args([
                if remove.is_some() { "d" } else { "a" },
                "-idq",
                "-o-",
                "-p-",
                "-r",
                "--",
            ])
            .arg(&rebuilt)
            .args(arguments)
            .output()?;
        anyhow::ensure!(
            result.status.success(),
            "The RAR tool could not update this archive"
        );
    } else {
        let paths = std::fs::read_dir(&contents)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        create_archive_contents(
            &rebuilt,
            &paths,
            CreateOptions {
                format,
                include_hidden: true,
                encrypt_headers: password.is_some() && format == ResolvedFormat::SevenZip,
                compression: if format
                    .compression_levels()
                    .contains(&CompressionLevel::Normal)
                {
                    CompressionLevel::Normal
                } else {
                    format.compression_levels()[0]
                },
                ..CreateOptions::default()
            },
            password,
            true,
        )?;
    }
    let current = path.metadata()?;
    anyhow::ensure!(
        original.len() == current.len() && original.modified()? == current.modified()?,
        "The archive changed while files were being added"
    );
    let mut output = NamedTempFile::new_in(parent)?;
    crate::task_queue::copy(&mut File::open(rebuilt)?, output.as_file_mut())?;
    output.as_file().set_permissions(original.permissions())?;
    output.as_file().sync_all()?;
    output.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

/// Collects regular files and directories while filtering hidden files and duplicate names.
fn collect_archive_sources(
    path: &Path,
    name: String,
    include_hidden: bool,
    names: &mut BTreeSet<String>,
    entries: &mut Vec<(PathBuf, String, bool)>,
) -> Result<()> {
    crate::task_queue::checkpoint()?;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_symlink() {
        return Ok(());
    }
    anyhow::ensure!(
        metadata.is_file() || metadata.is_dir(),
        "Unsupported file: {}",
        path.display()
    );
    anyhow::ensure!(
        names.insert(name.to_lowercase()),
        "Selected sources contain duplicate names: {name}"
    );
    entries.push((path.to_path_buf(), name.clone(), metadata.is_dir()));
    if metadata.is_dir() {
        let mut children = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let child_name = child.file_name();
            let child_name = child_name
                .to_str()
                .context("A source file name is not valid UTF-8")?;
            if !include_hidden && child_name.starts_with('.') {
                continue;
            }
            collect_archive_sources(
                &child.path(),
                format!("{name}/{child_name}"),
                include_hidden,
                names,
                entries,
            )?;
        }
    }
    Ok(())
}

/// Writes collected sources to TAR, optionally using deterministic metadata.
fn write_tar(
    writer: impl Write,
    entries: &[(PathBuf, String, bool)],
    deterministic: bool,
) -> Result<()> {
    let mut archive = tar::Builder::new(crate::task_queue::TaskWriter(writer));
    if deterministic {
        archive.mode(tar::HeaderMode::Deterministic);
    }
    for (source, name, is_directory) in entries {
        if *is_directory {
            archive.append_dir(name, source)?;
        } else {
            archive.append_path_with_name(source, name)?;
        }
    }
    archive.finish()?;
    Ok(())
}

#[cfg(unix)]
struct ArchiveLocale {
    locale: libc::locale_t,
    previous: libc::locale_t,
}

#[cfg(unix)]
impl ArchiveLocale {
    fn enter() -> Result<Self> {
        #[cfg(target_os = "macos")]
        let name = c"UTF-8";
        #[cfg(not(target_os = "macos"))]
        let name = c"C.UTF-8";
        let locale =
            unsafe { libc::newlocale(libc::LC_CTYPE_MASK, name.as_ptr(), std::ptr::null_mut()) };
        anyhow::ensure!(!locale.is_null(), "UTF-8 filename encoding is unavailable");
        let previous = unsafe { libc::uselocale(locale) };
        if previous.is_null() {
            unsafe { libc::freelocale(locale) };
            anyhow::bail!("Cannot enable UTF-8 filename encoding");
        }
        Ok(Self { locale, previous })
    }
}

#[cfg(unix)]
impl Drop for ArchiveLocale {
    fn drop(&mut self) {
        unsafe {
            libc::uselocale(self.previous);
            libc::freelocale(self.locale);
        }
    }
}

fn write_extended_archive(
    path: &Path,
    entries: &[(PathBuf, String, bool)],
    format: ResolvedFormat,
) -> Result<()> {
    use libarchive2::{ArchiveFormat as BackendFormat, EntryMut, FileType, WriteArchive};
    if format == ResolvedFormat::Xar && entries.is_empty() {
        return crate::xar::write_empty(&mut File::create(path)?);
    }
    #[cfg(unix)]
    let _locale = ArchiveLocale::enter()?;
    let backend = match format {
        ResolvedFormat::Cpio => BackendFormat::Cpio,
        ResolvedFormat::Ar => BackendFormat::Ar,
        ResolvedFormat::Pax => BackendFormat::TarPax,
        ResolvedFormat::Xar => BackendFormat::Xar,
        _ => anyhow::bail!("Unsupported extended archive format"),
    };
    anyhow::ensure!(
        format != ResolvedFormat::Ar
            || entries
                .iter()
                .all(|(_, name, directory)| !directory && !name.contains('/')),
        "AR archives support individual files, not folders"
    );
    let mut archive = WriteArchive::new().format(backend).open_file(path)?;
    let mut buffer = [0_u8; 65536];
    for (index, (source, name, directory)) in entries.iter().enumerate() {
        crate::task_queue::checkpoint()?;
        let metadata = source.metadata()?;
        let mut entry = EntryMut::new();
        entry.set_pathname(name)?;
        entry.set_file_type(if *directory {
            FileType::Directory
        } else {
            FileType::RegularFile
        });
        entry.set_size(if *directory {
            0
        } else {
            i64::try_from(metadata.len())?
        });
        entry.set_perm(if *directory { 0o755 } else { 0o644 })?;
        entry.set_nlink(1);
        entry.set_ino(index as u64 + 1);
        entry.set_mtime(metadata.modified()?);
        archive.write_header(&entry)?;
        if !directory {
            let mut input = File::open(source)?;
            loop {
                crate::task_queue::checkpoint()?;
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                let mut pending = &buffer[..count];
                crate::task_queue::report_bytes(count as u64);
                while !pending.is_empty() {
                    let written = archive.write_data(pending)?;
                    anyhow::ensure!(written > 0, "Archive writer did not accept data");
                    pending = &pending[written..];
                }
            }
        }
    }
    archive.finish()?;
    Ok(())
}

/// Writes TAR entries through the compressor selected by the output format.
fn write_compressed_tar(
    writer: &mut File,
    entries: &[(PathBuf, String, bool)],
    format: ResolvedFormat,
    options: CreateOptions,
) -> Result<()> {
    let level = options.compression.level();
    match format {
        ResolvedFormat::TarGz => {
            let mut encoder =
                flate2::write::GzEncoder::new(writer, flate2::Compression::new(level));
            write_tar(&mut encoder, entries, options.deterministic)?;
            encoder.finish()?;
        }
        ResolvedFormat::TarBz2 => {
            let mut encoder = bzip2::write::BzEncoder::new(writer, bzip2::Compression::new(level));
            write_tar(&mut encoder, entries, options.deterministic)?;
            encoder.finish()?;
        }
        ResolvedFormat::TarXz => {
            let mut encoder =
                lzma_rust2::XzWriter::new(writer, lzma_rust2::XzOptions::with_preset(level))?;
            write_tar(&mut encoder, entries, options.deterministic)?;
            encoder.finish()?;
        }
        ResolvedFormat::TarZstd => {
            let mut encoder = zstd::stream::write::Encoder::new(writer, level as i32)?;
            write_tar(&mut encoder, entries, options.deterministic)?;
            encoder.finish()?;
        }
        ResolvedFormat::TarLz4 => {
            let mut encoder = lz4_flex::frame::FrameEncoder::new(writer);
            write_tar(&mut encoder, entries, options.deterministic)?;
            encoder.finish()?;
        }
        ResolvedFormat::TarLzip => {
            let mut encoder =
                lzma_rust2::LzipWriter::new(writer, lzma_rust2::LzipOptions::with_preset(level));
            write_tar(&mut encoder, entries, options.deterministic)?;
            encoder.finish()?;
        }
        ResolvedFormat::TarBrotli => {
            let mut encoder = brotli::CompressorWriter::new(writer, 4096, level, 22);
            write_tar(&mut encoder, entries, options.deterministic)?;
            let _ = encoder.into_inner();
        }
        _ => anyhow::bail!("Unsupported compressed TAR format"),
    }
    Ok(())
}

/// Writes one source file through a standalone compression codec.
fn write_compressed_file(
    writer: &mut File,
    source: &Path,
    format: ResolvedFormat,
    level: u32,
) -> Result<()> {
    let mut source = File::open(source)?;
    match format {
        ResolvedFormat::Gzip => {
            let mut encoder =
                flate2::write::GzEncoder::new(writer, flate2::Compression::new(level));
            crate::task_queue::copy(&mut source, &mut encoder)?;
            encoder.finish()?;
        }
        ResolvedFormat::Bzip2 => {
            let mut encoder = bzip2::write::BzEncoder::new(writer, bzip2::Compression::new(level));
            crate::task_queue::copy(&mut source, &mut encoder)?;
            encoder.finish()?;
        }
        ResolvedFormat::Xz => {
            let mut encoder =
                lzma_rust2::XzWriter::new(writer, lzma_rust2::XzOptions::with_preset(level))?;
            crate::task_queue::copy(&mut source, &mut encoder)?;
            encoder.finish()?;
        }
        ResolvedFormat::Zstandard => {
            let mut encoder = zstd::stream::write::Encoder::new(writer, level as i32)?;
            crate::task_queue::copy(&mut source, &mut encoder)?;
            encoder.finish()?;
        }
        ResolvedFormat::Lz4 => {
            let mut encoder = lz4_flex::frame::FrameEncoder::new(writer);
            crate::task_queue::copy(&mut source, &mut encoder)?;
            encoder.finish()?;
        }
        ResolvedFormat::Lzip => {
            let mut encoder =
                lzma_rust2::LzipWriter::new(writer, lzma_rust2::LzipOptions::with_preset(level));
            crate::task_queue::copy(&mut source, &mut encoder)?;
            encoder.finish()?;
        }
        ResolvedFormat::Brotli => {
            let mut encoder = brotli::CompressorWriter::new(writer, 4096, level, 22);
            crate::task_queue::copy(&mut source, &mut encoder)?;
            let _ = encoder.into_inner();
        }
        _ => anyhow::bail!("Unsupported standalone compression format"),
    }
    Ok(())
}

/// Extracts one archive entry to a temporary file without a password.
pub fn extract_entry(path: &Path, entry_name: &str) -> Result<NamedTempFile> {
    extract_entry_with_password(path, entry_name, None)
}

pub(crate) fn read_entry_prefix(
    path: &Path,
    entry_name: &str,
    password: Option<&str>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u8>> {
    use crate::text_preview::{GuardedReader, read_prefix};
    use std::{cell::Cell, sync::atomic::Ordering};
    let format = ResolvedFormat::from_path(path).context("Unsupported archive format")?;
    let budget = Cell::new(64 * 1024 * 1024);
    let guarded = |reader| GuardedReader {
        reader,
        cancel,
        budget: &budget,
    };
    if format.is_single_compressed() {
        anyhow::ensure!(
            entry_name == single_compressed_entry_name(path)?,
            "Archive entry not found"
        );
        return Ok(read_prefix(
            &mut guarded(decode_archive_stream(File::open(path)?, format)?),
            cancel,
        )?);
    }
    if format.is_tar() {
        let mut archive = TarArchive::new(guarded(open_tar_reader(path, format)?));
        for entry in archive.entries()?.take(20_000) {
            let mut entry = entry?;
            if entry.path()?.to_string_lossy() == entry_name {
                anyhow::ensure!(
                    entry.header().entry_type().is_file(),
                    "Only regular files are opened"
                );
                return Ok(read_prefix(&mut entry, cancel)?);
            }
        }
        anyhow::bail!("Archive entry not found or preview safety limit reached");
    }
    match format {
        ResolvedFormat::Zip => {
            let mut archive = ZipArchive::new(File::open(path)?)?;
            let mut entry = if let Some(password) = password {
                archive.by_name_decrypt(entry_name, password.as_bytes())
            } else {
                archive.by_name(entry_name)
            }?;
            anyhow::ensure!(
                !entry.is_dir()
                    && !entry
                        .unix_mode()
                        .is_some_and(|mode| mode & 0o170000 == 0o120000),
                "Only regular files are opened"
            );
            Ok(read_prefix(&mut entry, cancel)?)
        }
        ResolvedFormat::SevenZip => {
            let file = File::open(path)?;
            let length = file.metadata()?.len();
            let input_budget = Cell::new(64 * 1024 * 1024);
            let reader = GuardedReader {
                reader: file,
                cancel,
                budget: &input_budget,
            };
            let mut archive = SevenZReader::new(
                reader,
                length,
                password.map(Password::from).unwrap_or_default(),
            )?;
            anyhow::ensure!(
                !archive
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
                    }),
                "Preview safety limit reached"
            );
            let mut bytes = None;
            let mut count = 0;
            archive.for_each_entries(|entry, reader| {
                count += 1;
                if count > 20_000 || cancel.load(Ordering::Relaxed) {
                    return Err(sevenz_rust::Error::io(io::Error::other(
                        "Preview safety limit reached",
                    )));
                }
                let mut reader = GuardedReader {
                    reader,
                    cancel,
                    budget: &budget,
                };
                if entry.name() == entry_name && !entry.is_directory() {
                    bytes = Some(read_prefix(&mut reader, cancel).map_err(sevenz_rust::Error::io)?);
                    Ok(false)
                } else {
                    crate::task_queue::copy(&mut reader, &mut io::sink()).map_err(sevenz_rust::Error::io)?;
                    Ok(true)
                }
            })?;
            bytes.context("Archive entry not found")
        }
        ResolvedFormat::Xar => {
            let mut archive = crate::xar::XarReader::new(File::open(path)?)?;
            let entry = archive
                .entries
                .iter()
                .find(|entry| entry.path == entry_name)
                .cloned()
                .context("Archive entry not found")?;
            Ok(read_prefix(&mut archive.entry_reader(&entry)?, cancel)?)
        }
        _ => {
            let mut archive = if let Some(password) = password {
                libarchive2::ReadArchive::open_with_passphrase(path, password)?
            } else {
                libarchive2::ReadArchive::open(path)?
            };
            for _ in 0..20_000 {
                if cancel.load(Ordering::Relaxed) {
                    anyhow::bail!("Preview cancelled");
                }
                let Some(entry) = archive.next_entry()? else {
                    break;
                };
                if entry
                    .pathname()
                    .as_deref()
                    .map(|name| name.replace('\\', "/"))
                    .as_deref()
                    != Some(entry_name)
                {
                    continue;
                }
                anyhow::ensure!(
                    entry.file_type() == libarchive2::FileType::RegularFile
                        && entry.symlink().is_none()
                        && entry.hardlink().is_none(),
                    "Only regular files are opened"
                );
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                while bytes.len() <= crate::text_preview::MAX_BYTES {
                    if cancel.load(Ordering::Relaxed) {
                        anyhow::bail!("Preview cancelled");
                    }
                    let length = buffer
                        .len()
                        .min(crate::text_preview::MAX_BYTES + 1 - bytes.len());
                    let count = archive.read_data(&mut buffer[..length])?;
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                return Ok(bytes);
            }
            anyhow::bail!("Archive entry not found or preview safety limit reached")
        }
    }
}

/// Extracts one regular file entry to a rewound temporary file.
pub fn extract_entry_with_password(
    path: &Path,
    entry_name: &str,
    password: Option<&str>,
) -> Result<NamedTempFile> {
    let mut output = Builder::new()
        .suffix(&temporary_suffix(entry_name))
        .tempfile()
        .context("Failed to create a temporary file for the archive entry")?;

    let format = ResolvedFormat::from_path(path)
        .with_context(|| format!("Unsupported archive format: {}", path.display()))?;
    anyhow::ensure!(
        format.capabilities().can_extract,
        "This format cannot be extracted"
    );
    match format {
        ResolvedFormat::Cpio | ResolvedFormat::Ar | ResolvedFormat::Pax | ResolvedFormat::Xar => {
            extract_extended_entry(path, entry_name, &mut output)?;
        }
        ResolvedFormat::Zip => {
            let file =
                File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
            let mut archive = ZipArchive::new(file).context("Invalid ZIP archive")?;
            let mut entry = if let Some(password) = password {
                archive.by_name_decrypt(entry_name, password.as_bytes())
            } else {
                archive.by_name(entry_name)
            }
            .with_context(|| {
                format!("Archive entry not found or password invalid: {entry_name}")
            })?;
            if entry.is_dir() {
                anyhow::bail!("Cannot open a directory as a document");
            }
            crate::task_queue::copy(&mut entry, &mut output).context("Failed to extract the ZIP entry")?;
        }
        ResolvedFormat::Gzip
        | ResolvedFormat::Bzip2
        | ResolvedFormat::Xz
        | ResolvedFormat::Zstandard
        | ResolvedFormat::Lz4
        | ResolvedFormat::Lzip
        | ResolvedFormat::Brotli => {
            let name = single_compressed_entry_name(path)?;
            anyhow::ensure!(
                entry_name == name,
                "Standalone compressed file entry not found"
            );
            let file = File::open(path)?;
            let mut reader = decode_archive_stream(file, format)?;
            crate::task_queue::copy(&mut reader, &mut output)?;
        }
        ResolvedFormat::Tar
        | ResolvedFormat::TarGz
        | ResolvedFormat::TarBz2
        | ResolvedFormat::TarXz
        | ResolvedFormat::TarZstd
        | ResolvedFormat::TarLz4
        | ResolvedFormat::TarLzip
        | ResolvedFormat::TarBrotli => extract_tar(
            open_tar_reader(path, format)?,
            path,
            entry_name,
            &mut output,
        )?,
        ResolvedFormat::SevenZip => {
            let mut archive =
                SevenZReader::open(path, password.map(Password::from).unwrap_or_default())
                    .map_err(|error| seven_zip_error(error, password))?;
            let mut found = false;
            archive
                .for_each_entries(|entry, reader| {
                    if entry.name() == entry_name && !entry.is_directory() {
                        crate::task_queue::copy(reader, &mut output).map_err(sevenz_rust::Error::io)?;
                        found = true;
                        Ok(false)
                    } else {
                        crate::task_queue::copy(reader, &mut io::sink()).map_err(sevenz_rust::Error::io)?;
                        Ok(true)
                    }
                })
                .context("Failed to extract the 7z entry")?;
            if !found {
                anyhow::bail!("Archive entry not found: {entry_name}");
            }
        }
        ResolvedFormat::Rar => {
            let scratch = tempfile::tempdir()?;
            let extracted_path = scratch.path().join("entry");
            extract_rar_entry_to(path, entry_name, &extracted_path, password)?;
            crate::task_queue::copy(&mut File::open(extracted_path)?, &mut output)
                .context("Failed to copy the RAR entry")?;
        }
        _ => anyhow::bail!("Unsupported resolved format for entry extraction"),
    }

    output
        .flush()
        .context("Failed to write the extracted archive entry")?;
    output
        .rewind()
        .context("Failed to reset the extracted file position")?;
    Ok(output)
}

/// Extracts an entire archive into a new destination directory.
pub fn extract_archive_to(path: &Path, destination: &Path, password: Option<&str>) -> Result<()> {
    extract_archive_selection_to(path, destination, password, None)
}

/// Extracts an archive or selected entry through staging after rejecting unsafe paths and links.
pub fn extract_archive_selection_to(
    path: &Path,
    destination: &Path,
    password: Option<&str>,
    selection: Option<&str>,
) -> Result<()> {
    let format = ResolvedFormat::from_path(path)
        .with_context(|| format!("Unsupported archive format: {}", path.display()))?;
    anyhow::ensure!(
        format.capabilities().can_extract,
        "This format cannot be extracted"
    );
    anyhow::ensure!(
        std::fs::symlink_metadata(destination).is_err(),
        "The destination already exists"
    );
    let entries = list_archive_with_password(path, password)?;
    crate::task_queue::report_byte_total(entries.iter().filter(|entry| {
        !entry.is_directory && selection.is_none_or(|selected| entry.name == selected || entry.name.starts_with(&format!("{}/", selected.trim_end_matches('/'))))
    }).fold(0_u64, |total, entry| total.saturating_add(entry.size)));
    for entry in &entries {
        crate::task_queue::checkpoint()?;
        safe_extraction_path(&entry.name)?;
    }
    let parent = destination
        .parent()
        .context("The destination has no parent directory")?;
    let selection = selection.map(safe_extraction_path).transpose()?;
    if let Some(selected) = &selection {
        if let Some(entry) = entries.iter().find(|entry| {
            !entry.is_directory && safe_extraction_path(&entry.name).ok().as_ref() == Some(selected)
        }) {
            let mut extracted = extract_entry_with_password(path, &entry.name, password)?;
            let mut staged = NamedTempFile::new_in(parent)?;
            crate::task_queue::copy(extracted.as_file_mut(), staged.as_file_mut())?;
            staged
                .persist_noclobber(destination)
                .map_err(|error| error.error)?;
            return Ok(());
        }
        anyhow::ensure!(
            entries.iter().any(|entry| safe_extraction_path(&entry.name)
                .is_ok_and(|name| name.starts_with(selected))),
            "Selected archive entry not found"
        );
    }
    let staged =
        tempfile::tempdir_in(parent).context("Cannot create extraction staging directory")?;
    match format {
        ResolvedFormat::Cpio | ResolvedFormat::Ar | ResolvedFormat::Pax | ResolvedFormat::Xar => {
            extract_extended_tree(path, staged.path(), selection.as_deref())?;
        }
        ResolvedFormat::Zip => {
            let mut archive = ZipArchive::new(File::open(path)?)?;
            for index in 0..archive.len() {
                crate::task_queue::checkpoint()?;
                let mut entry = if let Some(password) = password {
                    archive.by_index_decrypt(index, password.as_bytes())
                } else {
                    archive.by_index(index)
                }?;
                anyhow::ensure!(
                    !entry
                        .unix_mode()
                        .is_some_and(|mode| mode & 0o170000 == 0o120000),
                    "Symbolic links in archives are not extracted"
                );
                let name = entry.name()?.into_owned();
                let is_directory = entry.is_dir();
                write_extracted_entry(
                    staged.path(),
                    &name,
                    is_directory,
                    &mut entry,
                    selection.as_deref(),
                )?;
            }
        }
        ResolvedFormat::Tar
        | ResolvedFormat::TarGz
        | ResolvedFormat::TarBz2
        | ResolvedFormat::TarXz
        | ResolvedFormat::TarZstd
        | ResolvedFormat::TarLz4
        | ResolvedFormat::TarLzip
        | ResolvedFormat::TarBrotli => extract_tar_tree(
            open_tar_reader(path, format)?,
            staged.path(),
            selection.as_deref(),
        )?,
        ResolvedFormat::Gzip
        | ResolvedFormat::Bzip2
        | ResolvedFormat::Xz
        | ResolvedFormat::Zstandard
        | ResolvedFormat::Lz4
        | ResolvedFormat::Lzip
        | ResolvedFormat::Brotli => {
            let name = single_compressed_entry_name(path)?;
            let file = File::open(path)?;
            let mut reader = decode_archive_stream(file, format)?;
            write_extracted_entry(
                staged.path(),
                &name,
                false,
                &mut reader,
                selection.as_deref(),
            )?;
        }
        ResolvedFormat::SevenZip => {
            let mut archive =
                SevenZReader::open(path, password.map(Password::from).unwrap_or_default())
                    .map_err(|error| seven_zip_error(error, password))?;
            archive
                .for_each_entries(|entry, reader| {
                    write_extracted_entry(
                        staged.path(),
                        entry.name(),
                        entry.is_directory(),
                        &mut *reader,
                        selection.as_deref(),
                    )
                    .map_err(|error| sevenz_rust::Error::other(error.to_string()))?;
                    crate::task_queue::copy(reader, &mut io::sink()).map_err(sevenz_rust::Error::io)?;
                    Ok(true)
                })
                .map_err(|error| seven_zip_error(error, password))?;
        }
        ResolvedFormat::Rar => {
            extract_rar_tree(path, staged.path(), password, selection.as_deref())?;
        }
        _ => anyhow::bail!("Unsupported resolved format for archive extraction"),
    }
    crate::task_queue::checkpoint()?;
    std::fs::create_dir(destination)
        .context("The destination already exists or cannot be created")?;
    let published = (|| -> Result<()> {
        for entry in std::fs::read_dir(staged.path())? {
            let entry = entry?;
            std::fs::rename(entry.path(), destination.join(entry.file_name()))?;
        }
        Ok(())
    })();
    if published.is_err() {
        let _ = std::fs::remove_dir_all(destination);
    }
    published
}

/// Normalizes an archive path and rejects absolute, drive-qualified, or parent-traversal paths.
fn safe_extraction_path(name: &str) -> Result<PathBuf> {
    let normalized = name.replace('\\', "/");
    anyhow::ensure!(
        !normalized.starts_with('/') && !normalized.contains(':') && !normalized.contains('\0'),
        "Unsafe archive entry path"
    );
    let mut result = PathBuf::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => anyhow::bail!("Archive entry escapes its destination"),
            component => result.push(component),
        }
    }
    Ok(result)
}

/// Writes one validated entry below the staging root, honoring an optional selected subtree.
fn write_extracted_entry(
    root: &Path,
    name: &str,
    is_directory: bool,
    mut reader: impl Read,
    selection: Option<&Path>,
) -> Result<()> {
    crate::task_queue::checkpoint()?;
    let name = safe_extraction_path(name)?;
    let relative = if let Some(selection) = selection {
        let Ok(relative) = name.strip_prefix(selection) else {
            return Ok(());
        };
        relative
    } else {
        name.as_path()
    };
    if relative.as_os_str().is_empty() {
        anyhow::ensure!(is_directory, "An archive file has an empty path");
        return Ok(());
    }
    let path = root.join(relative);
    if is_directory {
        std::fs::create_dir_all(path)?;
    } else {
        std::fs::create_dir_all(path.parent().context("Missing entry parent")?)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        crate::task_queue::copy(&mut reader, &mut file)?;
    }
    Ok(())
}

/// Extracts only regular files and directories from a TAR stream.
fn extract_tar_tree(reader: impl Read, destination: &Path, selection: Option<&Path>) -> Result<()> {
    for entry in TarArchive::new(reader).entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        anyhow::ensure!(
            kind.is_file() || kind.is_dir(),
            "Only regular files and directories are extracted"
        );
        let name = entry
            .path()?
            .to_str()
            .context("Invalid archive filename")?
            .to_owned();
        write_extracted_entry(destination, &name, kind.is_dir(), &mut entry, selection)?;
    }
    Ok(())
}

/// Copies one regular-file entry from a TAR stream into a temporary file.
fn extract_tar<R: Read>(
    reader: R,
    path: &Path,
    entry_name: &str,
    output: &mut NamedTempFile,
) -> Result<()> {
    let mut archive = TarArchive::new(reader);
    let entries = archive
        .entries()
        .with_context(|| format!("Invalid TAR archive: {}", path.display()))?;
    for entry in entries {
        let mut entry = entry.context("Failed to read a TAR entry")?;
        if entry.path()?.to_string_lossy() == entry_name {
            if !entry.header().entry_type().is_file() {
                anyhow::bail!("Only regular files can be opened as documents");
            }
            crate::task_queue::copy(&mut entry, output).context("Failed to extract the TAR entry")?;
            return Ok(());
        }
    }
    anyhow::bail!("Archive entry not found: {entry_name}")
}

/// Returns a short alphanumeric extension for associating temporary files with system apps.
fn temporary_suffix(entry_name: &str) -> String {
    let extension = Path::new(entry_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 16
                && extension
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        });
    extension.map_or_else(String::new, |extension| format!(".{extension}"))
}

/// Lists ZIP entries and validates encrypted streams when a password is supplied.
fn list_zip(path: &Path, password: Option<&str>) -> Result<Vec<ArchiveEntry>> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut archive = ZipArchive::new(file).context("Invalid ZIP archive")?;
    let mut entries = Vec::with_capacity(archive.len());

    for index in 0..archive.len() {
        crate::task_queue::checkpoint()?;
        let encrypted = archive.by_index_raw(index)?.encrypted();
        if encrypted && password.is_none() {
            return Err(PasswordFailure::Required.into());
        }
        let mut file = if let Some(password) = password {
            archive.by_index_decrypt(index, password.as_bytes())
        } else {
            archive.by_index(index)
        }
        .map_err(|error| {
            if encrypted {
                anyhow::Error::from(PasswordFailure::Incorrect)
            } else {
                error.into()
            }
        })?;
        if encrypted && !file.is_dir() {
            crate::task_queue::copy(&mut file, &mut io::sink()).map_err(|_| PasswordFailure::Incorrect)?;
        }
        entries.push(ArchiveEntry {
            name: file.name()?.into_owned(),
            source_path: Some(file.name()?.into_owned()),
            size: file.size(),
            modified: file.last_modified().and_then(|date| {
                chrono::NaiveDate::from_ymd_opt(
                    date.year().into(),
                    date.month().into(),
                    date.day().into(),
                )
            }),
            is_directory: file.is_dir(),
        });
    }

    Ok(entries)
}

/// Lists entries from a TAR or decompressed TAR stream.
fn list_tar<R: Read>(reader: R, path: &Path) -> Result<Vec<ArchiveEntry>> {
    let mut archive = TarArchive::new(reader);
    let tar_entries = archive
        .entries()
        .with_context(|| format!("Invalid TAR archive: {}", path.display()))?;
    let mut entries = Vec::new();

    for entry in tar_entries {
        crate::task_queue::checkpoint()?;
        let entry = entry.context("Failed to read a TAR entry")?;
        let name = entry.path()?.to_string_lossy().into_owned();
        entries.push(ArchiveEntry {
            name: name.clone(),
            source_path: Some(name),
            size: entry.header().size()?,
            modified: entry
                .header()
                .mtime()
                .ok()
                .and_then(|time| chrono::DateTime::from_timestamp(i64::try_from(time).ok()?, 0))
                .map(|time| time.with_timezone(&chrono::Local).date_naive()),
            is_directory: entry.header().entry_type().is_dir(),
        });
    }

    Ok(entries)
}

/// Lists 7z entries and drains content streams to verify encrypted data when needed.
fn list_seven_zip(path: &Path, password: Option<&str>) -> Result<Vec<ArchiveEntry>> {
    let mut archive = SevenZReader::open(path, password.map(Password::from).unwrap_or_default())
        .map_err(|error| seven_zip_error(error, password))?;
    let encrypted = archive
        .archive()
        .folders
        .iter()
        .flat_map(|folder| &folder.coders)
        .any(|coder| coder.decompression_method_id() == [0x06, 0xf1, 0x07, 0x01]);
    if encrypted && password.is_none() {
        return Err(PasswordFailure::Required.into());
    }
    let mut entries = Vec::new();

    archive
        .for_each_entries(|entry, reader| {
            crate::task_queue::copy(reader, &mut io::sink()).map_err(sevenz_rust::Error::io)?;
            let name = entry.name().to_owned();
            entries.push(ArchiveEntry {
                name: name.clone(),
                source_path: Some(name),
                size: entry.size(),
                modified: entry
                    .has_last_modified_date
                    .then(|| crate::browser::date_from_filetime(entry.last_modified_date.to_raw()))
                    .flatten(),
                is_directory: entry.is_directory(),
            });
            Ok(true)
        })
        .map_err(|error| {
            if encrypted && password.is_some() {
                anyhow::Error::from(PasswordFailure::Incorrect)
            } else {
                seven_zip_error(error, password)
            }
        })?;

    Ok(entries)
}

/// Creates a RAR reader configured with an optional password.
pub(crate) fn rar_archive<'a>(path: &'a Path, password: Option<&'a str>) -> unrar::Archive<'a> {
    let archive = if let Some(password) = password {
        unrar::Archive::with_password(path, password.as_bytes())
    } else {
        unrar::Archive::new(path)
    };
    archive.as_first_part()
}

/// Maps RAR library password errors to the shared archive error variants.
pub(crate) fn map_rar_error(error: unrar::error::UnrarError) -> anyhow::Error {
    match error.code {
        unrar::error::Code::MissingPassword => PasswordFailure::Required.into(),
        unrar::error::Code::BadPassword => PasswordFailure::Incorrect.into(),
        _ => error.into(),
    }
}

/// Lists RAR entries and normalizes their path separators.
fn list_rar(path: &Path, password: Option<&str>) -> Result<Vec<ArchiveEntry>> {
    let archive = rar_archive(path, password)
        .open_for_listing()
        .map_err(map_rar_error)?;
    archive
        .map(|entry| {
            let header = entry.map_err(map_rar_error)?;
            let name = header.filename.to_string_lossy().replace('\\', "/");
            Ok(ArchiveEntry {
                name: name.clone(),
                source_path: Some(name),
                size: header.unpacked_size,
                modified: None,
                is_directory: header.is_directory(),
            })
        })
        .collect()
}

/// Extracts one regular-file RAR entry to the specified temporary path.
fn extract_rar_entry_to(
    path: &Path,
    entry_name: &str,
    destination: &Path,
    password: Option<&str>,
) -> Result<()> {
    let mut archive = rar_archive(path, password)
        .open_for_processing()
        .map_err(map_rar_error)?;
    loop {
        let Some(entry) = archive.read_header().map_err(map_rar_error)? else {
            anyhow::bail!("Archive entry not found: {entry_name}");
        };
        let header = entry.entry();
        let name = header.filename.to_string_lossy().replace('\\', "/");
        if name == entry_name {
            anyhow::ensure!(
                !header.is_directory(),
                "Cannot open a directory as a document"
            );
            entry.extract_to(destination).map_err(map_rar_error)?;
            return Ok(());
        }
        archive = entry.skip().map_err(map_rar_error)?;
    }
}

/// Extracts a selected RAR subtree while rejecting link entries and unsafe paths.
fn extract_rar_tree(
    path: &Path,
    root: &Path,
    password: Option<&str>,
    selection: Option<&Path>,
) -> Result<()> {
    let mut archive = rar_archive(path, password)
        .open_for_processing()
        .map_err(map_rar_error)?;
    let scratch = tempfile::tempdir()?;
    let mut index = 0_usize;
    while let Some(entry) = archive.read_header().map_err(map_rar_error)? {
        let header = entry.entry();
        let name = header.filename.to_string_lossy().replace('\\', "/");
        let is_directory = header.is_directory();
        let file_attr = header.file_attr;
        anyhow::ensure!(
            file_attr & 0x400 == 0 && file_attr & 0o170000 != 0o120000,
            "Symbolic links in archives are not extracted"
        );
        let safe_name = safe_extraction_path(&name)?;
        if is_directory {
            write_extracted_entry(root, &name, true, io::empty(), selection)?;
            archive = entry.skip().map_err(map_rar_error)?;
        } else if selection.is_none_or(|selection| safe_name.starts_with(selection)) {
            let extracted_path = scratch.path().join(format!("entry-{index}"));
            archive = entry.extract_to(&extracted_path).map_err(map_rar_error)?;
            write_extracted_entry(root, &name, false, File::open(extracted_path)?, selection)?;
        } else {
            archive = entry.skip().map_err(map_rar_error)?;
        }
        index += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn standalone_format_labels_omit_redundant_compression_word() {
        for format in [
            super::ResolvedFormat::Gzip,
            super::ResolvedFormat::Bzip2,
            super::ResolvedFormat::Xz,
            super::ResolvedFormat::Zstandard,
            super::ResolvedFormat::Lz4,
            super::ResolvedFormat::Lzip,
            super::ResolvedFormat::Brotli,
        ] {
            assert!(!format.label().to_ascii_lowercase().contains("compression"));
            assert!(!format.label().to_ascii_lowercase().contains("kompresja"));
        }
        assert_eq!(super::ResolvedFormat::Gzip.label(), "GZIP");
        assert_eq!(super::ResolvedFormat::TarGz.label(), "TAR.GZ");
        assert_eq!(super::ResolvedFormat::Zip.label(), "ZIP");
    }

    #[test]
    fn legacy_zipcrypto_copy_requires_a_valid_password_and_never_weakens_encryption() {
        use std::io::{Read, Write};
        use zip::unstable::write::FileOptionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for name in ["remove.txt", "keep.txt"] {
            let options = zip::write::SimpleFileOptions::default()
                .with_deprecated_encryption(b"test-password")
                .unwrap();
            writer.start_file(name, options).unwrap();
            writer.write_all(b"legacy private content").unwrap();
        }
        writer.finish().unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(super::remove_archive_entry(&path, "remove.txt", None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(super::remove_archive_entry(&path, "remove.txt", Some("wrong")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        super::remove_archive_entry(&path, "remove.txt", Some("test-password")).unwrap();
        let mut result = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.by_index_raw(0).unwrap().encrypted());
        let mut text = String::new();
        result
            .by_index_decrypt(0, b"test-password")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "legacy private content");
    }

    #[test]
    fn zip_removal_raw_copy_preserves_ciphertext_and_metadata_without_a_password() {
        use std::io::{Read, Write};
        let directory = tempfile::tempdir().unwrap();
        for (index, encryption) in [
            zip::AesMode::Aes128,
            zip::AesMode::Aes192,
            zip::AesMode::Aes256,
        ]
        .into_iter()
        .enumerate()
        {
            let path = directory.path().join(format!("raw-{index}.zip"));
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            writer.set_comment("archive comment 中文").unwrap();
            writer
                .start_file("remove.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"remove this content").unwrap();
            let mut options = zip::write::FullFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o640)
                .last_modified_time(
                    zip::DateTime::from_date_and_time(2024, 2, 29, 12, 34, 56).unwrap(),
                )
                .with_file_comment("entry comment");
            options
                .add_extra_field(
                    0xbeef,
                    b"custom metadata".to_vec().into_boxed_slice(),
                    false,
                )
                .unwrap();
            let options = options.with_aes_encryption(encryption, "test-password");
            writer.start_file("keep.txt", options).unwrap();
            let text = "private payload that must not be decrypted or recompressed 中文";
            writer.write_all(text.as_bytes()).unwrap();
            writer.finish().unwrap();
            let mut original = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
            let mut entry = original.by_index_raw(1).unwrap();
            let compression = entry.compression();
            let date = entry.last_modified();
            let permissions = entry.unix_mode();
            let crc = entry.crc32();
            let comment = entry.comment().to_owned();
            let size = entry.size();
            let compressed_size = entry.compressed_size();
            let mut raw = Vec::new();
            entry.read_to_end(&mut raw).unwrap();
            drop(entry);
            drop(original);
            super::remove_archive_entry(&path, "remove.txt", None).unwrap();
            let mut result = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
            assert_eq!(result.len(), 1);
            assert_eq!(result.comment(), "archive comment 中文".as_bytes());
            let mut entry = result.by_index_raw(0).unwrap();
            assert!(entry.encrypted());
            assert_eq!(entry.compression(), compression);
            assert_eq!(entry.last_modified(), date);
            assert_eq!(entry.unix_mode(), permissions);
            assert_eq!(entry.crc32(), crc);
            assert_eq!(entry.comment(), comment);
            assert_eq!(entry.size(), size);
            assert_eq!(entry.compressed_size(), compressed_size);
            let mut copied_raw = Vec::new();
            entry.read_to_end(&mut copied_raw).unwrap();
            assert_eq!(copied_raw, raw, "ciphertext changed for {encryption:?}");
            drop(entry);
            let mut decoded = String::new();
            result
                .by_index_decrypt(0, b"test-password")
                .unwrap()
                .read_to_string(&mut decoded)
                .unwrap();
            assert_eq!(decoded, text);
            assert!(result.by_index_decrypt(0, b"wrong").is_err());
        }
    }

    #[test]
    fn archive_entry_removal_keeps_encryption_and_respects_folder_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let folder = sources.path().join("docs");
        let other = sources.path().join("docs-old");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&other).unwrap();
        std::fs::write(folder.join("remove.txt"), b"remove").unwrap();
        std::fs::write(other.join("keep.txt"), b"private kept content").unwrap();
        for format in [super::ResolvedFormat::Zip, super::ResolvedFormat::SevenZip] {
            let path = directory
                .path()
                .join(format!("protected.{}", format.extension()));
            super::create_archive_with_password(
                &path,
                &[folder.clone(), other.clone()],
                super::CreateOptions {
                    format,
                    encrypt_headers: format == super::ResolvedFormat::SevenZip,
                    ..Default::default()
                },
                Some("test-password"),
            )
            .unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(super::remove_archive_entry(&path, "../docs", Some("test-password")).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            if format == super::ResolvedFormat::SevenZip {
                assert!(super::remove_archive_entry(&path, "docs", None).is_err());
                assert_eq!(std::fs::read(&path).unwrap(), before);
            }
            super::remove_archive_entry(&path, "docs", Some("test-password")).unwrap();
            assert!(super::list_archive(&path).is_err());
            let entries = super::list_archive_with_password(&path, Some("test-password")).unwrap();
            assert!(
                !entries
                    .iter()
                    .any(|entry| entry.name == "docs/" || entry.name.starts_with("docs/"))
            );
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.name == "docs-old/keep.txt")
            );
            let mut file = super::extract_entry_with_password(
                &path,
                "docs-old/keep.txt",
                Some("test-password"),
            )
            .unwrap();
            let mut text = String::new();
            std::io::Read::read_to_string(&mut file, &mut text).unwrap();
            assert_eq!(text, "private kept content");
            assert!(
                super::extract_entry_with_password(&path, "docs-old/keep.txt", Some("wrong"))
                    .is_err()
            );
        }
    }

    #[test]
    fn empty_xar_has_a_valid_header_and_toc_without_placeholder_entries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.xar");
        super::write_extended_archive(&path, &[], super::ResolvedFormat::Xar).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 28);
        assert_eq!(&bytes[..4], b"xar!");
        let archive = xara::XarArchive::open(std::io::Cursor::new(&bytes)).unwrap();
        assert_eq!(archive.header().header_size, 28);
        assert_eq!(archive.header().version, 1);
        assert_eq!(
            archive.header().toc_compressed_len as usize + 28,
            bytes.len()
        );
        assert!(archive.files().is_empty());
        assert!(super::list_archive(&path).unwrap().is_empty());
        #[cfg(target_os = "macos")]
        {
            let result = std::process::Command::new("/usr/bin/tar")
                .arg("-tf")
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(result.stdout.is_empty());
        }
    }

    #[test]
    fn archive_entry_removal_preserves_other_files_and_supports_empty_archives() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        std::fs::write(&first, b"keep first").unwrap();
        std::fs::write(&second, b"remove second").unwrap();
        for format in super::ResolvedFormat::CREATION_FORMATS
            .into_iter()
            .filter(|format| format.archive.is_some())
        {
            let path = directory
                .path()
                .join(format!("remove.{}", format.extension()));
            super::create_archive(
                &path,
                &[first.clone(), second.clone()],
                super::CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    ..Default::default()
                },
            )
            .unwrap();
            super::remove_archive_entry(&path, "second.txt", None)
                .unwrap_or_else(|error| panic!("{}: {error:#}", format.label()));
            let entries = super::list_archive(&path).unwrap();
            assert_eq!(entries.len(), 1, "{}", format.label());
            assert_eq!(entries[0].name, "first.txt");
            let mut file = super::extract_entry(&path, "first.txt").unwrap();
            let mut text = String::new();
            std::io::Read::read_to_string(&mut file, &mut text).unwrap();
            assert_eq!(text, "keep first");
            let before = std::fs::read(&path).unwrap();
            assert!(super::remove_archive_entry(&path, "missing.txt", None).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            super::remove_archive_entry(&path, "first.txt", None)
                .unwrap_or_else(|error| panic!("{}: {error:#}", format.label()));
            assert!(
                super::list_archive(&path)
                    .unwrap_or_else(|error| panic!("empty {}: {error:#}", format.label()))
                    .is_empty(),
                "{}",
                format.label()
            );
        }
    }

    #[test]
    fn adding_folders_preserves_nested_and_empty_entries_in_writable_archives() {
        let directory = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let original = sources.path().join("original");
        let addition = sources.path().join("addition");
        std::fs::create_dir_all(original.join("empty")).unwrap();
        std::fs::create_dir_all(addition.join("nested")).unwrap();
        std::fs::write(original.join(".hidden"), "old hidden").unwrap();
        std::fs::write(addition.join("nested/文字.txt"), "new unicode").unwrap();
        for format in super::ResolvedFormat::CREATION_FORMATS
            .into_iter()
            .filter(|format| format.archive.is_some() && *format != super::ResolvedFormat::Ar)
        {
            let path = directory
                .path()
                .join(format!("folders.{}", format.extension()));
            super::create_archive(
                &path,
                std::slice::from_ref(&original),
                super::CreateOptions {
                    format,
                    include_hidden: true,
                    compression: format.compression_levels()[0],
                    ..Default::default()
                },
            )
            .unwrap();
            super::add_to_archive(&path, std::slice::from_ref(&addition), "docs")
                .unwrap_or_else(|error| panic!("{}: {error:#}", format.label()));
            let entries = super::list_archive(&path).unwrap();
            for name in [
                "original/.hidden",
                "original/empty",
                "docs/addition/nested/文字.txt",
            ] {
                assert!(
                    entries
                        .iter()
                        .any(|entry| entry.name.trim_end_matches('/') == name),
                    "{}: {name}",
                    format.label()
                );
            }
        }
        let path = directory.path().join("flat.ar");
        let file = original.join(".hidden");
        super::create_archive(
            &path,
            &[file],
            super::CreateOptions {
                format: super::ResolvedFormat::Ar,
                compression: super::CompressionLevel::Stored,
                ..Default::default()
            },
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(super::add_to_archive(&path, &[addition], "").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn adding_to_rar_requires_its_writer_and_single_streams_are_not_modified() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        std::fs::write(&source, "data").unwrap();
        let path = directory.path().join("source.txt.gz");
        super::create_archive(
            &path,
            std::slice::from_ref(&source),
            super::CreateOptions {
                format: super::ResolvedFormat::Gzip,
                ..Default::default()
            },
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(!super::can_add_to_archive(&path));
        assert!(super::add_to_archive(&path, std::slice::from_ref(&source), "").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        if super::rar_program().is_none() {
            let path = directory.path().join("target.rar");
            std::fs::write(&path, "unchanged").unwrap();
            assert!(!super::can_add_to_archive(&path));
            let error = super::add_to_archive(&path, &[source], "").unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Install the rar command-line tool")
            );
            assert_eq!(std::fs::read_to_string(path).unwrap(), "unchanged");
        }
    }

    #[test]
    fn adding_files_supports_all_writable_archive_formats_and_preserves_old_content() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        std::fs::write(&first, "original 中文").unwrap();
        std::fs::write(&second, "added 🙂").unwrap();
        for format in super::ResolvedFormat::CREATION_FORMATS
            .into_iter()
            .filter(|format| format.archive.is_some())
        {
            let path = directory
                .path()
                .join(format!("test.{}", format.extension()));
            super::create_archive(
                &path,
                std::slice::from_ref(&first),
                super::CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    ..Default::default()
                },
            )
            .unwrap();
            super::add_to_archive(&path, std::slice::from_ref(&second), "")
                .unwrap_or_else(|error| panic!("{}: {error:#}", format.label()));
            let entries = super::list_archive(&path).unwrap();
            assert_eq!(entries.len(), 2, "{}", format.label());
            for (name, expected) in [("first.txt", "original 中文"), ("second.txt", "added 🙂")]
            {
                let mut file = super::extract_entry(&path, name).unwrap();
                let mut text = String::new();
                std::io::Read::read_to_string(&mut file, &mut text).unwrap();
                assert_eq!(text, expected, "{}: {name}", format.label());
            }
            let original = std::fs::read(&path).unwrap();
            assert!(super::add_to_archive(&path, std::slice::from_ref(&second), "").is_err());
            assert_eq!(
                std::fs::read(&path).unwrap(),
                original,
                "{}",
                format.label()
            );
        }
    }

    #[test]
    fn adding_files_preserves_zip_and_seven_zip_encryption() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        std::fs::write(&first, b"private original").unwrap();
        std::fs::write(&second, b"private addition").unwrap();
        for format in [super::ResolvedFormat::Zip, super::ResolvedFormat::SevenZip] {
            let path = directory
                .path()
                .join(format!("encrypted.{}", format.extension()));
            super::create_archive_with_password(
                &path,
                std::slice::from_ref(&first),
                super::CreateOptions {
                    format,
                    ..Default::default()
                },
                Some("test-password"),
            )
            .unwrap();
            let original = std::fs::read(&path).unwrap();
            assert!(super::add_to_archive(&path, std::slice::from_ref(&second), "").is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
            super::add_to_archive_with_password(
                &path,
                std::slice::from_ref(&second),
                "docs",
                Some("test-password"),
            )
            .unwrap();
            assert!(super::list_archive(&path).is_err());
            let entries = super::list_archive_with_password(&path, Some("test-password")).unwrap();
            assert!(entries.iter().any(|entry| entry.name == "docs/second.txt"));
            assert!(
                super::extract_entry_with_password(&path, "docs/second.txt", Some("wrong"))
                    .is_err()
            );
        }
    }

    #[test]
    fn adding_files_to_zip_preserves_entries_and_rejects_collisions_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        std::fs::write(&first, b"original content").unwrap();
        std::fs::write(&second, "nowe dane 中文").unwrap();
        let path = directory.path().join("archive.zip");
        super::create_zip(&path, &[first]).unwrap();
        super::add_to_archive(&path, std::slice::from_ref(&second), "docs").unwrap();
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        assert_eq!(archive.len(), 2);
        let mut text = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("first.txt").unwrap(), &mut text)
            .unwrap();
        assert_eq!(text, "original content");
        drop(archive);
        let before = std::fs::read(&path).unwrap();
        assert!(super::add_to_archive(&path, &[second], "docs").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(super::add_to_archive(&path, std::slice::from_ref(&path), "").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let folder = directory.path().join("nested");
        std::fs::create_dir_all(folder.join("empty")).unwrap();
        std::fs::write(folder.join("file.txt"), b"nested").unwrap();
        super::add_to_archive(&path, &[folder], "").unwrap();
        assert!(
            super::list_archive(&path)
                .unwrap()
                .iter()
                .any(|entry| entry.name == "nested/empty/")
        );
    }

    use super::{
        CompressionFormat, CompressionLevel, CreateOptions, FormatCapabilities, ResolvedFormat,
        create_archive, create_zip, extract_archive_to, extract_entry,
        extract_single_compressed_to, list_archive, temporary_suffix,
    };
    use std::{
        fs::File,
        io::{Read, Write},
        path::Path,
    };
    use zip::ZipArchive;

    #[test]
    /// Recognizes supported suffixes case-insensitively and excludes RAR from creation choices.
    fn recognizes_supported_extensions_without_case_sensitivity() {
        assert_eq!(
            ResolvedFormat::from_path(Path::new("backup.ZIP")),
            Some(ResolvedFormat::Zip)
        );
        assert_eq!(
            ResolvedFormat::from_path(Path::new("backup.tar")),
            Some(ResolvedFormat::Tar)
        );
        assert_eq!(
            ResolvedFormat::from_path(Path::new("backup.TAR.GZ")),
            Some(ResolvedFormat::TarGz)
        );
        assert_eq!(
            ResolvedFormat::from_path(Path::new("backup.tgz")),
            Some(ResolvedFormat::TarGz)
        );
        for (path, format) in [
            ("backup.tar.bz2", ResolvedFormat::TarBz2),
            ("backup.tbz", ResolvedFormat::TarBz2),
            ("backup.tbz2", ResolvedFormat::TarBz2),
            ("backup.tar.xz", ResolvedFormat::TarXz),
            ("backup.txz", ResolvedFormat::TarXz),
            ("backup.tar.zst", ResolvedFormat::TarZstd),
            ("backup.tar.lz4", ResolvedFormat::TarLz4),
            ("backup.tar.lz", ResolvedFormat::TarLzip),
            ("backup.tar.br", ResolvedFormat::TarBrotli),
            ("backup.gz", ResolvedFormat::Gzip),
            ("backup.bz2", ResolvedFormat::Bzip2),
            ("backup.xz", ResolvedFormat::Xz),
            ("backup.zst", ResolvedFormat::Zstandard),
            ("backup.LZ4", ResolvedFormat::Lz4),
            ("backup.LZ", ResolvedFormat::Lzip),
            ("backup.BR", ResolvedFormat::Brotli),
        ] {
            assert_eq!(ResolvedFormat::from_path(Path::new(path)), Some(format));
        }
        assert_eq!(
            ResolvedFormat::from_path(Path::new("backup.7z")),
            Some(ResolvedFormat::SevenZip)
        );
        assert_eq!(
            ResolvedFormat::from_path(Path::new("backup.rar")),
            Some(ResolvedFormat::Rar)
        );
        assert!(!ResolvedFormat::ALL.contains(&ResolvedFormat::Rar));
    }

    #[test]
    /// Keeps capabilities distinct for writable archives, standalone codecs, and future images.
    fn resolved_format_capabilities_match_implemented_operations() {
        let zip = ResolvedFormat::Zip.capabilities();
        assert!(zip.can_list && zip.can_extract && zip.can_create && zip.can_test);
        assert!(zip.can_modify);

        let tar = ResolvedFormat::TarGz.capabilities();
        assert!(tar.can_list && tar.can_extract && tar.can_create && tar.can_test);

        let standalone = ResolvedFormat::Gzip.capabilities();
        assert!(standalone.can_list && standalone.can_extract && standalone.can_create);

        let rar = ResolvedFormat::Rar.capabilities();
        assert!(rar.can_list && rar.can_extract && rar.can_test);
        assert!(!rar.can_create);
        assert_eq!(rar.can_modify, super::rar_program().is_some());

        assert_eq!(
            ResolvedFormat::Iso.capabilities(),
            FormatCapabilities::default()
        );
        assert_eq!(
            ResolvedFormat::Dmg.capabilities(),
            FormatCapabilities::default()
        );
        assert_eq!(
            ResolvedFormat::Qcow2.capabilities(),
            FormatCapabilities::default()
        );
        assert_eq!(
            ResolvedFormat::Vhdx.capabilities(),
            FormatCapabilities::default()
        );
    }

    #[test]
    /// Retains only short, safe file extensions for temporary document associations.
    fn preserves_safe_file_extensions_for_system_app_associations() {
        assert_eq!(temporary_suffix("reports/annual.pdf"), ".pdf");
        assert_eq!(temporary_suffix("README"), "");
        assert_eq!(temporary_suffix("file.bad-extension"), "");
    }

    #[test]
    /// Extracts a ZIP entry to a temporary file with its original extension and contents.
    fn extracts_a_zip_entry_to_a_temporary_file_with_its_extension() {
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("documents.zip");
        let mut archive = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
        archive
            .start_file(
                "reports/annual.txt",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive.write_all(b"Annual report").unwrap();
        archive.finish().unwrap();

        let mut extracted = extract_entry(&archive_path, "reports/annual.txt").unwrap();
        let mut contents = String::new();
        extracted.read_to_string(&mut contents).unwrap();

        assert_eq!(contents, "Annual report");
        assert_eq!(
            extracted
                .path()
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("txt")
        );
    }

    #[test]
    /// Creates a ZIP archive containing each selected source file.
    fn creates_a_zip_from_selected_files() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("notes.txt");
        let second_path = directory.path().join("data.csv");
        let archive_path = directory.path().join("new.zip");
        std::fs::write(&first_path, b"Notes").unwrap();
        std::fs::write(&second_path, b"Data").unwrap();

        create_zip(&archive_path, &[first_path, second_path]).unwrap();

        let mut archive = ZipArchive::new(File::open(archive_path).unwrap()).unwrap();
        assert_eq!(archive.len(), 2);
        assert_eq!(archive.by_name("notes.txt").unwrap().size(), 5);
        assert_eq!(archive.by_name("data.csv").unwrap().size(), 4);
    }

    #[test]
    /// Creates, lists, and extracts each supported format and compression preset.
    fn creates_and_extracts_every_format_with_all_supported_compression_levels() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("sources");
        std::fs::create_dir_all(source.join("nested/empty")).unwrap();
        let contents = "Archive contents: 中文, polskie znaki.\n".repeat(100);
        std::fs::write(source.join("nested/中文.txt"), contents.as_bytes()).unwrap();
        std::fs::write(source.join(".hidden"), b"hidden").unwrap();
        std::fs::write(source.join("empty.txt"), b"").unwrap();
        for format in ResolvedFormat::ALL
            .into_iter()
            .filter(|format| !format.is_single_compressed() && *format != ResolvedFormat::Ar)
        {
            for &compression in format.compression_levels() {
                let output = directory
                    .path()
                    .join(format!("archive.{}", format.extension()));
                create_archive(
                    &output,
                    &[source.clone()],
                    CreateOptions {
                        format,
                        compression,
                        include_hidden: false,
                        ..CreateOptions::default()
                    },
                )
                .unwrap_or_else(|error| panic!("{format:?}/{compression:?}: {error:#}"));
                let entries = list_archive(&output).unwrap();
                assert!(entries.iter().any(|entry| entry.name.trim_end_matches('/')
                    == "sources/nested/empty"
                    && entry.is_directory));
                assert!(!entries.iter().any(|entry| entry.name.ends_with(".hidden")));
                let mut extracted = extract_entry(&output, "sources/nested/中文.txt").unwrap();
                let mut data = String::new();
                extracted.read_to_string(&mut data).unwrap();
                assert_eq!(data, contents, "{format:?}/{compression:?}");
                let empty = extract_entry(&output, "sources/empty.txt").unwrap();
                assert_eq!(empty.as_file().metadata().unwrap().len(), 0);
                let expanded = directory
                    .path()
                    .join(format!("expanded-{format:?}-{compression:?}"));
                super::extract_archive_to(&output, &expanded, None).unwrap();
                assert_eq!(
                    std::fs::read(expanded.join("sources/nested/中文.txt")).unwrap(),
                    contents.as_bytes()
                );
                assert!(expanded.join("sources/nested/empty").is_dir());
                assert!(!expanded.join("sources/.hidden").exists());
            }
        }
    }

    #[test]
    fn extended_archive_formats_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let name = format!("{}-中文.txt", "long-name-".repeat(15));
        let source = directory.path().join(&name);
        let empty = directory.path().join("empty.txt");
        std::fs::write(&source, b"Extended archive contents\0\xff").unwrap();
        std::fs::write(&empty, b"").unwrap();
        for format in [
            ResolvedFormat::Cpio,
            ResolvedFormat::Ar,
            ResolvedFormat::Pax,
            ResolvedFormat::Xar,
        ] {
            let path = directory
                .path()
                .join(format!("archive.{}", format.extension()));
            assert!(ResolvedFormat::CREATION_FORMATS.contains(&format));
            create_archive(
                &path,
                &[source.clone(), empty.clone()],
                CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    ..CreateOptions::default()
                },
            )
            .unwrap_or_else(|error| panic!("{format:?}: {error:#}"));
            let entries = list_archive(&path)
                .unwrap_or_else(|error| panic!("{format:?} listing failed: {error:#}"));
            assert_eq!(entries.len(), 2, "{format:?}");
            assert!(
                entries.iter().any(|entry| entry.name == name),
                "{format:?}: {:?}",
                entries.iter().map(|entry| &entry.name).collect::<Vec<_>>()
            );
            let mut extracted = extract_entry(&path, &name).unwrap();
            let mut contents = Vec::new();
            extracted.read_to_end(&mut contents).unwrap();
            assert_eq!(contents, b"Extended archive contents\0\xff", "{format:?}");
            let destination = directory
                .path()
                .join(format!("expanded-{}", format.extension()));
            extract_archive_to(&path, &destination, None).unwrap();
            assert_eq!(std::fs::read(destination.join(&name)).unwrap(), contents);
            assert_eq!(
                std::fs::metadata(destination.join("empty.txt"))
                    .unwrap()
                    .len(),
                0
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn extended_archives_interoperate_with_system_reader() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        std::fs::write(&source, b"cross-reader archive contents").unwrap();
        for (format, backend) in [
            (ResolvedFormat::Cpio, "cpio"),
            (ResolvedFormat::Ar, "ar"),
            (ResolvedFormat::Pax, "pax"),
            (ResolvedFormat::Xar, "xar"),
        ] {
            let output = root.path().join(format!("ours.{}", format.extension()));
            create_archive(
                &output,
                std::slice::from_ref(&source),
                CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    ..CreateOptions::default()
                },
            )
            .unwrap();
            let listing = std::process::Command::new("/usr/bin/tar")
                .arg("-tf")
                .arg(&output)
                .output()
                .unwrap();
            assert!(
                listing.status.success(),
                "{format:?}: {}",
                String::from_utf8_lossy(&listing.stderr)
            );
            assert!(String::from_utf8_lossy(&listing.stdout).contains("notes.txt"));
            let external = root.path().join(format!("external.{}", format.extension()));
            let created = std::process::Command::new("/usr/bin/tar")
                .arg(format!("--format={backend}"))
                .arg("-cf")
                .arg(&external)
                .arg("-C")
                .arg(root.path())
                .arg("notes.txt")
                .output()
                .unwrap();
            assert!(
                created.status.success(),
                "{format:?}: {}",
                String::from_utf8_lossy(&created.stderr)
            );
            let mut extracted = extract_entry(&external, "notes.txt").unwrap();
            let mut contents = Vec::new();
            extracted.read_to_end(&mut contents).unwrap();
            assert_eq!(contents, b"cross-reader archive contents");
        }
    }

    #[test]
    fn extended_archives_reject_links_and_traversal() {
        use libarchive2::{ArchiveFormat as BackendFormat, EntryMut, FileType, WriteArchive};
        let root = tempfile::tempdir().unwrap();
        for (format, backend) in [
            (ResolvedFormat::Cpio, BackendFormat::Cpio),
            (ResolvedFormat::Pax, BackendFormat::TarPax),
            (ResolvedFormat::Xar, BackendFormat::Xar),
        ] {
            let path = root.path().join(format!("links.{}", format.extension()));
            let mut writer = WriteArchive::new()
                .format(backend)
                .open_file(&path)
                .unwrap();
            let mut entry = EntryMut::new();
            entry.set_pathname("link").unwrap();
            entry.set_file_type(FileType::SymbolicLink);
            entry.set_size(0);
            entry.set_symlink("../outside").unwrap();
            entry.set_perm(0o777).unwrap();
            writer.write_header(&entry).unwrap();
            writer.finish().unwrap();
            let destination = root
                .path()
                .join(format!("links-out-{}", format.extension()));
            assert!(
                extract_archive_to(&path, &destination, None).is_err(),
                "{format:?}"
            );
            assert!(!destination.exists());
            assert!(extract_entry(&path, "link").is_err());
        }
        for (format, backend) in [
            (ResolvedFormat::Cpio, BackendFormat::Cpio),
            (ResolvedFormat::Pax, BackendFormat::TarPax),
        ] {
            let path = root
                .path()
                .join(format!("traversal.{}", format.extension()));
            let mut writer = WriteArchive::new()
                .format(backend)
                .open_file(&path)
                .unwrap();
            writer.add_file("../outside", b"unsafe").unwrap();
            writer.finish().unwrap();
            let destination = root
                .path()
                .join(format!("unsafe-out-{}", format.extension()));
            assert!(extract_archive_to(&path, &destination, None).is_err());
            assert!(!destination.exists());
            assert!(!root.path().join("outside").exists());
        }
    }

    #[test]
    fn extended_ar_rejects_folders_without_overwriting_output() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("archive.ar");
        std::fs::write(&path, b"existing data").unwrap();
        assert!(
            create_archive(
                &path,
                &[root.path().to_path_buf()],
                CreateOptions {
                    format: ResolvedFormat::Ar,
                    compression: CompressionLevel::Stored,
                    ..CreateOptions::default()
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"existing data");
    }

    #[test]
    /// Resolves one logical TAR type to each selected compression format on disk.
    fn creates_logical_tar_with_each_compression_method() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("notes.txt");
        std::fs::write(&source, b"TAR compression methods").unwrap();

        for method in CompressionFormat::ALL {
            let options = CreateOptions {
                format: ResolvedFormat::tar(method),
                ..CreateOptions::default()
            };
            let format = options.output_format();
            let path = directory
                .path()
                .join(format!("logical-tar.{}", format.extension()));
            let compression = if format
                .compression_levels()
                .contains(&CompressionLevel::Normal)
            {
                CompressionLevel::Normal
            } else {
                format.compression_levels()[0]
            };
            create_archive(
                &path,
                &[source.clone()],
                CreateOptions {
                    compression,
                    ..options
                },
            )
            .unwrap();

            let entries = list_archive(&path).unwrap();
            assert_eq!(entries.len(), 1, "{method:?}");
            let mut extracted = extract_entry(&path, "notes.txt").unwrap();
            let mut contents = String::new();
            extracted.read_to_string(&mut contents).unwrap();
            assert_eq!(contents, "TAR compression methods", "{method:?}");
        }
    }

    #[test]
    /// Creates and extracts every supported standalone compression stream.
    fn creates_and_extracts_standalone_compressed_files() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("notes.txt");
        std::fs::write(&source, b"Standalone compressed file contents").unwrap();

        for format in [
            ResolvedFormat::Gzip,
            ResolvedFormat::Bzip2,
            ResolvedFormat::Xz,
            ResolvedFormat::Zstandard,
            ResolvedFormat::Lz4,
            ResolvedFormat::Lzip,
            ResolvedFormat::Brotli,
        ] {
            assert!(ResolvedFormat::CREATION_FORMATS.contains(&format));
            for &compression in format.compression_levels() {
                let path = directory
                    .path()
                    .join(format!("notes.txt.{}", format.extension()));
                assert!(super::is_supported_archive(&path));
                create_archive(
                    &path,
                    std::slice::from_ref(&source),
                    CreateOptions {
                        format,
                        compression,
                        ..CreateOptions::default()
                    },
                )
                .unwrap();
                let entries = list_archive(&path).unwrap();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].name, "notes.txt");
                assert!(!entries[0].is_directory);
                let mut extracted = extract_entry(&path, "notes.txt").unwrap();
                let mut contents = String::new();
                extracted.read_to_string(&mut contents).unwrap();
                assert_eq!(contents, "Standalone compressed file contents");

                let destination = directory
                    .path()
                    .join(format!("expanded-{}-{compression:?}", format.extension()));
                extract_archive_to(&path, &destination, None).unwrap();
                assert_eq!(
                    std::fs::read(destination.join("notes.txt")).unwrap(),
                    b"Standalone compressed file contents"
                );

                let file_destination = directory.path().join(format!(
                    "restored-{}-{compression:?}.txt",
                    format.extension()
                ));
                extract_single_compressed_to(&path, &file_destination).unwrap();
                assert_eq!(
                    std::fs::read(&file_destination).unwrap(),
                    b"Standalone compressed file contents"
                );
                assert!(extract_single_compressed_to(&path, &file_destination).is_err());
            }
        }

        assert!(
            create_archive(
                &directory.path().join("multiple.gz"),
                &[source.clone(), source.clone()],
                CreateOptions {
                    format: ResolvedFormat::Gzip,
                    ..CreateOptions::default()
                },
            )
            .is_err()
        );
        assert!(
            create_archive(
                &directory.path().join("folder.gz"),
                &[directory.path().to_path_buf()],
                CreateOptions {
                    format: ResolvedFormat::Gzip,
                    ..CreateOptions::default()
                },
            )
            .is_err()
        );
    }

    #[test]
    /// Preserves an existing output when source validation or option checks fail.
    fn archive_creation_preserves_existing_output_when_validation_fails() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output.zip");
        std::fs::write(&output, b"existing archive").unwrap();
        assert!(
            create_archive(
                &output,
                &[directory.path().join("missing")],
                CreateOptions::default()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"existing archive");
        assert!(
            create_archive(
                &output,
                &[directory.path().to_path_buf()],
                CreateOptions::default()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"existing archive");
        let invalid = CreateOptions {
            format: ResolvedFormat::SevenZip,
            compression: CompressionLevel::Stored,
            ..CreateOptions::default()
        };
        assert!(create_archive(&directory.path().join("invalid.7z"), &[output], invalid).is_err());
    }

    #[test]
    /// Includes dot-prefixed files when the creation option allows hidden files.
    fn archive_creation_can_include_hidden_files() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("folder");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join(".hidden"), b"hidden").unwrap();
        let output = directory.path().join("hidden.zip");
        create_archive(&output, &[source], CreateOptions::default()).unwrap();
        assert!(
            list_archive(&output)
                .unwrap()
                .iter()
                .any(|entry| entry.name == "folder/.hidden")
        );
    }

    #[test]
    /// Requires and validates passwords for ZIP and 7z creation and extraction.
    fn encrypted_archives_require_and_validate_passwords() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("private.txt");
        std::fs::write(&source, b"Private archive contents").unwrap();
        for (format, solid, encrypt_headers) in [
            (ResolvedFormat::Zip, false, false),
            (ResolvedFormat::SevenZip, false, false),
            (ResolvedFormat::SevenZip, false, true),
            (ResolvedFormat::SevenZip, true, true),
        ] {
            let options = CreateOptions {
                format,
                solid,
                encrypt_headers,
                zip64: true,
                compression: CompressionLevel::Fast,
                dictionary_mib: 1,
                ..CreateOptions::default()
            };
            let output = directory
                .path()
                .join(format!("private.{}", format.extension()));
            super::create_archive_with_password(
                &output,
                &[source.clone()],
                options,
                Some("test-password"),
            )
            .unwrap();
            let error = list_archive(&output)
                .err()
                .expect("password must be required");
            assert_eq!(
                error.downcast_ref::<super::PasswordFailure>(),
                Some(&super::PasswordFailure::Required),
                "{error:#}"
            );
            assert!(super::list_archive_with_password(&output, Some("wrong-password")).is_err());
            let entries =
                super::list_archive_with_password(&output, Some("test-password")).unwrap();
            assert_eq!(entries.len(), 1);
            let mut file =
                super::extract_entry_with_password(&output, "private.txt", Some("test-password"))
                    .unwrap();
            let mut contents = String::new();
            file.read_to_string(&mut contents).unwrap();
            assert_eq!(contents, "Private archive contents");
            let expanded = directory
                .path()
                .join(format!("expanded-{format:?}-{solid}-{encrypt_headers}"));
            assert!(super::extract_archive_to(&output, &expanded, Some("wrong-password")).is_err());
            assert!(!expanded.exists());
            super::extract_archive_to(&output, &expanded, Some("test-password")).unwrap();
            assert_eq!(
                std::fs::read(expanded.join("private.txt")).unwrap(),
                b"Private archive contents"
            );
        }
        let output = directory.path().join("invalid.tar");
        assert!(
            super::create_archive_with_password(
                &output,
                &[source],
                CreateOptions {
                    format: ResolvedFormat::Tar,
                    compression: CompressionLevel::Stored,
                    ..CreateOptions::default()
                },
                Some("test-password")
            )
            .is_err()
        );
        assert!(!output.exists());
    }

    #[test]
    /// Extracts only a selected file or folder across all supported archive formats.
    fn extracts_only_selected_file_or_folder_for_each_format() {
        let sources = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let source = sources.path().join("source");
        std::fs::create_dir_all(source.join("selected/nested/empty")).unwrap();
        std::fs::create_dir_all(source.join("selected-other")).unwrap();
        std::fs::create_dir_all(source.join("000-before")).unwrap();
        std::fs::write(source.join("selected/nested/notes.txt"), b"selected data").unwrap();
        std::fs::write(source.join("selected-other/skip.txt"), b"outside").unwrap();
        std::fs::write(source.join("000-before/skip.txt"), b"before").unwrap();
        for format in ResolvedFormat::ALL
            .into_iter()
            .filter(|format| !format.is_single_compressed() && *format != ResolvedFormat::Ar)
        {
            let password = format.supports_password().then_some("selection-password");
            let path = output
                .path()
                .join(format!("archive.{}", format.extension()));
            super::create_archive_with_password(
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
            let folder = output.path().join(format!("selected-{format:?}"));
            super::extract_archive_selection_to(&path, &folder, password, Some("source/selected"))
                .unwrap();
            assert_eq!(
                std::fs::read(folder.join("nested/notes.txt")).unwrap(),
                b"selected data"
            );
            assert!(folder.join("nested/empty").is_dir());
            assert!(!folder.join("source").exists());
            assert!(!folder.join("selected-other").exists());
            assert!(!folder.join("000-before").exists());
            let file = output.path().join(format!("single-{format:?}.txt"));
            super::extract_archive_selection_to(
                &path,
                &file,
                password,
                Some("./source/selected/nested/notes.txt"),
            )
            .unwrap();
            assert_eq!(std::fs::read(&file).unwrap(), b"selected data");
            assert!(
                super::extract_archive_selection_to(
                    &path,
                    &file,
                    password,
                    Some("source/selected/nested/notes.txt")
                )
                .is_err()
            );
            let missing = output.path().join(format!("missing-{format:?}"));
            assert!(
                super::extract_archive_selection_to(&path, &missing, password, Some("missing.txt"))
                    .is_err()
            );
            assert!(!missing.exists());
        }
    }

    #[test]
    /// Rejects traversal paths and existing destinations without publishing partial output.
    fn archive_extraction_rejects_unsafe_paths_and_existing_destinations() {
        let directory = tempfile::tempdir().unwrap();
        for name in [
            "../escape.txt",
            "/absolute.txt",
            "C:\\escape.txt",
            "nested/../../escape.txt",
            "..\\escape.txt",
        ] {
            assert!(super::safe_extraction_path(name).is_err());
        }
        let output = directory.path().join("malicious.zip");
        let mut zip = zip::ZipWriter::new(File::create(&output).unwrap());
        zip.start_file("../escape.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"unsafe").unwrap();
        zip.finish().unwrap();
        let destination = directory.path().join("expanded");
        assert!(super::extract_archive_to(&output, &destination, None).is_err());
        assert!(!destination.exists());
        assert!(!directory.path().join("escape.txt").exists());
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("keep.txt"), b"keep").unwrap();
        assert!(super::extract_archive_to(&output, &destination, None).is_err());
        assert_eq!(
            std::fs::read(destination.join("keep.txt")).unwrap(),
            b"keep"
        );
    }

    #[test]
    /// Verifies deterministic TAR output and ZIP64 metadata settings.
    fn deterministic_tar_and_zip64_are_applied() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("file.txt");
        std::fs::write(&source, b"contents").unwrap();
        let output = directory.path().join("deterministic.tar");
        let options = CreateOptions {
            format: ResolvedFormat::Tar,
            compression: CompressionLevel::Stored,
            deterministic: true,
            ..CreateOptions::default()
        };
        create_archive(&output, &[source.clone()], options).unwrap();
        let first = std::fs::read(&output).unwrap();
        create_archive(&output, &[source.clone()], options).unwrap();
        assert_eq!(first, std::fs::read(&output).unwrap());
        let output = directory.path().join("zip64.zip");
        create_archive(
            &output,
            &[source],
            CreateOptions {
                zip64: true,
                ..CreateOptions::default()
            },
        )
        .unwrap();
        let bytes = std::fs::read(&output).unwrap();
        assert_eq!(&bytes[4..6], &[45, 0]);
        assert_eq!(list_archive(&output).unwrap()[0].size, 8);
    }

    #[test]
    #[ignore = "creates user-requested encrypted samples"]
    /// Generates encrypted sample archives for manual password-flow testing.
    fn generate_password_samples() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("protected");
        std::fs::create_dir_all(source.join("documents")).unwrap();
        std::fs::write(
            source.join("documents/readme.txt"),
            b"Encrypted ZArchive sample.\n",
        )
        .unwrap();
        std::fs::write(
            source.join("documents/data.csv"),
            b"name,value\nalpha,10\nbeta,200\n",
        )
        .unwrap();
        std::fs::write(source.join("中文.txt"), "加密测试文件\n").unwrap();
        let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        std::fs::create_dir_all(&samples).unwrap();
        for (name, format, encrypt_headers, solid) in [
            ("08-password-aes.zip", ResolvedFormat::Zip, false, false),
            ("09-password.7z", ResolvedFormat::SevenZip, true, false),
            (
                "10-password-visible-names.7z",
                ResolvedFormat::SevenZip,
                false,
                false,
            ),
            ("11-password-solid.7z", ResolvedFormat::SevenZip, true, true),
        ] {
            let output = samples.join(name);
            if !output.exists() {
                super::create_archive_with_password(
                    &output,
                    &[source.clone()],
                    CreateOptions {
                        format,
                        encrypt_headers,
                        solid,
                        dictionary_mib: 1,
                        compression: CompressionLevel::Fast,
                        ..CreateOptions::default()
                    },
                    Some("zarchive-test-2026"),
                )
                .unwrap();
            }
            assert!(list_archive(&output).is_err());
            assert!(super::list_archive_with_password(&output, Some("zarchive-test-2026")).is_ok());
        }
    }

    #[test]
    #[ignore = "generates samples for TAR and standalone compression methods"]
    /// Generates and verifies TAR variants, aliases, and standalone codec samples.
    fn generate_tar_compression_samples() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("tar-samples");
        std::fs::create_dir_all(source.join("documents/nested/empty")).unwrap();
        std::fs::write(
            source.join("README.txt"),
            b"Sample archive for testing TAR compression methods.\n",
        )
        .unwrap();
        std::fs::write(source.join("documents/data.csv"), b"name,value\nalpha,10\n").unwrap();
        std::fs::write(
            source.join("documents/nested/中文.txt"),
            "Unicode TAR sample\n",
        )
        .unwrap();

        let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        std::fs::create_dir_all(&samples).unwrap();
        let variants = [
            ("15-tar.tar", CompressionFormat::None),
            ("16-tar-gzip.tar.gz", CompressionFormat::Gzip),
            ("17-tar-bzip2.tar.bz2", CompressionFormat::Bzip2),
            ("18-tar-xz.tar.xz", CompressionFormat::Xz),
            ("19-tar-zstd.tar.zst", CompressionFormat::Zstandard),
            ("20-tar-lz4.tar.lz4", CompressionFormat::Lz4),
            ("21-tar-lzip.tar.lz", CompressionFormat::Lzip),
            ("22-tar-brotli.tar.br", CompressionFormat::Brotli),
        ];

        for (filename, method) in variants {
            let options = CreateOptions {
                format: ResolvedFormat::tar(method),
                compression: if method == CompressionFormat::None {
                    CompressionLevel::Stored
                } else if method == CompressionFormat::Lz4 {
                    CompressionLevel::Fast
                } else {
                    CompressionLevel::Normal
                },
                deterministic: true,
                ..CreateOptions::default()
            };
            let output = samples.join(filename);
            if !output.exists() {
                create_archive(&output, std::slice::from_ref(&source), options).unwrap();
            }
            let entries = list_archive(&output).unwrap();
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.name == "tar-samples/README.txt")
            );
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.name == "tar-samples/documents/nested/中文.txt")
            );
            assert!(entries.iter().any(|entry| {
                entry.name.trim_end_matches('/') == "tar-samples/documents/nested/empty"
                    && entry.is_directory
            }));
            let extracted = directory.path().join(format!("extracted-{filename}"));
            extract_archive_to(&output, &extracted, None).unwrap();
            assert_eq!(
                std::fs::read(extracted.join("tar-samples/README.txt")).unwrap(),
                b"Sample archive for testing TAR compression methods.\n"
            );
            assert_eq!(
                std::fs::read(extracted.join("tar-samples/documents/nested/中文.txt")).unwrap(),
                "Unicode TAR sample\n".as_bytes()
            );
        }

        for (source_name, alias_name) in [
            ("16-tar-gzip.tar.gz", "23-tar-gzip.tgz"),
            ("17-tar-bzip2.tar.bz2", "24-tar-bzip2.tbz"),
            ("17-tar-bzip2.tar.bz2", "25-tar-bzip2.tbz2"),
            ("18-tar-xz.tar.xz", "26-tar-xz.txz"),
        ] {
            let original = samples.join(source_name);
            let alias = samples.join(alias_name);
            if !alias.exists() {
                std::fs::copy(&original, &alias).unwrap();
            }
            assert!(list_archive(&alias).is_ok(), "{}", alias.display());
            let mut extracted = extract_entry(&alias, "tar-samples/README.txt").unwrap();
            let mut contents = String::new();
            extracted.read_to_string(&mut contents).unwrap();
            assert!(contents.contains("TAR compression methods"));
        }

        let standalone_source = source.join("README.txt");
        for (filename, format) in [
            ("27-standalone-gzip.txt.gz", ResolvedFormat::Gzip),
            ("28-standalone-bzip2.txt.bz2", ResolvedFormat::Bzip2),
            ("29-standalone-xz.txt.xz", ResolvedFormat::Xz),
            ("30-standalone-zstd.txt.zst", ResolvedFormat::Zstandard),
        ] {
            let output = samples.join(filename);
            if !output.exists() {
                create_archive(
                    &output,
                    std::slice::from_ref(&standalone_source),
                    CreateOptions {
                        format,
                        compression: CompressionLevel::Normal,
                        ..CreateOptions::default()
                    },
                )
                .unwrap();
            }
            let entries = list_archive(&output).unwrap();
            let expected_name = filename.rsplit_once('.').unwrap().0;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, expected_name);
            let restored = directory.path().join(format!("restored-{filename}"));
            extract_single_compressed_to(&output, &restored).unwrap();
            assert_eq!(
                std::fs::read(restored).unwrap(),
                b"Sample archive for testing TAR compression methods.\n"
            );
        }
    }

    #[test]
    #[ignore = "generates user-requested CPIO, AR, PAX, and XAR samples"]
    fn generate_extended_archive_samples() {
        use crate::inspection::{InspectionMode, Integrity, inspect};
        use std::sync::atomic::AtomicBool;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("archive-samples");
        std::fs::create_dir_all(source.join("documents/nested/empty")).unwrap();
        let files = [
            (
                "README.txt",
                "Sample archive for testing CPIO, AR, PAX, and XAR.\n",
            ),
            ("documents/data.csv", "name,value\nalpha,10\nbeta,20\n"),
            (
                "documents/nested/中文.txt",
                "Unicode filename sample: 中文, polskie znaki.\n",
            ),
            ("empty.txt", ""),
        ];
        for (name, contents) in files {
            std::fs::write(source.join(name), contents).unwrap();
        }
        let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        std::fs::create_dir_all(&samples).unwrap();
        for (filename, format) in [
            ("31-cpio.cpio", ResolvedFormat::Cpio),
            ("32-ar.ar", ResolvedFormat::Ar),
            ("33-pax.pax", ResolvedFormat::Pax),
            ("34-xar.xar", ResolvedFormat::Xar),
        ] {
            let output = samples.join(filename);
            let flat = format == ResolvedFormat::Ar;
            let sources = if flat {
                files
                    .iter()
                    .map(|(name, _)| source.join(name))
                    .collect::<Vec<_>>()
            } else {
                vec![source.clone()]
            };
            if !output.exists() {
                create_archive(
                    &output,
                    &sources,
                    CreateOptions {
                        format,
                        compression: format.compression_levels()[0],
                        ..CreateOptions::default()
                    },
                )
                .unwrap();
            }
            let entries = list_archive(&output).unwrap();
            let extracted = directory.path().join(format!("extracted-{filename}"));
            extract_archive_to(&output, &extracted, None).unwrap();
            for (name, contents) in files {
                let expected_name = if flat {
                    Path::new(name)
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned()
                } else {
                    format!("archive-samples/{name}")
                };
                assert!(
                    entries.iter().any(|entry| entry.name == expected_name),
                    "{filename}: {expected_name}"
                );
                assert_eq!(
                    std::fs::read(extracted.join(expected_name)).unwrap(),
                    contents.as_bytes()
                );
            }
            if flat {
                assert!(entries.iter().all(|entry| !entry.is_directory));
            } else {
                assert!(
                    extracted
                        .join("archive-samples/documents/nested/empty")
                        .is_dir()
                );
            }
            let report = inspect(
                &output,
                None,
                InspectionMode::Audit,
                &AtomicBool::new(false),
            );
            assert_eq!(
                report.integrity,
                Integrity::Passed,
                "{filename}: {:?}",
                report.error
            );
            assert!(
                report.findings.is_empty(),
                "{filename}: {:?}",
                report.findings
            );
            println!("Verified sample: {}", output.display());
        }
    }

    #[test]
    /// Rejects empty, duplicate-name, and output-overwriting ZIP inputs.
    fn rejects_empty_duplicate_and_overwriting_zip_inputs() {
        let directory = tempfile::tempdir().unwrap();
        let output_path = directory.path().join("archive.zip");
        assert!(create_zip(&output_path, &[]).is_err());

        let first_directory = directory.path().join("first");
        let second_directory = directory.path().join("second");
        std::fs::create_dir(&first_directory).unwrap();
        std::fs::create_dir(&second_directory).unwrap();
        let first_path = first_directory.join("same.txt");
        let second_path = second_directory.join("same.txt");
        std::fs::write(&first_path, b"first").unwrap();
        std::fs::write(&second_path, b"second").unwrap();
        assert!(create_zip(&output_path, &[first_path.clone(), second_path]).is_err());

        assert!(create_zip(&first_path, &[first_path.clone()]).is_err());
        assert_eq!(std::fs::read(first_path).unwrap(), b"first");
    }
}
