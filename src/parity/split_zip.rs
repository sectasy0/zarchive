use super::*;
use std::io::{Read, Seek, SeekFrom};

fn split_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let extension = extension.to_ascii_lowercase();
            extension.len() >= 3
                && extension.len() <= 6
                && extension.starts_with('z')
                && extension.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        })
}

pub(crate) fn is_split(path: &Path) -> bool {
    (split_extension(path) && !path.with_extension("rar").is_file())
        || (path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
            && ["z01", "z02", "Z01", "Z02"]
                .iter()
                .any(|extension| path.with_extension(extension).exists()))
}

pub(crate) fn layout(path: &Path) -> Result<Option<(Vec<crate::volumes::RecoveryVolume>, bool)>> {
    if !is_split(path) {
        return Ok(None);
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("Invalid split ZIP filename")?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut parts = std::collections::BTreeMap::new();
    let mut final_zip = None;
    let mut total = 0u64;
    for (count, entry) in std::fs::read_dir(parent)?.enumerate() {
        crate::task_queue::checkpoint()?;
        anyhow::ensure!(count < 20_000, "Split ZIP directory safety limit reached");
        let candidate = entry?.path();
        if !candidate
            .file_stem()
            .is_some_and(|value| value.to_string_lossy().eq_ignore_ascii_case(stem))
        {
            continue;
        }
        let extension = candidate
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if extension != "zip" && !split_extension(&candidate) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&candidate)?;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Split ZIP requires regular files without symlinks"
        );
        total = total
            .checked_add(metadata.len())
            .context("Split ZIP input size limit reached")?;
        anyhow::ensure!(
            total <= MAX_SET_BYTES && parts.len() < MAX_SET_FILES,
            "Split ZIP input safety limit reached"
        );
        if extension == "zip" {
            anyhow::ensure!(final_zip.is_none(), "Conflicting split ZIP filenames");
            final_zip = Some((candidate, metadata.len()));
        } else {
            let index: usize = extension[1..].parse()?;
            anyhow::ensure!(
                index > 0 && index < MAX_SET_FILES && !parts.contains_key(&index),
                "Split ZIP volume count or name conflict"
            );
            parts.insert(index, (candidate, metadata.len()));
        }
    }
    let mut disks = None;
    if let Some((zip, size)) = &final_zip {
        let mut file = File::open(zip)?;
        let length = (*size).min(65557);
        file.seek(SeekFrom::Start(size - length))?;
        let mut tail = vec![0; length as usize];
        file.read_exact(&mut tail)?;
        for offset in (0..tail.len().saturating_sub(21)).rev() {
            if tail[offset..].starts_with(b"PK\x05\x06")
                && offset
                    + 22
                    + u16::from_le_bytes(tail[offset + 20..offset + 22].try_into()?) as usize
                    == tail.len()
            {
                disks =
                    Some(u16::from_le_bytes(tail[offset + 4..offset + 6].try_into()?) as usize + 1);
                break;
            }
        }
    }
    let inferred = disks.is_none() || final_zip.is_none();
    let last_part = parts.keys().next_back().copied().unwrap_or(0);
    let disks = disks.unwrap_or(last_part + 1);
    anyhow::ensure!(
        disks > 1 && disks <= MAX_SET_FILES && last_part < disks,
        "Split ZIP disk count exceeds the safety limit or conflicts with metadata"
    );
    let normal_size = parts.values().map(|(_, size)| *size).max().unwrap_or(0);
    anyhow::ensure!(
        normal_size > 0,
        "Split ZIP volume sizes cannot be determined"
    );
    let mut volumes = Vec::new();
    for index in 1..disks {
        let part = parts.remove(&index);
        volumes.push(crate::volumes::RecoveryVolume {
            size: part.as_ref().map_or(normal_size, |(_, size)| *size),
            path: part.map(|(path, _)| path),
        });
    }
    volumes.push(crate::volumes::RecoveryVolume {
        size: final_zip.as_ref().map_or(normal_size, |(_, size)| *size),
        path: final_zip.map(|(path, _)| path),
    });
    let missing = volumes.iter().any(|volume| volume.path.is_none());
    Ok(Some((volumes, inferred || missing)))
}

