use anyhow::{Context, Result};
use sha2::digest::DynDigest;
use std::io::{self, Read, Seek, SeekFrom, Write};

pub(crate) fn write_empty(writer: &mut impl Write) -> Result<()> {
    let toc = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xar><toc></toc></xar>\n";
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(toc)?;
    let compressed = encoder.finish()?;
    writer.write_all(&xara::header::XAR_MAGIC.to_be_bytes())?;
    writer.write_all(&28_u16.to_be_bytes())?;
    writer.write_all(&1_u16.to_be_bytes())?;
    writer.write_all(&(compressed.len() as u64).to_be_bytes())?;
    writer.write_all(&(toc.len() as u64).to_be_bytes())?;
    writer.write_all(&0_u32.to_be_bytes())?;
    writer.write_all(&compressed)?;
    Ok(())
}

#[derive(Debug)]
pub(crate) enum ReadError {
    Limit,
    Unsupported(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Limit => {
                formatter.write_str("XAR table of contents exceeds the supported size limit")
            }
            Self::Unsupported(method) => {
                write!(formatter, "Unsupported XAR compression method: {method}")
            }
        }
    }
}

impl std::error::Error for ReadError {}

#[derive(Debug)]
pub(crate) enum EntryError {
    Size,
    Checksum,
}

impl std::fmt::Display for EntryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Size => "XAR entry size mismatch",
            Self::Checksum => "XAR entry checksum mismatch",
        })
    }
}

impl std::error::Error for EntryError {}

pub(crate) struct XarReader<R> {
    reader: R,
    pub(crate) entries: Vec<xara::XarFile>,
    heap_offset: u64,
    archive_size: u64,
}

impl<R: Read + Seek> XarReader<R> {
    pub(crate) fn for_recovery(mut reader: R) -> Result<(Self, bool)> {
        const LIMIT: u64 = 16 * 1024 * 1024;
        let archive_size = reader.seek(SeekFrom::End(0))?;
        reader.rewind()?;
        let mut bytes = [0u8; 28];
        reader.read_exact(&mut bytes)?;
        let declared_offset = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as u64;
        for offset in [declared_offset, 28] {
            if !(28..=4096).contains(&offset) || offset >= archive_size {
                continue;
            }
            reader.seek(SeekFrom::Start(offset))?;
            let mut decoder = flate2::read::ZlibDecoder::new(reader.by_ref().take(LIMIT));
            let mut xml = Vec::new();
            let result = decoder.by_ref().take(LIMIT + 1).read_to_end(&mut xml);
            if xml.len() as u64 > LIMIT || (result.is_err() && decoder.total_in() >= LIMIT) {
                return Err(ReadError::Limit.into());
            }
            if result.is_err() {
                continue;
            }
            let compressed = decoder.total_in();
            drop(decoder);
            let xml_length = xml.len() as u64;
            drop(xml);
            let header = xara::header::XarHeader {
                magic: xara::header::XAR_MAGIC,
                header_size: offset as u16,
                version: 1,
                toc_compressed_len: compressed,
                toc_uncompressed_len: xml_length,
                checksum_algo: xara::header::ChecksumAlgo::None,
            };
            reader.seek(SeekFrom::Start(offset))?;
            let (entries, _) = match xara::toc::parse_toc(&mut reader, &header) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            let heap_offset = offset
                .checked_add(compressed)
                .context("Invalid XAR heap offset")?;
            anyhow::ensure!(heap_offset <= archive_size, "Invalid XAR heap offset");
            let repaired = bytes[..4] != xara::header::XAR_MAGIC.to_be_bytes()
                || offset != declared_offset
                || u16::from_be_bytes(bytes[6..8].try_into().unwrap()) != 1
                || compressed != u64::from_be_bytes(bytes[8..16].try_into().unwrap())
                || xml_length != u64::from_be_bytes(bytes[16..24].try_into().unwrap());
            return Ok((
                Self {
                    reader,
                    entries,
                    heap_offset,
                    archive_size,
                },
                repaired,
            ));
        }
        anyhow::bail!("XAR contents table could not be reconstructed");
    }

    pub(crate) fn new(mut reader: R) -> Result<Self> {
        let archive_size = reader.seek(SeekFrom::End(0))?;
        reader.rewind()?;
        let header = xara::header::parse_header(&mut reader)?;
        anyhow::ensure!(
            header.version == 1 && header.header_size >= 28,
            "Invalid XAR header"
        );
        if header.toc_compressed_len > 16 * 1024 * 1024
            || header.toc_uncompressed_len > 16 * 1024 * 1024
        {
            return Err(ReadError::Limit.into());
        }
        let (entries, heap_offset) = xara::toc::parse_toc(&mut reader, &header)?;
        anyhow::ensure!(heap_offset <= archive_size, "Invalid XAR heap offset");
        Ok(Self {
            reader,
            entries,
            heap_offset,
            archive_size,
        })
    }

