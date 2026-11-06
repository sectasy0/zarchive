use crate::{ArchiveManager, Theme, file_actions::EntryTarget, i18n::Language};
use anyhow::Context as _;
use gpui::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
    input::{Editor, EditorState},
};
use gpui::{Context, IntoElement, Render, Window, div, prelude::*, px, rgb};
mod loader;
pub(super) use loader::*;

use std::{
    io::{self, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) struct Preview {
    target: EntryTarget,
    manager: gpui::WeakEntity<ArchiveManager>,
    content: Option<TextContent>,
    hex: Option<HexPreview>,
    raw_bytes: Vec<u8>,
    editor: Option<gpui::Entity<EditorState>>,
    csv: Option<CsvTable>,
    hex_editor: Option<gpui::Entity<EditorState>>,
    error: Option<String>,
    editing: bool,
    saving: bool,
    loading_more: bool,
    load_more_error: Option<String>,
    cancel: Arc<AtomicBool>,
    task: Option<u64>,
    scroll: gpui::UniformListScrollHandle,
    pub theme: Theme,
    pub language: Language,
    limits: crate::preferences::AdvancedSettings,


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
        let limits = self.advanced_settings;
        let max_bytes = limits.preview_max_bytes();
        let worker_target = target.clone();
        let worker_cancel = cancel.clone();
        let preview = cx.new(|_: &mut Context<Preview>| Preview {
            target,
            manager,
            content: None,
            hex: None,
            raw_bytes: Vec::new(),
            editor: None,
            csv: None,
            hex_editor: None,
            error: None,
            editing: false,
            saving: false,
            loading_more: false,
            load_more_error: None,
            cancel: cancel.clone(),
            task: None,
            scroll: gpui::UniformListScrollHandle::new(),
            theme,
            language,
            limits,
        });
        let view = preview.downgrade();
        let task_id = self.background_queue.enqueue(


            "Loading text preview...",
            crate::task_queue::TaskControl::with_cancel(cancel).for_paths([worker_target.path.clone()]),
            move |control, reporter| async move {
                reporter.set(0, 1);
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                let loaded = load_with_limit(&worker_target, &worker_cancel, max_bytes)?;
                let table = match &loaded.data {
                    PreviewData::Text(content) if is_csv_target(&worker_target) => {
                        CsvTable::parse_with_limits(
                            content,
                            limits.csv_max_rows as usize,
                            limits.csv_max_columns as usize,
                        )
                    }
                    _ => None,
                };
                let result = (loaded, table);
                reporter.set(1, 1);


                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                Ok(result)
            },
            move |_, result, window, cx| {


                let _ = view.update(cx, |preview, cx| {
                    if preview.cancel.load(Ordering::Relaxed) { preview.task = None; cx.notify(); return; }
                            match result {
                                Ok((loaded, table)) => {
                                    preview.raw_bytes = loaded.raw_bytes;
                                    match loaded.data {
                                        PreviewData::Text(content) => {
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
                                        PreviewData::Binary(hex) => preview.hex = Some(hex),
                                    }
                                }
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

    fn enqueue_preview_remainder(
        &mut self,
        target: EntryTarget,
        prefix: Vec<u8>,
        read_remainder: bool,
        cancel: Arc<AtomicBool>,
        preview: gpui::WeakEntity<Preview>,
        cx: &mut Context<Self>,
    ) {
        let offset = prefix.len();
        let worker_target = target.clone();
        let worker_cancel = cancel.clone();
        self.background_queue.enqueue(
            "Loading remaining preview...",
            crate::task_queue::TaskControl::with_cancel(cancel)
                .for_paths([target.path.clone()]),
            move |control, reporter| async move {
                if !control.checkpoint() {
                    anyhow::bail!("cancelled");
                }
                let remainder = if read_remainder {
                    load_remaining(&worker_target, &worker_cancel, offset)?
                } else {
                    Vec::new()
                };
                let mut bytes = prefix;
                bytes.extend_from_slice(&remainder);
                let loaded = loaded_preview_from_bytes(bytes, usize::MAX);
                let table = match &loaded.data {
                    PreviewData::Text(content) if is_csv_target(&worker_target) => {
                        CsvTable::parse_with_limits(content, usize::MAX, usize::MAX)
                    }
                    _ => None,
                };
                reporter.set(1, 1);
                if !control.checkpoint() {
                    anyhow::bail!("cancelled");
                }
                Ok((loaded, table))
            },
            move |_, result, window, cx| {
                let _ = preview.update(cx, |preview, cx| {
                    preview.loading_more = false;
                    match result {
                        Ok((loaded, table)) => {
                            preview.raw_bytes = loaded.raw_bytes;
                            preview.load_more_error = None;
                            preview.csv = table;
                            preview.editor = None;
                            preview.content = None;
                            preview.hex = None;
                            match loaded.data {
                                PreviewData::Text(content) => {
                                    if preview.csv.is_none() {
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
                                    preview.content = Some(content);
                                }
                                PreviewData::Binary(hex) => preview.hex = Some(hex),
                            }
                        }
                        Err(crate::task_queue::TaskFailure::Cancelled) => {}
                        Err(crate::task_queue::TaskFailure::Failed(error)) => {
                            preview.load_more_error = Some(format!("{error:#}"));
                        }
                    }
                    cx.notify();
                });
            },
            cx,
        );
    }

    fn save_preview_contents(
        &mut self,
        target: EntryTarget,
        contents: Vec<u8>,
        preview: gpui::WeakEntity<Preview>,
        cx: &mut Context<Self>,
    ) {
        let worker_target = target.clone();
        let password = target
            .password
            .as_ref()
            .map(|password| password.as_str().to_owned());
        let resource = target.path.clone();
        self.background_queue.enqueue(
            "Saving file...",
            crate::task_queue::TaskControl::default().for_paths([resource]),
            move |control, _| async move {
                if !control.checkpoint() {
                    anyhow::bail!("save cancelled");
                }
                if let Some(entry) = &worker_target.archive_entry {
                    crate::archive::replace_archive_entry_with_password(
                        &worker_target.path,
                        entry,
                        &contents,
                        password.as_deref(),
                    )?;
                } else {
                    let format = crate::archive::ResolvedFormat::from_path(&worker_target.path);
                    anyhow::ensure!(
                        !format.is_some_and(|format| format.is_single_compressed()),
                        "Editing standalone compressed streams is not supported"
                    );
                    let parent = worker_target
                        .path
                        .parent()
                        .context("File has no parent directory")?;
                    let metadata = std::fs::metadata(&worker_target.path)?;
                    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
                    std::io::Write::write_all(&mut staged, &contents)?;
                    staged.as_file().set_permissions(metadata.permissions())?;
                    staged.as_file().sync_all()?;
                    staged
                        .persist(&worker_target.path)
                        .map_err(|error| error.error)?;
                }
                if !control.checkpoint() {
                    anyhow::bail!("save cancelled");
                }
                Ok(())
            },
            move |manager, result, window, cx| match result {
                Ok(()) => {
                    manager.status_message = Some(manager.language.text("File saved").to_owned());
                    if target.archive_entry.is_some() {
                        manager.inspect_sources(
                            vec![(target.path.clone(), target.password.clone())],
                            crate::inspection::InspectionMode::Preview,
                            window,
                            cx,
                        );
                    } else if let (Some(directory), Ok(metadata)) =
                        (manager.current_directory.as_ref(), std::fs::metadata(&target.path))
                    {
                        for entry in &mut manager.entries {
                            let entry_path = directory.join(
                                entry.source_path.as_deref().unwrap_or(&entry.name),
                            );
                            if entry_path == target.path {
                                entry.size = metadata.len();
                            }
                        }
                    }
                    manager.preview_text_target(&target, window, cx);
                }
                Err(crate::task_queue::TaskFailure::Cancelled) => {
                    let _ = preview.update(cx, |preview, cx| {
                        preview.saving = false;
                        cx.notify();
                    });
                }
                Err(crate::task_queue::TaskFailure::Failed(error)) => {
                    let _ = preview.update(cx, |preview, cx| {
                        preview.saving = false;
                        preview.editing = true;
                        preview.error = Some(format!("{}: {error:#}", preview.language.text("Could not save file")));
                        cx.notify();
                    });
                }
            },
            cx,
        );
    }
}

impl Preview {
    pub(crate) fn matches_target(&self, target: &EntryTarget) -> bool {
        self.target.path == target.path && self.target.archive_entry == target.archive_entry
    }
    fn load_remaining(&mut self, cx: &mut Context<Self>) {
        if self.loading_more {
            return;
        }
        self.loading_more = true;
        self.load_more_error = None;
        let target = self.target.clone();
        let prefix = self.raw_bytes.clone();
        let read_remainder = self
            .content
            .as_ref()
            .is_some_and(|content| content.truncated)
            || self.hex.as_ref().is_some_and(|hex| hex.truncated);
        let cancel = self.cancel.clone();
        let preview = cx.entity().downgrade();
        let _ = self.manager.update(cx, |manager, cx| {
            manager.enqueue_preview_remainder(
                target,
                prefix,
                read_remainder,
                cancel,
                preview,
                cx,
            );
        });
        cx.notify();
    }

    fn can_edit(&self) -> bool {
        let complete = self
            .content
            .as_ref()
            .is_some_and(|content| !content.truncated)
            || self.hex.as_ref().is_some_and(|hex| !hex.truncated);
        if !complete {
            return false;
        }
        if self.target.archive_entry.is_some() {
            crate::archive::can_replace_archive_entry(&self.target.path)
        } else {
            std::fs::metadata(&self.target.path)
                .is_ok_and(|metadata| metadata.is_file() && !metadata.permissions().readonly())
                && !crate::archive::ResolvedFormat::from_path(&self.target.path)
                    .is_some_and(|format| format.is_single_compressed())
        }
    }

    fn begin_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit() {
            return;
        }


        if let Some(hex) = &self.hex {
            let value = hex
                .rows
                .iter()
                .map(|row| row.hex.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            self.hex_editor = Some(cx.new(|cx| {
                EditorState::new(window, cx)
                    .line_number(true)
                    .soft_wrap(false)
                    .scroll_bounce(false)
                    .default_value(value)
            }));
        } else if self.editor.is_none() {
            if let Some(content) = &self.content {
                self.editor = Some(cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(syntax_language(&self.target))
                        .line_number(true)
                        .soft_wrap(false)
                        .scroll_bounce(false)
                        .default_value(content.text.clone())
                }));
            }
        }
        self.error = None;
        self.editing = true;
        cx.notify();
    }

    fn cancel_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(content) = &self.content {
            let value = content.text.clone();
            let language = syntax_language(&self.target);
            self.editor = Some(cx.new(|cx| {
                EditorState::new(window, cx)
                    .language(language)
                    .line_number(true)
                    .soft_wrap(false)
                    .scroll_bounce(false)
                    .default_value(value)
            }));
        }
        self.hex_editor = None;
        self.error = None;
        self.editing = false;
        cx.notify();
    }

    fn save_editing(&mut self, cx: &mut Context<Self>) {
        let contents = if self.hex.is_some() {
            let Some(editor) = &self.hex_editor else {
                return;
            };
            match parse_hex_bytes(editor.read(cx).value().as_str(), self.limits.preview_max_bytes()) {
                Ok(contents) => contents,
                Err(_) => {
                    self.error = Some(self.language.text("Invalid hex data").to_owned());
                    cx.notify();
                    return;
                }
            }
        } else {
            let Some(editor) = &self.editor else {
                return;
            };
            let text = editor.read(cx).value().to_string();
            if text.len() > self.limits.preview_max_bytes() {
                self.error = Some(
                    self.language
                        .text("Edited preview exceeds the configured size limit")
                        .to_owned(),
                );
                cx.notify();
                return;
            }
            text.into_bytes()
        };
        self.error = None;
        self.saving = true;
        let target = self.target.clone();
        let preview = cx.entity().downgrade();
        let _ = self.manager.update(cx, |manager, cx| {
            manager.save_preview_contents(target, contents, preview, cx);
        });
        cx.notify();
    }
}

fn parse_hex_bytes(text: &str, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let tokens: Vec<_> = text.split_whitespace().collect();
    anyhow::ensure!(tokens.len() <= max_bytes, "Hex data exceeds the configured preview limit");
    tokens
        .into_iter()
        .map(|token| {
            anyhow::ensure!(token.len() == 2, "Each byte must use exactly two hex digits");
            Ok(u8::from_str_radix(token, 16)?)
        })
        .collect()
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
        } else if self.saving {
            Some(language.text("Saving file..."))
        } else if !self.editing && let Some(error) = &self.error {
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
            if self.editing { "Hex editor" } else { "Binary preview" }
        } else if self.editing {
            "Text editor"
        } else if self.csv.is_some() {
            "CSV preview"
        } else {
            "Text preview"
        };
        let can_edit = self.can_edit();
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
                    } else if self.csv.is_some() {
                        crate::app_icon("csv", 14.0, theme.accent).into_any_element()
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
                            .tooltip(language.text(if self.hex.is_some() { "Copy hex" } else { "Copy text" }))
                            .accessibility_label(language.text(if self.hex.is_some() { "Copy hex" } else { "Copy text" }))
                            .disabled(self.content.is_none() && self.hex.is_none())
                            .on_click(cx.listener(|preview, _, _, cx| {
                                if preview.editing {
                                    if let Some(editor) = &preview.hex_editor {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            editor.read(cx).value().to_string(),
                                        ));
                                    } else if let Some(editor) = &preview.editor {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            editor.read(cx).value().to_string(),
                                        ));
                                    }
                                } else if let Some(content) = &preview.content {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        content.text.clone(),
                                    ));
                                } else if let Some(hex) = &preview.hex {
                                    let text = hex.rows.iter().map(|row| row.hex.as_str()).collect::<Vec<_>>().join("\n");
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                                }
                            })),
                    )
                    .when(!self.editing && (self.content.is_some() || self.hex.is_some()), |header| {
                        header.child(
                            Button::new("edit-preview")
                                .debug_selector(|| "edit-preview".to_owned())
                                .ghost()
                                .xsmall()
                                .icon(gpui::assets::IconName::Pencil)
                                .tooltip(language.text(if can_edit { "Edit file" } else { "Editing unavailable" }))
                                .accessibility_label(language.text("Edit file"))
                                .disabled(!can_edit)
                                .on_click(cx.listener(|preview, _, window, cx| {
                                    preview.begin_editing(window, cx);
                                })),
                        )
                    })
                    .when(self.editing, |header| {
                        header
                            .child(
                                Button::new("save-preview-edits")
                                    .ghost()
                                    .xsmall()
                                    .icon(gpui::assets::IconName::Save)
                                    .tooltip(language.text("Save changes"))
                                    .accessibility_label(language.text("Save changes"))
                                    .disabled(self.saving)
                                    .on_click(cx.listener(|preview, _, _, cx| {
                                        preview.save_editing(cx);
                                    })),
                            )
                            .child(
                                Button::new("cancel-preview-edits")
                                    .ghost()
                                    .xsmall()
                                    .icon(gpui::assets::IconName::X)
                                    .tooltip(language.text("Cancel editing"))
                                    .accessibility_label(language.text("Cancel editing"))
                                    .disabled(self.saving)
                                    .on_click(cx.listener(|preview, _, window, cx| {
                                        preview.cancel_editing(window, cx);
                                    })),
                            )
                    })
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
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .py_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .text_xs()
                                    .text_color(rgb(theme.muted_text))
                                    .child(
                                        language.text(
                                            "Preview truncated to keep the application responsive",
                                        ),
                                    ),
                            )
                            .child(
                                Button::new("load-remaining-preview")
                                    .debug_selector(|| "load-remaining-preview".to_owned())
                                    .ghost()
                                    .xsmall()
                                    .label(language.text(if self.loading_more {
                                        "Loading remaining content..."
                                    } else {
                                        "Load remaining content"
                                    }))
                                    .tooltip(language.text("May use significant memory"))
                                    .disabled(self.loading_more)
                                    .on_click(cx.listener(|preview, _, _, cx| {
                                        preview.load_remaining(cx);
                                    })),
                            ),
                    )
                },
            )
            .when_some(self.load_more_error.as_deref(), |panel, error| {
                panel.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(if theme.window < 0x808080 {
                            0xfca5a5
                        } else {
                            0xb91c1c
                        }))
                        .child(language.error_message(error)),
                )
            })
            .when(self.editing && self.error.is_some(), |panel| {
                panel.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(if theme.window < 0x808080 { 0xfca5a5 } else { 0xb91c1c }))
                        .child(language.error_message(self.error.as_deref().unwrap_or_default())),
                )
            })
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
                .when(message.is_none() && self.editing && self.hex.is_none(), |panel| {
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
                                panel.child(
                                    Editor::new(editor)
                                        .readonly(false)
                                        .appearance(false)
                                        .bordered(false)
                                        .aria_label(language.text("Text editor"))
                                        .w_full()
                                        .h_full()
                                        .min_h(px(0.0))
                                        .rounded_none()
                                        .bg(rgb(theme.surface))
                                        .text_color(rgb(theme.text))
                                        .text_size(px(12.0)),
                                )
                            }),
                    )
                })
                .when(message.is_none() && self.editing && self.hex.is_some(), |panel| {
                    panel.child(
                        div()
                            .id("binary-preview-editor")
                            .debug_selector(|| "binary-preview-editor".to_owned())
                            .flex()
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_hidden()
                            .bg(rgb(theme.surface))
                            .when_some(self.hex_editor.as_ref(), |panel, editor| {
                                panel.child(
                                    Editor::new(editor)
                                        .readonly(false)
                                        .appearance(false)
                                        .bordered(false)
                                        .aria_label(language.text("Hex editor"))
                                        .w_full()
                                        .h_full()
                                        .min_h(px(0.0))
                                        .rounded_none()
                                        .bg(rgb(theme.surface))
                                        .text_color(rgb(theme.text))
                                        .text_size(px(12.0)),
                                )
                            }),
                    )
                })
                .when(message.is_none() && !self.editing && self.hex.is_some(), |panel| {
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
            .when(message.is_none() && !self.editing && self.csv.is_some(), |panel| {
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
            .when(message.is_none() && !self.editing && self.csv.is_none() && self.hex.is_none(), |panel| {
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
        assert!(cx.debug_bounds("csv-preview-table").is_none());
        cx.simulate_keystrokes("space");
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
        cx.simulate_keystrokes("space");
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
        cx.simulate_keystrokes("space");
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

    #[test]
    fn csv_preview_uses_configured_limits() {
        let content = TextContent {
            text: "a,b,c\n1,2,3\n4,5,6\n7,8,9\n".into(),
            truncated: false,
        };
        let table = CsvTable::parse_with_limits(&content, 2, 2).unwrap();
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.headers.len(), 2);
        assert_eq!(table.rows[0].len(), 2);
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
        assert!(cx.debug_bounds("text-preview").is_none());
        cx.simulate_keystrokes("space");
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
        cx.simulate_keystrokes("space");
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
        cx.simulate_keystrokes("space");
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
        cx.simulate_keystrokes("space");
        cx.run_until_parked();
        assert!(cx.debug_bounds("binary-preview-table").is_some());
        assert!(cx.debug_bounds("binary-preview-row-0").is_some());
        assert!(cx.debug_bounds("edit-preview").is_some());
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
    fn preview_editor_saves_local_text_and_binary_files(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let text_path = directory.path().join("notes.txt");
        let csv_path = directory.path().join("data.csv");
        let binary_path = directory.path().join("payload.bin");
        std::fs::write(&text_path, b"before").unwrap();
        std::fs::write(&csv_path, b"name,value\nitem,old\n").unwrap();
        std::fs::write(&binary_path, [0x00, 0x11, 0x22]).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));

        let mut target = EntryTarget {
            name: "notes.txt".into(),
            key: "notes.txt".into(),
            path: text_path.clone(),
            archive_entry: None,
            password: None,
            is_directory: false,
        };
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| manager.preview_text_target(&target, window, cx));
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.click("edit-preview", cx));
        let text_editor = cx.read(|cx| {
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
        cx.update(|window, cx| {
            text_editor.update(cx, |editor, cx| editor.set_value("after", window, cx));
            window.click("save-preview-edits", cx);
        });
        cx.run_until_parked();
        assert_eq!(std::fs::read(&text_path).unwrap(), b"after");

        target.name = "data.csv".into();
        target.key = "data.csv".into();
        target.path = csv_path.clone();
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| manager.preview_text_target(&target, window, cx));
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("csv-preview-table").is_some());
        assert!(cx.debug_bounds("edit-preview").is_some());
        cx.update(|window, cx| window.click("edit-preview", cx));
        let csv_editor = cx.read(|cx| {
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
        cx.update(|window, cx| {
            csv_editor.update(cx, |editor, cx| editor.set_value("name,value\nitem,new\n", window, cx));
            window.click("save-preview-edits", cx);
        });
        cx.run_until_parked();
        assert_eq!(std::fs::read(&csv_path).unwrap(), b"name,value\nitem,new\n");

        target.name = "payload.bin".into();
        target.key = "payload.bin".into();
        target.path = binary_path.clone();
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| manager.preview_text_target(&target, window, cx));
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.click("edit-preview", cx));
        let hex_editor = cx.read(|cx| {
            view.read(cx)
                .inspector
                .text_preview
                .as_ref()
                .unwrap()
                .read(cx)
                .hex_editor
                .clone()
                .unwrap()
        });
        cx.update(|window, cx| {
            hex_editor.update(cx, |editor, cx| editor.set_value("AA BB CC DD", window, cx));
            window.click("save-preview-edits", cx);
        });
        cx.run_until_parked();
        assert_eq!(std::fs::read(&binary_path).unwrap(), [0xaa, 0xbb, 0xcc, 0xdd]);
    }

    #[gpui::test]
    fn preview_editor_replaces_an_archive_entry_without_visible_extraction(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::test::TestWindowExt;
        use std::io::Read;
        let directory = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let source = sources.path().join("notes.csv");
        let archive = directory.path().join("notes.zip");
        std::fs::write(&source, b"name,value\ninside,before\n").unwrap();
        crate::archive::create_archive(
            &archive,
            std::slice::from_ref(&source),
            crate::archive::CreateOptions::default(),
        )
        .unwrap();
        let target = EntryTarget {
            name: "notes.csv".into(),
            key: "notes.csv".into(),
            path: archive.clone(),
            archive_entry: Some("notes.csv".into()),
            password: None,
            is_directory: false,
        };
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| manager.preview_text_target(&target, window, cx));
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("csv-preview-table").is_some());
        assert!(cx.debug_bounds("edit-preview").is_some());
        cx.update(|window, cx| window.click("edit-preview", cx));
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
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| editor.set_value("name,value\ninside,after\n", window, cx));
            window.click("save-preview-edits", cx);
        });
        cx.run_until_parked();
        let mut saved = crate::archive::extract_entry(&archive, "notes.csv").unwrap();
        let mut saved_bytes = Vec::new();
        saved.read_to_end(&mut saved_bytes).unwrap();
        assert_eq!(saved_bytes, b"name,value\ninside,after\n");
        assert!(!directory.path().join("notes.csv").exists());
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
        for format in ResolvedFormat::CREATION_FORMATS
            .into_iter()
            .filter(|format| format.capabilities().can_create)
        {
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
    fn preview_loads_only_the_missing_local_file_suffix() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.txt");
        let expected = MAX_BYTES * 2;
        std::fs::write(&path, vec![b'a'; expected]).unwrap();
        let target = EntryTarget {
            name: "large.txt".into(),
            key: "large.txt".into(),
            path,
            archive_entry: None,
            password: None,
            is_directory: false,
        };

        let loaded = load_with_limit(
            &target,
            &AtomicBool::new(false),
            crate::preferences::AdvancedSettings::default().preview_max_bytes(),
        )
        .unwrap();
        assert_eq!(loaded.raw_bytes.len(), MAX_BYTES);
        let remainder = load_remaining(&target, &AtomicBool::new(false), loaded.raw_bytes.len())
            .unwrap();
        assert_eq!(remainder.len(), expected - MAX_BYTES);
        let mut bytes = loaded.raw_bytes;
        bytes.extend(remainder);
        let loaded = loaded_preview_from_bytes(bytes, usize::MAX);
        let PreviewData::Text(content) = loaded.data else {
            panic!("expected a text preview");
        };
        assert_eq!(content.text.len(), expected);
        assert!(!content.truncated);
    }

    #[test]
    fn preview_loads_only_the_missing_archive_entry_suffix() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("large.txt");
        let archive = directory.path().join("large.zip");
        let text = format!("{}🙂{}", "a".repeat(MAX_BYTES - 1), "b".repeat(MAX_BYTES));
        std::fs::write(&source, text.as_bytes()).unwrap();
        crate::archive::create_zip(&archive, std::slice::from_ref(&source)).unwrap();
        let target = EntryTarget {
            name: "large.txt".into(),
            key: "large.txt".into(),
            path: archive,
            archive_entry: Some("large.txt".into()),
            password: None,
            is_directory: false,
        };

        let loaded = load_with_limit(
            &target,
            &AtomicBool::new(false),
            crate::preferences::AdvancedSettings::default().preview_max_bytes(),
        )
        .unwrap();
        let offset = loaded.raw_bytes.len();
        let remainder = load_remaining(&target, &AtomicBool::new(false), offset).unwrap();
        assert_eq!(remainder.len(), text.len() - offset);
        let mut bytes = loaded.raw_bytes;
        bytes.extend(remainder);
        let loaded = loaded_preview_from_bytes(bytes, usize::MAX);
        let PreviewData::Text(content) = loaded.data else {
            panic!("expected a text preview");
        };
        assert_eq!(content.text, text);
        assert!(!content.truncated);
    }

    #[gpui::test]
    fn load_remaining_preview_button_appends_to_the_current_preview(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.txt");
        let expected = MAX_BYTES * 2;
        std::fs::write(&path, vec![b'a'; expected]).unwrap();
        let target = EntryTarget {
            name: "large.txt".into(),
            key: "large.txt".into(),
            path,
            archive_entry: None,
            password: None,
            is_directory: false,
        };
        let (view, cx) =
            crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| manager.preview_text_target(&target, window, cx));
        });
        cx.run_until_parked();
        let preview = cx.read(|cx| {
            let manager = view.read(cx);
            let preview = manager.inspector.text_preview.as_ref().unwrap().clone();
            assert_eq!(preview.read(cx).raw_bytes.len(), MAX_BYTES);
            assert!(preview.read(cx).content.as_ref().unwrap().truncated);
            preview
        });
        assert!(cx.debug_bounds("load-remaining-preview").is_some());

        cx.update(|window, cx| window.click("load-remaining-preview", cx));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            let current = manager.inspector.text_preview.as_ref().unwrap();
            let content = current.read(cx).content.as_ref().unwrap();
            assert_eq!(content.text.len(), expected);
            assert!(!content.truncated);
            assert_eq!(preview.read(cx).raw_bytes.len(), expected);
        });
        assert!(cx.debug_bounds("load-remaining-preview").is_none());
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
    fn hex_editor_parser_accepts_byte_pairs_and_rejects_invalid_values() {
        assert_eq!(
            parse_hex_bytes("00 01 ff\n10", MAX_BYTES).unwrap(),
            [0x00, 0x01, 0xff, 0x10]
        );
        assert!(parse_hex_bytes("0f f", MAX_BYTES).is_err());
        assert!(parse_hex_bytes("gg", MAX_BYTES).is_err());
        assert!(parse_hex_bytes(&"00 ".repeat(MAX_BYTES + 1), MAX_BYTES).is_err());
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
