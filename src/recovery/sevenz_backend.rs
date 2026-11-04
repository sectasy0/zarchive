use super::*;

pub(super) fn recover_seven_zip(
    source: &Source,
    password: Option<&str>,
    recovery: &mut Recovery,
) -> Result<bool> {
    let mut signature = [0u8; 6];
    let _ = source.reader().read_exact(&mut signature);
    if signature != *b"7z\xbc\xaf\x27\x1c"
        && crate::archive::ResolvedFormat::from_path(&recovery.source)
            != Some(crate::archive::ResolvedFormat::SevenZip)
    {
        return Ok(false);
    }
    let password_bytes = zeroize::Zeroizing::new(
        password
            .map(sevenz_rust::Password::from)
            .unwrap_or_default()
            .to_vec(),
    );
    let mut start_header = [0u8; 32];
    if source.reader().read_exact(&mut start_header).is_ok()
        && u64::from_le_bytes(start_header[20..28].try_into().unwrap()) > MAX_INDEX_BYTES
    {
        recovery.limited = true;
        recovery.diagnose(None, Reason::Limit, Some(20), Some(8));
        return Ok(true);
    }
    let working = repair_seven_zip_start_header(source, recovery)?;
    working
        .metadata_remaining
        .store(MAX_INDEX_BYTES * 4, Ordering::Relaxed);
    let parsed = sevenz_rust::Archive::read(&mut working.reader(), working.length, &password_bytes);
    let metadata_limited = working.metadata_remaining.load(Ordering::Relaxed) == 0;
    working
        .metadata_remaining
        .store(u64::MAX, Ordering::Relaxed);
    let mut archive = match parsed {
        Ok(archive) => archive,
        Err(error) => {
            let reason = if metadata_limited {
                Reason::Limit
            } else {
                match seven_zip_reason(&error) {
                    Reason::Checksum => Reason::Header,
                    reason => reason,
                }
            };
            recovery.limited |= reason == Reason::Limit;
            recovery.blocks.push(Block {
                offset: 0,
                length: source.length.min(32),
                issue: reason.label(),
            });
            recovery.diagnose(None, reason, Some(0), Some(source.length.min(32)));
            return Ok(true);
        }
    };
    if archive.files.len() > MAX_FILES || archive.folders.len() > MAX_FILES {
        recovery.limited = true;
        recovery.diagnose(None, Reason::Limit, None, None);
        return Ok(true);
    }
    for folder_index in 0..archive.folders.len() {
        crate::task_queue::checkpoint()?;
        let Some(&start) = archive.stream_map.folder_first_file_index.get(folder_index) else {
            recovery.diagnose(None, Reason::Header, None, None);
            break;
        };
        let Some(end) = start
            .checked_add(archive.folders[folder_index].num_unpack_sub_streams)
            .filter(|end| *end <= archive.files.len())
        else {
            recovery.diagnose(None, Reason::Header, None, None);
            break;
        };
        let metadata = archive.files[start..end].to_vec();
        let Some((offset, packed)) = seven_zip_range(&archive, folder_index) else {
            for entry in metadata {
                recovery.unavailable(entry.name, Reason::Header, 0, 0);
            }
            continue;
        };
        let oversized_dictionary = archive.folders[folder_index].coders.iter().any(|coder| {
            let dictionary = match coder.decompression_method_id() {
                [0x21] => coder.properties.first().map(|property| {
                    if *property >= 40 {
                        u64::MAX
                    } else {
                        (2u64 | (*property as u64 & 1)) << (*property as u32 / 2 + 11)
                    }
                }),
                [3, 1, 1] if coder.properties.len() >= 5 => {
                    Some(u32::from_le_bytes(coder.properties[1..5].try_into().unwrap()) as u64)
                }
                _ => None,
            };
            dictionary.is_some_and(|size| size > MAX_FILE_BYTES)
        });
        let expanded = metadata
            .iter()
            .try_fold(0u64, |total, entry| total.checked_add(entry.size));
        let encrypted = archive.folders[folder_index]
            .coders
            .iter()
            .any(|coder| coder.decompression_method_id() == [0x06, 0xf1, 0x07, 0x01]);
        let rejection = if oversized_dictionary
            || expanded.is_none_or(|size| {
                size > recovery.remaining
                    || (size > 10 * 1024 * 1024 && size / packed.max(1) > 1000)
            })
            || metadata.iter().any(|entry| entry.size > MAX_FILE_BYTES)
        {
            Some(Reason::Limit)
        } else if encrypted && password_bytes.is_empty() {
            Some(Reason::Password)
        } else {
            None
        };
        if let Some(reason) = rejection {
            recovery.limited |= reason == Reason::Limit;
            for entry in metadata {
                recovery.unavailable(entry.name, reason, offset, packed);
            }
            continue;
        }
        let expected_block_crc = archive.folders[folder_index]
            .has_crc
            .then_some(archive.folders[folder_index].crc as u32);
        archive.folders[folder_index].has_crc = false;
        for file in &mut archive.files[start..end] {
            file.has_crc = false;
        }
        let mut reader = working.reader();
        let decoder =
            sevenz_rust::BlockDecoder::new(folder_index, &archive, &password_bytes, &mut reader);
        let mut visited = 0usize;
        let first_output = recovery.entries.len();
        let mut failure = None;
        let mut block_hash = crc32fast::Hasher::new();
        let result = decoder.for_each_entries(&mut |_entry, reader| {
            let entry = &metadata[visited];
            visited += 1;
            if entry.is_directory() || entry.is_anti_item() {
                recovery.skipped(
                    entry.name.clone(),
                    Status::Skipped,
                    "Directories and links are not recovered",
                );
                return Ok(true);
            }
            if let Err(error) = recovery.retain(
                entry.name.clone(),
                BlockHashReader {
                    inner: reader,
                    hash: &mut block_hash,
                },
                Some(entry.size),
                entry.has_crc.then_some(entry.crc as u32),
                offset,
                packed,
            ) {
                failure = Some(error);
                return Err(sevenz_rust::Error::other("Recovery failed"));
            }
            Ok(recovery.entries.len() < MAX_FILES
                && !recovery.entries.last().is_some_and(|entry| {
                    matches!(
                        entry.status,
                        Status::Partial | Status::NotRecovered | Status::Skipped
                    )
                }))
        });
        if let Some(error) = failure {
            return Err(error);
        }
        crate::task_queue::checkpoint()?;
        if encrypted {
            for diagnostic in recovery
                .diagnostics
                .iter_mut()
                .filter(|diagnostic| diagnostic.entry.is_some_and(|index| index >= first_output))
            {
                if matches!(
                    diagnostic.reason,
                    Reason::Checksum | Reason::Read | Reason::Truncated
                ) {
                    diagnostic.reason = Reason::EncryptedData;
                }
            }
        }
        let reason = match &result {
            Err(error) => Some(seven_zip_reason(error)),
            Ok(false) => Some(if recovery.limited {
                Reason::Limit
            } else {
                Reason::SolidBlock
            }),
            _ if expected_block_crc.is_some_and(|crc| crc != block_hash.finalize()) => {
                Some(Reason::Checksum)
            }
            _ => None,
        };
        if let Some(reason) = reason {
            let reason = if source.segments.iter().any(|segment| {
                segment.path.is_none()
                    && segment.start < offset.saturating_add(packed)
                    && segment.start + segment.size > offset
            }) {
                Reason::MissingVolume
            } else {
                reason
            };
            let reason = if encrypted
                && matches!(reason, Reason::Checksum | Reason::Read | Reason::Truncated)
            {
                Reason::EncryptedData
            } else {
                reason
            };
            recovery.blocks.push(Block {
                offset,
                length: packed,
                issue: reason.label(),
            });
            recovery.diagnose(None, reason, Some(offset), Some(packed));
            for entry in metadata.iter().skip(visited) {
                recovery.unavailable(
                    entry.name.clone(),
                    if metadata.len() > 1 {
                        Reason::SolidBlock
                    } else {
                        reason
                    },
                    offset,
                    packed,
                );
            }
            for index in first_output..recovery.entries.len() {
                if recovery.entries[index].status == Status::Unverified
                    && expected_block_crc.is_some()
                {
                    mark_partial(recovery, index, reason);
                }
            }
        }
    }
    for (index, entry) in archive.files.iter().enumerate() {
        if archive
            .stream_map
            .file_folder_index
            .get(index)
            .is_some_and(Option::is_none)
        {
            if entry.is_directory() {
                recovery.skipped(
                    entry.name.clone(),
                    Status::Skipped,
                    "Directories and links are not recovered",
                );
            } else {
                recovery.retain(
                    entry.name.clone(),
                    io::empty(),
                    Some(0),
                    entry.has_crc.then_some(entry.crc as u32),
                    0,
                    0,
                )?;
            }
        }
    }
    Ok(true)
}