    pub(crate) fn entry_reader(&mut self, entry: &xara::XarFile) -> Result<XarEntryReader<'_>> {
        self.entry_reader_inner(entry, false)
    }

    pub(crate) fn recovery_entry_reader(
        &mut self,
        entry: &xara::XarFile,
    ) -> Result<XarEntryReader<'_>> {
        self.entry_reader_inner(entry, true)
    }

    pub(crate) fn entry_range(&self, entry: &xara::XarFile) -> Option<(u64, u64)> {
        let data = entry.data.as_ref()?;
        Some((self.heap_offset.checked_add(data.offset)?, data.length))
    }

    fn entry_reader_inner(
        &mut self,
        entry: &xara::XarFile,
        partial: bool,
    ) -> Result<XarEntryReader<'_>> {
        anyhow::ensure!(
            entry.file_type == xara::XarFileType::File,
            "Only regular files are opened"
        );
        let Some(data) = &entry.data else {
            return Ok(XarEntryReader {
                reader: Box::new(io::empty()),
                expected_size: 0,
                read: 0,
                checksum: None,
                finished: false,
                recovering: partial,
            });
        };
        let offset = self
            .heap_offset
            .checked_add(data.offset)
            .context("Invalid XAR data offset")?;
        anyhow::ensure!(
            offset <= self.archive_size
                && (partial
                    || offset
                        .checked_add(data.length)
                        .is_some_and(|end| end <= self.archive_size)),
            "XAR entry exceeds the archive size"
        );
        self.reader.seek(SeekFrom::Start(offset))?;
        let compressed = self
            .reader
            .by_ref()
            .take(data.length.min(self.archive_size - offset));
        let reader: Box<dyn Read + '_> = match data.encoding.as_str() {
            "application/octet-stream" => Box::new(compressed),
            "application/x-gzip" | "application/zlib" | "application/x-zlib" => {
                Box::new(flate2::read::ZlibDecoder::new(compressed))
            }
            "application/x-bzip2" => Box::new(bzip2::read::BzDecoder::new(compressed)),
            "application/x-lzma" => Box::new(lzma_rust2::LzmaReader::new_mem_limit(
                compressed,
                128 * 1024,
                None,
            )?),
            "application/x-xz" => Box::new(lzma_rust2::XzReader::new(compressed, true)),
            encoding => return Err(ReadError::Unsupported(encoding.to_owned()).into()),
        };
        let checksum = data
            .extracted_checksum
            .as_ref()
            .map(|expected| -> Result<_> {
                let digest: Box<dyn DynDigest> = match expected.len() {
                    32 => Box::new(md5::Md5::default()),
                    40 => Box::new(sha1::Sha1::default()),
                    64 => Box::new(sha2::Sha256::default()),
                    _ => anyhow::bail!("Unsupported XAR checksum"),
                };
                Ok((digest, expected.to_ascii_lowercase()))
            })
            .transpose()?;
        Ok(XarEntryReader {
            reader,
            expected_size: data.size,
            read: 0,
            checksum,
            finished: false,
            recovering: partial,
        })
    }
}

pub(crate) struct XarEntryReader<'a> {
    reader: Box<dyn Read + 'a>,
    expected_size: u64,
    read: u64,
    checksum: Option<(Box<dyn DynDigest>, String)>,
    finished: bool,
    recovering: bool,
}

impl Read for XarEntryReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.finished {
            return Ok(0);
        }
        let count = self.reader.read(buffer)?;
        self.read = self
            .read
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("XAR entry size overflow"))?;
        if (!self.recovering && self.read > self.expected_size)
            || (count == 0 && self.read != self.expected_size)
        {
            return Err(io::Error::other(EntryError::Size));
        }
        if let Some((digest, expected)) = &mut self.checksum {
            digest.update(&buffer[..count]);
            if count == 0 {
                let actual = digest
                    .finalize_reset()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                if actual != *expected {
                    return Err(io::Error::other(EntryError::Checksum));
                }
            }
        }
        self.finished = count == 0;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xar_rejects_bad_checksums_truncation_and_oversized_toc() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let path = root.path().join("archive.xar");
        std::fs::write(&source, b"XAR contents with a verified checksum").unwrap();
        crate::archive::create_archive(
            &path,
            &[source],
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::Xar,
                ..crate::archive::CreateOptions::default()
            },
        )
        .unwrap();
        let mut reader = XarReader::new(std::fs::File::open(&path).unwrap()).unwrap();
        let mut entry = reader.entries[0].clone();
        entry.data.as_mut().unwrap().extracted_checksum = Some("00".repeat(20));
        assert!(
            reader
                .entry_reader(&entry)
                .unwrap()
                .read_to_end(&mut Vec::new())
                .is_err()
        );

        let mut truncated = std::fs::read(&path).unwrap();
        let data = entry.data.as_ref().unwrap();
        truncated.truncate((reader.heap_offset + data.offset + data.length - 1) as usize);
        let mut reader = XarReader::new(io::Cursor::new(truncated)).unwrap();
        let entry = reader.entries[0].clone();
        assert!(reader.entry_reader(&entry).is_err());

        let mut oversized = std::fs::read(&path).unwrap();
        oversized[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
        std::fs::write(&path, &oversized).unwrap();
        let report = crate::inspection::inspect(
            &path,
            None,
            crate::inspection::InspectionMode::Integrity,
            &std::sync::atomic::AtomicBool::new(false),
        );
        assert_eq!(report.integrity, crate::inspection::Integrity::Incomplete);
        assert!(XarReader::new(io::Cursor::new(oversized)).is_err());
    }
}
