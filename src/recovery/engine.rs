use super::*;
use native_backend::recover_native;
use rar_backend::recover_rar;
use salvage::recover_headers;
use sevenz_backend::recover_seven_zip;
use zip_backend::{recover_zip_index, zip_index_allowed};

pub(crate) fn recover_with_parity(
    path: &Path,
    password: Option<&str>,
    salvage: bool,
    parity: bool,
) -> Result<Recovery> {
    let split_zip = crate::parity::is_split_zip(path);
    if (!parity || !crate::parity::supported(path)) && !split_zip {
        return recover_raw(path, password, salvage);
    }
    let preparation = if parity && crate::parity::supported(path) {
        crate::parity::reconstruct(path)
    } else {
        crate::parity::reconstruct_split_zip(path)
    };
    let prepared = match preparation {
        Ok(prepared) => prepared,
        Err(error) => {
            crate::task_queue::checkpoint()?;
            let mut result = recover_raw(path, password, salvage)?;
            let limited = error.to_string().contains("limit");
            let status = if limited {
                crate::parity::Status::Limited
            } else {
                crate::parity::Status::Failed
            };
            result.limited |= limited;
            result.notes.push(status.label());
            result.parity =
                Some(serde_json::json!({ "status": status.label(), "candidate_created": false }));
            result.diagnose(
                None,
                if limited {
                    Reason::Limit
                } else {
                    Reason::ParityData
                },
                None,
                None,
            );
            return Ok(result);
        }
    };
    let mut used_repaired_copy = false;
    let mut result = if let Some(candidate) = &prepared.archive {
        match recover(candidate, password, salvage) {
            Ok(result) => {
                used_repaired_copy = true;
                result
            }
            Err(_) => {
                crate::task_queue::checkpoint()?;
                recover_raw(path, password, salvage)?
            }
        }
    } else if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("par2"))
    {
        Recovery::new(path, salvage)?
    } else {
        recover_raw(path, password, salvage)?
    };
    for file in &prepared.files {
        let index = result.entries.len();
        if !file.verified {
            result.unavailable(file.name.clone(), Reason::ParityData, 0, file.size);
            continue;
        }
        result.retain(
            file.name.clone(),
            File::open(&file.path)?,
            Some(file.size),
            None,
            0,
            file.size,
        )?;
        if let Some(entry) = result.entries.get_mut(index) {
            if entry.status == Status::Unverified {
                entry.status = Status::Recovered;
                if let Some(name) = &entry.output_name {
                    entry.output_name = Some(name.replacen("unverified/", "recovered/", 1));
                }
                result.diagnostics.retain(|diagnostic| {
                    diagnostic.entry != Some(index) || diagnostic.reason != Reason::Unverified
                });
            }
        }
    }
    used_repaired_copy |= prepared.files.iter().any(|file| file.verified);
    result.source = path.to_owned();
    result.notes.push(prepared.status.label());
    if used_repaired_copy {
        result.data_range_source = "private_repaired_copy";
        result
            .notes
            .push("Reported data ranges refer to the private repaired copy");
    }
    result.parity = Some(prepared.json());
    if let Some(parity) = &mut result.parity {
        parity["data_range_source"] = serde_json::json!(result.data_range_source);
    }
    let reason = match prepared.status {
        crate::parity::Status::ToolMissing => Some(Reason::ParityTool),
        crate::parity::Status::ZipToolMissing => Some(Reason::Unsupported),
        crate::parity::Status::IncompleteVolumes => Some(Reason::MissingVolume),
        crate::parity::Status::SplitInvalid => Some(Reason::Checksum),
        crate::parity::Status::Failed => Some(Reason::ParityData),
        crate::parity::Status::Limited => {
            result.limited = true;
            Some(Reason::Limit)
        }
        _ => None,
    };
    if let Some(reason) = reason {
        result.diagnose(None, reason, None, None);
    }
    Ok(result)
}

pub(crate) fn recover(path: &Path, password: Option<&str>, salvage: bool) -> Result<Recovery> {
    recover_with_parity(path, password, salvage, false)
}

fn recover_raw(path: &Path, password: Option<&str>, salvage: bool) -> Result<Recovery> {
    let mut recovery = Recovery::new(path, salvage)?;
    let source = Source::open(path, &mut recovery)?;
    recover_source(path, password, salvage, source, recovery)
}

pub(super) fn recover_source(
    path: &Path,
    password: Option<&str>,
    salvage: bool,
    source: Source,
    mut recovery: Recovery,
) -> Result<Recovery> {
    if let Some(recovered) = structured::recover_compressed(path, &source, salvage)? {
        return Ok(recovered);
    }
    let mut signature = [0u8; 4];
    let _ = source.reader().read_exact(&mut signature);
    if signature == *b"Rar!"
        || crate::archive::ResolvedFormat::from_path(path)
            == Some(crate::archive::ResolvedFormat::Rar)
    {
        recover_rar(&source, password, &mut recovery)?;
        if salvage || recovery.entries.is_empty() || !recovery.blocks.is_empty() {
            recover_headers(&source, &mut recovery)?;
        }
        recovery.limited |= source.remaining.load(Ordering::Relaxed) == 0;
        recovery.complete_diagnostics();
        return Ok(recovery);
    }
    let indexed = if zip_index_allowed(&source, &mut recovery)? {
        source
            .metadata_remaining
            .store(MAX_INDEX_BYTES * 4, Ordering::Relaxed);
        let result = zip::ZipArchive::new(source.reader()).ok();
        if source.metadata_remaining.load(Ordering::Relaxed) == 0 {
            recovery.limited = true;
            recovery
                .notes
                .push("ZIP directory exceeds the recovery metadata limit; scanning local headers");
        }
        source.metadata_remaining.store(u64::MAX, Ordering::Relaxed);
        result
    } else {
        None
    };
    if let Some(archive) = indexed {
        recover_zip_index(archive, password, &mut recovery)?;
    } else {
        let mut signature = [0u8; 4];
        let _ = source.reader().read_exact(&mut signature);
        if signature.starts_with(b"PK")
            || crate::archive::ResolvedFormat::from_path(path)
                == Some(crate::archive::ResolvedFormat::Zip)
        {
            recovery
                .notes
                .push("The archive directory is damaged; local headers will be scanned");
        } else if structured::recover_xar(&source, &mut recovery)? {
        } else if !signature.starts_with(b"PK")
            && !recover_seven_zip(&source, password, &mut recovery)?
        {
            recover_native(&source, password, &mut recovery)?;
        }
    }
    if salvage
        || recovery.entries.is_empty()
        || !recovery.blocks.is_empty()
        || recovery
            .entries
            .iter()
            .any(|entry| entry.issue == Some("Damaged archive header"))
    {
        recover_headers(&source, &mut recovery)?;
    }
    recovery.limited |= source.remaining.load(Ordering::Relaxed) == 0;
    if !recovery.blocks.is_empty() {
        recovery
            .notes
            .push("Recovery is best effort; unreadable regions may contain other files");
    }
    recovery.complete_diagnostics();
    Ok(recovery)
}
