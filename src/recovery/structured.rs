//! Resynchronizes self-contained records using bounded native archive readers.

use super::*;

fn number(bytes: &[u8], radix: u32) -> Option<u64> {
    let text = std::str::from_utf8(bytes).ok()?.trim_matches(['\0', ' ']);
    (!text.is_empty())
        .then(|| u64::from_str_radix(text, radix).ok())
        .flatten()
}

pub(super) fn recover_compressed(
    path: &Path,
    source: &Source,
    salvage: bool,
) -> Result<Option<Recovery>> {
    use crate::archive::{CompressionFormat, ResolvedFormat};
    let Some(format) = ResolvedFormat::from_path(path) else {
        return Ok(None);
    };
    if format.compression == CompressionFormat::None
        || !(format.is_tar() || format.is_single_compressed())
    {
        return Ok(None);
    }
    let mut result = Recovery::new(path, salvage)?;
    let mut decoded = tempfile::Builder::new()
        .prefix("zarchive-repair-")
        .suffix(if format.is_tar() { ".tar" } else { ".data" })
        .tempfile()?;
    let mut decoder = match crate::archive::decode_archive_stream(source.reader(), format) {
        Ok(decoder) => decoder,
        Err(_) => {
            result.diagnose(None, Reason::Header, Some(0), None);
            return Ok(Some(result));
        }
    };
    let mut written = 0u64;
    let maximum = MAX_SCAN_BYTES.min(MAX_OUTPUT_BYTES);
    let mut buffer = vec![0u8; 65536];
    let mut failure = None;
    loop {
        crate::task_queue::checkpoint()?;
        if written >= maximum {
            failure = Some(Reason::Limit);
            break;
        }
        let length = (maximum - written).min(buffer.len() as u64) as usize;
        match decoder.read(&mut buffer[..length]) {
            Ok(0) => break,
            Ok(count) => {
                decoded.write_all(&buffer[..count])?;
                written += count as u64;
                if written > 10 * 1024 * 1024 && written / source.length.max(1) > 1000 {
                    failure = Some(Reason::Limit);
                    break;
                }
            }
            Err(error) => {
                failure = Some(if error.kind() == io::ErrorKind::UnexpectedEof {
                    Reason::Truncated
                } else {
                    Reason::Read
                });
                break;
            }
        }
    }
    crate::task_queue::checkpoint()?;
    decoded.flush()?;
    if format.is_tar() && written > 0 {
        let mut inner = Recovery::new(decoded.path(), salvage)?;
        let mut input = Source::open(decoded.path(), &mut inner)?;
        input.remaining = source.remaining.clone();
        input.scan_remaining = source.scan_remaining.clone();
        result = recover_source(decoded.path(), None, salvage, input, inner)?;
        result.source = path.to_owned();
        result.data_range_source = "decompressed_stream";
        result
            .notes
            .push("Recovered member ranges refer to the decompressed stream");
    } else if !format.is_tar() {
        let name = crate::archive::single_compressed_entry_name(path)
            .unwrap_or_else(|_| "recovered-stream.data".to_owned());
        if written == 0 && failure.is_some() {
            result.unavailable(name, failure.unwrap(), 0, source.length);
        } else {
            result.retain(
                name,
                File::open(decoded.path())?,
                Some(written),
                None,
                0,
                source.length,
            )?;
            if failure.is_some() && !result.entries.is_empty() {
                mark_partial(&mut result, 0, failure.unwrap());
            }
        }
    }
    if let Some(reason) = failure {
        result.limited |= reason == Reason::Limit;
        result.notes.push(
            "The compression wrapper is damaged or incomplete; only decoded bytes were analyzed",
        );
        result.diagnose(None, reason, None, None);
    }
    result.complete_diagnostics();
    Ok(Some(result))
}

