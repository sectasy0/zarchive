//! Lists, creates, inspects, and safely extracts supported archive formats.

use std::{
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
use crate::volumes::ArchiveReader;

mod format;
#[allow(unused_imports)]
pub use format::{ArchiveEntry, ArchiveFormat, CompressionFormat, CompressionLevel, ContainerFormat, CreateOptions, FormatCapabilities, ResolvedFormat, VolumeCapabilities};
mod write;
pub use write::{create_archive, create_archive_with_password};
#[cfg(test)]
pub use write::create_zip;
#[cfg(test)]
pub(crate) use write::add_to_archive;
pub(crate) use write::{add_to_archive_with_password, can_add_to_archive, can_replace_archive_entry, remove_archive_entry, replace_archive_entry_with_password};
mod read;
pub use read::{can_extract_archive, is_supported_archive, list_archive, list_archive_with_password};
pub(crate) use read::{decode_archive_stream, extract_single_compressed_to, is_multipart_archive, is_password_protected, is_primary_volume, is_single_compressed_archive, open_archive_reader, open_extended_archive, single_compressed_entry_name, ExtendedEntryReader};
use read::{extract_extended_entry, extract_extended_tree, open_seven_zip, open_tar_reader};
mod extract;
pub use extract::{extract_archive_selection_to, extract_archive_to, extract_entry, extract_entry_with_password};
pub(crate) use extract::{read_entry_from_offset, read_entry_prefix_with_limit};
use extract::{safe_extraction_path, write_extracted_entry};
#[cfg(test)]
use extract::temporary_suffix;


#[cfg(test)]
#[allow(unused_imports)]
use write::{collect_archive_sources, write_extended_archive, write_rar_archive_with_program};
#[cfg(test)]
use write::rar_program;




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

fn list_zip(path: &Path, password: Option<&str>) -> Result<Vec<ArchiveEntry>> {
    let file = open_archive_reader(path)?;
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
            is_encrypted: encrypted,
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
            is_encrypted: false,
        });
    }

    Ok(entries)
}

/// Lists 7z entries and drains content streams to verify encrypted data when needed.
fn list_seven_zip(path: &Path, password: Option<&str>) -> Result<Vec<ArchiveEntry>> {
    let mut archive = open_seven_zip(path, password)?;
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
                is_encrypted: encrypted && !entry.is_directory(),
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
                is_encrypted: false,
            })
        })
        .collect()
}

/// Extracts one regular-file RAR entry to the specified temporary path.
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
            .filter(|format| format.archive.is_some() && format.capabilities().can_create)
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
            .filter(|format| {
                format.archive.is_some()
                    && *format != super::ResolvedFormat::Ar
                    && format.capabilities().can_create
            })
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
    fn rar_creation_is_listed_and_reports_a_missing_writer() {
        assert!(ResolvedFormat::CREATION_FORMATS.contains(&ResolvedFormat::Rar));
        assert_eq!(ResolvedFormat::Rar.compression_levels().len(), 4);
        if super::rar_program().is_none() {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("source.txt");
            let output = directory.path().join("output.rar");
            std::fs::write(&source, "RAR source").unwrap();
            let error = create_archive(
                &output,
                &[source],
                CreateOptions {
                    format: ResolvedFormat::Rar,
                    ..CreateOptions::default()
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("Install the rar command-line tool"));
            assert!(!output.exists());
            assert!(!ResolvedFormat::Rar.capabilities().can_create);
        }
    }

    #[cfg(unix)]
    #[test]
    fn rar_writer_uses_staged_entry_names_and_compression_level() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("folder/nested/item.txt");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "RAR contents").unwrap();
        let mut entries = Vec::new();
        super::collect_archive_sources(
            &directory.path().join("folder"),
            "folder".into(),
            true,
            &mut std::collections::BTreeSet::new(),
            &mut entries,
        )
        .unwrap();
        let program = directory.path().join("rar-test-tool");
        std::fs::write(
            &program,
            "#!/bin/sh\nset -eu\n[ \"$6\" = \"-m5\" ]\n[ \"$9\" = \"folder\" ]\n[ -f \"$9/nested/item.txt\" ]\nprintf 'fake RAR data' > \"$8\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).unwrap();
        let output = directory.path().join("output.rar");

        super::write_rar_archive_with_program(
            &program,
            &output,
            &entries,
            CompressionLevel::Best,
        )
        .unwrap();

        assert_eq!(std::fs::read(output).unwrap(), b"fake RAR data");
    }

    #[test]
    fn replacing_archive_entry_preserves_other_files_in_zip_and_tar() {
        use std::io::Read;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("edit.txt");
        let keep = directory.path().join("keep.txt");
        std::fs::write(&source, b"before").unwrap();
        std::fs::write(&keep, b"untouched").unwrap();
        for format in [ResolvedFormat::Zip, ResolvedFormat::Tar] {
            let path = directory.path().join(format!("replace.{}", format.extension()));
            create_archive(
                &path,
                &[source.clone(), keep.clone()],
                CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    ..CreateOptions::default()
                },
            )
            .unwrap();
            super::replace_archive_entry_with_password(
                &path,
                "edit.txt",
                b"after editing",
                None,
            )
            .unwrap();
            let mut edited = extract_entry(&path, "edit.txt").unwrap();
            let mut edited_bytes = Vec::new();
            edited.read_to_end(&mut edited_bytes).unwrap();
            assert_eq!(edited_bytes, b"after editing", "{format:?}");
            let mut kept = extract_entry(&path, "keep.txt").unwrap();
            let mut kept_bytes = Vec::new();
            kept.read_to_end(&mut kept_bytes).unwrap();
            assert_eq!(kept_bytes, b"untouched", "{format:?}");
        }
    }

    #[test]
    fn replacing_aes_zip_entry_requires_and_preserves_its_password() {
        use std::io::Read;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("edit.txt");
        std::fs::write(&source, b"before").unwrap();
        let path = directory.path().join("replace.zip");
        super::create_archive_with_password(
            &path,
            std::slice::from_ref(&source),
            CreateOptions::default(),
            Some("edit-password"),
        )
        .unwrap();
        assert!(super::replace_archive_entry_with_password(&path, "edit.txt", b"after", None).is_err());
        super::replace_archive_entry_with_password(
            &path,
            "edit.txt",
            b"after",
            Some("edit-password"),
        )
        .unwrap();
        let mut edited = super::extract_entry_with_password(&path, "edit.txt", Some("edit-password"))
            .unwrap();
        let mut bytes = Vec::new();
        edited.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"after");
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
            .filter(|format| format.archive.is_some() && format.capabilities().can_create)
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