fn tool() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ZARCHIVE_ZIP_TOOL") {
        return PathBuf::from(path)
            .canonicalize()
            .ok()
            .filter(|path| path.is_file());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(if cfg!(windows) { "zip.exe" } else { "zip" }))
            .find_map(|path| path.canonicalize().ok().filter(|path| path.is_file()))
    })
}

pub(crate) fn reconstruct(source: &Path) -> Result<Reconstruction> {
    reconstruct_with_tool(source, tool().as_deref())
}

fn verified_copy(path: &Path) -> Result<bool> {
    crate::recovery::verify_zip_copy(path)
}

fn reconstruct_with_tool(source: &Path, tool: Option<&Path>) -> Result<Reconstruction> {
    let (volumes, _) = layout(source)?.context("Not a native split ZIP set")?;
    let directory = tempfile::tempdir()?;
    let selected = directory.path().join("input.zip");
    let mut archives = Vec::new();
    for (index, volume) in volumes.iter().enumerate() {
        crate::task_queue::checkpoint()?;
        let Some(path) = &volume.path else {
            continue;
        };
        let name = if index + 1 == volumes.len() {
            "input.zip".to_owned()
        } else {
            format!("input.z{:02}", index + 1)
        };
        let destination = directory.path().join(name);
        let mut input = File::open(path)?;
        let mut output = File::options()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let copied =
            crate::task_queue::copy(&mut input.by_ref().take(volume.size + 1), &mut output)?;
        anyhow::ensure!(
            copied == volume.size,
            "Split ZIP source changed during staging"
        );
        archives.push(destination);
    }
    let mut result = Reconstruction {
        status: Status::SplitInvalid,
        archive: None,
        method: "Info-ZIP native split conversion",
        exit_code: None,
        changed_files: Vec::new(),
        files: Vec::new(),
        workspace: Some(Workspace {
            directory,
            selected,
            archives,
            recovery_volumes: Vec::new(),
        }),
    };
    if volumes.iter().any(|volume| volume.path.is_none()) {
        result.status = Status::IncompleteVolumes;
        return Ok(result);
    }
    let Some(tool) = tool else {
        result.status = Status::ZipToolMissing;
        return Ok(result);
    };
    let workspace = result.workspace.as_ref().unwrap();
    let mut command = Command::new(tool);
    command
        .current_dir(workspace.directory.path())
        .env_remove("ZIPOPT")
        .env_remove("UNZIPOPT")
        .args(["input.zip", "-s", "0", "--out", "merged.zip"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let outcome = match run_engine(&mut command, workspace.directory.path()) {
        Ok(outcome) => outcome,
        Err(_) => {
            crate::task_queue::checkpoint()?;
            result.status = Status::ZipToolMissing;
            return Ok(result);
        }
    };
    result.exit_code = outcome.code;
    if outcome.limited {
        result.status = Status::Limited;
        return Ok(result);
    }
    let merged = workspace.directory.path().join("merged.zip");
    if outcome.success
        && merged.is_file()
        && merged.metadata()?.len() <= MAX_SET_BYTES
        && workspace_size(workspace.directory.path())? <= MAX_WORKSPACE_BYTES
        && verified_copy(&merged).unwrap_or(false)
    {
        result.status = Status::Merged;
        result.archive = Some(merged);
    }
    crate::task_queue::checkpoint()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(directory: &Path) -> PathBuf {
        let mut cursor = std::io::Cursor::new(Vec::new());
        cursor.write_all(b"PK\x07\x08").unwrap();
        let mut writer = zip::ZipWriter::new(cursor);
        for (name, bytes) in [
            ("first.txt", b"first intact payload".to_vec()),
            ("middle.bin", vec![b'x'; 200_000]),
            ("last.txt", b"last intact payload".to_vec()),
        ] {
            writer
                .start_file(
                    name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        let mut bytes = writer.finish().unwrap().into_inner();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let mut headers = Vec::new();
        for index in 0..archive.len() {
            let file = archive.by_index_raw(index).unwrap();
            headers.push((
                file.central_header_start() as usize,
                file.header_start() as usize,
            ));
        }
        let central = headers[0].0;
        drop(archive);
        let size = 65536usize;
        for (header, local) in headers {
            bytes[header + 34..header + 36].copy_from_slice(&((local / size) as u16).to_le_bytes());
            bytes[header + 42..header + 46].copy_from_slice(&((local % size) as u32).to_le_bytes());
        }
        let end = bytes.len() - 22;
        let disks = bytes.len().div_ceil(size);
        bytes[end + 4..end + 6].copy_from_slice(&((disks - 1) as u16).to_le_bytes());
        bytes[end + 6..end + 8].copy_from_slice(&((central / size) as u16).to_le_bytes());
        bytes[end + 16..end + 20].copy_from_slice(&((central % size) as u32).to_le_bytes());
        let final_zip = directory.join("set.zip");
        for (index, part) in bytes.chunks(size).enumerate() {
            let path = if index + 1 == disks {
                final_zip.clone()
            } else {
                directory.join(format!("set.z{:02}", index + 1))
            };
            std::fs::write(path, part).unwrap();
        }
        final_zip
    }

    #[test]
    fn split_zip_missing_parts_preserve_offsets_and_recover_later_local_entries() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture(root.path());
        std::fs::remove_file(root.path().join("set.z02")).unwrap();
        let (volumes, inferred) = layout(&source).unwrap().unwrap();
        assert!(inferred);
        assert_eq!(volumes.len(), 4);
        assert!(volumes[1].path.is_none());
        assert_eq!(volumes[1].size, 65536);
        assert_eq!(
            reconstruct_with_tool(&source, None).unwrap().status,
            Status::IncompleteVolumes
        );
        let report =
            crate::recovery::recover_with_parity(&root.path().join("set.z01"), None, false, false)
                .unwrap();
        let output = root.path().join("recovered.zip");
        report.export_zip(&output).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(output).unwrap()).unwrap();
        let names = archive
            .file_names()
            .map(|name| name.unwrap().into_owned())
            .collect::<Vec<_>>();
        for suffix in ["first.txt", "last.txt"] {
            let name = names
                .iter()
                .find(|name| name.ends_with(suffix))
                .expect("intact local entry must be recovered");
            let mut bytes = String::new();
            archive
                .by_name(name)
                .unwrap()
                .read_to_string(&mut bytes)
                .unwrap();
            assert!(bytes.ends_with("intact payload"));
        }
    }

    #[test]
    fn split_zip_complete_set_without_engine_reports_missing_tool_without_modification() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture(root.path());
        let before = std::fs::read(&source).unwrap();
        assert_eq!(
            reconstruct_with_tool(&source, None).unwrap().status,
            Status::ZipToolMissing
        );
        assert_eq!(std::fs::read(source).unwrap(), before);
    }

    #[test]
    #[ignore = "requires Info-ZIP on PATH"]
    fn split_zip_real_infozip_merges_native_disks_and_verifies_all_entries() {
        let root = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("first.txt", b"first intact payload".to_vec()),
            ("middle.bin", vec![b'x'; 200_000]),
            ("last.txt", b"last intact payload".to_vec()),
        ] {
            std::fs::write(root.path().join(name), bytes).unwrap();
        }
        assert!(
            Command::new(tool().expect("Info-ZIP must be installed"))
                .current_dir(root.path())
                .args([
                    "-q",
                    "-0",
                    "-s",
                    "64k",
                    "set.zip",
                    "first.txt",
                    "middle.bin",
                    "last.txt"
                ])
                .stdin(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        let source = root.path().join("set.zip");
        let before = digest(&source).unwrap();
        let result = reconstruct(&root.path().join("set.z02")).unwrap();
        if let Some(candidate) = &result.archive {
            assert!(verified_copy(candidate).unwrap());
        }
        let report = crate::recovery::recover_with_parity(&source, None, false, false).unwrap();
        let output = root.path().join("recovered.zip");
        report.export_zip(&output).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(output).unwrap()).unwrap();
        assert_eq!(archive.len(), 4);
        let names = archive
            .file_names()
            .map(|name| name.unwrap().into_owned())
            .collect::<Vec<_>>();
        let name = names
            .iter()
            .find(|name| name.ends_with("middle.bin"))
            .unwrap();
        let mut payload = Vec::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_end(&mut payload)
            .unwrap();
        assert_eq!(payload, vec![b'x'; 200_000]);
        assert_eq!(digest(&source).unwrap(), before);
    }
}
