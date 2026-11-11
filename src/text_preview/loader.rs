use super::*;
use gpui::RenderImage;
use image::{AnimationDecoder as _, ImageDecoder as _};
use std::process::{Command, Stdio};

#[cfg(test)]
pub(crate) const MAX_BYTES: usize = 128 * 1024;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PDF_BYTES: usize = 16 * 1024 * 1024;
const MAX_PDF_RENDER_WIDTH: f64 = 1200.0;
const MAX_PDF_RENDER_HEIGHT: f64 = 1800.0;
const MAX_MEDIA_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 8192;
const MAX_IMAGE_ALLOC: u64 = 128 * 1024 * 1024;
const MAX_GIF_RENDER_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GIF_FRAMES: usize = 24;
const MAX_IMAGE_PREVIEW_DIMENSION: u32 = 1920;
const MAX_VIDEO_PREVIEW_WIDTH: u32 = 960;
const MAX_VIDEO_PREVIEW_HEIGHT: u32 = 540;

pub(super) enum VideoMessage {
    Frame(Arc<RenderImage>),
    Finished,
    Failed(String),
}

pub(super) type VideoProcesses = Arc<std::sync::Mutex<Vec<std::process::Child>>>;

pub(super) fn stop_video_processes(processes: &VideoProcesses) {
    let mut processes = processes.lock().unwrap_or_else(|error| error.into_inner());
    for process in processes.iter_mut() {
        let _ = process.kill();
    }
}

struct VideoProcessGuard(VideoProcesses);

impl Drop for VideoProcessGuard {
    fn drop(&mut self) {
        let processes = {
            let mut active = self.0.lock().unwrap_or_else(|error| error.into_inner());
            std::mem::take(&mut *active)
        };
        for mut process in processes {
            if process.try_wait().ok().flatten().is_none() {
                let _ = process.kill();
            }
            let _ = process.wait();
        }
    }
}

