use crate::{ArchiveManager, Theme, file_actions::EntryTarget, i18n::Language};
use gpui::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
    input::{Editor, EditorState},
};
use gpui::{Context, IntoElement, Render, Window, div, prelude::*, px, rgb};
use std::{
    io::{self, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

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

fn hex_preview(bytes: &[u8]) -> HexPreview {
    let truncated = bytes.len() > MAX_BYTES;
    let bytes = &bytes[..bytes.len().min(MAX_BYTES)];
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

struct CsvTable {
    headers: Vec<gpui::SharedString>,
    rows: Vec<Vec<gpui::SharedString>>,
    truncated: bool,
}

impl CsvTable {
    fn parse(content: &TextContent) -> Option<Self> {
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
            if records.len() >= 5001 {
                truncated = true;
                break;
            }
            truncated |= record.len() > 64;
            records.push(
                record
                    .iter()
                    .take(64)
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
            .max(headers.len());
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

fn preview_filename(target: &EntryTarget) -> String {
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

fn syntax_language(target: &EntryTarget) -> String {
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

fn is_csv_target(target: &EntryTarget) -> bool {
    let name = preview_filename(target);
    std::path::Path::new(&name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
}

pub(crate) fn load(
    target: &crate::file_actions::EntryTarget,
    cancel: &AtomicBool,
) -> anyhow::Result<PreviewData> {
    let bytes = if let Some(entry) = &target.archive_entry {
        crate::archive::read_entry_prefix(
            &target.path,
            entry,
            target.password.as_ref().map(|password| password.as_str()),
            cancel,
        )?
    } else if crate::archive::ResolvedFormat::from_path(&target.path)
        .is_some_and(|format| format.is_single_compressed())
    {
        let name = crate::archive::single_compressed_entry_name(&target.path)?;
        crate::archive::read_entry_prefix(&target.path, &name, None, cancel)?
    } else {
        read_prefix(&mut std::fs::File::open(&target.path)?, cancel)?
    };
    Ok(match decode(&bytes) {
        Some(content) => PreviewData::Text(content),
        None => PreviewData::Binary(hex_preview(&bytes)),
    })
}

pub(crate) fn read_prefix(reader: &mut impl Read, cancel: &AtomicBool) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    while bytes.len() <= MAX_BYTES {
        crate::task_queue::checkpoint()?;
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let length = buffer.len().min(MAX_BYTES + 1 - bytes.len());
        let count = reader.read(&mut buffer[..length])?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        crate::task_queue::report_bytes(count as u64);
    }
    Ok(bytes)
}

pub(crate) fn decode(bytes: &[u8]) -> Option<TextContent> {
    let truncated = bytes.len() > MAX_BYTES;
    let bytes = &bytes[..bytes.len().min(MAX_BYTES)];
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

pub(crate) struct Preview {
    target: EntryTarget,
    manager: gpui::WeakEntity<ArchiveManager>,
    content: Option<TextContent>,
    hex: Option<HexPreview>,
    editor: Option<gpui::Entity<EditorState>>,
    csv: Option<CsvTable>,
    error: Option<String>,
    cancel: Arc<AtomicBool>,
    task: Option<u64>,
    scroll: gpui::UniformListScrollHandle,
    pub theme: Theme,
    pub language: Language,
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl ArchiveManager {
    pub(crate) fn preview_text_target(
        &mut self,
        target: &EntryTarget,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.inspector.clear_text_preview();
        self.inspector.text_cancel = Arc::new(AtomicBool::new(false));
        self.inspector.visible = true;
        self.inspector.selected_name = target.archive_entry.clone();
        let dark = self.theme_mode.is_dark(window.appearance());
        let theme = Theme::new(dark).with_accent(self.accent_color.color(dark));
        let manager = cx.entity().downgrade();
        let cancel = self.inspector.text_cancel.clone();
        let target = target.clone();
        let language = self.language;
        let worker_target = target.clone();
        let worker_cancel = cancel.clone();
        let preview = cx.new(|_: &mut Context<Preview>| Preview {
            target,
            manager,
            content: None,
            hex: None,
            editor: None,
            csv: None,
            error: None,
            cancel: cancel.clone(),
            task: None,
            scroll: gpui::UniformListScrollHandle::new(),
            theme,
            language,
        });
        let view = preview.downgrade();
        let task_id = self.background_queue.enqueue(
            "Loading text preview...",
            crate::task_queue::TaskControl::with_cancel(cancel).for_paths([worker_target.path.clone()]),
            move |control, reporter| async move {
                reporter.set(0, 1);
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                let result = match load(&worker_target, &worker_cancel)? {
                    PreviewData::Text(content) => {
                        let table = is_csv_target(&worker_target)
                            .then(|| CsvTable::parse(&content))
                            .flatten();
                        (PreviewData::Text(content), table)
                    }
                    PreviewData::Binary(hex) => (PreviewData::Binary(hex), None),
                };
                reporter.set(1, 1);
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                Ok(result)
            },
            move |_, result, window, cx| {
                let _ = view.update(cx, |preview, cx| {
                    if preview.cancel.load(Ordering::Relaxed) { preview.task = None; cx.notify(); return; }
                            match result {
                                Ok((PreviewData::Text(content), table)) => {
                                    if table.is_none() {
                                        preview.editor = Some(cx.new(|cx| {
                                            EditorState::new(window, cx)
                                                .language(syntax_language(&preview.target))
                                                .line_number(true)
                                                .soft_wrap(false)
                                                .scroll_bounce(false)
                                                .scroll_beyond_last_line(Some(0))
                                                .default_value(content.text.clone())
                                        }));
                                    }
                                    preview.csv = table;
                                    preview.content = Some(content);
                                }
                                Ok((PreviewData::Binary(hex), _)) => preview.hex = Some(hex),
                                Err(crate::task_queue::TaskFailure::Cancelled) => {},
                                Err(crate::task_queue::TaskFailure::Failed(error)) => preview.error = Some(format!("{error:#}")),
                            }
                            preview.task = None;
                            cx.notify();
                });
            },
            cx,
        );
        preview.update(cx, |preview, _| preview.task = Some(task_id));
        self.inspector.text_preview = Some(preview);
        cx.notify();
    }
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let language = self.language;
        let title = self
            .target
            .archive_entry
            .as_ref()
            .map(|entry| {
                format!(
                    "{} / {entry}",
                    self.target
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                )
            })
            .unwrap_or_else(|| self.target.name.clone());
        let message = if self.task.is_some() {
            Some(language.text("Loading text preview..."))
        } else if let Some(error) = &self.error {
            Some(language.error_message(error))
        } else if self
            .content
            .as_ref()
            .is_some_and(|content| content.text.is_empty())
        {
            Some(language.text("This file is empty"))
        } else {
            None
        };
        let preview_label = if self.hex.is_some() {
            "Binary preview"
        } else if self.csv.is_some() {
            "CSV preview"
        } else {
            "Text preview"
        };
        div()
            .id("text-preview")
            .debug_selector(|| "text-preview".to_owned())
            .flex()
            .flex_col()
            .size_full()
            .min_h(px(0.0))
            .overflow_hidden()
            .bg(rgb(theme.surface))
            .text_color(rgb(theme.text))
            .child(
                div()
                    .id("text-preview-header")
                    .debug_selector(|| "text-preview-header".to_owned())
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(34.0))
                    .flex_shrink_0()
                    .px_2()
                    .border_b_1()
                    .border_color(rgb(theme.border))
                    .child(if self.hex.is_some() {
                        crate::app_icon("binary", 14.0, theme.accent).into_any_element()
                    } else {
                        gpui::component::Icon::new(gpui::assets::IconName::FileText)
                            .size(px(14.0))
                            .text_color(rgb(theme.accent))
                            .into_any_element()
                    })
                    .child(
                        div()
                            .text_xs()
                            .flex_shrink_0()
                            .child(language.text(preview_label)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_xs()
                            .text_color(rgb(theme.muted_text))
                            .truncate()
                            .child(title),
                    )
                    .child(
                        Button::new("copy-text-preview")
                            .ghost()
                            .xsmall()
                            .icon(gpui::assets::IconName::Copy)
                            .tooltip(language.text("Copy text"))
                            .accessibility_label(language.text("Copy text"))
                            .disabled(self.content.is_none())
                            .on_click(cx.listener(|preview, _, _, cx| {
                                if let Some(content) = &preview.content {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        content.text.clone(),
                                    ));
                                }
                            })),
                    )
                    .child(
                        Button::new("close-text-preview")
                            .ghost()
                            .xsmall()
                            .icon(gpui::assets::IconName::X)
                            .tooltip(language.text("Close"))
                            .accessibility_label(language.text("Close"))
                            .on_click(cx.listener(|preview, _, _, cx| {
                                preview.cancel.store(true, Ordering::Relaxed);
                                let _ = preview.manager.update(cx, |manager, cx| {
                                    manager.inspector.close();
                                    cx.notify();
                                });
                            })),
                    ),
            )
            .when(
                self.content
                    .as_ref()
                    .is_some_and(|content| content.truncated)
                    || self.hex.as_ref().is_some_and(|hex| hex.truncated)
                    || self.csv.as_ref().is_some_and(|table| table.truncated),
                |panel| {
                    panel.child(
                        div()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(theme.muted_text))
                            .child(
                                language
                                    .text("Preview truncated to keep the application responsive"),
                            ),
                    )
                },
            )
            .when_some(message, |panel, message| {
                panel.child(
                    div()
                        .id("text-preview-message")
                        .debug_selector(|| "text-preview-message".to_owned())
                        .flex_1()
                        .p_3()
                        .text_sm()
                        .text_color(rgb(theme.muted_text))
                        .child(message),
                )
            })
            .when(message.is_none() && self.hex.is_some(), |panel| {
                let hex = self.hex.as_ref().unwrap();
                panel.child(
                    div()
                        .id("binary-preview-scroll")
                        .debug_selector(|| "binary-preview-scroll".to_owned())
                        .flex()
                        .flex_1()
                        .h_full()
                        .min_h(px(0.0))
                        .overflow_x_scroll()
                        .child(
                            div()
                                .id("binary-preview-table")
                                .debug_selector(|| "binary-preview-table".to_owned())
                                .flex()
                                .flex_col()
                                .w_full()
                                .min_w(px(720.0))
                                .h_full()
                                .min_h(px(0.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .flex_shrink_0()
                                        .bg(rgb(theme.sidebar))
                                        .border_b_1()
                                        .border_color(rgb(theme.border))
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(rgb(theme.muted_text))
                                        .child(div().w(px(100.0)).px_2().child(language.text("Offset")))
                                        .child(div().flex_1().min_w(px(390.0)).px_2().child(language.text("Hex bytes")))
                                        .child(
                                            div()
                                                .id("binary-preview-ascii-heading")
                                                .debug_selector(|| "binary-preview-ascii-heading".to_owned())
                                                .w(px(220.0))
                                                .px_2()
                                                .child(language.text("ASCII")),
                                        ),
                                )
                                .child(
                                    gpui::uniform_list(
                                        "binary-preview-rows",
                                        hex.rows.len(),
                                        cx.processor(|preview, range: std::ops::Range<usize>, _, _| {
                                            let hex = preview.hex.as_ref().unwrap();
                                            range
                                                .filter_map(|index| hex.rows.get(index).cloned().map(|row| hex_row(index, row, preview.theme)))
                                                .collect()
                                        }),
                                    )
                                    .track_scroll(&self.scroll)
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .w_full(),
                                ),
                        )
                        .child(
                            gpui::component::scroll::Scrollbar::vertical(&self.scroll)
                                .id("binary-preview-scrollbar")
                                .mode(gpui::component::scroll::ScrollbarMode::Always),
                        ),
                )
            })
            .when(message.is_none() && self.csv.is_some(), |panel| {
                let table = self.csv.as_ref().unwrap();
                let width = 48.0 + table.headers.len() as f32 * 180.0;
                panel.child(
                    div().id("csv-preview-scroll").flex_1().min_h(px(0.0)).overflow_x_scroll()
                        .child(gpui::base::Table::new("csv-preview-table")
                            .debug_selector(|| "csv-preview-table".to_owned())
                            .flex().flex_col().w(px(width)).h_full().min_h(px(0.0))
                            .child(gpui::base::TableRow::new("csv-preview-header", 0)
                                .debug_selector(|| "csv-preview-header".to_owned())
                                .flex().h(px(28.0)).flex_shrink_0().bg(rgb(theme.sidebar))
                                .border_b_1().border_color(rgb(theme.border))
                                .child(div().w(px(48.0)).flex_shrink_0())
                                .children(table.headers.iter().enumerate().map(|(index, header)| {
                                    gpui::base::TableHead::new(("csv-column", index), index)
                                        .flex().items_center().w(px(180.0)).flex_shrink_0().px_2()
                                        .border_r_1().border_color(rgb(theme.border))
                                        .text_xs().font_weight(gpui::FontWeight::MEDIUM)
                                        .child(div().min_w(px(0.0)).truncate().child(header.replace(['\n', '\r'], " ")))
                                })))
                            .child(gpui::uniform_list("csv-preview-rows", table.rows.len(),
                                cx.processor(|preview, range: std::ops::Range<usize>, _, _| {
                                    let table = preview.csv.as_ref().unwrap();
                                    range.map(|index| {
                                        gpui::base::TableRow::new(("csv-row", index), index + 1)
                                            .debug_selector(move || format!("csv-preview-row:{index}"))
                                            .flex().h(px(26.0)).w_full().text_xs()
                                            .border_b_1().border_color(rgb(preview.theme.subtle_border))
                                            .when(index % 2 == 1, |row| row.bg(rgb(preview.theme.sidebar)))
                                            .child(div().w(px(48.0)).flex_shrink_0().px_2().text_right()
                                                .text_color(rgb(preview.theme.faint_text)).child((index + 1).to_string()))
                                            .children((0..table.headers.len()).map(|column| {
                                                gpui::base::TableCell::new(("csv-cell", column), column)
                                                    .flex().items_center().w(px(180.0)).flex_shrink_0().px_2()
                                                    .border_r_1().border_color(rgb(preview.theme.subtle_border))
                                                    .child(div().min_w(px(0.0)).truncate().child(
                                                        table.rows[index].get(column).map(|cell| {
                                                            cell.chars().take(512).collect::<String>().replace(['\n', '\r'], " ")
                                                        }).unwrap_or_default(),
                                                    ))
                                            }))
                                    }).collect()
                                }))
                                .track_scroll(&self.scroll).flex_1().min_h(px(0.0)).w_full()))
                )
            })
            .when(message.is_none() && self.csv.is_none() && self.hex.is_none(), |panel| {
                panel.child(
                    div()
                        .id("text-preview-editor")
                        .debug_selector(|| "text-preview-editor".to_owned())
                        .flex()
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_hidden()
                        .bg(rgb(theme.surface))
                        .when_some(self.editor.as_ref(), |panel, editor| {
                            panel.child(Editor::new(editor)
                                .readonly(true)
                                .appearance(false)
                                .bordered(false)
                                .aria_label(language.text("Text preview"))
                                .w_full()
                                .h_full()
                                .min_h(px(0.0))
                                .rounded_none()
                                .bg(rgb(theme.surface))
                                .text_color(rgb(theme.text))
                                .text_size(px(12.0)))
                        }),
                )
            })
    }
}

fn hex_row(index: usize, row: HexRow, theme: Theme) -> impl IntoElement {
    div()
        .id(("binary-preview-row", index))
        .debug_selector(move || format!("binary-preview-row-{index}"))
        .flex()
        .items_center()
        .w_full()
        .h(px(24.0))
        .flex_shrink_0()
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
        .when(index % 2 == 1, |row| row.bg(rgb(theme.sidebar)))
        .hover(|style| style.bg(rgb(theme.hover)))
        .text_size(px(11.0))
        .font_family("Menlo")
        .child(
            div()
                .w(px(100.0))
                .flex_shrink_0()
                .px_2()
                .text_color(rgb(theme.faint_text))
                .child(format!("{:08X}", row.offset)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(390.0))
                .px_2()
                .text_color(rgb(theme.text))
                .child(row.hex),
        )
        .child(
            div()
                .id(("binary-preview-ascii", index))
                .debug_selector(move || format!("binary-preview-ascii-{index}"))
                .w(px(220.0))
                .flex_shrink_0()
                .px_2()
                .text_color(rgb(theme.muted_text))
                .child(row.ascii),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_text_preview(target: &EntryTarget, cancel: &AtomicBool) -> TextContent {
        match load(target, cancel).unwrap() {
            PreviewData::Text(content) => content,
            PreviewData::Binary(_) => panic!("expected text preview"),
        }
    }

    #[gpui::test]
    fn preview_editor_receives_code_language_for_plain_archived_and_compressed_files(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("main.rs");
        let code = "fn main() { let answer = 42; println!(\"hello\"); }\n";
        std::fs::write(&source, code).unwrap();
        let archive = directory.path().join("source.zip");
        crate::archive::create_zip(&archive, std::slice::from_ref(&source)).unwrap();
        let compressed = directory.path().join("main.rs.gz");
        crate::archive::create_archive(
            &compressed,
            std::slice::from_ref(&source),
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::Gzip,
                ..Default::default()
            },
        )
        .unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        for (path, entry) in [
            (source, None),
            (archive, Some("main.rs")),
            (compressed, None),
        ] {
            let target = EntryTarget {
                name: path.file_name().unwrap().to_string_lossy().into_owned(),
                path,
                key: "test".into(),
                archive_entry: entry.map(str::to_owned),
                password: None,
                is_directory: false,
            };
            cx.update(|window, cx| {
                view.update(cx, |manager, cx| {
                    manager.preview_text_target(&target, window, cx)
                })
            });
            cx.run_until_parked();
            cx.read(|cx| {
                let manager = view.read(cx);
                let preview = manager.inspector.text_preview.as_ref().unwrap().read(cx);
                let editor = preview.editor.as_ref().unwrap().read(cx);
                assert_eq!(editor.language_name().as_ref(), "rs");
                assert_eq!(editor.value().as_str(), code);
            });
            assert!(cx.debug_bounds("text-preview-editor").is_some());
        }
    }

    #[test]
    fn preview_syntax_uses_the_original_filename_inside_archives_and_compression() {
        let mut target = EntryTarget {
            name: "main.RS".into(),
            key: "test".into(),
            path: "main.RS".into(),
            archive_entry: None,
            password: None,
            is_directory: false,
        };
        assert_eq!(syntax_language(&target), "rs");
        target.path = "project.zip".into();
        target.archive_entry = Some("src/config.JSON".into());
        assert_eq!(syntax_language(&target), "json");
        target.archive_entry = None;
        target.name = "script.py.gz".into();
        target.path = target.name.clone().into();
        assert_eq!(syntax_language(&target), "py");
        for (name, expected) in [
            ("Dockerfile", "dockerfile"),
            ("Makefile", "makefile"),
            (".bashrc", "sh"),
            ("README", "text"),
        ] {
            target.name = name.into();
            target.path = name.into();
            assert_eq!(syntax_language(&target), expected);
        }
    }

    #[gpui::test]
    fn text_preview_editor_is_readonly_scrollable_and_matches_the_theme(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let short = directory.path().join("short.txt");
        let long = directory.path().join("long.txt");
        std::fs::write(&short, "short line\n").unwrap();
        let line = "m".repeat(300);
        std::fs::write(
            &long,
            (0..1000).map(|_| format!("{line}\n")).collect::<String>(),
        )
        .unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        cx.simulate_resize(gpui::size(px(1100.0), px(720.0)));
        cx.run_until_parked();
        let mut target = EntryTarget {
            name: "short.txt".into(),
            key: "preview".into(),
            path: short,
            archive_entry: None,
            password: None,
            is_directory: false,
        };
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.preview_text_target(&target, window, cx)
            })
        });
        cx.run_until_parked();
        let editor = cx.read(|cx| {
            let manager = view.read(cx);
            let preview = manager.inspector.text_preview.as_ref().unwrap().read(cx);
            preview.editor.clone().unwrap()
        });
        cx.update(|window, cx| editor.update(cx, |editor, cx| editor.focus(window, cx)));
        cx.simulate_input("must not edit");
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(editor.read(cx).value().as_str(), "short line\n"));
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-a cmd-c");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-a ctrl-c");
        cx.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "short line\n"
            )
        });
        editor.update(cx, |editor, cx| {
            editor.set_scroll_offset(gpui::point(px(-100_000.0), px(-100_000.0)), cx)
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                editor.read(cx).scroll_offset(),
                gpui::point(px(0.0), px(0.0))
            );
        });
        target.name = "long.txt".into();
        target.path = long;
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.preview_text_target(&target, window, cx)
            })
        });
        cx.run_until_parked();
        let editor = cx.read(|cx| {
            view.read(cx)
                .inspector
                .text_preview
                .as_ref()
                .unwrap()
                .read(cx)
                .editor
                .clone()
                .unwrap()
        });
        let viewport = cx.debug_bounds("text-preview-editor").unwrap();
        let header = cx.debug_bounds("text-preview-header").unwrap();
        editor.update(cx, |editor, cx| {
            editor.set_scroll_offset(gpui::point(px(-100_000.0), px(-100_000.0)), cx)
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(editor.read(cx).scroll_offset().x < px(0.0));
            assert!(editor.read(cx).scroll_offset().y < px(0.0));
        });
        assert_eq!(cx.debug_bounds("text-preview-header").unwrap(), header);
        cx.simulate_resize(gpui::size(px(1800.0), px(720.0)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("text-preview-editor").unwrap().size.width > viewport.size.width);
        for mode in [crate::ThemeMode::Light, crate::ThemeMode::Dark] {
            view.update(cx, |manager, cx| {
                manager.theme_mode = mode;
                cx.notify();
            });
            cx.run_until_parked();
            cx.read(|cx| {
                let palette = Theme::new(mode == crate::ThemeMode::Dark);
                let style = &cx.global::<gpui::component::Theme>().highlight_theme.style;
                assert_eq!(style.editor_background, Some(rgb(palette.surface).into()));
                assert_eq!(
                    style.editor_gutter_background,
                    Some(rgb(palette.surface).into())
                );
            });
        }
    }

    #[gpui::test]
    fn csv_preview_renders_virtualized_tables_for_plain_archived_and_compressed_files(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("items.CSV");
        let text = format!(
            "Name,Note,Value\n{}",
            (0..1000)
                .map(|index| { format!("item {index},\"first\nsecond\",{index}\n") })
                .collect::<String>()
        );
        std::fs::write(&source, &text).unwrap();
        let archive = directory.path().join("items.zip");
        crate::archive::create_zip(&archive, std::slice::from_ref(&source)).unwrap();
        let compressed = directory.path().join("items.csv.gz");
        crate::archive::create_archive(
            &compressed,
            std::slice::from_ref(&source),
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::Gzip,
                ..Default::default()
            },
        )
        .unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = crate::browser::read_directory(directory.path()).unwrap();
            manager
        });
        cx.update(|window, cx| {
            window.click(
                gpui::SharedString::from(source.to_string_lossy().into_owned()),
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("csv-preview-table").is_some());
        assert!(cx.debug_bounds("csv-preview-row:0").is_some());
        assert!(cx.debug_bounds("csv-preview-row:999").is_none());
        let preview = cx.read(|cx| view.read(cx).inspector.text_preview.clone().unwrap());
        cx.read(|cx| {
            let table = preview.read(cx).csv.as_ref().unwrap();
            assert_eq!(table.rows.len(), 1000);
            assert_eq!(table.headers[0].as_ref(), "Name");
            assert_eq!(table.rows[0][1].as_ref(), "first\nsecond");
        });
        let header = cx.debug_bounds("csv-preview-header").unwrap();
        preview.update(cx, |preview, cx| {
            preview
                .scroll
                .scroll_to_item(999, gpui::ScrollStrategy::Bottom);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("csv-preview-row:999").is_some());
        assert!(cx.debug_bounds("csv-preview-row:0").is_none());
        assert_eq!(cx.debug_bounds("csv-preview-header").unwrap(), header);
        cx.update(|window, cx| window.click("copy-text-preview", cx));
        cx.read(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), text));
        cx.update(|window, cx| {
            window.click(
                gpui::SharedString::from(compressed.to_string_lossy().into_owned()),
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("csv-preview-table").is_some());
        cx.update(|_, cx| {
            view.update(cx, |manager, cx| {
                manager.install_archive(
                    archive.clone(),
                    crate::archive::list_archive(&archive).unwrap(),
                    None,
                    cx,
                )
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.click("items.CSV", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("csv-preview-table").is_some());
        cx.read(|cx| {
            assert_eq!(
                view.read(cx)
                    .inspector
                    .text_preview
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .csv
                    .as_ref()
                    .unwrap()
                    .rows
                    .len(),
                1000
            )
        });
    }

    #[test]
    fn csv_preview_parses_quoted_multiline_fields_and_common_separators() {
        for (text, expected) in [
            (
                "name,value\r\n\"Doe, Jane\",\"a\"\"b\"\r\n",
                vec!["Doe, Jane", "a\"b"],
            ),
            (
                "name;value\nZażółć;\"first\nsecond\"\n",
                vec!["Zażółć", "first\nsecond"],
            ),
            ("name\tvalue\n東京\t42\n", vec!["東京", "42"]),
        ] {
            let table = CsvTable::parse(&TextContent {
                text: text.into(),
                truncated: false,
            })
            .unwrap();
            assert_eq!(
                table
                    .headers
                    .iter()
                    .map(|value| value.as_ref())
                    .collect::<Vec<_>>(),
                ["name", "value"]
            );
            assert_eq!(
                table.rows[0]
                    .iter()
                    .map(|value| value.as_ref())
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(!table.truncated);
        }
        let table = CsvTable::parse(&TextContent {
            text: "name,value\ncomplete,1\npartial,\"missing".into(),
            truncated: true,
        })
        .unwrap();
        assert_eq!(table.rows.len(), 1);
        assert!(table.truncated);
        let table = CsvTable::parse(&TextContent {
            text: "name,value\na\nb,2,extra\n".into(),
            truncated: false,
        })
        .unwrap();
        assert_eq!(table.headers.len(), 3);
        assert_eq!(table.headers[2].as_ref(), "3");
    }

    #[test]
    fn csv_preview_limits_rows_and_columns() {
        let content = TextContent {
            text: format!("value\n{}", "data\n".repeat(6000)),
            truncated: false,
        };
        let table = CsvTable::parse(&content).unwrap();
        assert_eq!(table.rows.len(), 5000);
        assert!(table.truncated);
        let record = (0..80)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let content = TextContent {
            text: format!("{record}\n{record}\n"),
            truncated: false,
        };
        let table = CsvTable::parse(&content).unwrap();
        assert_eq!(table.headers.len(), 64);
        assert_eq!(table.rows[0].len(), 64);
        assert!(table.truncated);
    }

    #[gpui::test]
    fn text_preview_ui_handles_plain_archived_compressed_and_binary_files(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("notes.txt");
        std::fs::write(&source, "preview content\n文字 🙂\n").unwrap();
        let archive = directory.path().join("notes.zip");
        crate::archive::create_zip(&archive, std::slice::from_ref(&source)).unwrap();
        let compressed = directory.path().join("notes.txt.gz");
        crate::archive::create_archive(
            &compressed,
            std::slice::from_ref(&source),
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::Gzip,
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::write(directory.path().join("binary.bin"), [0, 1, 2, 255]).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = crate::browser::read_directory(directory.path()).unwrap();
            manager
        });
        cx.update(|window, cx| {
            window.click(
                gpui::SharedString::from(source.to_string_lossy().into_owned()),
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("text-preview").is_some());
        cx.read(|cx| {
            let manager = view.read(cx);
            let preview = manager.inspector.text_preview.as_ref().unwrap().read(cx);
            assert!(preview.task.is_none());
            assert_eq!(
                preview.content.as_ref().unwrap().text,
                "preview content\n文字 🙂\n"
            );
        });
        cx.update(|window, cx| window.click("copy-text-preview", cx));
        cx.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "preview content\n文字 🙂\n"
            )
        });
        cx.update(|window, cx| {
            window.click(
                gpui::SharedString::from(compressed.to_string_lossy().into_owned()),
                cx,
            )
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(
                manager
                    .inspector
                    .text_preview
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .content
                    .as_ref()
                    .unwrap()
                    .text,
                "preview content\n文字 🙂\n"
            );
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.install_archive(
                    archive.clone(),
                    crate::archive::list_archive(&archive).unwrap(),
                    None,
                    cx,
                )
            });
            window.render_frame(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.click("notes.txt", cx));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(
                manager
                    .inspector
                    .text_preview
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .content
                    .as_ref()
                    .unwrap()
                    .text,
                "preview content\n文字 🙂\n"
            );
        });
        cx.update(|window, cx| window.click("close-text-preview", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("text-preview").is_none());
        cx.update(|_, cx| {
            view.update(cx, |manager, cx| {
                manager.navigate_to(directory.path().to_path_buf(), cx)
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.click(
                gpui::SharedString::from(
                    directory
                        .path()
                        .join("binary.bin")
                        .to_string_lossy()
                        .into_owned(),
                ),
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("binary-preview-table").is_some());
        assert!(cx.debug_bounds("binary-preview-row-0").is_some());
        assert!(cx.debug_bounds("binary-preview-ascii-0").unwrap().size.width >= px(220.0));
        assert!(cx.debug_bounds("binary-preview-table").unwrap().size.width >= px(720.0));
        let preview_bounds = cx.debug_bounds("text-preview").unwrap();
        let scroll_bounds = cx.debug_bounds("binary-preview-scroll").unwrap();
        let table_bounds = cx.debug_bounds("binary-preview-table").unwrap();
        assert!(scroll_bounds.size.height > px(100.0));
        assert!(
            scroll_bounds.bottom() >= preview_bounds.bottom() - px(2.0),
            "binary viewport {:?}, table {:?}, preview {:?}",
            scroll_bounds,
            table_bounds,
            preview_bounds
        );
        cx.read(|cx| {
            let manager = view.read(cx);
            let preview = manager.inspector.text_preview.as_ref().unwrap().read(cx);
            assert!(preview.content.is_none());
            let row = &preview.hex.as_ref().unwrap().rows[0];
            assert_eq!(row.hex, "00 01 02 FF");
            assert_eq!(row.ascii, "....");
        });
    }

    #[gpui::test]
    fn text_preview_cancels_stale_reads_when_selection_changes(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                let mut target = EntryTarget {
                    name: "first.txt".into(),
                    key: "first".into(),
                    path: first,
                    archive_entry: None,
                    password: None,
                    is_directory: false,
                };
                manager.preview_text_target(&target, window, cx);
                let cancel = manager.inspector.text_cancel.clone();
                target.name = "second.txt".into();
                target.path = second;
                manager.preview_text_target(&target, window, cx);
                assert!(cancel.load(Ordering::Relaxed));
            })
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(
                manager
                    .inspector
                    .text_preview
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .content
                    .as_ref()
                    .unwrap()
                    .text,
                "second"
            );
        });
    }

    #[test]
    fn text_preview_reads_files_and_all_created_archive_formats() {
        use crate::archive::{CreateOptions, ResolvedFormat};
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("文字.txt");
        let text = "Zażółć gęślą jaźń 中文 🙂\nsecond line\n";
        std::fs::write(&source, text).unwrap();
        let cancel = AtomicBool::new(false);
        let mut target = crate::file_actions::EntryTarget {
            name: "文字.txt".into(),
            key: "test".into(),
            path: source.clone(),
            archive_entry: None,
            password: None,
            is_directory: false,
        };
        assert_eq!(load_text_preview(&target, &cancel).text, text);
        for format in ResolvedFormat::CREATION_FORMATS {
            let path = directory
                .path()
                .join(format!("preview.{}", format.extension()));
            crate::archive::create_archive(
                &path,
                std::slice::from_ref(&source),
                CreateOptions {
                    format,
                    compression: format.compression_levels()[0],
                    ..CreateOptions::default()
                },
            )
            .unwrap();
            target.path = path;
            target.archive_entry = if format.is_single_compressed() {
                Some(crate::archive::single_compressed_entry_name(&target.path).unwrap())
            } else {
                Some("文字.txt".into())
            };
            let preview = load_text_preview(&target, &cancel);
            assert_eq!(preview.text, text, "{}", format.label());
            assert!(!preview.truncated);
            if format.is_single_compressed() {
                target.archive_entry = None;
                assert_eq!(load_text_preview(&target, &cancel).text, text);
            }
        }
    }

    #[test]
    fn text_preview_reads_protected_archives_and_limits_large_entries() {
        use crate::archive::{CreateOptions, ResolvedFormat};
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("large.txt");
        std::fs::write(&source, vec![b'a'; MAX_BYTES * 8]).unwrap();
        let cancel = AtomicBool::new(false);
        for format in [ResolvedFormat::Zip, ResolvedFormat::SevenZip] {
            let path = directory
                .path()
                .join(format!("protected.{}", format.extension()));
            crate::archive::create_archive_with_password(
                &path,
                std::slice::from_ref(&source),
                CreateOptions {
                    format,
                    ..CreateOptions::default()
                },
                Some("preview-password"),
            )
            .unwrap();
            let target = crate::file_actions::EntryTarget {
                name: "large.txt".into(),
                key: "test".into(),
                path,
                archive_entry: Some("large.txt".into()),
                password: Some(std::sync::Arc::new(zeroize::Zeroizing::new(
                    "preview-password".into(),
                ))),
                is_directory: false,
            };
            let preview = load_text_preview(&target, &cancel);
            assert!(preview.truncated);
            assert_eq!(preview.text.len(), MAX_BYTES);
            let mut invalid = target.clone();
            invalid.password = Some(std::sync::Arc::new(zeroize::Zeroizing::new("wrong".into())));
            assert!(load(&invalid, &cancel).is_err());
        }
    }

    #[test]
    fn text_preview_decodes_unicode_and_rejects_binary() {
        for text in ["", "hello\nworld\t!", "Zażółć gęślą jaźń 中文 🙂"] {
            assert_eq!(decode(text.as_bytes()).unwrap().text, text);
            let mut utf16 = vec![0xff, 0xfe];
            utf16.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            assert_eq!(decode(&utf16).unwrap().text, text);
            let mut utf16 = vec![0xfe, 0xff];
            utf16.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
            assert_eq!(decode(&utf16).unwrap().text, text);
        }
        assert_eq!(decode(b"\xef\xbb\xbftext").unwrap().text, "text");
        assert!(decode(b"text\0binary").is_none());
        assert!(decode(&[0xff, 0x80, 0x01]).is_none());
    }

    #[test]
    fn hex_preview_formats_offsets_bytes_and_truncates_to_preview_limit() {
        let preview = hex_preview(b"Hello, binary!");
        assert_eq!(preview.rows.len(), 1);
        assert_eq!(preview.rows[0].offset, 0);
        assert_eq!(preview.rows[0].hex, "48 65 6C 6C 6F 2C 20 62 69 6E 61 72 79 21");
        assert_eq!(preview.rows[0].ascii, "Hello, binary!");
        assert!(!preview.truncated);

        let large = vec![0_u8; MAX_BYTES + 1];
        let preview = hex_preview(&large);
        assert!(preview.truncated);
        assert_eq!(preview.rows.len(), MAX_BYTES / 16);
        assert_eq!(preview.rows.last().unwrap().offset, MAX_BYTES - 16);
    }

    #[test]
    fn text_preview_limits_reads_and_handles_split_utf8() {
        let cancel = AtomicBool::new(false);
        let mut input = io::repeat(b'a');
        let bytes = read_prefix(&mut input, &cancel).unwrap();
        assert_eq!(bytes.len(), MAX_BYTES + 1);
        assert!(decode(&bytes).unwrap().truncated);
        let mut bytes = vec![b'a'; MAX_BYTES - 1];
        bytes.extend_from_slice("🙂".as_bytes());
        assert_eq!(decode(&bytes).unwrap().text.len(), MAX_BYTES - 1);
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            read_prefix(&mut input, &cancel).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }
}
