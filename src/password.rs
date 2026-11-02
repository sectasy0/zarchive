//! Manages archive password prompts, background unlocks, and session credentials.

use crate::{
    ArchiveManager, Modal, Theme,
    archive::{self, ArchiveEntry},
    secret::{self, SecretField, SecretInput},
};
use gpui::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
};
use gpui::{App, Context, IntoElement, Window, div, prelude::*, px, rgb};
use std::{path::PathBuf, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use zeroize::Zeroizing;

/// Shared, zeroizing password retained for the lifetime of an unlocked archive.
pub type ArchivePassword = Arc<Zeroizing<String>>;

/// Tracks the archive being unlocked, its input, errors, and active worker task.
pub struct PasswordPrompt {
    /// Archive path associated with the current unlock attempt.
    pub path: Option<PathBuf>,
    /// Masked password input state.
    pub input: SecretInput,
    /// Localized error shown after a failed unlock.
    pub error: Option<String>,
    /// Background archive-reading task, when one is active.
    pub task: Option<u64>,
    cancel: Arc<AtomicBool>,
    /// Whether a successful unlock should resume extraction instead of navigation.
    pub for_extraction: bool,
}

impl PasswordPrompt {
    /// Creates a prompt optionally associated with an archive path.
    pub fn new(cx: &mut App, path: Option<PathBuf>) -> Self {
        Self {
            path,
            input: SecretInput::new(cx),
            error: None,
            task: None,
            cancel: Arc::new(AtomicBool::new(false)),
            for_extraction: false,
        }
    }
    /// Clears prompt state and erases its password input.
    pub fn clear(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.path = None;
        self.input.clear();
        self.error = None;
        self.task = None;
        self.for_extraction = false;
    }
}

impl ArchiveManager {
    /// Opens a password prompt for an archive that requires credentials.
    pub fn request_archive_password(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.password_prompt.clear();
        self.password_prompt.path = Some(path);
        self.modal = Some(Modal::Password);
        cx.notify();
    }
    /// Installs a successfully read archive and retains its optional session password.
    pub fn install_archive(
        &mut self,
        path: PathBuf,
        entries: Vec<ArchiveEntry>,
        password: Option<ArchivePassword>,
        cx: &mut Context<Self>,
    ) {
        self.navigation_control.cancel();
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        self.cancel_directory_size_calculation();
        self.inspector.clear_text_preview();
        self.directory_sizes = None;
        self.current_directory = None;
        self.directory_history.clear();
        self.navigation_history_index = 0;
        self.archive_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        self.archive_path = Some(path);
        self.archive_password = password;
        self.entries = entries;
        self.error = None;
        self.selected_folder.clear();
        self.selected_entry = None;
        self.status_message = None;
        cx.notify();
    }
    /// Unlocks the pending archive asynchronously and resumes its requested operation.
    pub fn unlock_archive(&mut self, _window: &Window, cx: &mut Context<Self>) {
        if self.password_prompt.task.is_some() || self.password_prompt.input.text.is_empty() {
            return;
        }
        let Some(path) = self.password_prompt.path.clone() else {
            return;
        };
        let password = Arc::new(self.password_prompt.input.text.clone());
        self.password_prompt.input.clear();
        self.password_prompt.error = None;
        let key = password.clone();
        let source = path.clone();
        self.password_prompt.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.password_prompt.cancel.clone();
        self.password_prompt.task = Some(self.background_queue.enqueue(
            "Opening archive...",
            crate::task_queue::TaskControl::with_cancel(cancel.clone()).for_paths([path.clone()]),
            move |control, reporter| async move {
                reporter.set(0, 1);
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                let entries = archive::list_archive_with_password(&source, Some(key.as_str()))?;
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                reporter.set(1, 1);
                Ok(entries)
            },
            move |this, result, _, cx| {
                if !Arc::ptr_eq(&this.password_prompt.cancel, &cancel) { return; }
                this.password_prompt.task = None;
                match result {
                    Ok(entries) => {
                        if this.password_prompt.for_extraction {
                            this.resume_extract_with_password(password);
                        } else {
                            this.install_archive(path, entries, Some(password), cx);
                        }
                        this.password_prompt.clear();
                        this.modal = None;
                        this.password_blur.clear();
                    }
                    Err(crate::task_queue::TaskFailure::Cancelled) => {},
                    Err(crate::task_queue::TaskFailure::Failed(_)) => {
                        this.password_prompt.error = Some(
                            this.language
                                .text("Incorrect password or damaged archive")
                                .to_owned(),
                        )
                    }
                }
                cx.notify();
            },
            cx,
        ));
        cx.notify();
    }
}

/// Renders the archive identity, masked input, and unlock progress or error message.
pub fn view(
    manager: &ArchiveManager,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    let prompt = &manager.password_prompt;
    let checking = prompt.task.is_some();
    let path = prompt.path.as_ref();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .w_full()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_w(px(0.0))
                .child(crate::app_icon("archive", 20.0, theme.accent))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .truncate()
                                .child(
                                    path.and_then(|path| path.file_name())
                                        .map(|name| name.to_string_lossy().into_owned())
                                        .unwrap_or_default(),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(rgb(theme.muted_text))
                                .child(
                                    path.map(|path| path.display().to_string())
                                        .unwrap_or_default(),
                                ),
                        ),
                ),
        )
        .child(div().text_xs().child(language.text("Password")))
        .child(secret::field(
            "unlock-password",
            &prompt.input,
            SecretField::Unlock,
            !checking,
            language,
            cx,
        ))
        .child(
            div()
                .min_h(px(34.0))
                .text_xs()
                .text_color(rgb(theme.muted_text))
                .child(if checking {
                    language.text("Unlocking archive...").to_owned()
                } else {
                    prompt.error.clone().unwrap_or_default()
                }),
        )
}

