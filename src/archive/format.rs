use std::path::{Path, PathBuf};

use super::write::rar_program;

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
    /// Whether this entry is encrypted or is a password-protected archive.
    pub is_encrypted: bool,
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
    /// RAR archive, written through the external `rar` command-line tool.
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
pub struct VolumeCapabilities {
    pub can_read: bool,
    pub can_create: bool,
    pub native: bool,
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
    pub volumes: VolumeCapabilities,
}

impl FormatCapabilities {
    const READ_ONLY_ARCHIVE: Self = Self {
        can_list: true,
        can_extract: true,
        can_create: false,
        can_modify: false,
        can_test: true,
        volumes: VolumeCapabilities {
            can_read: false,
            can_create: false,
            native: false,
        },
    };

    const CREATEABLE_ARCHIVE: Self = Self {
        can_list: true,
        can_extract: true,
        can_create: true,
        can_modify: false,
        can_test: true,
        volumes: VolumeCapabilities {
            can_read: false,
            can_create: false,
            native: false,
        },
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
    pub const CREATION_FORMATS: [Self; 15] = [
        Self::Zip,
        Self::Tar,
        Self::Cpio,
        Self::Ar,
        Self::Pax,
        Self::Xar,
        Self::SevenZip,
        Self::Rar,
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
        let mut capabilities = match (self.archive, self.compression, self.container) {
            (Some(archive), codec, None)
                if archive.can_create()
                    && (archive == ArchiveFormat::Tar || codec == CompressionFormat::None) =>
            {
                FormatCapabilities {
                    can_modify: true,
                    ..FormatCapabilities::CREATEABLE_ARCHIVE
                }
            }
            (Some(ArchiveFormat::Rar), CompressionFormat::None, None) => {
                let available = rar_program().is_some();
                FormatCapabilities {
                    can_create: available,
                    can_modify: available,
                    ..FormatCapabilities::READ_ONLY_ARCHIVE
                }
            }
            (None, codec, None) if codec.supports_standalone() => {
                FormatCapabilities::CREATEABLE_ARCHIVE
            }
            _ => FormatCapabilities::default(),
        };
        capabilities.volumes = match (self.archive, self.compression, self.container) {
            (Some(ArchiveFormat::Zip | ArchiveFormat::SevenZip | ArchiveFormat::Tar), _, None)
                if capabilities.can_create =>
            {
                VolumeCapabilities {
                    can_read: true,
                    can_create: true,
                    native: false,
                }
            }
            (Some(ArchiveFormat::Rar), CompressionFormat::None, None) => VolumeCapabilities {
                can_read: true,
                can_create: false,
                native: true,
            },
            _ if capabilities.can_list => VolumeCapabilities {
                can_read: true,
                can_create: false,
                native: false,
            },
            _ => VolumeCapabilities::default(),
        };
        capabilities
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
            (Some(ArchiveFormat::Rar), CompressionFormat::None, None) => {
                &[Stored, Fast, Normal, Best]
            }
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
        if let Some(base) = crate::volumes::numbered_base(path) {
            return Self::from_path(&base)
                .filter(|format| *format != Self::Rar && format.capabilities().can_list);
        }
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                let extension = extension.to_ascii_lowercase();
                extension.len() == 3
                    && extension.starts_with('r')
                    && extension.as_bytes()[1..].iter().all(u8::is_ascii_digit)
            })
        {
            return Some(Self::Rar);
        }
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
    pub(super) fn level(self) -> u32 {
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
    pub volume_size: Option<u64>,
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
            volume_size: None,
        }
    }
}

impl CreateOptions {
    pub(crate) fn first_output_path(self, path: &Path) -> PathBuf {
        if self.volume_size.is_some() {
            crate::volumes::part_path(path, 1)
        } else {
            path.to_owned()
        }
    }

    /// Resolves the selected type and TAR method to a concrete output format.
    pub(crate) fn output_format(self) -> ResolvedFormat {
        self.format
    }
}