pub(super) fn repair_seven_zip_start_header(
    source: &Source,
    recovery: &mut Recovery,
) -> Result<Source> {
    let mut working = source.clone();
    let mut header = [0u8; 32];
    if source.reader().read_exact(&mut header).is_err()
        || !header.starts_with(b"7z\xbc\xaf\x27\x1c")
    {
        return Ok(working);
    }
    let crc = crc32fast::hash(&header[12..]);
    if u32::from_le_bytes(header[8..12].try_into().unwrap()) == crc {
        return Ok(working);
    }
    let position = u64::from_le_bytes(header[12..20].try_into().unwrap()).checked_add(32);
    let length = u64::from_le_bytes(header[20..28].try_into().unwrap());
    let Some(position) = position.filter(|position| {
        length <= MAX_INDEX_BYTES
            && position
                .checked_add(length)
                .is_some_and(|end| end <= source.length)
    }) else {
        return Ok(working);
    };
    let mut reader = source.reader();
    reader.seek(SeekFrom::Start(position))?;
    let mut remaining = length;
    let mut hash = crc32fast::Hasher::new();
    let mut buffer = vec![0u8; 65536];
    while remaining > 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        if reader.read_exact(&mut buffer[..count]).is_err() {
            return Ok(working);
        }
        hash.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if hash.finalize() != u32::from_le_bytes(header[28..32].try_into().unwrap()) {
        return Ok(working);
    }
    header[8..12].copy_from_slice(&crc.to_le_bytes());
    working.header_patch = Some(Arc::new(header));
    recovery
        .notes
        .push("7z start header checksum reconstructed; next header verified");
    recovery.blocks.push(Block {
        offset: 8,
        length: 4,
        issue: "7z start header checksum reconstructed; next header verified",
    });
    Ok(working)
}

