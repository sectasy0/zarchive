//! Reads numbered archive volumes as one seekable stream.

use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const MAX_VOLUMES: usize = 10_000;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn part_path(base: &Path, index: usize) -> PathBuf {
    let mut name = base.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{index:03}"));
    base.with_file_name(name)
}

fn manifest_path(base: &Path) -> PathBuf {
    let mut name = base.file_name().unwrap_or_default().to_os_string();
    name.push(".volumes.json");
    base.with_file_name(name)
}

struct Manifest {
    sizes: Vec<u64>,
    hashes: Vec<String>,
    total_size: u64,
}

fn invalid_manifest() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid archive volume manifest",
    )
}

fn read_manifest(base: &Path) -> io::Result<Option<Manifest>> {
    let file = match File::open(manifest_path(base)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > MAX_MANIFEST_BYTES {
        return Err(invalid_manifest());
    }
    let value: serde_json::Value = serde_json::from_reader(file.take(MAX_MANIFEST_BYTES + 1))
        .map_err(|_| invalid_manifest())?;
    if value["version"].as_u64() != Some(1) || value["format"].as_str() != Some("numbered-stream") {
        return Err(invalid_manifest());
    }
    let size = value["volume_size"]
        .as_u64()
        .filter(|size| *size > 0)
        .ok_or_else(invalid_manifest)?;
    let total_size = value["total_size"]
        .as_u64()
        .filter(|size| *size > 0)
        .ok_or_else(invalid_manifest)?;
    let parts = value["volumes"]
        .as_array()
        .filter(|parts| !parts.is_empty() && parts.len() <= MAX_VOLUMES)
        .ok_or_else(invalid_manifest)?;
    let mut manifest = Manifest {
        sizes: Vec::new(),
        hashes: Vec::new(),
        total_size,
    };
    let mut total = 0u64;
    for (index, part) in parts.iter().enumerate() {
        let expected = part_path(base, index + 1);
        if part["name"].as_str() != expected.file_name().and_then(|name| name.to_str()) {
            return Err(invalid_manifest());
        }
        let length = part["size"]
            .as_u64()
            .filter(|length| *length > 0 && *length <= size)
            .ok_or_else(invalid_manifest)?;
        if index + 1 < parts.len() && length != size {
            return Err(invalid_manifest());
        }
        let hash = part["sha256"]
            .as_str()
            .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(invalid_manifest)?;
        total = total.checked_add(length).ok_or_else(invalid_manifest)?;
        manifest.sizes.push(length);
        manifest.hashes.push(hash.to_ascii_lowercase());
    }
    if total != total_size {
        return Err(invalid_manifest());
    }
    Ok(Some(manifest))
}

fn ensure_available(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The destination already exists. Choose another location.",
        )),
    }
}

struct Publication {
    paths: Vec<PathBuf>,
    committed: bool,
}