pub(super) fn recover_xar(source: &Source, recovery: &mut Recovery) -> Result<bool> {
    let mut magic = [0u8; 4];
    let _ = source.reader().read_exact(&mut magic);
    if magic != *b"xar!"
        && crate::archive::ResolvedFormat::from_path(&recovery.source)
            != Some(crate::archive::ResolvedFormat::Xar)
    {
        return Ok(false);
    }
    let (mut archive, repaired) = match crate::xar::XarReader::for_recovery(source.reader()) {
        Ok(archive) => archive,
        Err(error) => {
            let reason = if matches!(
                error.downcast_ref::<crate::xar::ReadError>(),
                Some(crate::xar::ReadError::Limit)
            ) {
                Reason::Limit
            } else {
                Reason::Header
            };
            recovery.limited |= reason == Reason::Limit;
            recovery.diagnose(None, reason, Some(0), Some(28));
            return Ok(true);
        }
    };
    if repaired {
        recovery
            .notes
            .push("XAR header rebuilt from the validated contents table");
    }
    if archive.entries.len() > MAX_FILES {
        recovery.limited = true;
    }
    for entry in archive.entries.clone().into_iter().take(MAX_FILES) {
        crate::task_queue::checkpoint()?;
        if entry.file_type != xara::XarFileType::File || entry.link.is_some() {
            recovery.skipped(
                entry.path,
                Status::Skipped,
                "Directories and links are not recovered",
            );
            continue;
        }
        let expected = entry.data.as_ref().map_or(0, |data| data.size);
        let (offset, packed) = archive.entry_range(&entry).unwrap_or((0, 0));
        if expected > MAX_FILE_BYTES
            || expected > recovery.remaining
            || (expected > 10 * 1024 * 1024 && expected / packed.max(1) > 1000)
        {
            recovery.limited = true;
            recovery.unavailable(entry.path, Reason::Limit, offset, packed);
            continue;
        }
        let verified = entry
            .data
            .as_ref()
            .is_some_and(|data| data.extracted_checksum.is_some());
        let reader = match archive.recovery_entry_reader(&entry) {
            Ok(reader) => reader,
            Err(error) => {
                let reason = if error.downcast_ref::<crate::xar::ReadError>().is_some() {
                    Reason::Unsupported
                } else {
                    Reason::Header
                };
                recovery.unavailable(entry.path, reason, offset, packed);
                continue;
            }
        };
        let index = recovery.entries.len();
        recovery.retain(entry.path, reader, Some(expected), None, offset, packed)?;
        if verified
            && recovery
                .entries
                .get(index)
                .is_some_and(|entry| entry.status == Status::Unverified)
        {
            let entry = &mut recovery.entries[index];
            entry.status = Status::Recovered;
            if let Some(name) = &entry.output_name {
                entry.output_name = Some(name.replacen("unverified/", "recovered/", 1));
            }
            recovery.diagnostics.retain(|diagnostic| {
                !(diagnostic.entry == Some(index) && diagnostic.reason == Reason::Unverified)
            });
        }
    }
    Ok(true)
}

