use super::*;

#[derive(Clone, Copy)]
enum Candidate {
    Zip,
    Tar,
    Cpio,
    Ar,
    Png,
    Jpeg,
    Pdf,
}

fn scan(source: &Source, salvage: bool, recovery: &mut Recovery) -> Result<Vec<(u64, Candidate)>> {
    let signatures: &[(&[u8], Candidate)] = if salvage {
        &[
            (b"PK\x03\x04", Candidate::Zip),
            (b"ustar", Candidate::Tar),
            (b"070701", Candidate::Cpio),
            (b"070702", Candidate::Cpio),
            (b"070707", Candidate::Cpio),
            (b"`\n", Candidate::Ar),
            (b"\x89PNG\r\n\x1a\n", Candidate::Png),
            (b"\xff\xd8\xff", Candidate::Jpeg),
            (b"%PDF-", Candidate::Pdf),
        ]
    } else {
        &[
            (b"PK\x03\x04", Candidate::Zip),
            (b"ustar", Candidate::Tar),
            (b"070701", Candidate::Cpio),
            (b"070702", Candidate::Cpio),
            (b"070707", Candidate::Cpio),
            (b"`\n", Candidate::Ar),
        ]
    };
    let mut candidates = std::collections::BTreeMap::new();
    let mut carry = Vec::new();
    let mut previous_end = None;
    let mut buffer = vec![0u8; 65536];
    for segment in source
        .segments
        .iter()
        .filter(|segment| segment.path.is_some())
    {
        if previous_end != Some(segment.start) {
            carry.clear();
        }
        let mut reader = source.reader();
        reader.seek(SeekFrom::Start(segment.start))?;
        let mut position = segment.start;
        while position < segment.start + segment.size {
            crate::task_queue::checkpoint()?;
            let scan_left = source.scan_remaining.load(Ordering::Relaxed);
            if scan_left == 0 || candidates.len() >= MAX_CANDIDATES {
                recovery.limited = true;
                return Ok(candidates.into_iter().collect());
            }
            let length = (segment.start + segment.size - position)
                .min(buffer.len() as u64)
                .min(scan_left) as usize;
            let count = match reader.read(&mut buffer[..length]) {
                Ok(count) => count,
                Err(_) => {
                    recovery.blocks.push(Block {
                        offset: position,
                        length: segment.start + segment.size - position,
                        issue: "Damaged or missing data",
                    });
                    break;
                }
            };
            if count == 0 {
                break;
            }
            let start = position.saturating_sub(carry.len() as u64);
            carry.extend_from_slice(&buffer[..count]);
            for (signature, kind) in signatures {
                for offset in memchr::memmem::find_iter(&carry, signature) {
                    if candidates.len() >= MAX_CANDIDATES {
                        recovery.limited = true;
                        break;
                    }
                    let found = start + offset as u64;
                    let found = if matches!(kind, Candidate::Tar) {
                        if let Some(header) = found.checked_sub(257) {
                            header
                        } else {
                            continue;
                        }
                    } else if matches!(kind, Candidate::Ar) {
                        if let Some(header) = found.checked_sub(58) {
                            header
                        } else {
                            continue;
                        }
                    } else {
                        found
                    };
                    candidates.insert(found, *kind);
                }
            }
            let keep = carry.len().saturating_sub(15);
            carry.drain(..keep);
            source
                .scan_remaining
                .fetch_sub(count as u64, Ordering::Relaxed);
            position += count as u64;
        }
        previous_end = Some(segment.start + segment.size);
    }
    Ok(candidates.into_iter().collect())
}