pub(super) fn decode_video_stream(
    target: EntryTarget,
    cancel: Arc<AtomicBool>,
    sender: std::sync::mpsc::SyncSender<VideoMessage>,
    processes: VideoProcesses,
) -> anyhow::Result<()> {
    let ffmpeg = video_tool("ffmpeg").ok_or_else(|| anyhow::anyhow!("FFmpeg is not installed"))?;
    let ffprobe = video_tool("ffprobe").ok_or_else(|| anyhow::anyhow!("FFprobe is not installed"))?;
    let staged = if target.archive_entry.is_some() {
        Some(target.extract()?)
    } else {
        None
    };
    let path = staged
        .as_ref()
        .map(|file| file.path())
        .unwrap_or(&target.path);
    let _process_guard = VideoProcessGuard(processes.clone());
    let probe = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=s=x:p=0",
        ])
        .arg(path)
        .output()?;
    anyhow::ensure!(probe.status.success(), "FFprobe could not inspect video stream");
    let dimensions = String::from_utf8_lossy(&probe.stdout);
    let (source_width, source_height) = dimensions
        .trim()
        .split_once('x')
        .and_then(|(width, height)| Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?)) )
        .ok_or_else(|| anyhow::anyhow!("Video dimensions are unavailable"))?;
    anyhow::ensure!(source_width > 0 && source_height > 0, "Video dimensions are invalid");
    let scale = (MAX_VIDEO_PREVIEW_WIDTH as f64 / source_width as f64)
        .min(MAX_VIDEO_PREVIEW_HEIGHT as f64 / source_height as f64)
        .min(1.0);
    let width = ((source_width as f64 * scale).floor() as u32 & !1).max(2);
    let height = ((source_height as f64 * scale).floor() as u32 & !1).max(2);

    let audio = video_tool("ffplay").and_then(|ffplay| {
        Command::new(ffplay)
            .args(["-nodisp", "-autoexit", "-vn", "-loglevel", "error"])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
                .spawn()
                .ok()
    });
            if let Some(audio) = audio {
            processes.lock().unwrap_or_else(|error| error.into_inner()).push(audio);
            }
    let video_result = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-re", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"])
        .arg(format!("scale={width}:{height},fps=24"))
        .args(["-pix_fmt", "rgba", "-f", "rawvideo", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let video = match video_result {
        Ok(video) => video,
        Err(error) => return Err(error.into()),
    };
    let video_index = {
        let mut processes = processes.lock().unwrap_or_else(|error| error.into_inner());
        let index = processes.len();
        processes.push(video);
        index
    };
    let stdout = processes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get_mut(video_index)
        .and_then(|video| video.stdout.take())
        .ok_or_else(|| anyhow::anyhow!("FFmpeg video output is unavailable"))?;
    let frame_size = width as usize * height as usize * 4;
    let mut reader = std::io::BufReader::new(stdout);
    let mut bytes = vec![0; frame_size];
    loop {
        if cancel.load(Ordering::Acquire) {
            stop_video_processes(&processes);
            break;
        }
        match reader.read_exact(&mut bytes) {
            Ok(()) => {
                let Some(mut image) = image::RgbaImage::from_raw(width, height, bytes.clone()) else {
                    stop_video_processes(&processes);
                    return Err(anyhow::anyhow!("FFmpeg returned an invalid video frame"));
                };
                for pixel in image.pixels_mut() {
                    pixel.0.swap(0, 2);
                }
                if sender
                    .send(VideoMessage::Frame(Arc::new(RenderImage::new(vec![image::Frame::new(image)]))))
                    .is_err()
                {
                    stop_video_processes(&processes);
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => {
                stop_video_processes(&processes);
                return Err(error.into());
            }
        }
    }
    let status = processes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get_mut(video_index)
        .and_then(|video| video.try_wait().ok().flatten());
    if !cancel.load(Ordering::Acquire) && status.is_some_and(|status| !status.success()) {
        anyhow::bail!("FFmpeg could not decode the video");
    }
    Ok(())
}

pub(super) fn video_tool(name: &str) -> Option<std::path::PathBuf> {
    let executable = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let configured = std::env::var_os(name.to_ascii_uppercase()).map(std::path::PathBuf::from);
    let path_candidates = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(&executable));
    let platform_candidates = if cfg!(target_os = "macos") {
        vec![
            std::path::PathBuf::from(format!("/opt/homebrew/bin/{executable}")),
            std::path::PathBuf::from(format!("/usr/local/bin/{executable}")),
        ]
    } else {
        Vec::new()
    };
    configured
        .into_iter()
        .chain(path_candidates)
        .chain(platform_candidates)
        .find(|path| path.is_file())
}

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
    Image(Arc<RenderImage>),
    Pdf(PdfPreview),
    Media(MediaPreview),
}

pub(crate) struct PdfPreview {
    pub bytes: Arc<Vec<u8>>,
    pub image: Arc<RenderImage>,
    pub page_index: usize,
    pub page_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MediaKind {
    Audio,
    Video,
}

pub(crate) struct MediaPreview {
    pub kind: MediaKind,
    pub file_type: String,
    pub duration: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub sample_rate: Option<String>,
    pub channels: Option<String>,
}

impl MediaPreview {
    pub(crate) fn details(&self) -> Vec<(&'static str, &str)> {
        let mut details = vec![("File type", self.file_type.as_str())];
        details.extend(self.title.as_deref().map(|value| ("Title", value)));
        details.extend(self.artist.as_deref().map(|value| ("Artist", value)));
        details.extend(self.album.as_deref().map(|value| ("Album", value)));
        details.extend(self.duration.as_deref().map(|value| ("Duration", value)));
        details.extend(self.sample_rate.as_deref().map(|value| ("Sample rate", value)));
        details.extend(self.channels.as_deref().map(|value| ("Channels", value)));
        details
    }
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
        if crate::archive::ResolvedFormat::from_file(&target.path)
            .or_else(|| (!target.path.exists()).then(|| crate::archive::ResolvedFormat::from_path(&target.path)).flatten())
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
    let image_target = is_image_target(target);
    let pdf_target = is_pdf_target(target);
    let media_kind = media_kind(target);
    let read_limit = if image_target {
        MAX_IMAGE_BYTES
    } else if pdf_target {
        MAX_PDF_BYTES
    } else if media_kind.is_some() {
        MAX_MEDIA_BYTES
    } else {
        max_bytes
    };
    let bytes = read_target_bytes(target, cancel, read_limit)?;
    if image_target {
        anyhow::ensure!(bytes.len() <= MAX_IMAGE_BYTES, "Image exceeds the preview size limit");
        return Ok(LoadedPreview {
            data: PreviewData::Image(decode_image_preview(&bytes)?),
            raw_bytes: Vec::new(),
        });
    }
    if pdf_target {
        anyhow::ensure!(bytes.len() <= MAX_PDF_BYTES, "PDF exceeds the preview size limit");
        let bytes = Arc::new(bytes);
        return Ok(LoadedPreview {
            data: PreviewData::Pdf(render_pdf_page(bytes, 0)?),
            raw_bytes: Vec::new(),
        });
    }
    if let Some(kind) = media_kind {
        anyhow::ensure!(bytes.len() <= MAX_MEDIA_BYTES, "Media file exceeds the preview size limit");
        return Ok(LoadedPreview {
            data: PreviewData::Media(decode_media_preview(&bytes, kind)),
            raw_bytes: Vec::new(),
        });
    }
    Ok(loaded_preview_from_bytes(bytes, max_bytes))
}

fn read_target_bytes(
    target: &EntryTarget,
    cancel: &AtomicBool,
    read_limit: usize,
) -> anyhow::Result<Vec<u8>> {
    let bytes = if let Some(entry) = &target.archive_entry {
        crate::archive::read_entry_prefix_with_limit(
            &target.path,
            entry,
            target.password.as_ref().map(|password| password.as_str()),
            cancel,
            read_limit,
        )?
    } else if crate::archive::ResolvedFormat::from_file(&target.path)
        .is_some_and(|format| format.is_single_compressed())
    {
        let name = crate::archive::single_compressed_entry_name(&target.path)?;
        crate::archive::read_entry_prefix_with_limit(&target.path, &name, None, cancel, read_limit)?
    } else {
        read_prefix_with_limit(&mut std::fs::File::open(&target.path)?, cancel, read_limit)?
    };
    Ok(bytes)
}

fn is_image_target(target: &EntryTarget) -> bool {
    let extension = std::path::Path::new(&preview_filename(target))
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "avif" | "bmp" | "gif" | "ico" | "jpg" | "jpeg" | "png" | "pnm" | "pbm"
            | "pgm" | "ppm" | "qoi" | "tga" | "tif" | "tiff" | "webp"
    )
}

pub(super) fn is_pdf_target(target: &EntryTarget) -> bool {
    std::path::Path::new(&preview_filename(target))
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

pub(super) fn media_kind(target: &EntryTarget) -> Option<MediaKind> {
    let extension = std::path::Path::new(&preview_filename(target))
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match extension.as_str() {
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "opus" | "m4a" | "aiff" | "ape" => {
            Some(MediaKind::Audio)
        }
        "mp4" | "m4v" | "mov" | "mkv" | "avi" | "webm" => Some(MediaKind::Video),
        _ => None,
    }
}

pub(super) fn render_pdf_page(bytes: Arc<Vec<u8>>, page_index: usize) -> anyhow::Result<PdfPreview> {
    use pdf_render::{RenderSettings, pdf_syntax::Pdf, vello_cpu::color::palette::css::WHITE};

    let pdf = Pdf::new(bytes.as_ref().clone())
        .map_err(|error| anyhow::anyhow!("could not parse PDF: {error:?}"))?;
    let pages = pdf.pages();
    anyhow::ensure!(!pages.is_empty(), "PDF has no pages");
    let page_count = pages.len();
    let page = pages
        .get(page_index)
        .context("PDF page does not exist")?;
    let (page_width, page_height) = page.render_dimensions();
    anyhow::ensure!(page_width.is_finite() && page_height.is_finite() && page_width > 0.0 && page_height > 0.0,
        "PDF page dimensions are invalid");
    let scale = (MAX_PDF_RENDER_WIDTH as f32 / page_width)
        .min(MAX_PDF_RENDER_HEIGHT as f32 / page_height)
        .min(1.0) as f32;
    let pixmap = pdf_render::render(
        page,
        &pdf_render::pdf_interpret::InterpreterSettings::default(),
        &RenderSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
            ..Default::default()
        },
    );
    let width = u32::from(pixmap.width());
    let height = u32::from(pixmap.height());
    anyhow::ensure!(
        width > 0 && height > 0 && u64::from(width) * u64::from(height) <= 2_160_000,
        "PDF rendered page exceeds the preview pixel limit"
    );
    let mut image = image::RgbaImage::from_raw(width, height, pixmap.data_as_u8_slice().to_vec())
        .context("PDF renderer returned invalid pixel data")?;
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(PdfPreview {
        bytes,
        image: Arc::new(RenderImage::new(vec![image::Frame::new(image)])),
        page_index,
        page_count,
    })
}

fn decode_media_preview(bytes: &[u8], kind: MediaKind) -> MediaPreview {
    use lofty::{
        file::{AudioFile, TaggedFileExt},
        probe::Probe,
        tag::ItemKey,
    };

    let mut preview = MediaPreview {
        kind,
        file_type: match kind {
            MediaKind::Audio => "Audio".to_owned(),
            MediaKind::Video => "Video".to_owned(),
        },
        duration: None,
        title: None,
        artist: None,
        album: None,
        sample_rate: None,
        channels: None,
    };
    let Ok(probe) = Probe::new(std::io::Cursor::new(bytes)).guess_file_type() else {
        return preview;
    };
    let Ok(tagged) = probe.read() else {
        return preview;
    };
    preview.file_type = format!("{:?}", tagged.file_type());
    let duration = tagged.properties().duration().as_secs();
    if duration > 0 {
        preview.duration = Some(format!("{}:{:02}:{:02}", duration / 3600, (duration / 60) % 60, duration % 60));
    }
    preview.sample_rate = tagged.properties().sample_rate().map(|rate| format!("{rate} Hz"));
    preview.channels = tagged.properties().channels().map(|channels| channels.to_string());
    if let Some(tag) = tagged.primary_tag() {
        preview.title = tag.get_string(ItemKey::TrackTitle).map(str::to_owned);
        preview.artist = tag.get_string(ItemKey::TrackArtist).map(str::to_owned);
        preview.album = tag.get_string(ItemKey::AlbumTitle).map(str::to_owned);
    }
    preview
}

fn checked_image_dimensions(width: u32, height: u32) -> anyhow::Result<()> {
    anyhow::ensure!(
        width > 0
            && height > 0
            && width <= MAX_IMAGE_DIMENSION
            && height <= MAX_IMAGE_DIMENSION
            && u64::from(width) * u64::from(height) * 4 <= MAX_IMAGE_ALLOC,
        "Image dimensions exceed the preview safety limit"
    );
    Ok(())
}

fn thumbnail_dimensions(width: u32, height: u32) -> (u32, u32) {
    let scale = (MAX_IMAGE_PREVIEW_DIMENSION as f32 / width.max(height) as f32).min(1.0);
    (
        (width as f32 * scale).round().max(1.0) as u32,
        (height as f32 * scale).round().max(1.0) as u32,
    )
}

fn image_decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC);
    limits
}

