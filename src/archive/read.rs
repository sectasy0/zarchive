use super::*;

/// Reports whether the path has a supported extension and matching content signature.
pub fn is_supported_archive(path: &Path) -> bool {
    ResolvedFormat::from_file(path).is_some_and(|format| format.capabilities().can_list)
}

/// Checks ZIP and 7z metadata for password protection without extracting entries.
pub(crate) fn is_password_protected(path: &Path) -> bool {
    match ResolvedFormat::from_file(path) {
        Some(ResolvedFormat::Zip) => open_archive_reader(path)
            .ok()
            .and_then(|file| ZipArchive::new(file).ok())
            .is_some_and(|mut archive| {
                (0..archive.len()).any(|index| {
                    archive
                        .by_index_raw(index)
                        .is_ok_and(|entry| entry.encrypted())
                })
            }),
        Some(ResolvedFormat::SevenZip) => match open_seven_zip(path, None) {
            Ok(archive) => archive
                .archive()
                .folders
                .iter()
                .flat_map(|folder| &folder.coders)
                .any(|coder| coder.decompression_method_id() == [0x06, 0xf1, 0x07, 0x01]),
            Err(error) => matches!(
                error.downcast_ref::<PasswordFailure>(),
                Some(PasswordFailure::Required)
            ),
        },
        _ => false,
    }
}

pub(crate) fn open_archive_reader(path: &Path) -> Result<ArchiveReader> {
    if crate::volumes::numbered_base(path).is_some() {
        crate::volumes::paths(path)
            .map_err(|error| anyhow::anyhow!("Failed to open {}: {error}", path.display()))?;
    }
    let format = ResolvedFormat::from_path(path).context("Unsupported archive format")?;
    anyhow::ensure!(
        !is_spanned_zip(path),
        "Native split ZIP archives (.z01) are not supported"
    );
    let path = if format == ResolvedFormat::Rar {
        unrar::Archive::new(path).first_part()
    } else {
        path.to_owned()
    };
    ArchiveReader::open(&path).with_context(|| format!("Failed to open {}", path.display()))
}

pub(super) fn open_seven_zip(
    path: &Path,
    password: Option<&str>,
) -> Result<SevenZReader<ArchiveReader>> {
    let reader = open_archive_reader(path)?;
    let length = reader.len();
    SevenZReader::new(
        reader,
        length,
        password.map(Password::from).unwrap_or_default(),
    )
    .map_err(|error| seven_zip_error(error, password))
}

pub(crate) fn is_multipart_archive(path: &Path) -> bool {
    crate::volumes::numbered_base(path).is_some()
        || is_spanned_zip(path)
        || (ResolvedFormat::from_path(path) == Some(ResolvedFormat::Rar)
            && (unrar::Archive::new(path).first_part_option().is_some()
                || path.with_extension("r00").is_file()))
}

pub(super) fn is_spanned_zip(path: &Path) -> bool {
    ResolvedFormat::from_path(path) == Some(ResolvedFormat::Zip)
        && path.with_extension("z01").is_file()
}

pub(crate) fn is_primary_volume(path: &Path) -> bool {
    if crate::volumes::numbered_base(path).is_some() {
        return path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(|extension| extension.parse::<usize>().ok())
            == Some(1);
    }
    if ResolvedFormat::from_path(path) == Some(ResolvedFormat::Rar) {
        return unrar::Archive::new(path).first_part() == path;
    }
    true
}