pub(super) fn recover_ar_record(
    source: &Source,
    offset: u64,
    recovery: &mut Recovery,
    names: &mut Option<Vec<u8>>,
) -> Result<()> {
    let mut input = source.reader();
    input.seek(SeekFrom::Start(offset))?;
    let mut header = [0u8; 60];
    if input.read_exact(&mut header).is_err() || &header[58..] != b"`\n" {
        return Ok(());
    }
    let Some(size) = number(&header[48..58], 10) else {
        return Ok(());
    };
    let Some(raw_name) = std::str::from_utf8(&header[..16])
        .ok()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return Ok(());
    };
    if raw_name == "//" {
        if size > MAX_INDEX_BYTES {
            recovery.limited = true;
            return Ok(());
        }
        let mut table = header.to_vec();
        let length = size
            .checked_add(size % 2)
            .context("Invalid AR filename table size")?;
        if input.take(length).read_to_end(&mut table).is_ok() && table.len() as u64 == 60 + length {
            *names = Some(table);
        }
        return Ok(());
    }
    if raw_name == "/" || raw_name == "/SYM64/" || number(&header[40..48], 8).is_none() {
        return Ok(());
    }
    if let Some(length) = raw_name.strip_prefix("#1/") {
        if length
            .parse::<u64>()
            .ok()
            .is_none_or(|length| length > 4096 || length > size)
        {
            return Ok(());
        }
    }
    if size > MAX_FILE_BYTES + 4096 || size > recovery.remaining.saturating_add(4096) {
        recovery.limited = true;
        recovery.unavailable(format!("member-at-{offset}"), Reason::Limit, offset, size);
        return Ok(());
    }
    if raw_name
        .strip_prefix('/')
        .is_some_and(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()))
        && names.is_none()
    {
        let name = format!("unnamed-member-at-{offset}");
        let index = recovery.entries.len();
        recovery.retain(name, input.take(size), Some(size), None, offset + 60, size)?;
        recovery
            .notes
            .push("AR filename table is missing; a generated name was used");
        recovery.diagnose(Some(index), Reason::Header, Some(offset), Some(60));
        return Ok(());
    }
    let mut prefix = b"!<arch>\n".to_vec();
    if let Some(table) = names {
        prefix.extend_from_slice(table);
    }
    let mut input = source.reader();
    input.seek(SeekFrom::Start(offset))?;
    let mut archive = match libarchive2::ReadArchive::open_callback(
        libarchive2::CallbackReader::new(io::Cursor::new(prefix).chain(input)),
    ) {
        Ok(archive) => archive,
        Err(_) => return Ok(()),
    };
    let entry = match archive.next_entry() {
        Ok(Some(entry)) => entry,
        _ => return Ok(()),
    };
    let name = entry
        .pathname()
        .unwrap_or_else(|| format!("member-at-{offset}"));
    let size = u64::try_from(entry.size()).ok();
    let regular = entry.file_type() == libarchive2::FileType::RegularFile
        && entry.symlink().is_none()
        && entry.hardlink().is_none();
    drop(entry);
    if recovery
        .entries
        .iter()
        .any(|entry| entry.name == name && entry.file.is_some())
    {
        return Ok(());
    }
    if regular {
        recovery.retain(
            name,
            crate::archive::ExtendedEntryReader(&mut archive),
            size,
            None,
            offset + 60,
            size.unwrap_or(0),
        )?;
    } else {
        recovery.skipped(
            name,
            Status::Skipped,
            "Directories and links are not recovered",
        );
    }
    Ok(())
}

pub(super) fn recover_cpio_record(
    source: &Source,
    offset: u64,
    recovery: &mut Recovery,
) -> Result<()> {
    let mut reader = source.reader();
    reader.seek(SeekFrom::Start(offset))?;
    let mut magic = [0u8; 6];
    if reader.read_exact(&mut magic).is_err() {
        return Ok(());
    }
    let (header_size, name_field, size_field, radix) = match &magic {
        b"070701" | b"070702" => (110usize, 94..102, 54..62, 16),
        b"070707" => (76usize, 59..65, 65..76, 8),
        _ => return Ok(()),
    };
    let mut header = vec![0u8; header_size];
    header[..6].copy_from_slice(&magic);
    if reader.read_exact(&mut header[6..]).is_err() {
        return Ok(());
    }
    let Some(name_size) =
        number(&header[name_field], radix).filter(|size| *size > 0 && *size <= 4096)
    else {
        return Ok(());
    };
    let Some(size) = number(&header[size_field], radix) else {
        return Ok(());
    };
    let mut name = vec![0u8; name_size as usize];
    if reader.read_exact(&mut name).is_err() || name.last() != Some(&0) {
        return Ok(());
    }
    let name = String::from_utf8_lossy(&name[..name.len() - 1]).into_owned();
    if name == "TRAILER!!!" || name.is_empty() || name.chars().any(char::is_control) {
        return Ok(());
    }
    let checksum = if magic == *b"070702" {
        number(&header[102..110], 16).map(|crc| crc as u32)
    } else {
        None
    };
    if let Some(index) = recovery
        .entries
        .iter()
        .position(|entry| entry.name == name && entry.file.is_some())
    {
        if let Some(expected) = checksum {
            verify_cpio_sum(recovery, index, expected, size)?;
        }
        return Ok(());
    }
    if size > MAX_FILE_BYTES || size > recovery.remaining {
        recovery.limited = true;
        recovery.unavailable(name, Reason::Limit, offset, size);
        return Ok(());
    }
    let mut input = source.reader();
    input.seek(SeekFrom::Start(offset))?;
    let mut archive =
        match libarchive2::ReadArchive::open_callback(libarchive2::CallbackReader::new(input)) {
            Ok(archive) => archive,
            Err(_) => {
                recovery.unavailable(name, Reason::Header, offset, header_size as u64);
                return Ok(());
            }
        };
    let entry = match archive.next_entry() {
        Ok(Some(entry)) => entry,
        _ => {
            recovery.unavailable(name, Reason::Header, offset, header_size as u64);
            return Ok(());
        }
    };
    let regular = entry.file_type() == libarchive2::FileType::RegularFile
        && entry.symlink().is_none()
        && entry.hardlink().is_none();
    drop(entry);
    if !regular {
        recovery.skipped(
            name,
            Status::Skipped,
            "Directories and links are not recovered",
        );
        return Ok(());
    }
    let index = recovery.entries.len();
    recovery.retain(
        name,
        crate::archive::ExtendedEntryReader(&mut archive),
        Some(size),
        None,
        offset,
        size,
    )?;
    if let Some(expected) = checksum {
        verify_cpio_sum(recovery, index, expected, size)?;
    }
    Ok(())
}

