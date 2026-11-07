use super::*;

/// Extracts one archive entry to a temporary file without a password.
pub fn extract_entry(path: &Path, entry_name: &str) -> Result<NamedTempFile> {
    extract_entry_with_password(path, entry_name, None)
}

pub(crate) fn read_entry_prefix_with_limit(
    path: &Path,
    entry_name: &str,
    password: Option<&str>,
    cancel: &std::sync::atomic::AtomicBool,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    read_entry_range(path, entry_name, password, cancel, 0, max_bytes)
}

pub(crate) fn read_entry_from_offset(
    path: &Path,
    entry_name: &str,
    password: Option<&str>,
    cancel: &std::sync::atomic::AtomicBool,
    offset: usize,
) -> Result<Vec<u8>> {
    read_entry_range(path, entry_name, password, cancel, offset, usize::MAX)
}

pub(super) fn read_entry_range(
    path: &Path,
    entry_name: &str,
    password: Option<&str>,
    cancel: &std::sync::atomic::AtomicBool,
    offset: usize,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    read_entry_range_with_budget(path, entry_name, password, cancel, offset, max_bytes, 64 * 1024 * 1024)
}

pub(crate) fn read_entry_range_with_budget(
    path: &Path,
    entry_name: &str,
    password: Option<&str>,
    cancel: &std::sync::atomic::AtomicBool,
    offset: usize,
    max_bytes: usize,
    read_budget: u64,
) -> Result<Vec<u8>> {
    use crate::text_preview::{GuardedReader, read_from_offset, read_prefix_with_limit};
    use std::{cell::Cell, sync::atomic::Ordering};
    fn read_entry_bytes(
        reader: &mut impl Read,
        cancel: &std::sync::atomic::AtomicBool,
        offset: usize,
        max_bytes: usize,
    ) -> io::Result<Vec<u8>> {
        if offset == 0 {
            read_prefix_with_limit(reader, cancel, max_bytes)
        } else {
            read_from_offset(reader, cancel, offset)
        }
    }

    let format = ResolvedFormat::from_path(path).context("Unsupported archive format")?;
    let budget = Cell::new(read_budget);
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
        return Ok(read_entry_bytes(
            &mut guarded(decode_archive_stream(open_archive_reader(path)?, format)?),
            cancel,
            offset,
            max_bytes,
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
                return Ok(read_entry_bytes(&mut entry, cancel, offset, max_bytes)?);
            }
        }
        anyhow::bail!("Archive entry not found or preview safety limit reached");
    }
    match format {
        ResolvedFormat::Zip => {
            let mut archive = ZipArchive::new(open_archive_reader(path)?)?;
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
            Ok(read_entry_bytes(&mut entry, cancel, offset, max_bytes)?)
        }
        ResolvedFormat::SevenZip => {
            let file = open_archive_reader(path)?;
            let length = file.len();
            let input_budget = Cell::new(read_budget);
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
                    bytes = Some(
                        read_entry_bytes(&mut reader, cancel, offset, max_bytes)
                            .map_err(sevenz_rust::Error::io)?,
                    );
                    Ok(false)
                } else {
                    crate::task_queue::copy(&mut reader, &mut io::sink())
                        .map_err(sevenz_rust::Error::io)?;
                    Ok(true)
                }
            })?;
            bytes.context("Archive entry not found")
        }
        ResolvedFormat::Xar => {
            let mut archive = crate::xar::XarReader::new(open_archive_reader(path)?)?;
            let entry = archive
                .entries
                .iter()
                .find(|entry| entry.path == entry_name)
                .cloned()
                .context("Archive entry not found")?;
            Ok(read_entry_bytes(
                &mut archive.entry_reader(&entry)?,
                cancel,
                offset,
                max_bytes,
            )?)
        }
        _ => {
            let mut archive = if let Some(password) = password {
                let mut archive = open_extended_archive(path)?;
                archive.add_passphrase(password)?;
                archive
            } else {
                open_extended_archive(path)?
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
                let mut remaining = offset;
                while remaining > 0 {
                    if cancel.load(Ordering::Relaxed) {
                        anyhow::bail!("Preview cancelled");
                    }
                    let length = buffer.len().min(remaining);
                    let count = archive.read_data(&mut buffer[..length])?;
                    if count == 0 {
                        anyhow::bail!("Preview source changed while loading");
                    }
                    remaining -= count;
                }
                while bytes.len() <= max_bytes {
                    if cancel.load(Ordering::Relaxed) {
                        anyhow::bail!("Preview cancelled");
                    }
                    let length = buffer
                        .len()
                        .min(max_bytes.saturating_add(1).saturating_sub(bytes.len()));
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
            let file = open_archive_reader(path)?;
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
            crate::task_queue::copy(&mut entry, &mut output)
                .context("Failed to extract the ZIP entry")?;
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
            let file = open_archive_reader(path)?;
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
            let mut archive = open_seven_zip(path, password)?;
            let mut found = false;
            archive
                .for_each_entries(|entry, reader| {
                    if entry.name() == entry_name && !entry.is_directory() {
                        crate::task_queue::copy(reader, &mut output)
                            .map_err(sevenz_rust::Error::io)?;
                        found = true;
                        Ok(false)
                    } else {
                        crate::task_queue::copy(reader, &mut io::sink())
                            .map_err(sevenz_rust::Error::io)?;
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
    crate::task_queue::report_byte_total(
        entries
            .iter()
            .filter(|entry| {
                !entry.is_directory
                    && selection.is_none_or(|selected| {
                        entry.name == selected
                            || entry
                                .name
                                .starts_with(&format!("{}/", selected.trim_end_matches('/')))
                    })
            })
            .fold(0_u64, |total, entry| total.saturating_add(entry.size)),
    );
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
            let mut archive = ZipArchive::new(open_archive_reader(path)?)?;
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
            let file = open_archive_reader(path)?;
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
            let mut archive = open_seven_zip(path, password)?;
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
                    crate::task_queue::copy(reader, &mut io::sink())
                        .map_err(sevenz_rust::Error::io)?;
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
pub(crate) fn safe_extraction_path(name: &str) -> Result<PathBuf> {
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
pub(super) fn write_extracted_entry(
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
pub(super) fn extract_tar_tree(
    reader: impl Read,
    destination: &Path,
    selection: Option<&Path>,
) -> Result<()> {
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
pub(super) fn extract_tar<R: Read>(
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
            crate::task_queue::copy(&mut entry, output)
                .context("Failed to extract the TAR entry")?;
            return Ok(());
        }
    }
    anyhow::bail!("Archive entry not found: {entry_name}")
}

/// Returns a short alphanumeric extension for associating temporary files with system apps.
pub(super) fn temporary_suffix(entry_name: &str) -> String {
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
pub(super) fn extract_rar_entry_to(
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
pub(super) fn extract_rar_tree(
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
