use super::*;

pub(super) fn plausible_tar_header(bytes: &[u8; 512]) -> Option<tar::Header> {
    if bytes.iter().all(|byte| *byte == 0) {
        return None;
    }
    let modern = bytes.get(257..262) == Some(b"ustar");
    let old = bytes[257..263].iter().all(|byte| *byte == 0);
    if !modern && !old {
        return None;
    }
    let mut header = tar::Header::new_ustar();
    header.as_mut_bytes().copy_from_slice(bytes);
    let kind = header.entry_type();
    if !(kind.is_file()
        || kind.is_dir()
        || kind.is_symlink()
        || kind.is_hard_link()
        || kind.is_gnu_longname()
        || kind.is_gnu_longlink()
        || kind.is_pax_local_extensions()
        || kind.is_pax_global_extensions())
    {
        return None;
    }
    header.size().ok()?;
    if old {
        header.mode().ok()?;
    }
    let path = header.path().ok()?.to_string_lossy().into_owned();
    if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
        return None;
    }
    Some(header)
}

pub(super) fn recover_tar_blocks(source: &Source, recovery: &mut Recovery) -> Result<()> {
    let mut position = 0u64;
    let end = source.length.min(MAX_SCAN_BYTES);
    let mut damaged = None;
    let mut pending_name = None;
    let mut known_data_end = 0u64;
    let mut reader = source.reader();
    let mut bytes = [0u8; 512];
    while position.checked_add(512).is_some_and(|next| next <= end) {
        crate::task_queue::checkpoint()?;
        if recovery.entries.len() >= MAX_FILES
            || source.scan_remaining.load(Ordering::Relaxed) < 512
        {
            recovery.limited = true;
            break;
        }
        let segment = source
            .segments
            .iter()
            .find(|segment| position >= segment.start && position < segment.start + segment.size);
        if let Some(segment) = segment.filter(|segment| segment.path.is_none()) {
            position = (segment.start + segment.size).div_ceil(512) * 512;
            pending_name = None;
            continue;
        }
        reader.seek(SeekFrom::Start(position))?;
        source.scan_remaining.fetch_sub(512, Ordering::Relaxed);
        if reader.read_exact(&mut bytes).is_err() {
            damaged.get_or_insert(position);
            position += 512;
            pending_name = None;
            continue;
        }
        let Some(header) = plausible_tar_header(&bytes) else {
            if position >= known_data_end && !bytes.iter().all(|byte| *byte == 0) {
                damaged.get_or_insert(position);
            }
            position += 512;
            pending_name = None;
            continue;
        };
        if let Some(start) = damaged.take() {
            recovery.blocks.push(Block {
                offset: start,
                length: position - start,
                issue: "Damaged archive header",
            });
        }
        let size = header.size()?;
        let kind = header.entry_type();
        let mut checked = header.clone();
        let stored_checksum = checked.cksum().ok();
        checked.set_cksum();
        let checksum_valid = stored_checksum == checked.cksum().ok();
        if kind.is_gnu_longname() && size <= 4096 {
            let mut name = Vec::new();
            if reader.by_ref().take(size).read_to_end(&mut name).is_ok()
                && name.len() as u64 == size
            {
                pending_name = Some(
                    String::from_utf8_lossy(&name)
                        .trim_end_matches('\0')
                        .to_owned(),
                );
            }
        } else if kind.is_file() {
            recover_tar_header_named(source, position, recovery, pending_name.take())?;
        } else {
            pending_name = None;
            if kind.is_dir() || kind.is_symlink() || kind.is_hard_link() {
                let name = header.path()?.to_string_lossy().into_owned();
                if !recovery.entries.iter().any(|entry| entry.name == name) {
                    recovery.skipped(
                        name,
                        Status::Skipped,
                        "Directories and links are not recovered",
                    );
                }
            }
        }
        let next = size
            .checked_add(511)
            .map(|size| size / 512 * 512)
            .and_then(|size| position.checked_add(512)?.checked_add(size));
        known_data_end = next.unwrap_or(position + 512);
        if let Some(next) = next
            .filter(|next| *next <= end && checksum_valid && !(recovery.salvage && kind.is_file()))
        {
            position = next;
        } else {
            position += 512;
            pending_name = None;
        }
    }
    if let Some(start) = damaged {
        recovery.blocks.push(Block {
            offset: start,
            length: position.saturating_sub(start),
            issue: "Damaged archive header",
        });
    }
    if source.length > MAX_SCAN_BYTES {
        recovery.limited = true;
    }
    Ok(())
}

pub(super) fn recover_tar_header(
    source: &Source,
    offset: u64,
    recovery: &mut Recovery,
) -> Result<()> {
    recover_tar_header_named(source, offset, recovery, None)
}

pub(super) fn recover_tar_header_named(
    source: &Source,
    offset: u64,
    recovery: &mut Recovery,
    name: Option<String>,
) -> Result<()> {
    let mut reader = source.reader();
    reader.seek(SeekFrom::Start(offset))?;
    let mut bytes = [0u8; 512];
    if reader.read_exact(&mut bytes).is_err() {
        return Ok(());
    }
    let Some(mut header) = plausible_tar_header(&bytes) else {
        return Ok(());
    };
    if !header.entry_type().is_file() {
        return Ok(());
    }
    let name = name.unwrap_or_else(|| header.path().unwrap().to_string_lossy().into_owned());
    let size = match header.size() {
        Ok(size) => size,
        Err(_) => return Ok(()),
    };
    if name.is_empty()
        || recovery.entries.iter().any(|entry| {
            entry.name == name && matches!(entry.status, Status::Recovered | Status::Unverified)
        })
    {
        return Ok(());
    }
    let stored = header.cksum().ok();
    header.set_cksum();
    if stored != header.cksum().ok() {
        recovery.blocks.push(Block {
            offset,
            length: 512,
            issue: "TAR header checksum reconstructed",
        });
    }
    recovery.retain(
        name,
        reader.take(size),
        Some(size),
        None,
        offset + 512,
        size,
    )?;
    Ok(())
}