fn verify_cpio_sum(recovery: &mut Recovery, index: usize, expected: u32, size: u64) -> Result<()> {
    if let Some(path) = recovery
        .entries
        .get(index)
        .and_then(|entry| entry.file.clone())
    {
        let mut file = File::open(path)?;
        let mut sum = 0u32;
        let mut buffer = vec![0u8; 65536];
        loop {
            crate::task_queue::checkpoint()?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            sum = buffer[..count]
                .iter()
                .fold(sum, |sum, byte| sum.wrapping_add(u32::from(*byte)));
        }
        if sum == expected && recovery.entries[index].bytes == size {
            let entry = &mut recovery.entries[index];
            entry.status = Status::Recovered;
            entry.issue = None;
            if let Some(name) = &entry.output_name {
                entry.output_name = Some(name.replacen("unverified/", "recovered/", 1).replacen(
                    "partial/",
                    "recovered/",
                    1,
                ));
            }
            recovery
                .diagnostics
                .retain(|diagnostic| diagnostic.entry != Some(index));
        } else if sum != expected {
            mark_partial(recovery, index, Reason::Checksum);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_repair_cpio_resynchronizes_after_destroyed_middle_header() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = Vec::new();
        for name in ["first.txt", "middle.txt", "last.txt"] {
            let path = root.path().join(name);
            std::fs::write(&path, name.as_bytes()).unwrap();
            sources.push(path);
        }
        let path = root.path().join("damaged.cpio");
        crate::archive::create_archive(
            &path,
            &sources,
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::Cpio,
                compression: crate::archive::CompressionLevel::Stored,
                ..Default::default()
            },
        )
        .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let magic = bytes[..6].to_vec();
        let offsets = memchr::memmem::find_iter(&bytes, &magic).collect::<Vec<_>>();
        assert!(offsets.len() >= 3);
        bytes[offsets[1]..offsets[1] + 6].copy_from_slice(b"BROKEN");
        std::fs::write(&path, &bytes).unwrap();
        let result = recover(&path, None, true).unwrap();
        for name in ["first.txt", "last.txt"] {
            let entry = result
                .entries
                .iter()
                .find(|entry| entry.name == name && entry.file.is_some())
                .unwrap();
            assert_eq!(
                std::fs::read(entry.file.as_ref().unwrap()).unwrap(),
                name.as_bytes()
            );
        }
        result
            .export_tar(&root.path().join("repaired.tar"))
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn complete_repair_ar_resynchronizes_after_destroyed_member_header() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = Vec::new();
        for name in ["first.txt", "middle.txt", "last-long-member-name.txt"] {
            let path = root.path().join(name);
            std::fs::write(&path, name.as_bytes()).unwrap();
            sources.push(path);
        }
        let path = root.path().join("damaged.ar");
        crate::archive::create_archive(
            &path,
            &sources,
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::Ar,
                compression: crate::archive::CompressionLevel::Stored,
                ..Default::default()
            },
        )
        .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let offsets = memchr::memmem::find_iter(&bytes, b"`\n")
            .map(|offset| offset - 58)
            .collect::<Vec<_>>();
        let middle = offsets
            .iter()
            .copied()
            .find(|offset| {
                bytes
                    .get(*offset..*offset + 16)
                    .is_some_and(|name| String::from_utf8_lossy(name).starts_with("middle.txt"))
            })
            .unwrap();
        bytes[middle..middle + 60].fill(b'Z');
        std::fs::write(&path, &bytes).unwrap();
        let result = recover(&path, None, true).unwrap();
        for name in ["first.txt", "last-long-member-name.txt"] {
            let entry = result
                .entries
                .iter()
                .find(|entry| entry.name == name && entry.file.is_some())
                .unwrap();
            assert_eq!(
                std::fs::read(entry.file.as_ref().unwrap()).unwrap(),
                name.as_bytes()
            );
        }
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn complete_repair_xar_rebuilds_lengths_and_reads_independent_entries() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = Vec::new();
        for name in ["first.txt", "middle.txt", "last.txt"] {
            let path = root.path().join(name);
            std::fs::write(&path, name.as_bytes().repeat(100)).unwrap();
            sources.push(path);
        }
        let path = root.path().join("damaged.xar");
        crate::archive::create_archive(
            &path,
            &sources,
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::Xar,
                ..Default::default()
            },
        )
        .unwrap();
        let archive = crate::xar::XarReader::new(File::open(&path).unwrap()).unwrap();
        let entry = archive
            .entries
            .iter()
            .find(|entry| entry.path == "middle.txt")
            .unwrap();
        let (offset, length) = archive.entry_range(entry).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[offset as usize + (length / 2) as usize] ^= 0x5a;
        bytes[8..24].fill(0xff);
        std::fs::write(&path, &bytes).unwrap();
        let result = recover(&path, None, true).unwrap();
        for name in ["first.txt", "last.txt"] {
            let entry = result
                .entries
                .iter()
                .find(|entry| entry.name == name && entry.file.is_some())
                .unwrap();
            assert_eq!(
                std::fs::read(entry.file.as_ref().unwrap()).unwrap(),
                name.as_bytes().repeat(100)
            );
        }
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.name == "middle.txt" && entry.status != Status::Recovered)
        );
        assert!(
            result
                .notes
                .iter()
                .any(|note| note.starts_with("XAR header rebuilt"))
        );
        result.export_zip(&root.path().join("fixed.zip")).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn complete_repair_compressed_tar_recovers_headers_and_payloads_for_every_codec() {
        use crate::archive::{CompressionFormat, CreateOptions, ResolvedFormat};
        let root = tempfile::tempdir().unwrap();
        for compression in CompressionFormat::ALL
            .into_iter()
            .filter(|codec| *codec != CompressionFormat::None)
        {
            let source = root.path().join("notes.txt");
            std::fs::write(&source, b"compressed TAR recovery").unwrap();
            let format = ResolvedFormat::tar(compression);
            let path = root.path().join(format!("test.{}", format.extension()));
            crate::archive::create_archive(
                &path,
                std::slice::from_ref(&source),
                CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    ..Default::default()
                },
            )
            .unwrap();
            let result = recover(&path, None, true).unwrap();
            let entry = result
                .entries
                .iter()
                .find(|entry| entry.name == "notes.txt" && entry.file.is_some())
                .unwrap();
            assert_eq!(
                std::fs::read(entry.file.as_ref().unwrap()).unwrap(),
                b"compressed TAR recovery"
            );
            assert_eq!(result.data_range_source, "decompressed_stream");
        }
    }

    #[test]
    fn complete_repair_truncated_gzip_retains_available_tar_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("truncated.tar.gz");
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in [
            ("first.txt", b"first payload".as_slice()),
            ("last.txt", b"last payload"),
        ] {
            let mut header = tar::Header::new_ustar();
            header.set_mode(0o644);
            header.set_size(data.len() as u64);
            header.set_cksum();
            builder.append_data(&mut header, name, data).unwrap();
        }
        let decoded = builder.into_inner().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&decoded).unwrap();
        let mut bytes = encoder.finish().unwrap();
        bytes.truncate(bytes.len() - 8);
        std::fs::write(&path, &bytes).unwrap();
        let result = recover(&path, None, true).unwrap();
        for name in ["first.txt", "last.txt"] {
            assert!(
                result
                    .entries
                    .iter()
                    .any(|entry| entry.name == name && entry.file.is_some())
            );
        }
        assert!(
            result
                .notes
                .iter()
                .any(|note| note.starts_with("The compression wrapper is damaged"))
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn complete_repair_roundtrips_all_archive_writers_without_changing_original() {
        use crate::archive::{self, ResolvedFormat};
        use crate::inspection::{InspectionMode, Integrity, inspect};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        std::fs::write(&source, b"all-format repaired output").unwrap();
        let original = root.path().join("source.zip");
        archive::create_zip(&original, &[source]).unwrap();
        let before = std::fs::read(&original).unwrap();
        let result = recover(&original, None, true).unwrap();
        for format in ResolvedFormat::ALL
            .into_iter()
            .filter(|format| format.archive.is_some())
        {
            let destination = root.path().join(format!("fixed.{}", format.extension()));
            result.export_format(&destination, format).unwrap();
            let report = inspect(
                &destination,
                None,
                InspectionMode::Integrity,
                &std::sync::atomic::AtomicBool::new(false),
            );
            assert_eq!(
                report.integrity,
                Integrity::Passed,
                "{format:?}: {:?}",
                report.error
            );
            let entries = archive::list_archive(&destination).unwrap();
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.name == "recovery-report.json")
            );
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.name.ends_with("notes.txt"))
            );
            assert!(result.export_format(&destination, format).is_err());
        }
        assert!(
            result
                .export_format(&original, ResolvedFormat::Zip)
                .is_err()
        );
        assert_eq!(std::fs::read(original).unwrap(), before);
    }

    #[test]
    fn complete_repair_recovers_every_standalone_compression_stream() {
        use crate::archive::{self, CreateOptions, ResolvedFormat};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let data = b"standalone stream recovery".repeat(16);
        std::fs::write(&source, &data).unwrap();
        for format in ResolvedFormat::ALL
            .into_iter()
            .filter(|format| format.is_single_compressed())
        {
            let path = root
                .path()
                .join(format!("notes.txt.{}", format.extension()));
            archive::create_archive(
                &path,
                std::slice::from_ref(&source),
                CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    ..Default::default()
                },
            )
            .unwrap();
            let before = std::fs::read(&path).unwrap();
            let result = recover(&path, None, true).unwrap();
            let entry = result
                .entries
                .iter()
                .find(|entry| entry.file.is_some())
                .unwrap();
            assert_eq!(entry.name, "notes.txt");
            assert_eq!(std::fs::read(entry.file.as_ref().unwrap()).unwrap(), data);
            assert_eq!(std::fs::read(path).unwrap(), before);
        }
    }

    #[test]
    fn complete_repair_does_not_claim_an_empty_file_from_unreadable_compression() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("notes.txt.gz");
        std::fs::write(&path, b"not a gzip stream").unwrap();
        let result = recover(&path, None, true).unwrap();
        assert!(result.entries.iter().all(|entry| entry.file.is_none()));
        assert!(!result.diagnostics.is_empty());
    }

    #[gpui::test]
    fn complete_repair_original_format_export_is_available_and_keeps_footer_visible(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("notes.txt");
        std::fs::write(&data, b"native format recovery UI").unwrap();
        let path = root.path().join("source.cpio");
        let format = crate::archive::ResolvedFormat::Cpio;
        crate::archive::create_archive(
            &path,
            &[data],
            crate::archive::CreateOptions {
                format,
                compression: crate::archive::CompressionLevel::Stored,
                ..Default::default()
            },
        )
        .unwrap();
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.language = crate::i18n::Language::Polish;
            manager.recovery.source = Some(path.clone());
            manager.show_modal(crate::Modal::Recovery, window, cx);
            manager
        });
        view.update(cx, |manager, cx| manager.start_recovery(cx));
        super::super::tests::complete(cx, &view);
        assert!(cx.debug_bounds("export-recovery").is_some());
        assert!(cx.debug_bounds("save-recovered-zip").is_none());
        assert!(cx.debug_bounds("save-recovered-tar").is_none());
        assert!(cx.debug_bounds("save-recovery-report").is_none());
        let output = root.path().join("fixed.cpio");
        view.update(cx, |manager, cx| {
            manager.export_recovered_archive(
                manager.recovery.result.clone().unwrap(),
                output.clone(),
                cx,
            )
        });
        super::super::tests::complete(cx, &view);
        assert!(
            crate::archive::list_archive(&output)
                .unwrap()
                .iter()
                .any(|entry| entry.name == "recovery-report.json")
        );
        for viewport in [
            gpui::size(px(1100.0), px(720.0)),
            gpui::size(px(560.0), px(440.0)),
        ] {
            cx.simulate_resize(viewport);
            cx.run_until_parked();
            let panel = cx.debug_bounds("dialog-0").unwrap();
            let footer = cx.debug_bounds("recovery-actions").unwrap();
            assert!(footer.bottom() <= panel.bottom());
            assert!(panel.bottom() <= viewport.height);
        }
    }

    #[test]
    fn complete_repair_cpio_crc_verifies_and_rejects_damaged_content() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("crc.cpio");
        let name = b"notes.txt\0";
        let data = b"CRC CPIO payload";
        let sum = data.iter().map(|byte| u32::from(*byte)).sum::<u32>();
        let fields = [
            1u32,
            0o100600,
            0,
            0,
            1,
            0,
            data.len() as u32,
            0,
            0,
            0,
            0,
            name.len() as u32,
            sum,
        ];
        let mut bytes = b"070702".to_vec();
        for field in fields {
            bytes.extend_from_slice(format!("{field:08x}").as_bytes());
        }
        bytes.extend_from_slice(name);
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        let offset = bytes.len();
        bytes.extend_from_slice(data);
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        std::fs::write(&path, &bytes).unwrap();
        let result = recover(&path, None, true).unwrap();
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.name == "notes.txt" && entry.status == Status::Recovered)
        );
        bytes[offset] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let result = recover(&path, None, true).unwrap();
        assert!(
            !result
                .entries
                .iter()
                .any(|entry| entry.name == "notes.txt" && entry.status == Status::Recovered)
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.reason == Reason::Checksum)
        );
    }

    #[test]
    fn complete_repair_xar_keeps_decoded_bytes_when_declared_file_size_is_wrong() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let payload = b"XAR contents with wrong declared length";
        std::fs::write(&source, payload).unwrap();
        let path = root.path().join("wrong-size.xar");
        crate::archive::create_archive(
            &path,
            &[source],
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::Xar,
                ..Default::default()
            },
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        let header = xara::header::parse_header(&mut io::Cursor::new(&original)).unwrap();
        let heap = header.header_size as usize + header.toc_compressed_len as usize;
        let mut xml = String::new();
        flate2::read::ZlibDecoder::new(&original[header.header_size as usize..heap])
            .read_to_string(&mut xml)
            .unwrap();
        let document = roxmltree::Document::parse(&xml).unwrap();
        let data = document
            .descendants()
            .find(|node| node.has_tag_name("data"))
            .unwrap();
        let size = data
            .children()
            .find(|node| node.has_tag_name("size"))
            .unwrap()
            .first_child()
            .unwrap()
            .range();
        drop(document);
        xml.replace_range(size, "1");
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(xml.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut damaged = original[..header.header_size as usize].to_vec();
        damaged[8..16].copy_from_slice(&(compressed.len() as u64).to_be_bytes());
        damaged[16..24].copy_from_slice(&(xml.len() as u64).to_be_bytes());
        damaged.extend_from_slice(&compressed);
        damaged.extend_from_slice(&original[heap..]);
        std::fs::write(&path, &damaged).unwrap();
        let result = recover(&path, None, true).unwrap();
        let index = result
            .entries
            .iter()
            .position(|entry| entry.name == "notes.txt")
            .unwrap();
        assert_eq!(result.entries[index].status, Status::Partial);
        assert_eq!(
            std::fs::read(result.entries[index].file.as_ref().unwrap()).unwrap(),
            payload
        );
        assert!(result.diagnostics.iter().any(
            |diagnostic| diagnostic.entry == Some(index) && diagnostic.reason == Reason::Header
        ));
        assert_eq!(std::fs::read(path).unwrap(), damaged);
    }
}