struct BlockHashReader<'a> {
    inner: &'a mut dyn Read,
    hash: &'a mut crc32fast::Hasher,
}

impl Read for BlockHashReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(bytes)?;
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
}

pub(super) fn seven_zip_range(archive: &sevenz_rust::Archive, block: usize) -> Option<(u64, u64)> {
    let first = *archive
        .stream_map
        .folder_first_pack_stream_index
        .get(block)?;
    let offset = 32u64
        .checked_add(archive.pack_pos)?
        .checked_add(*archive.stream_map.pack_stream_offsets.get(first)?)?;
    let end = first.checked_add(archive.folders.get(block)?.packed_streams.len())?;
    let length = archive
        .pack_sizes
        .get(first..end)?
        .iter()
        .try_fold(0u64, |total, size| total.checked_add(*size))?;
    Some((offset, length))
}

pub(super) fn seven_zip_reason(error: &sevenz_rust::Error) -> Reason {
    match error {
        sevenz_rust::Error::PasswordRequired => Reason::Password,
        sevenz_rust::Error::MaybeBadPassword(_) => Reason::EncryptedData,
        sevenz_rust::Error::ChecksumVerificationFailed => Reason::Checksum,
        sevenz_rust::Error::MaxMemLimited { .. } => Reason::Limit,
        sevenz_rust::Error::UnsupportedCompressionMethod(_)
        | sevenz_rust::Error::Unsupported(_)
        | sevenz_rust::Error::ExternalUnsupported
        | sevenz_rust::Error::UnsupportedVersion { .. } => Reason::Unsupported,
        sevenz_rust::Error::Io(error, _) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Reason::Truncated
        }
        _ => Reason::Header,
    }
}
