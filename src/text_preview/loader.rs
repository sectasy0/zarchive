use super::*;

#[cfg(test)]
pub(crate) const MAX_BYTES: usize = 128 * 1024;

pub(crate) struct GuardedReader<'a, R> {
    pub reader: R,
    pub cancel: &'a AtomicBool,
    pub budget: &'a std::cell::Cell<u64>,
}

impl<R: Read> Read for GuardedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("Preview cancelled"));
        }
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.budget.get() == 0 {
            return Err(io::Error::other("Preview safety limit reached"));
        }
        let length = buffer
            .len()
            .min(self.budget.get().min(usize::MAX as u64) as usize);
        let count = self.reader.read(&mut buffer[..length])?;
        self.budget.set(self.budget.get() - count as u64);
        Ok(count)
    }
}

impl<R: io::Seek> io::Seek for GuardedReader<'_, R> {
    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("Preview cancelled"));
        }
        self.reader.seek(position)
    }
}

pub(crate) struct TextContent {
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone)]
pub(crate) struct HexRow {
    pub offset: usize,
    pub hex: String,
    pub ascii: String,
}

pub(crate) struct HexPreview {
    pub rows: Vec<HexRow>,
    pub truncated: bool,
}

pub(crate) enum PreviewData {
    Text(TextContent),
    Binary(HexPreview),
}

pub(super) struct LoadedPreview {
    pub(super) data: PreviewData,
    pub(super) raw_bytes: Vec<u8>,
}

#[cfg(test)]
pub(super) fn hex_preview(bytes: &[u8]) -> HexPreview {
    hex_preview_with_limit(bytes, MAX_BYTES)
}

pub(super) fn hex_preview_with_limit(bytes: &[u8], max_bytes: usize) -> HexPreview {
    let truncated = bytes.len() > max_bytes;
    let bytes = &bytes[..bytes.len().min(max_bytes)];
    let rows = bytes
        .chunks(16)
        .enumerate()
        .map(|(index, chunk)| HexRow {
            offset: index * 16,
            hex: chunk
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" "),
            ascii: chunk
                .iter()
                .map(|byte| {
                    if (b' '..=b'~').contains(byte) {
                        char::from(*byte)
                    } else {
                        '.'
                    }
                })
                .collect(),
        })
        .collect();
    HexPreview { rows, truncated }
}

pub(super) struct CsvTable {
    pub(super) headers: Vec<gpui::SharedString>,
    pub(super) rows: Vec<Vec<gpui::SharedString>>,
    pub(super) truncated: bool,
}

impl CsvTable {
    #[cfg(test)]
    pub(super) fn parse(content: &TextContent) -> Option<Self> {
        Self::parse_with_limits(content, 5_000, 64)
    }

    pub(super) fn parse_with_limits(
        content: &TextContent,
        max_rows: usize,
        max_columns: usize,
    ) -> Option<Self> {
        let delimiter = [b',', b';', b'\t'].into_iter().max_by_key(|delimiter| {
            let mut reader = csv::ReaderBuilder::new()
                .delimiter(*delimiter)
                .has_headers(false)
                .flexible(true)
                .from_reader(content.text.as_bytes());
            let records: Vec<_> = reader.records().take(20).filter_map(Result::ok).collect();
            let columns = records.first().map_or(0, |record| record.len());
            let consistent = records
                .iter()
                .filter(|record| record.len() == columns)
                .count();
            (
                columns > 1 && consistent == records.len(),
                columns > 1,
                consistent,
                columns,
            )
        })?;
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .has_headers(false)
            .flexible(true)
            .from_reader(content.text.as_bytes());
        let mut record = csv::StringRecord::new();
        let mut records = Vec::new();
        let mut truncated = content.truncated;
        while reader.read_record(&mut record).ok()? {
            if content.truncated && reader.position().byte() as usize >= content.text.len() {
                break;
            }
            if records.len() >= max_rows.saturating_add(1) {
                truncated = true;
                break;
            }
            truncated |= record.len() > max_columns;
            records.push(
                record
                    .iter()
                    .take(max_columns)
                    .map(|cell| gpui::SharedString::from(cell.to_owned()))
                    .collect::<Vec<_>>(),
            );
        }
        let mut records = records.into_iter();
        let mut headers = records.next()?;
        let rows: Vec<_> = records.collect();
        let columns = rows
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .max(headers.len())
            .min(max_columns);
        while headers.len() < columns {
            headers.push(gpui::SharedString::default());
        }
        for (index, header) in headers.iter_mut().enumerate() {
            if header.is_empty() {
                *header = (index + 1).to_string().into();
            }
        }
        Some(Self {
            headers,
            rows,
            truncated,
        })
    }
}

pub(super) fn preview_filename(target: &EntryTarget) -> String {
    target.archive_entry.clone().unwrap_or_else(|| {
        if crate::archive::ResolvedFormat::from_path(&target.path)
            .is_some_and(|format| format.is_single_compressed())
        {
            crate::archive::single_compressed_entry_name(&target.path)
                .unwrap_or_else(|_| target.name.clone())
        } else {
            target.name.clone()
        }
    })
}

pub(super) fn syntax_language(target: &EntryTarget) -> String {
    let name = preview_filename(target);
    let path = std::path::Path::new(&name);
    let filename = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    match filename.as_str() {
        "makefile" | "gnumakefile" => "makefile".into(),
        "dockerfile" => "dockerfile".into(),
        ".bashrc" | ".bash_profile" | ".zshrc" | ".profile" => "sh".into(),
        _ => path
            .extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_else(|| "text".into()),
    }
}

