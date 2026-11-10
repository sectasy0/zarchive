use std::collections::BTreeSet;

use super::*;

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

pub(super) fn create_archive_contents(
    path: &Path,
    source_paths: &[PathBuf],
    options: CreateOptions,
    password: Option<&str>,
    allow_empty: bool,
) -> Result<()> {
    let format = options.output_format();
    if let Some(size) = options.volume_size {
        anyhow::ensure!(
            format.capabilities().volumes.can_create,
            "This format cannot create volumes"
        );
        anyhow::ensure!(size > 0, "Enter a valid volume size");
    }
    if format == ResolvedFormat::Rar {
        anyhow::ensure!(
            rar_program().is_some(),
            "Install the rar command-line tool to create RAR archives"
        );
    } else {
        anyhow::ensure!(
            format.capabilities().can_create,
            "This format cannot be created"
        );
    }
    anyhow::ensure!(
        password.is_none() || format.supports_password(),
        "This format does not support encryption"
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
    let total_bytes = entries
        .iter()
        .filter(|(_, _, directory)| !directory)
        .try_fold(0_u64, |total, (source, _, _)| {
            Ok::<_, io::Error>(total.saturating_add(source.metadata()?.len()))
        })?;
    crate::task_queue::report_byte_total(total_bytes);
    if format == ResolvedFormat::Rar {
        let output = Builder::new()
            .suffix(".rar")
            .tempfile_in(&parent)
            .context("Failed to create temporary RAR archive")?
            .into_temp_path();
        std::fs::remove_file(&output)?;
        write_rar_archive(&output, &entries, options.compression)?;
        File::options().write(true).open(&output)?.sync_all()?;
        crate::task_queue::checkpoint()?;
        output
            .persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("Failed to save archive {}", path.display()))?;
        return Ok(());
    }
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
                    readers.push(sevenz_rust::SourceReader::new(
                        crate::task_queue::TaskReader(File::open(source)?),
                    ));
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
        ResolvedFormat::Rar => unreachable!("RAR archives use the external writer"),
        _ => anyhow::bail!("Unsupported resolved format for archive creation"),
    }
    output.as_file().sync_all()?;
    crate::task_queue::checkpoint()?;
    if let Some(size) = options.volume_size {
        return crate::volumes::publish(output.as_file_mut(), path, size);
    }
    output
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to save archive {}", path.display()))?;
    Ok(())
}

pub(super) fn rar_program() -> Option<&'static Path> {
    static PROGRAM: std::sync::LazyLock<Option<PathBuf>> = std::sync::LazyLock::new(|| {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join(if cfg!(windows) { "rar.exe" } else { "rar" }))
                .find(|path| path.is_file())
        })
    });
    PROGRAM.as_deref()
}

pub(super) fn write_rar_archive(
    output: &Path,
    entries: &[(PathBuf, String, bool)],
    compression: CompressionLevel,
) -> Result<()> {
    let program =
        rar_program().context("Install the rar command-line tool to create RAR archives")?;
    write_rar_archive_with_program(program, output, entries, compression)
}

pub(super) fn write_rar_archive_with_program(
    program: &Path,
    output: &Path,
    entries: &[(PathBuf, String, bool)],
    compression: CompressionLevel,
) -> Result<()> {
    let staging = tempfile::tempdir()?;
    let mut roots = BTreeSet::new();
    for (source, name, is_directory) in entries {
        crate::task_queue::checkpoint()?;
        let relative = safe_extraction_path(name)?;
        let root = relative
            .components()
            .next()
            .context("Archive entry has no root name")?;
        roots.insert(PathBuf::from(root.as_os_str()));
        let destination = staging.path().join(relative);
        if *is_directory {
            std::fs::create_dir_all(destination)?;
        } else {
            std::fs::create_dir_all(destination.parent().context("Missing entry parent")?)?;
            if std::fs::hard_link(source, &destination).is_err() {
                crate::task_queue::copy(&mut File::open(source)?, &mut File::create(destination)?)?;
            }
        }
    }
    let compression = match compression {
        CompressionLevel::Stored => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Normal => 3,
        CompressionLevel::Best => 5,
    };
    let result = std::process::Command::new(program)
        .current_dir(staging.path())
        .args([
            "a",
            "-idq",
            "-o+",
            "-p-",
            "-r",
            &format!("-m{compression}"),
            "--",
        ])
        .arg(output)
        .args(roots)
        .output()?;
    anyhow::ensure!(
        result.status.success(),
        "The RAR tool could not create this archive"
    );
    anyhow::ensure!(
        output.metadata()?.len() > 0,
        "The RAR tool created an empty archive"
    );
    Ok(())
}