impl Drop for Publication {
    fn drop(&mut self) {
        if !self.committed {
            for path in self.paths.iter().rev() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

pub(crate) fn publish(file: &mut File, base: &Path, size: u64) -> anyhow::Result<()> {
    anyhow::ensure!(size > 0, "Enter a valid volume size");
    let length = file.metadata()?.len();
    let count = length.div_ceil(size);
    anyhow::ensure!(
        count > 0 && count <= MAX_VOLUMES as u64,
        "Archive volume limit exceeded"
    );
    ensure_available(base)?;
    ensure_available(&manifest_path(base))?;
    anyhow::ensure!(
        discover_parts(base)?.is_empty(),
        "The destination already exists. Choose another location."
    );
    let parent = base
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut staged = Vec::new();
    let mut records = Vec::new();
    let mut remaining = length;
    file.rewind()?;
    let mut buffer = [0u8; 65536];
    for index in 1..=count as usize {
        crate::task_queue::checkpoint()?;
        let destination = part_path(base, index);
        ensure_available(&destination)?;
        let mut part = tempfile::NamedTempFile::new_in(parent)?;
        let part_size = remaining.min(size);
        let mut bytes_left = part_size;
        let mut hash = Sha256::new();
        while bytes_left > 0 {
            crate::task_queue::checkpoint()?;
            let count = bytes_left.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..count])?;
            part.write_all(&buffer[..count])?;
            hash.update(&buffer[..count]);
            bytes_left -= count as u64;
        }
        part.as_file().sync_all()?;
        records.push(serde_json::json!({
            "name": destination.file_name().and_then(|name| name.to_str()).ok_or_else(invalid_manifest)?,
            "size": part_size, "sha256": format!("{:x}", hash.finalize()),
        }));
        staged.push((part.into_temp_path(), destination));
        remaining -= part_size;
    }
    let mut manifest = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(
        &mut manifest,
        &serde_json::json!({
            "version": 1, "format": "numbered-stream", "volume_size": size,
            "total_size": length, "volumes": records,
        }),
    )?;
    manifest.as_file().sync_all()?;
    staged.push((manifest.into_temp_path(), manifest_path(base)));
    let mut publication = Publication {
        paths: Vec::new(),
        committed: false,
    };
    for (temporary, destination) in staged {
        crate::task_queue::checkpoint()?;
        temporary
            .persist_noclobber(&destination)
            .map_err(|error| error.error)?;
        publication.paths.push(destination);
    }
    publication.committed = true;
    Ok(())
}

pub(crate) fn verify(
    path: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    budget: u64,
) -> io::Result<bool> {
    let Some(base) = numbered_base(path) else {
        return Ok(true);
    };
    let Some(manifest) = read_manifest(&base)? else {
        return Ok(true);
    };
    let parts = paths(path)?;
    if manifest.total_size > budget {
        return Ok(false);
    }
    let mut buffer = [0u8; 65536];
    for (part, (expected, size)) in parts
        .iter()
        .zip(manifest.hashes.iter().zip(&manifest.sizes))
    {
        let mut file = File::open(part)?;
        let mut hash = Sha256::new();
        let mut remaining = *size;
        while remaining > 0 {
            crate::task_queue::checkpoint()?;
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(io::Error::other("Inspection cancelled"));
            }
            let limit = remaining.min(buffer.len() as u64) as usize;
            let count = file.read(&mut buffer[..limit])?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Archive volume was truncated",
                ));
            }
            hash.update(&buffer[..count]);
            remaining -= count as u64;
        }
        if file.read(&mut buffer[..1])? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Archive volume size is invalid",
            ));
        }
        if format!("{:x}", hash.finalize()) != *expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Archive volume checksum mismatch",
            ));
        }
    }
    Ok(true)
}

pub(crate) fn numbered_base(path: &Path) -> Option<PathBuf> {
    let extension = path.extension()?.to_str()?;
    if extension.len() < 3 || !extension.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(path.with_extension(""))
}

pub(crate) fn paths(path: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(base) = numbered_base(path) else {
        return Ok(vec![path.to_owned()]);
    };
    let parts = discover_parts(&base)?;
    if parts.is_empty() || !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Archive volume is missing",
        ));
    }
    for (expected, index) in (1..).zip(parts.keys()) {
        if expected != *index {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Archive volume is missing",
            ));
        }
    }
    let parts: Vec<_> = parts.into_values().collect();
    if let Some(manifest) = read_manifest(&base)? {
        if parts.len() < manifest.sizes.len() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Archive volume is missing",
            ));
        }
        if parts.len() != manifest.sizes.len() {
            return Err(invalid_manifest());
        }
        for (index, (part, expected)) in parts.iter().zip(manifest.sizes).enumerate() {
            if part.file_name() != part_path(&base, index + 1).file_name()
                || part.metadata()?.len() != expected
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Archive volume size is invalid",
                ));
            }
        }
    }
    Ok(parts)
}

