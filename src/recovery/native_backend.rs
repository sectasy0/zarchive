use super::*;

pub(super) fn recover_native(
    source: &Source,
    password: Option<&str>,
    recovery: &mut Recovery,
) -> Result<()> {
    let mut archive = match libarchive2::ReadArchive::open_callback(
        libarchive2::CallbackReader::new(source.reader()),
    ) {
        Ok(archive) => archive,
        Err(_) => {
            recovery
                .notes
                .push("The native reader could not open the damaged archive");
            recovery.diagnose(None, Reason::Header, None, None);
            return Ok(());
        }
    };
    if let Some(password) = password {
        archive.add_passphrase(password)?;
    }
    for index in 0..MAX_FILES {
        crate::task_queue::checkpoint()?;
        let entry = match archive.next_entry() {
            Ok(Some(entry)) => entry,
            Ok(None) => return Ok(()),
            Err(_) => {
                recovery.blocks.push(Block {
                    offset: 0,
                    length: source.length,
                    issue: "Native parser stopped at damaged data",
                });
                return Ok(());
            }
        };
        let name = entry.pathname().unwrap_or_else(|| format!("entry-{index}"));
        let regular = entry.file_type() == libarchive2::FileType::RegularFile
            && entry.symlink().is_none()
            && entry.hardlink().is_none();
        let size = u64::try_from(entry.size()).ok();
        drop(entry);
        if !regular {
            recovery.skipped(
                name,
                Status::Skipped,
                "Directories and links are not recovered",
            );
            if size.is_some_and(|size| size > 0) {
                return Ok(());
            }
            continue;
        }
        recovery.retain(
            name,
            crate::archive::ExtendedEntryReader(&mut archive),
            size,
            None,
            0,
            source.length,
        )?;
        if recovery.limited
            || recovery
                .entries
                .last()
                .is_some_and(|entry| matches!(entry.status, Status::Partial | Status::NotRecovered))
        {
            return Ok(());
        }
    }
    recovery.limited = true;
    Ok(())
}