pub(crate) fn can_add_to_archive(path: &Path) -> bool {
    !is_multipart_archive(path)
        && ResolvedFormat::from_file(path).is_some_and(|format| format.capabilities().can_modify)
}

pub(crate) fn can_replace_archive_entry(path: &Path) -> bool {
    !is_multipart_archive(path)
        && ResolvedFormat::from_file(path)
            .is_some_and(|format| format.capabilities().can_modify && format != ResolvedFormat::Rar)
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
    anyhow::ensure!(
        !is_multipart_archive(path),
        "Multipart archives are read-only"
    );
    let format = ResolvedFormat::from_file(path).context("Unsupported archive format")?;
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
            crate::task_queue::copy(&mut entry, &mut io::sink())
                .map_err(|_| PasswordFailure::Incorrect)?;
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
    anyhow::ensure!(
        !is_multipart_archive(path),
        "Multipart archives are read-only"
    );
    let format = ResolvedFormat::from_file(path).context("Unsupported archive format")?;
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

/// Replaces one regular-file entry by rebuilding the archive through private staging.
pub(crate) fn replace_archive_entry_with_password(
    path: &Path,
    name: &str,
    contents: &[u8],
    password: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !is_multipart_archive(path),
        "Multipart archives are read-only"
    );
    let format = ResolvedFormat::from_file(path).context("Unsupported archive format")?;
    anyhow::ensure!(
        format.capabilities().can_modify && format != ResolvedFormat::Rar,
        "This archive format cannot replace entries"
    );
    let selected = safe_extraction_path(name)?;
    let entries = list_archive_with_password(path, password)?;
    anyhow::ensure!(
        entries.iter().any(|entry| {
            !entry.is_directory
                && safe_extraction_path(&entry.name).is_ok_and(|entry_name| entry_name == selected)
        }),
        "A regular archive entry must be selected for editing"
    );
    let file_name = selected
        .file_name()
        .context("Archive entry has no filename")?;
    let parent = selected
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let staging = tempfile::tempdir()?;
    let replacement = staging.path().join(file_name);
    std::fs::write(&replacement, contents)?;
    rebuild_archive_with_changes(
        path,
        &[replacement],
        &parent.to_string_lossy(),
        password,
        format,
        Some(&selected),
    )
}

pub(super) fn rebuild_archive_with_additions(
    path: &Path,
    sources: &[PathBuf],
    folder: &str,
    password: Option<&str>,
    format: ResolvedFormat,
) -> Result<()> {
    rebuild_archive_with_changes(path, sources, folder, password, format, None)
}

pub(super) fn rebuild_archive_with_changes(
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
        let safe_name = safe_extraction_path(&entry.name)?;
        if remove.is_some_and(|selected| safe_name == selected || safe_name.starts_with(selected)) {
            continue;
        }
        let name = safe_name
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
pub(super) fn collect_archive_sources(
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
pub(super) fn write_tar(
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

pub(super) fn write_extended_archive(
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
    let mut writer = WriteArchive::new().format(backend);
    if format == ResolvedFormat::Cpio {
        writer = writer.format_option(libarchive2::FormatOption::CpioHeaderCharset("UTF-8".into()));
    }
    let mut archive = writer.open_file(path)?;
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
pub(super) fn write_compressed_tar(
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
pub(super) fn write_compressed_file(
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