pub(super) fn recover_headers(source: &Source, recovery: &mut Recovery) -> Result<()> {
    if matches!(
        crate::archive::ResolvedFormat::from_path(&recovery.source),
        Some(crate::archive::ResolvedFormat::Tar | crate::archive::ResolvedFormat::Pax)
    ) {
        recover_tar_blocks(source, recovery)?;
    }
    let candidates = scan(source, recovery.salvage, recovery)?;
    let mut ar_names = None;
    for (offset, kind) in candidates {
        crate::task_queue::checkpoint()?;
        if recovery.entries.len() >= MAX_FILES {
            recovery.limited = true;
            break;
        }
        if matches!(kind, Candidate::Tar) {
            recover_tar_header(source, offset, recovery)?;
            continue;
        }
        if matches!(kind, Candidate::Cpio) {
            structured::recover_cpio_record(source, offset, recovery)?;
            continue;
        }
        if matches!(kind, Candidate::Ar) {
            structured::recover_ar_record(source, offset, recovery, &mut ar_names)?;
            continue;
        }
        if !matches!(kind, Candidate::Zip) {
            carve(source, offset, kind, recovery)?;
            continue;
        }
        let mut reader = source.reader();
        reader.seek(SeekFrom::Start(offset))?;
        match zip::read::read_zipfile_from_stream(&mut reader) {
            Ok(Some(entry)) => {
                let name = entry
                    .name()
                    .map(|name| name.into_owned())
                    .unwrap_or_else(|_| format!("entry-at-{offset}"));
                if recovery
                    .entries
                    .iter()
                    .any(|existing| existing.name == name && existing.file.is_some())
                {
                    continue;
                }
                if entry.is_dir()
                    || entry
                        .unix_mode()
                        .is_some_and(|mode| mode & 0o170000 == 0o120000)
                {
                    recovery.skipped(
                        name,
                        Status::Skipped,
                        "Directories and links are not recovered",
                    );
                    continue;
                }
                let size = entry.size();
                let packed = entry.compressed_size();
                let crc = entry.crc32();
                recovery.retain(name, entry, Some(size), Some(crc), offset, packed)?;
            }
            _ => recovery.blocks.push(Block {
                offset,
                length: 30,
                issue: "Damaged archive header",
            }),
        }
    }
    Ok(())
}

fn carve(source: &Source, offset: u64, kind: Candidate, recovery: &mut Recovery) -> Result<()> {
    let extension = match kind {
        Candidate::Png => "png",
        Candidate::Jpeg => "jpg",
        Candidate::Pdf => "pdf",
        _ => return Ok(()),
    };
    let mut reader = source.reader();
    reader.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    let length = recovery.remaining.min(MAX_CARVE_BYTES);
    if length == 0 {
        recovery.limited = true;
        return Ok(());
    }
    let _ = reader.take(length).read_to_end(&mut bytes);
    crate::task_queue::checkpoint()?;
    if !infer::get(&bytes).is_some_and(|signature| signature.extension() == extension) {
        return Ok(());
    }
    let end = match kind {
        Candidate::Png => memchr::memmem::find(&bytes, b"\0\0\0\0IEND").map(|offset| offset + 12),
        Candidate::Jpeg => memchr::memmem::find(&bytes, b"\xff\xd9").map(|offset| offset + 2),
        Candidate::Pdf => memchr::memmem::rfind(&bytes, b"%%EOF").map(|offset| offset + 5),
        _ => None,
    };
    if let Some(end) = end.filter(|end| *end <= bytes.len()) {
        bytes.truncate(end);
    }
    let previous = recovery.entries.len();
    recovery.retain(
        format!("signature-at-{offset}.{extension}"),
        std::io::Cursor::new(&bytes),
        None,
        None,
        offset,
        bytes.len() as u64,
    )?;
    if recovery.entries.len() > previous {
        if let Some(entry) = recovery
            .entries
            .last_mut()
            .filter(|entry| entry.file.is_some())
        {
            entry.status = Status::Candidate;
            entry.issue = Some("Signature found; file boundaries are not guaranteed");
            entry.output_name = Some(format!(
                "carved/{previous:05}-signature-at-{offset}.{extension}"
            ));
        }
    }
    Ok(())
}