/// Renders cancel and unlock actions, disabling submission while invalid or busy.
pub fn footer(manager: &ArchiveManager, cx: &mut Context<ArchiveManager>) -> impl IntoElement {
    let language = manager.language;
    let prompt = &manager.password_prompt;
    let checking = prompt.task.is_some();
    div()
        .id("password-actions")
        .debug_selector(|| "password-actions".to_owned())
        .flex()
        .justify_end()
        .gap_2()
        .child(
            Button::new("cancel-unlock")
                .debug_selector(|| "cancel-unlock".to_owned())
                .ghost()
                .small()
                .label(language.text("Cancel"))
                .on_click(cx.listener(|this, _, window, cx| this.close_modal(window, cx))),
        )
        .child(
            Button::new("unlock-archive")
                .debug_selector(|| "unlock-archive".to_owned())
                .primary()
                .small()
                .disabled(checking || prompt.input.text.is_empty())
                .label(language.text("Unlock"))
                .on_click(cx.listener(|this, _, window, cx| this.unlock_archive(window, cx))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{CompressionLevel, CreateOptions, ResolvedFormat};
    use gpui::{ClipboardItem, Modifiers};

    #[gpui::test]
    /// Covers masking, clipboard protection, retry behavior, and session-password reuse.
    fn password_modal_masks_input_blocks_background_and_allows_retry(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("private.txt");
        std::fs::write(&source, b"private contents").unwrap();
        let key = "中文🙂test-password";
        for format in [ResolvedFormat::Zip, ResolvedFormat::SevenZip] {
            let path = directory
                .path()
                .join(format!("protected.{}", format.extension()));
            archive::create_archive_with_password(
                &path,
                &[source.clone()],
                CreateOptions {
                    format,
                    dictionary_mib: 1,
                    compression: CompressionLevel::Fast,
                    ..CreateOptions::default()
                },
                Some(key),
            )
            .unwrap();
            let (view, cx) = crate::tests::component_window(cx, |_, cx| {
                let mut manager = crate::tests::test_manager(cx);
                manager.current_directory = None;
                manager.navigate_into_archive(path.clone(), cx);
                manager
            });
            cx.read(|cx| {
                let manager = view.read(cx);
                assert!(manager.modal == Some(Modal::Password));
                assert!(manager.archive_password.is_none());
                assert!(manager.archive_path.is_none());
            });
            let sort = cx.debug_bounds("sort-size").unwrap();
            cx.simulate_mouse_move(sort.center(), None, Modifiers::default());
            cx.simulate_click(sort.center(), Modifiers::default());
            cx.read(|cx| assert_eq!(view.read(cx).sort_column, crate::SortColumn::Name));
            assert!(
                cx.debug_bounds("unlock-password").is_some(),
                "password field must be rendered before typing"
            );
            for viewport in [
                gpui::size(px(1100.0), px(720.0)),
                gpui::size(px(560.0), px(440.0)),
            ] {
                cx.simulate_resize(viewport);
                cx.run_until_parked();
                let panel = cx.debug_bounds("dialog-0").unwrap();
                let field = cx.debug_bounds("unlock-password").unwrap();
                let actions = cx.debug_bounds("password-actions").unwrap();
                let cancel = cx.debug_bounds("cancel-unlock").unwrap();
                let unlock = cx.debug_bounds("unlock-archive").unwrap();
                assert!(field.origin.y >= panel.origin.y);
                assert!(field.bottom() <= actions.origin.y);
                assert!(actions.bottom() <= panel.bottom());
                assert!(cancel.right() <= unlock.origin.x);
                assert!(unlock.right() <= panel.right());
                assert!(panel.bottom() <= viewport.height);
            }
            cx.simulate_input("wrong-password");
            cx.read(|cx| {
                assert_eq!(
                    view.read(cx).password_prompt.input.text.as_str(),
                    "wrong-password"
                )
            });
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            cx.read(|cx| {
                let manager = view.read(cx);
                assert!(manager.modal == Some(Modal::Password));
                assert!(manager.password_prompt.error.is_some());
                assert!(manager.password_prompt.input.text.is_empty());
                assert!(manager.archive_password.is_none());
            });
            cx.simulate_input(key);
            cx.write_to_clipboard(ClipboardItem::new_string("unchanged".to_owned()));
            cx.simulate_keystrokes("cmd-a cmd-c");
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("unchanged")
            );
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            cx.read(|cx| {
                let manager = view.read(cx);
                assert!(manager.modal.is_none());
                assert_eq!(manager.archive_path.as_ref(), Some(&path));
                assert_eq!(manager.archive_password.as_ref().unwrap().as_str(), key);
                assert!(manager.password_prompt.input.text.is_empty());
                assert!(
                    manager
                        .entries
                        .iter()
                        .any(|entry| entry.name == "private.txt")
                );
            });
            view.update(cx, |manager, cx| {
                manager.navigate_into_archive(path.clone(), cx)
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
            });
            cx.run_until_parked();
            cx.simulate_keystrokes("escape");
            cx.read(|cx| {
                let manager = view.read(cx);
                assert!(manager.modal.is_none());
                assert!(manager.password_prompt.path.is_none());
                assert!(manager.password_prompt.input.text.is_empty());
                assert_eq!(manager.archive_path.as_ref(), Some(&path));
            });
            view.update(cx, |manager, cx| {
                manager.navigate_to(directory.path().to_path_buf(), cx)
            });
            cx.read(|cx| assert!(view.read(cx).archive_password.is_none()));
        }
    }
}