fn discover_parts(base: &Path) -> io::Result<BTreeMap<usize, PathBuf>> {
    let prefix = format!("{}.", base.file_name().unwrap().to_string_lossy());
    let directory = base
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut parts = BTreeMap::new();
    for entry in std::fs::read_dir(directory)? {
        crate::task_queue::checkpoint().map_err(io::Error::other)?;
        let entry = entry?;
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix(&prefix)) else {
            continue;
        };
        if suffix.len() < 3 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let index = suffix.parse::<usize>().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "Invalid archive volume number")
        })?;
        if index == 0 || parts.len() >= MAX_VOLUMES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Archive volume limit exceeded",
            ));
        }
        if parts.insert(index, entry.path()).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Duplicate archive volume number",
            ));
        }
    }
    Ok(parts)
}

struct Volume {
    path: PathBuf,
    start: u64,
    size: u64,
}

pub(crate) struct ArchiveReader {
    volumes: Vec<Volume>,
    current: Option<(usize, File)>,
    position: u64,
    length: u64,
}

impl ArchiveReader {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let mut volumes = Vec::new();
        let mut length = 0u64;
        for path in paths(path)? {
            let metadata = std::fs::metadata(&path)?;
            if !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Archive volume is not a regular file",
                ));
            }
            let size = metadata.len();
            volumes.push(Volume {
                path,
                start: length,
                size,
            });
            length = length.checked_add(size).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "Archive volumes are too large")
            })?;
        }
        Ok(Self {
            volumes,
            current: None,
            position: 0,
            length,
        })
    }

    pub(crate) fn len(&self) -> u64 {
        self.length
    }
}

