use super::*;

pub(crate) fn verify_copy(path: &Path) -> Result<bool> {
    let mut recovery = Recovery::new(path, false)?;
    let source = Source::open(path, &mut recovery)?;
    if !zip_index_allowed(&source, &mut recovery)? {
        return Ok(false);
    }
    source
        .metadata_remaining
        .store(MAX_INDEX_BYTES * 4, Ordering::Relaxed);
    let mut archive = zip::ZipArchive::new(source.reader())?;
    if source.metadata_remaining.load(Ordering::Relaxed) == 0 {
        return Ok(false);
    }
    source.metadata_remaining.store(u64::MAX, Ordering::Relaxed);
    if archive.len() > MAX_FILES {
        return Ok(false);
    }
    let started = std::time::Instant::now();
    let mut total = 0u64;
    let mut buffer = vec![0; 65536];
    for index in 0..archive.len() {
        crate::task_queue::checkpoint()?;
        let mut file = archive.by_index(index)?;
        if file.size() > MAX_FILE_BYTES {
            return Ok(false);
        }
        let mut length = 0u64;
        loop {
            crate::task_queue::checkpoint()?;
            if started.elapsed().as_secs() >= 120 {
                return Ok(false);
            }
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            length += count as u64;
            total += count as u64;
            if length > MAX_FILE_BYTES || total > MAX_OUTPUT_BYTES {
                return Ok(false);
            }
        }
        if length != file.size() {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn zip_index_allowed(source: &Source, recovery: &mut Recovery) -> Result<bool> {
    let length = source.length.min(65557);
    let mut reader = source.reader();
    reader.seek(SeekFrom::End(-(length as i64)))?;
    let mut tail = Vec::new();
    if reader.take(length).read_to_end(&mut tail).is_err() {
        return Ok(false);
    }
    for offset in memchr::memmem::find_iter(&tail, b"PK\x05\x06") {
        let Some(record) = tail.get(offset..offset + 22) else {
            continue;
        };
        let comment = u16::from_le_bytes([record[20], record[21]]) as usize;
        if offset + 22 + comment != tail.len() {
            continue;
        }
        let count = u16::from_le_bytes([record[10], record[11]]) as usize;
        let bytes = u32::from_le_bytes(record[12..16].try_into().unwrap()) as u64;
        let zip64 = offset >= 20 && tail.get(offset - 20..offset - 16) == Some(b"PK\x06\x07");
        if count > MAX_FILES || bytes > MAX_INDEX_BYTES || zip64 {
            recovery.limited = true;
            recovery
                .notes
                .push("ZIP directory exceeds the recovery metadata limit; scanning local headers");
            return Ok(false);
        }
        return Ok(true);
    }
    Ok(false)
}

pub(super) fn recover_zip_index(
    mut archive: zip::ZipArchive<SourceReader>,
    password: Option<&str>,
    recovery: &mut Recovery,
) -> Result<()> {
    for index in 0..archive.len().min(MAX_FILES) {
        crate::task_queue::checkpoint()?;
        let raw = match archive.by_index_raw(index) {
            Ok(raw) => raw,
            Err(_) => {
                recovery.skipped(
                    format!("entry-{index}"),
                    Status::NotRecovered,
                    "Damaged archive header",
                );
                continue;
            }
        };
        let name = raw
            .name()
            .map(|name| name.into_owned())
            .unwrap_or_else(|_| format!("entry-{index}"));
        let size = raw.size();
        let packed = raw.compressed_size();
        let offset = raw.data_start().unwrap_or(raw.header_start());
        let crc = raw.crc32();
        let encrypted = raw.encrypted();
        if raw.is_dir()
            || raw
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            recovery.skipped(
                name,
                Status::Skipped,
                "Directories and links are not recovered",
            );
        } else if encrypted {
            drop(raw);
            if let Some(password) = password {
                match archive.by_index_decrypt(index, password.as_bytes()) {
                    Ok(reader) => {
                        recovery.retain(name, reader, Some(size), None, offset, packed)?
                    }
                    Err(_) => {
                        recovery.skipped(name, Status::NotRecovered, "Password required or invalid")
                    }
                }
            } else {
                recovery.skipped(name, Status::NotRecovered, "Password required or invalid");
            }
        } else {
            match raw.compression() {
                zip::CompressionMethod::Stored => {
                    recovery.retain(name, raw, Some(size), Some(crc), offset, packed)?
                }
                zip::CompressionMethod::Deflated => recovery.retain(
                    name,
                    flate2::read::DeflateDecoder::new(raw),
                    Some(size),
                    Some(crc),
                    offset,
                    packed,
                )?,
                _ => {
                    drop(raw);
                    recovery.skipped(
                        name,
                        Status::Skipped,
                        "Unsupported recovery compression method",
                    );
                }
            }
        }
    }
    recovery.limited |= archive.len() > MAX_FILES;
    Ok(())
}