/// Reports whether the resolved file format supports content extraction.
pub fn can_extract_archive(path: &Path) -> bool {
    ResolvedFormat::from_file(path).is_some_and(|format| format.capabilities().can_extract)
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
    if crate::volumes::numbered_base(path).is_some() {
        crate::volumes::paths(path)
            .map_err(|error| anyhow::anyhow!("Failed to open {}: {error}", path.display()))?;
    }
    let format = ResolvedFormat::from_file(path).with_context(|| {
        if ResolvedFormat::from_path(path) == Some(ResolvedFormat::Zip) {
            "The ZIP archive is invalid or damaged.".to_owned()
        } else {
            format!("Unsupported archive format: {}", path.display())
        }
    })?;
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
pub(super) fn open_tar_reader(path: &Path, format: ResolvedFormat) -> Result<Box<dyn Read>> {
    let file = open_archive_reader(path)?;
    decode_archive_stream(file, format)
}

pub(crate) struct ExtendedEntryReader<'a, 'b>(pub(crate) &'a mut libarchive2::ReadArchive<'b>);

pub(crate) fn open_extended_archive(path: &Path) -> Result<libarchive2::ReadArchive<'static>> {
    let format = ResolvedFormat::from_path(path).context("Unsupported archive format")?;
    let options: &[(&str, &str, &str)] = if format == ResolvedFormat::Cpio {
        &[("cpio", "hdrcharset", "UTF-8")]
    } else {
        &[]
    };
    let archive = if crate::volumes::numbered_base(path).is_some() {
        libarchive2::ReadArchive::open_callback_with_options(
            libarchive2::CallbackReader::new(crate::task_queue::TaskReader(open_archive_reader(
                path,
            )?)),
            options,
        )?
    } else {
        libarchive2::ReadArchive::open_with_options(path, options)?
    };
    Ok(archive)
}

impl Read for ExtendedEntryReader<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read_data(buffer).map_err(io::Error::other)
    }
}

pub(super) fn list_extended_archive(path: &Path) -> Result<Vec<ArchiveEntry>> {
    if ResolvedFormat::from_file(path) == Some(ResolvedFormat::Xar) {
        let reader = crate::xar::XarReader::new(open_archive_reader(path)?)?;
        return Ok(reader
            .entries
            .iter()
            .map(|entry| ArchiveEntry {
                name: entry.path.clone(),
                source_path: Some(entry.path.clone()),
                size: entry.data.as_ref().map_or(0, |data| data.size),
                modified: None,
                is_directory: entry.file_type == xara::XarFileType::Directory,
                is_encrypted: false,
            })
            .collect());
    }
    let mut reader = open_extended_archive(path)?;
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
            is_encrypted: false,
        });
    }
    Ok(entries)
}

pub(super) fn validate_extended_entry(entry: &libarchive2::Entry<'_>) -> Result<()> {
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

pub(super) fn extract_extended_entry(
    path: &Path,
    name: &str,
    output: &mut impl Write,
) -> Result<()> {
    if ResolvedFormat::from_file(path) == Some(ResolvedFormat::Xar) {
        let mut reader = crate::xar::XarReader::new(open_archive_reader(path)?)?;
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
    let mut reader = open_extended_archive(path)?;
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

pub(super) fn extract_extended_tree(
    path: &Path,
    destination: &Path,
    selection: Option<&Path>,
) -> Result<()> {
    if ResolvedFormat::from_file(path) == Some(ResolvedFormat::Xar) {
        let mut reader = crate::xar::XarReader::new(open_archive_reader(path)?)?;
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
    let mut reader = open_extended_archive(path)?;
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
pub(super) fn list_single_compressed(
    path: &Path,
    format: ResolvedFormat,
) -> Result<Vec<ArchiveEntry>> {
    let name = single_compressed_entry_name(path)?;
    let file = open_archive_reader(path)?;
    let mut reader = decode_archive_stream(file, format)?;
    let size = crate::task_queue::copy(&mut reader, &mut io::sink())?;
    Ok(vec![ArchiveEntry {
        name: name.clone(),
        source_path: Some(name),
        size,
        is_directory: false,
        modified: None,
        is_encrypted: false,
    }])
}

/// Removes a standalone compression suffix to recover the decompressed filename.
pub(crate) fn single_compressed_entry_name(path: &Path) -> Result<String> {
    let base = crate::volumes::numbered_base(path);
    let path = base.as_deref().unwrap_or(path);
    let format = ResolvedFormat::from_file(path)
        .or_else(|| (!path.exists()).then(|| ResolvedFormat::from_path(path)).flatten())
        .context("Unsupported compressed file format")?;
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
    ResolvedFormat::from_file(path).is_some_and(ResolvedFormat::is_single_compressed)
}

/// Decompresses one standalone stream to a new file without overwriting existing data.
pub(crate) fn extract_single_compressed_to(path: &Path, destination: &Path) -> Result<()> {
    let format = ResolvedFormat::from_file(path).context("Unsupported compressed file format")?;
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