pub(super) fn is_csv_target(target: &EntryTarget) -> bool {
    let name = preview_filename(target);
    std::path::Path::new(&name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
}

#[cfg(test)]
pub(crate) fn load(
    target: &crate::file_actions::EntryTarget,
    cancel: &AtomicBool,
) -> anyhow::Result<PreviewData> {
    load_with_settings(
        target,
        cancel,
        crate::preferences::AdvancedSettings::default(),
    )
}

#[cfg(test)]
pub(crate) fn load_with_settings(
    target: &crate::file_actions::EntryTarget,
    cancel: &AtomicBool,
    settings: crate::preferences::AdvancedSettings,
) -> anyhow::Result<PreviewData> {
    Ok(load_with_limit(target, cancel, settings.preview_max_bytes())?.data)
}

pub(super) fn load_with_limit(
    target: &crate::file_actions::EntryTarget,
    cancel: &AtomicBool,
    max_bytes: usize,
) -> anyhow::Result<LoadedPreview> {
    let bytes = if let Some(entry) = &target.archive_entry {
        crate::archive::read_entry_prefix_with_limit(
            &target.path,
            entry,
            target.password.as_ref().map(|password| password.as_str()),
            cancel,
            max_bytes,
        )?
    } else if crate::archive::ResolvedFormat::from_path(&target.path)
        .is_some_and(|format| format.is_single_compressed())
    {
        let name = crate::archive::single_compressed_entry_name(&target.path)?;
        crate::archive::read_entry_prefix_with_limit(&target.path, &name, None, cancel, max_bytes)?
    } else {
        read_prefix_with_limit(&mut std::fs::File::open(&target.path)?, cancel, max_bytes)?
    };
    Ok(loaded_preview_from_bytes(bytes, max_bytes))
}

pub(super) fn loaded_preview_from_bytes(bytes: Vec<u8>, max_bytes: usize) -> LoadedPreview {
    let raw_bytes = bytes[..bytes.len().min(max_bytes)].to_vec();
    let data = match decode_with_limit(&bytes, max_bytes) {
        Some(content) => PreviewData::Text(content),
        None => PreviewData::Binary(hex_preview_with_limit(&bytes, max_bytes)),
    };
    LoadedPreview { data, raw_bytes }
}

pub(super) fn load_remaining(
    target: &EntryTarget,
    cancel: &AtomicBool,
    offset: usize,
) -> anyhow::Result<Vec<u8>> {
    if let Some(entry) = &target.archive_entry {
        crate::archive::read_entry_from_offset(
            &target.path,
            entry,
            target.password.as_ref().map(|password| password.as_str()),
            cancel,
            offset,
        )
    } else if crate::archive::ResolvedFormat::from_path(&target.path)
        .is_some_and(|format| format.is_single_compressed())
    {
        let name = crate::archive::single_compressed_entry_name(&target.path)?;
        crate::archive::read_entry_from_offset(&target.path, &name, None, cancel, offset)
    } else {
        use std::io::Seek;
        let mut file = std::fs::File::open(&target.path)?;
        file.seek(std::io::SeekFrom::Start(offset as u64))?;
        Ok(read_prefix_with_limit(&mut file, cancel, usize::MAX)?)
    }
}

#[cfg(test)]
pub(crate) fn read_prefix(reader: &mut impl Read, cancel: &AtomicBool) -> io::Result<Vec<u8>> {
    read_prefix_with_limit(reader, cancel, MAX_BYTES)
}

pub(crate) fn read_prefix_with_limit(
    reader: &mut impl Read,
    cancel: &AtomicBool,
    max_bytes: usize,
) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    while bytes.len() <= max_bytes {
        crate::task_queue::checkpoint()?;
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let length = buffer
            .len()
            .min(max_bytes.saturating_add(1).saturating_sub(bytes.len()));
        let count = reader.read(&mut buffer[..length])?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        crate::task_queue::report_bytes(count as u64);
    }
    Ok(bytes)
}

pub(crate) fn read_from_offset(
    reader: &mut impl Read,
    cancel: &AtomicBool,
    offset: usize,
) -> io::Result<Vec<u8>> {
    let mut remaining = offset;
    let mut buffer = [0; 8192];
    while remaining > 0 {
        crate::task_queue::checkpoint()?;
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let length = buffer.len().min(remaining);
        let count = reader.read(&mut buffer[..length])?;
        if count == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        remaining -= count;
        crate::task_queue::report_bytes(count as u64);
    }
    read_prefix_with_limit(reader, cancel, usize::MAX)
}

#[cfg(test)]
pub(crate) fn decode(bytes: &[u8]) -> Option<TextContent> {
    decode_with_limit(bytes, MAX_BYTES)
}

pub(super) fn decode_with_limit(bytes: &[u8], max_bytes: usize) -> Option<TextContent> {
    let truncated = bytes.len() > max_bytes;
    let bytes = &bytes[..bytes.len().min(max_bytes)];
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let little_endian = bytes[0] == 0xff;
        let data = &bytes[2..];
        if data.len() % 2 != 0 && !truncated {
            return None;
        }
        let mut words: Vec<_> = data
            .chunks_exact(2)
            .map(|pair| {
                let pair = [pair[0], pair[1]];
                if little_endian {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            })
            .collect();
        if truncated
            && words
                .last()
                .is_some_and(|word| (0xd800..=0xdbff).contains(word))
        {
            words.pop();
        }
        String::from_utf16(&words).ok()?
    } else {
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(error) if truncated && error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()])
                    .ok()?
                    .to_owned()
            }
            Err(_) => return None,
        }
    };
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return None;
    }
    Some(TextContent { text, truncated })
}