impl Read for ArchiveReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        crate::task_queue::checkpoint().map_err(io::Error::other)?;
        if buffer.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let index = self
            .volumes
            .partition_point(|volume| volume.start + volume.size <= self.position);
        let volume = &self.volumes[index];
        if self
            .current
            .as_ref()
            .is_none_or(|(current, _)| *current != index)
        {
            self.current = Some((index, File::open(&volume.path)?));
        }
        let file = &mut self.current.as_mut().unwrap().1;
        let offset = self.position - volume.start;
        file.seek(SeekFrom::Start(offset))?;
        let length = usize::try_from((volume.size - offset).min(buffer.len() as u64)).unwrap();
        let count = file.read(&mut buffer[..length])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Archive volume was truncated",
            ));
        }
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for ArchiveReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let position = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        self.position = u64::try_from(position).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "Invalid archive seek position")
        })?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_creation_capabilities_are_explicit() {
        use crate::archive::{CreateOptions, ResolvedFormat};
        for format in [
            ResolvedFormat::Zip,
            ResolvedFormat::SevenZip,
            ResolvedFormat::Tar,
            ResolvedFormat::TarGz,
        ] {
            let capabilities = format.capabilities().volumes;
            assert!(capabilities.can_create);
            assert!(capabilities.can_read);
            assert!(!capabilities.native);
        }
        for format in [
            ResolvedFormat::Rar,
            ResolvedFormat::Cpio,
            ResolvedFormat::Gzip,
            ResolvedFormat::Iso,
        ] {
            assert!(!format.capabilities().volumes.can_create);
        }
        assert!(ResolvedFormat::Rar.capabilities().volumes.native);
        let options = CreateOptions {
            volume_size: Some(100_000_000),
            ..CreateOptions::default()
        };
        assert_eq!(
            options.first_output_path(Path::new("archive.zip")),
            Path::new("archive.zip.001")
        );
        assert_eq!(
            part_path(Path::new("archive.7z"), 1000),
            Path::new("archive.7z.1000")
        );
    }

    #[test]
    fn volume_creation_roundtrips_with_manifests_and_passwords() {
        use crate::archive::{self, CreateOptions, ResolvedFormat};
        use crate::inspection::{InspectionMode, Integrity, inspect};
        use std::sync::atomic::AtomicBool;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.txt");
        let contents = b"private volume creation payload".repeat(128);
        std::fs::write(&source, &contents).unwrap();
        for format in [
            ResolvedFormat::Zip,
            ResolvedFormat::SevenZip,
            ResolvedFormat::TarGz,
        ] {
            for size in [97, 100_000_000, 4_000_000_000] {
                let base = root
                    .path()
                    .join(format!("test-{size}.{}", format.extension()));
                let password = format
                    .supports_password()
                    .then_some("private-volume-password");
                let options = CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    volume_size: Some(size),
                    dictionary_mib: 1,
                    ..CreateOptions::default()
                };
                archive::create_archive_with_password(
                    &base,
                    std::slice::from_ref(&source),
                    options,
                    password,
                )
                .unwrap();
                assert!(!base.exists());
                let first = options.first_output_path(&base);
                let parts = paths(&first).unwrap();
                assert!(
                    parts
                        .iter()
                        .all(|part| part.metadata().unwrap().len() <= size)
                );
                let manifest = read_manifest(&base).unwrap().unwrap();
                assert_eq!(parts.len(), manifest.sizes.len());
                assert!(
                    !std::fs::read_to_string(manifest_path(&base))
                        .unwrap()
                        .contains("private-volume-password")
                );
                let entries =
                    archive::list_archive_with_password(parts.last().unwrap(), password).unwrap();
                assert_eq!(entries[0].name, "payload.txt");
                let preview = archive::read_entry_prefix_with_limit(
                    parts.last().unwrap(),
                    "payload.txt",
                    password,
                    &AtomicBool::new(false),
                    80,
                )
                .unwrap();
                assert_eq!(&preview[..80], &contents[..80]);
                let mut extracted =
                    archive::extract_entry_with_password(&first, "payload.txt", password).unwrap();
                let mut bytes = Vec::new();
                extracted.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, contents);
                let report = inspect(
                    &first,
                    password,
                    InspectionMode::Integrity,
                    &AtomicBool::new(false),
                );
                assert_eq!(report.integrity, Integrity::Passed, "{:?}", report.error);
                assert!(verify(&first, &AtomicBool::new(false), u64::MAX).unwrap());
                assert!(!verify(&first, &AtomicBool::new(false), 1).unwrap());
                assert!(verify(&first, &AtomicBool::new(true), u64::MAX).is_err());
            }
        }
    }

    #[test]
    fn volume_creation_detects_missing_final_parts_and_unused_padding_corruption() {
        use crate::archive::{self, CreateOptions, ResolvedFormat};
        use crate::inspection::{InspectionMode, Integrity, inspect};
        use std::sync::atomic::AtomicBool;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.txt");
        std::fs::write(&source, b"volume payload").unwrap();
        let base = root.path().join("test.tar");
        let options = CreateOptions {
            format: ResolvedFormat::Tar,
            compression: archive::CompressionLevel::Stored,
            volume_size: Some(1024),
            ..CreateOptions::default()
        };
        archive::create_archive(&base, &[source], options).unwrap();
        let first = options.first_output_path(&base);
        let parts = paths(&first).unwrap();
        assert!(parts.len() > 1);
        let last = parts.last().unwrap();
        let bytes = std::fs::read(last).unwrap();
        std::fs::remove_file(last).unwrap();
        assert!(
            archive::list_archive(&first)
                .err()
                .unwrap()
                .to_string()
                .contains("Failed to open")
        );
        assert!(format!("{:#}", ArchiveReader::open(&first).err().unwrap()).contains("missing"));
        std::fs::write(last, &bytes).unwrap();
        let mut damaged = bytes;
        *damaged.last_mut().unwrap() ^= 1;
        std::fs::write(last, damaged).unwrap();
        let report = inspect(
            &first,
            None,
            InspectionMode::Integrity,
            &AtomicBool::new(false),
        );
        assert_eq!(report.integrity, Integrity::Failed);
        assert!(report.error.unwrap().contains("checksum"));
    }

    #[test]
    fn volume_creation_rejects_collisions_limits_and_unsupported_formats() {
        use crate::archive::{self, CreateOptions, ResolvedFormat};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.txt");
        std::fs::write(&source, b"payload".repeat(2000)).unwrap();
        let base = root.path().join("test.zip");
        let options = CreateOptions {
            compression: archive::CompressionLevel::Stored,
            volume_size: Some(1000),
            ..CreateOptions::default()
        };
        let existing = part_path(&base, 999);
        std::fs::write(&existing, b"existing data").unwrap();
        assert!(archive::create_archive(&base, std::slice::from_ref(&source), options).is_err());
        assert_eq!(std::fs::read(&existing).unwrap(), b"existing data");
        assert!(!part_path(&base, 1).exists());
        std::fs::remove_file(existing).unwrap();
        for size in [0, 1] {
            assert!(
                archive::create_archive(
                    &base,
                    std::slice::from_ref(&source),
                    CreateOptions {
                        volume_size: Some(size),
                        ..options
                    }
                )
                .is_err()
            );
            assert!(!part_path(&base, 1).exists());
            assert!(!manifest_path(&base).exists());
        }
        assert!(
            archive::create_archive(
                &root.path().join("test.cpio"),
                &[source],
                CreateOptions {
                    format: ResolvedFormat::Cpio,
                    ..options
                }
            )
            .is_err()
        );
    }

    #[test]
    fn volume_creation_manifest_rejects_unsafe_names_and_inconsistent_metadata() {
        use crate::archive::{self, CreateOptions};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.txt");
        std::fs::write(&source, b"volume payload").unwrap();
        let base = root.path().join("test.zip");
        let options = CreateOptions {
            volume_size: Some(31),
            ..CreateOptions::default()
        };
        archive::create_archive(&base, &[source], options).unwrap();
        let first = options.first_output_path(&base);
        let manifest = manifest_path(&base);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        for field in ["name", "sha256", "size"] {
            let mut damaged = value.clone();
            damaged["volumes"][0][field] = match field {
                "name" => serde_json::json!("../outside.zip.001"),
                "sha256" => serde_json::json!("not-a-checksum"),
                _ => serde_json::json!(u64::MAX),
            };
            std::fs::write(&manifest, serde_json::to_vec(&damaged).unwrap()).unwrap();
            assert!(archive::list_archive(&first).is_err());
        }
        std::fs::write(&manifest, b"{invalid").unwrap();
        assert!(archive::list_archive(&first).is_err());
        std::fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(archive::list_archive(&first).is_ok());
    }

    #[test]
    fn volume_creation_publication_rolls_back_without_removing_unrelated_files() {
        let root = tempfile::tempdir().unwrap();
        let unrelated = root.path().join("existing.zip.001");
        std::fs::write(&unrelated, b"keep").unwrap();
        let published = root.path().join("new.zip.001");
        std::fs::write(&published, b"incomplete").unwrap();
        {
            let _publication = Publication {
                paths: vec![published.clone()],
                committed: false,
            };
        }
        assert!(!published.exists());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep");
        std::fs::write(&published, b"complete").unwrap();
        {
            let _publication = Publication {
                paths: vec![published.clone()],
                committed: true,
            };
        }
        assert_eq!(std::fs::read(&published).unwrap(), b"complete");
    }

    fn split(path: &Path, size: usize) -> Vec<PathBuf> {
        let bytes = std::fs::read(path).unwrap();
        bytes
            .chunks(size)
            .enumerate()
            .map(|(index, bytes)| {
                let part = path.with_file_name(format!(
                    "{}.{:03}",
                    path.file_name().unwrap().to_string_lossy(),
                    index + 1
                ));
                std::fs::write(&part, bytes).unwrap();
                part
            })
            .collect()
    }

    #[gpui::test]
    fn multipart_archive_opens_in_browser_from_last_part(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.txt");
        std::fs::write(&source, b"multipart browser test").unwrap();
        let path = root.path().join("test.zip");
        crate::archive::create_zip(&path, &[source]).unwrap();
        let parts = split(&path, 31);
        let selected = parts.last().unwrap().clone();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.navigate_into_archive(selected.clone(), cx);
            manager
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.archive_path.as_ref(), Some(&selected));
            assert!(manager.error.is_none());
            assert_eq!(manager.entries.len(), 1);
            assert_eq!(manager.entries[0].name, "payload.txt");
        });
        assert!(cx.debug_bounds("file:payload.txt").is_some());
    }

    #[test]
    fn multipart_names_are_recognized_without_treating_every_number_as_an_archive() {
        use crate::archive::{self, ResolvedFormat};
        for (name, format) in [
            ("backup.zip.001", ResolvedFormat::Zip),
            ("backup.7z.099", ResolvedFormat::SevenZip),
            ("backup.TAR.GZ.001", ResolvedFormat::TarGz),
            ("backup.r00", ResolvedFormat::Rar),
            ("backup.part02.rar", ResolvedFormat::Rar),
        ] {
            assert_eq!(ResolvedFormat::from_path(Path::new(name)), Some(format));
        }
        assert!(!archive::is_supported_archive(Path::new("database.001")));
        assert!(archive::is_primary_volume(Path::new("backup.zip.001")));
        assert!(!archive::is_primary_volume(Path::new("backup.zip.002")));
        assert!(!archive::is_primary_volume(Path::new("backup.r00")));
        assert!(!archive::is_primary_volume(Path::new("backup.part02.rar")));
    }

    #[test]
    fn multipart_archives_list_preview_extract_and_verify_from_any_part() {
        use crate::archive::{self, CreateOptions, ResolvedFormat};
        use crate::inspection::{InspectionMode, Integrity, inspect};
        use std::sync::atomic::AtomicBool;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.txt");
        let contents = b"multipart archive test contents".repeat(128);
        std::fs::write(&source, &contents).unwrap();
        for format in [
            ResolvedFormat::Zip,
            ResolvedFormat::SevenZip,
            ResolvedFormat::TarGz,
            ResolvedFormat::Cpio,
            ResolvedFormat::Ar,
            ResolvedFormat::Pax,
            ResolvedFormat::Xar,
            ResolvedFormat::Gzip,
            ResolvedFormat::Zstandard,
        ] {
            for password in [None, Some("multipart-password")] {
                if password.is_some() && !format.supports_password() {
                    continue;
                }
                let path = root.path().join(format!("test.{}", format.extension()));
                archive::create_archive_with_password(
                    &path,
                    std::slice::from_ref(&source),
                    CreateOptions {
                        format,
                        compression: format.compression_levels()[0],
                        dictionary_mib: 1,
                        ..CreateOptions::default()
                    },
                    password,
                )
                .unwrap();
                let parts = split(&path, 37);
                std::fs::remove_file(&path).unwrap();
                assert!(parts.len() > 1);
                let selected = parts.last().unwrap();
                assert!(archive::is_supported_archive(selected));
                assert_eq!(archive::is_password_protected(selected), password.is_some());
                assert!(!archive::can_add_to_archive(selected));
                assert!(!archive::can_replace_archive_entry(selected));
                let entries = archive::list_archive_with_password(selected, password)
                    .unwrap_or_else(|error| panic!("{format:?}: {error:#}"));
                let entry_name = if format.is_single_compressed() {
                    "test"
                } else {
                    "payload.txt"
                };
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].name, entry_name);
                let mut extracted =
                    archive::extract_entry_with_password(selected, entry_name, password).unwrap();
                let mut bytes = Vec::new();
                extracted.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, contents);
                let preview = archive::read_entry_prefix_with_limit(
                    selected,
                    entry_name,
                    password,
                    &AtomicBool::new(false),
                    80,
                )
                .unwrap();
                assert_eq!(&preview[..80], &contents[..80]);
                let destination = root.path().join("extracted");
                archive::extract_archive_to(selected, &destination, password).unwrap();
                assert_eq!(
                    std::fs::read(destination.join(entry_name)).unwrap(),
                    contents
                );
                std::fs::remove_dir_all(destination).unwrap();
                let report = inspect(
                    selected,
                    password,
                    InspectionMode::Integrity,
                    &AtomicBool::new(false),
                );
                assert_eq!(
                    report.integrity,
                    Integrity::Passed,
                    "{format:?}: {:?}",
                    report.error
                );
                assert!(report.expected == Some(format));
                if password.is_some() {
                    assert!(archive::list_archive(selected).is_err());
                    assert!(
                        archive::extract_entry_with_password(selected, entry_name, Some("wrong"))
                            .is_err()
                    );
                }
                for part in parts {
                    std::fs::remove_file(part).unwrap();
                }
            }
        }
    }

    #[test]
    fn multipart_spanned_zip_is_rejected_without_modification() {
        use crate::archive;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("test.zip");
        std::fs::write(&path, b"last volume").unwrap();
        std::fs::write(root.path().join("test.z01"), b"first volume").unwrap();
        assert!(archive::is_multipart_archive(&path));
        assert!(!archive::can_add_to_archive(&path));
        assert!(archive::list_archive(&path).is_err());
        assert!(archive::remove_archive_entry(&path, "entry.txt", None).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"last volume");
    }

    #[test]
    fn multipart_missing_part_fails_without_publishing_or_modifying() {
        use crate::archive::{self, CreateOptions};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.txt");
        std::fs::write(&source, b"multipart contents").unwrap();
        let path = root.path().join("test.zip");
        archive::create_archive(
            &path,
            std::slice::from_ref(&source),
            CreateOptions::default(),
        )
        .unwrap();
        let parts = split(&path, 31);
        let first = &parts[0];
        let before = std::fs::read(first).unwrap();
        assert!(
            archive::add_to_archive_with_password(first, std::slice::from_ref(&source), "", None)
                .is_err()
        );
        assert!(archive::remove_archive_entry(first, "payload.txt", None).is_err());
        assert!(
            archive::replace_archive_entry_with_password(first, "payload.txt", b"changed", None)
                .is_err()
        );
        assert_eq!(std::fs::read(first).unwrap(), before);
        std::fs::remove_file(&parts[1]).unwrap();
        assert!(archive::list_archive(first).is_err());
        let destination = root.path().join("extracted");
        assert!(archive::extract_archive_to(first, &destination, None).is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn numbered_volumes_support_reads_and_seeks_from_any_part() {
        let root = tempfile::tempdir().unwrap();
        for (index, bytes) in [b"abc".as_slice(), b"defg", b"hij"].into_iter().enumerate() {
            std::fs::write(root.path().join(format!("test.7z.{:03}", index + 1)), bytes).unwrap();
        }
        let mut reader = ArchiveReader::open(&root.path().join("test.7z.002")).unwrap();
        assert_eq!(reader.len(), 10);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"abcdefghij");
        reader.seek(SeekFrom::Start(2)).unwrap();
        let mut bytes = [0; 6];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"cdefgh");
        reader.seek(SeekFrom::End(-2)).unwrap();
        let mut bytes = [0; 2];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ij");
        assert!(reader.seek(SeekFrom::Current(-11)).is_err());
        reader.seek(SeekFrom::Start(20)).unwrap();
        assert_eq!(reader.read(&mut bytes).unwrap(), 0);
    }

    #[test]
    fn numbered_volumes_reject_missing_and_duplicate_parts() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("test.zip.001");
        std::fs::write(&first, b"one").unwrap();
        let third = root.path().join("test.zip.003");
        std::fs::write(&third, b"three").unwrap();
        assert!(ArchiveReader::open(&first).is_err());
        std::fs::remove_file(&first).unwrap();
        assert!(ArchiveReader::open(&third).is_err());
        std::fs::write(&first, b"one").unwrap();
        std::fs::write(root.path().join("test.zip.002"), b"two").unwrap();
        std::fs::write(root.path().join("test.zip.0001"), b"duplicate").unwrap();
        assert!(ArchiveReader::open(&first).is_err());
    }
}
