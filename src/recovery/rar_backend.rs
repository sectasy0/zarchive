use super::*;

pub(super) fn rar_reason(error: crate::rar_recovery::Failure) -> Reason {
    use crate::rar_recovery::Failure;
    match error {
        Failure::MissingVolume => Reason::MissingVolume,
        Failure::Limit => Reason::Limit,
        Failure::Cancelled => Reason::Limit,
        Failure::Write => Reason::Read,
        Failure::Native(code) => match code {
            unrar_sys::ERAR_MISSING_PASSWORD => Reason::Password,
            unrar_sys::ERAR_BAD_PASSWORD => Reason::WrongPassword,
            unrar_sys::ERAR_BAD_DATA => Reason::Checksum,
            unrar_sys::ERAR_EOPEN => Reason::MissingVolume,
            unrar_sys::ERAR_EREAD => Reason::Truncated,
            unrar_sys::ERAR_NO_MEMORY => Reason::Limit,
            unrar_sys::ERAR_UNKNOWN_FORMAT | unrar_sys::ERAR_EREFERENCE => Reason::Unsupported,
            _ => Reason::Header,
        },
    }
}

pub(super) fn recover_rar(
    source: &Source,
    password: Option<&str>,
    recovery: &mut Recovery,
) -> Result<()> {
    let first = unrar::Archive::new(&recovery.source).first_part();
    let mut reader =
        match crate::rar_recovery::Reader::open(&first, password, source.remaining.clone()) {
            Ok(reader) => reader,
            Err(error) => {
                let reason = match rar_reason(error) {
                    Reason::Checksum => Reason::Header,
                    reason => reason,
                };
                recovery.diagnose(None, reason, None, None);
                recovery.limited |= reason == Reason::Limit;
                return Ok(());
            }
        };
    if reader.recovery_record {
        recovery.notes.push("RAR recovery record detected");
    }
    if reader.open_warning {
        recovery.diagnose(None, Reason::Header, None, None);
    }
    let mut solid_damaged = false;
    for _ in 0..MAX_FILES {
        crate::task_queue::checkpoint()?;
        let header = match reader.next() {
            Ok(Some(header)) => header,
            Ok(None) => return Ok(()),
            Err(error) => {
                crate::task_queue::checkpoint()?;
                let reason = match rar_reason(error) {
                    Reason::Checksum => Reason::Header,
                    reason => reason,
                };
                recovery.diagnose(None, reason, None, None);
                recovery.limited |= reason == Reason::Limit;
                return Ok(());
            }
        };
        if header.directory || header.link {
            recovery.skipped(
                header.name,
                Status::Skipped,
                "Directories and links are not recovered",
            );
            if let Err(error) = reader.skip() {
                recovery.diagnose(None, rar_reason(error), None, None);
                return Ok(());
            }
            continue;
        }
        let allowance = recovery.remaining.min(MAX_FILE_BYTES);
        let reject = if header.size > allowance
            || u64::from(header.dictionary) * 1024 > MAX_FILE_BYTES
            || (header.size > 10 * 1024 * 1024 && header.size / header.packed.max(1) > 1000)
        {
            Some(Reason::Limit)
        } else if header.encrypted && password.is_none_or(str::is_empty) {
            Some(Reason::Password)
        } else {
            None
        };
        if let Some(reason) = reject {
            recovery.unavailable(header.name, reason, 0, header.packed);
            if let Some(diagnostic) = recovery.diagnostics.last_mut() {
                diagnostic.offset = None;
            }
            recovery.limited |= reason == Reason::Limit;
            solid_damaged |= reader.solid || header.solid;
            if let Err(error) = reader.skip() {
                recovery.diagnose(None, rar_reason(error), None, None);
                return Ok(());
            }
            continue;
        }
        let temporary = tempfile::NamedTempFile::new_in(recovery.directory.path())?;
        let outcome = reader.read_to(temporary.as_file().try_clone()?, allowance);
        crate::task_queue::checkpoint()?;
        if matches!(outcome, Err(crate::rar_recovery::Failure::Write)) {
            anyhow::bail!("Failed to write recovered data");
        }
        let index = recovery.entries.len();
        let block_count = recovery.blocks.len();
        recovery.retain(
            header.name,
            File::open(temporary.path())?,
            Some(header.size),
            header.crc,
            0,
            header.packed,
        )?;
        recovery.blocks.truncate(block_count);
        for diagnostic in recovery
            .diagnostics
            .iter_mut()
            .filter(|diagnostic| diagnostic.entry == Some(index))
        {
            diagnostic.offset = None;
        }
        match outcome {
            Ok(()) => {
                if let Some(entry) = recovery
                    .entries
                    .get_mut(index)
                    .filter(|entry| entry.status == Status::Unverified)
                {
                    entry.status = Status::Recovered;
                    entry.issue = None;
                    if let Some(name) = &entry.output_name {
                        entry.output_name = Some(name.replacen("unverified/", "recovered/", 1));
                    }
                    recovery.diagnostics.retain(|diagnostic| {
                        !(diagnostic.entry == Some(index)
                            && diagnostic.reason == Reason::Unverified)
                    });
                }
            }
            Err(error) => {
                let reason = if solid_damaged && header.solid {
                    Reason::SolidBlock
                } else if header.encrypted
                    && matches!(rar_reason(error), Reason::Checksum | Reason::Read)
                {
                    Reason::EncryptedData
                } else {
                    rar_reason(error)
                };
                if let Some(entry) = recovery.entries.get_mut(index) {
                    entry.status = if entry.file.is_some() {
                        Status::Partial
                    } else {
                        Status::NotRecovered
                    };
                    entry.issue = Some(reason.label());
                    if let Some(name) = &entry.output_name {
                        entry.output_name =
                            Some(name.replacen("recovered/", "partial/", 1).replacen(
                                "unverified/",
                                "partial/",
                                1,
                            ));
                    }
                }
                if let Some(diagnostic) = recovery
                    .diagnostics
                    .iter_mut()
                    .find(|diagnostic| diagnostic.entry == Some(index))
                {
                    diagnostic.reason = reason;
                } else {
                    recovery.diagnose(Some(index), reason, None, Some(header.packed));
                }
                solid_damaged |= reader.solid || header.solid;
                if matches!(
                    reason,
                    Reason::Limit
                        | Reason::MissingVolume
                        | Reason::WrongPassword
                        | Reason::Password
                ) {
                    recovery.limited |= reason == Reason::Limit;
                    return Ok(());
                }
            }
        }
    }
    recovery.limited = true;
    recovery.diagnose(None, Reason::Limit, None, None);
    Ok(())
}
