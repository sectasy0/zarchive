//! Reads numbered archive volumes as one seekable stream.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const MAX_VOLUMES: usize = 10_000;

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
    let prefix = format!("{}.", base.file_name().unwrap().to_string_lossy());
    let directory = path
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
    Ok(parts.into_values().collect())
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
