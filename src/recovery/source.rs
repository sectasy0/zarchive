use super::*;

pub(super) struct Segment {
    pub(super) path: Option<PathBuf>,
    pub(super) start: u64,
    pub(super) size: u64,
}

#[derive(Clone)]
pub(super) struct Source {
    pub(super) segments: Arc<Vec<Segment>>,
    pub(super) length: u64,
    pub(super) remaining: Arc<AtomicU64>,
    pub(super) metadata_remaining: Arc<AtomicU64>,
    pub(super) scan_remaining: Arc<AtomicU64>,
    pub(super) header_patch: Option<Arc<[u8; 32]>>,
}

impl Source {
    pub(super) fn open(path: &Path, recovery: &mut Recovery) -> Result<Self> {
        let (volumes, inferred) = match crate::parity::split_zip_layout(path)? {
            Some(layout) => layout,
            None => crate::volumes::recovery_layout(path)?,
        };
        if inferred {
            recovery
                .notes
                .push("Volume positions are inferred; offsets may be uncertain");
        }
        let mut segments = Vec::new();
        let mut length = 0u64;
        for volume in volumes {
            if volume.path.is_none() {
                recovery.blocks.push(Block {
                    offset: length,
                    length: volume.size,
                    issue: "Missing archive volume",
                });
            } else if let Some(path) = &volume.path {
                let metadata = path.metadata()?;
                anyhow::ensure!(metadata.is_file(), "Recovery requires regular files");
                if metadata.len() != volume.size {
                    recovery.blocks.push(Block {
                        offset: length,
                        length: volume.size,
                        issue: "Archive volume size mismatch",
                    });
                }
            }
            segments.push(Segment {
                path: volume.path,
                start: length,
                size: volume.size,
            });
            length = length
                .checked_add(volume.size)
                .context("Archive volumes are too large")?;
        }
        Ok(Self {
            segments: Arc::new(segments),
            length,
            remaining: Arc::new(AtomicU64::new(MAX_INPUT_BYTES)),
            metadata_remaining: Arc::new(AtomicU64::new(u64::MAX)),
            scan_remaining: Arc::new(AtomicU64::new(MAX_SCAN_BYTES)),
            header_patch: None,
        })
    }

    pub(super) fn reader(&self) -> SourceReader {
        SourceReader {
            source: self.clone(),
            position: 0,
            current: None,
        }
    }
}

pub(super) struct SourceReader {
    pub(super) source: Source,
    pub(super) position: u64,
    pub(super) current: Option<(usize, File)>,
}

impl Read for SourceReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        crate::task_queue::checkpoint()?;
        if buffer.is_empty() || self.position >= self.source.length {
            return Ok(0);
        }
        let allowance = self
            .source
            .remaining
            .load(Ordering::Relaxed)
            .min(self.source.metadata_remaining.load(Ordering::Relaxed));
        if allowance == 0 {
            return Err(io::Error::other("Recovery safety limit reached"));
        }
        if let Some(header) = &self.source.header_patch {
            if self.position < header.len() as u64 {
                let count = (header.len() as u64 - self.position)
                    .min(buffer.len() as u64)
                    .min(allowance) as usize;
                buffer[..count].copy_from_slice(
                    &header[self.position as usize..self.position as usize + count],
                );
                self.position += count as u64;
                self.source
                    .remaining
                    .fetch_sub(count as u64, Ordering::Relaxed);
                self.source
                    .metadata_remaining
                    .fetch_sub(count as u64, Ordering::Relaxed);
                return Ok(count);
            }
        }
        let index = self
            .source
            .segments
            .partition_point(|segment| segment.start + segment.size <= self.position);
        let segment = &self.source.segments[index];
        let path = segment.path.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "Missing archive volume")
        })?;
        if self
            .current
            .as_ref()
            .is_none_or(|(current, _)| *current != index)
        {
            self.current = Some((index, File::open(path)?));
        }
        let offset = self.position - segment.start;
        let file = &mut self.current.as_mut().unwrap().1;
        file.seek(SeekFrom::Start(offset))?;
        let length = (segment.size - offset)
            .min(buffer.len() as u64)
            .min(allowance) as usize;
        let count = file.read(&mut buffer[..length])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Damaged or missing data",
            ));
        }
        self.position += count as u64;
        self.source
            .remaining
            .fetch_sub(count as u64, Ordering::Relaxed);
        self.source
            .metadata_remaining
            .fetch_sub(count as u64, Ordering::Relaxed);
        Ok(count)
    }
}

impl Seek for SourceReader {
    fn seek(&mut self, offset: SeekFrom) -> io::Result<u64> {
        let position = match offset {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.source.length) + i128::from(offset),
        };
        self.position = u64::try_from(position)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid recovery offset"))?;
        Ok(self.position)
    }
}