fn decode_image_preview(bytes: &[u8]) -> anyhow::Result<Arc<RenderImage>> {
    use std::io::Cursor;

    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let format = reader.format().context("Unsupported image format")?;
    if format == image::ImageFormat::Gif {
        let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
        let (width, height) = decoder.dimensions();
        checked_image_dimensions(width, height)?;
        decoder.set_limits(image_decode_limits())?;
        let frame_bytes = u64::from(width) * u64::from(height) * 4;
        let frame_limit = (MAX_GIF_RENDER_BYTES / frame_bytes)
            .clamp(1, MAX_GIF_FRAMES as u64) as usize;
        let frames = decoder
            .into_frames()
            .take(frame_limit)
            .collect::<image::ImageResult<Vec<_>>>()?
            .into_iter()
            .map(|frame| {
                let delay = frame.delay();
                let source = frame.into_buffer();
                let (width, height) = thumbnail_dimensions(source.width(), source.height());
                let buffer = image::DynamicImage::ImageRgba8(source)
                    .resize_exact(width, height, image::imageops::FilterType::Lanczos3)
                    .into_rgba8();
                image::Frame::from_parts(buffer, 0, 0, delay)
            })
            .map(convert_frame_to_bgra)
            .collect::<Vec<_>>();
        anyhow::ensure!(!frames.is_empty(), "GIF contains no image frames");
        return Ok(Arc::new(RenderImage::new(frames)));
    }

    let dimensions_reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let (width, height) = dimensions_reader.into_dimensions()?;
    checked_image_dimensions(width, height)?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(image_decode_limits());
    let decoded = reader.decode()?;
    let (width, height) = thumbnail_dimensions(width, height);
    let mut buffer = decoded
        .resize_exact(width, height, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    for pixel in buffer.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

fn convert_frame_to_bgra(mut frame: image::Frame) -> image::Frame {
    for pixel in frame.buffer_mut().pixels_mut() {
        pixel.0.swap(0, 2);
    }
    frame
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
    } else if crate::archive::ResolvedFormat::from_file(&target.path)
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
