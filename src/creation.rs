//! Holds archive-creation form state and writes selected sources asynchronously.

use std::path::PathBuf;

use gpui::{App, Context, IntoElement, Window, div, prelude::*, px, rgb};

use crate::archive::{
    self, ArchiveFormat, CompressionFormat, CompressionLevel, CreateOptions, ResolvedFormat,
};
use crate::{ArchiveManager, Theme, app_icon, toolbar_button};
use gpui::component::checkbox::Checkbox;
use gpui::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
};

#[derive(Clone, Copy, PartialEq, Eq)]
/// Identifies which creation-form choice is being read or updated.
pub(crate) enum ChoiceField {
    /// Output archive format.
    Format,
    /// Compression level supported by the selected format.
    Compression,
    /// Outer codec used when the selected archive type is TAR.
    CompressionFormat,
    /// 7z dictionary size in mebibytes.
    Dictionary,
}

/// Stores the selected sources, output options, encryption fields, and validation error.
pub(crate) struct ArchiveCreationForm {
    /// Files and directories selected for inclusion.
    pub sources: Vec<PathBuf>,
    /// Format-specific output settings.
    pub options: CreateOptions,
    /// Current validation or creation error.
    pub error: Option<String>,
    /// Whether password-based encryption is enabled.
    pub encrypt: bool,
    /// Password entered for encryption.
    pub password: crate::secret::SecretInput,
    /// Password confirmation entered for encryption.
    pub confirmation: crate::secret::SecretInput,
}

impl ArchiveCreationForm {
    fn generate_password(&mut self) {
        if !self.encrypt || !self.options.format.supports_password() {
            return;
        }
        match crate::secret::generate_password() {
            Ok(password) => {
                self.password.clear();
                self.confirmation.clear();
                self.confirmation.text = password.clone();
                self.password.text = password;
                self.error = None;
            }
            Err(_) => self.error = Some("Could not securely generate a password".to_owned()),
        }
    }

    /// Creates a form with default archive options and fresh secret inputs.
    pub fn new(cx: &mut App) -> Self {
        Self {
            sources: Vec::new(),
            options: CreateOptions::default(),
            error: None,
            encrypt: false,
            password: crate::secret::SecretInput::new(cx),
            confirmation: crate::secret::SecretInput::new(cx),
        }
    }

    /// Returns the selected option's index for the requested form field.
    pub(crate) fn selected(&self, field: ChoiceField) -> usize {
        match field {
            ChoiceField::Format => ResolvedFormat::CREATION_FORMATS
                .iter()
                .position(|format| *format == self.options.format)
                .unwrap_or(0),
            ChoiceField::Compression => self
                .options
                .output_format()
                .compression_levels()
                .iter()
                .position(|level| *level == self.options.compression)
                .unwrap_or(0),
            ChoiceField::CompressionFormat => CompressionFormat::ALL
                .iter()
                .position(|method| *method == self.options.format.compression)
                .unwrap_or(0),
            ChoiceField::Dictionary => [1, 8, 32, 64]
                .iter()
                .position(|size| *size == self.options.dictionary_mib)
                .unwrap_or(1),
        }
    }

    /// Applies a selector choice and clears options that the new format cannot support.
    pub(crate) fn apply(&mut self, field: ChoiceField, index: usize) {
        match field {
            ChoiceField::Format => {
                self.options.format = ResolvedFormat::CREATION_FORMATS[index];
                if !self.options.format.supports_password() {
                    self.encrypt = false;
                    self.password.clear();
                    self.confirmation.clear();
                }
                self.update_compression_level();
            }
            ChoiceField::Compression => {
                self.options.compression = self.options.output_format().compression_levels()[index]
            }
            ChoiceField::CompressionFormat => {
                let previous = self.options.format.compression;
                let compression = CompressionFormat::ALL[index];
                self.options.format = ResolvedFormat::tar(compression);
                if previous == CompressionFormat::None
                    && compression != CompressionFormat::None
                    && self.options.compression == CompressionLevel::Stored
                {
                    self.options.compression = CompressionLevel::Normal;
                }
                self.update_compression_level();
            }
            ChoiceField::Dictionary => self.options.dictionary_mib = [1, 8, 32, 64][index],
        }
        self.error = None;
    }

    fn update_compression_level(&mut self) {
        let levels = self.options.output_format().compression_levels();
        if !levels.contains(&self.options.compression) {
            self.options.compression = if levels.contains(&CompressionLevel::Normal) {
                CompressionLevel::Normal
            } else {
                levels[0]
            };
        }
    }

    fn validate_sources(&self, format: ResolvedFormat) -> Result<(), &'static str> {
        if format == ResolvedFormat::Ar && self.sources.iter().any(|source| source.is_dir()) {
            return Err(
                "AR archives support files only. Select individual files instead of folders.",
            );
        }
        Ok(())
    }

    /// Validates required encryption credentials and their confirmation.
    fn validate_password(&self) -> Result<(), &'static str> {
        if !self.encrypt {
            return Ok(());
        }
        if self.password.text.is_empty() {
            return Err("Enter a password");
        }
        if self.password.text.as_str() != self.confirmation.text.as_str() {
            return Err("Passwords do not match");
        }
        Ok(())
    }
}

impl ArchiveManager {
    /// Opens a system picker and adds unique files or folders to the form.
    fn add_archive_sources(&mut self, folders: bool, cx: &mut Context<Self>) {
        let mut dialog = rfd::FileDialog::new().set_title(self.language.text(if folders {
            "Select folders to archive"
        } else {
            "Select files to archive"
        }));
        if let Some(directory) = &self.current_directory {
            dialog = dialog.set_directory(directory);
        }
        let paths = if folders {
            dialog.pick_folders()
        } else {
            dialog.pick_files()
        };
        if let Some(paths) = paths {
            for path in paths {
                if !self.creation.sources.contains(&path) {
                    self.creation.sources.push(path);
                }
            }
            self.creation.error = None;
        }
        cx.notify();
    }

    /// Validates the form, asks for an output path, and starts archive creation.
    pub(crate) fn submit_archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creation.sources.is_empty() || self.creation_task.is_some() {
            return;
        }
        if let Err(key) = self.creation.validate_password().and_then(|_| {
            self.creation
                .validate_sources(self.creation.options.output_format())
        }) {
            self.creation.error = Some(self.language.text(key).to_owned());
            cx.notify();
            return;
        }
        let options = self.creation.options;
        let format = options.output_format();
        let extension = format.extension();
        let mut dialog = rfd::FileDialog::new()
            .set_title(self.language.text("Save archive"))
            .set_file_name(format!("archive.{extension}"))
            .add_filter(format.label(), &[extension]);
        if let Some(directory) = &self.current_directory {
            dialog = dialog.set_directory(directory);
        }
        let Some(output) = dialog.save_file() else {
            cx.notify();
            return;
        };
        if !output.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(&format!(".{extension}"))
        }) {
            self.creation.error = Some(
                self.language
                    .text("The file extension must match the selected format.")
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        self.start_archive_creation(output, options, window, cx);
        self.close_modal(window, cx);
    }

    /// Creates the archive in the background and installs it after a successful write.
    fn start_archive_creation(
        &mut self,
        output: PathBuf,
        options: CreateOptions,
        _window: &Window,
        cx: &mut Context<Self>,
    ) {
        let sources = self.creation.sources.clone();
        if let Err(key) = self
            .creation
            .validate_password()
            .and_then(|_| self.creation.validate_sources(options.output_format()))
        {
            self.creation.error = Some(self.language.text(key).to_owned());
            cx.notify();
            return;
        }
        let password = self
            .creation
            .encrypt
            .then(|| std::sync::Arc::new(self.creation.password.text.clone()));
        let write_password = password.clone();
        let path = output.clone();
        let work_path = path.clone();
        let total_sources = sources.len() as u64;
        self.creation.error = None;
        self.status_message = None;
        let task_id = self.background_queue.enqueue(
            "Creating archive...",
            crate::task_queue::TaskControl::default().for_paths(std::iter::once(output.clone()).chain(sources.clone())),
            move |control, reporter| async move {
                reporter.phase("Creating archive...");
                reporter.set(0, total_sources.max(1));
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                if let Some(password) = &write_password {
                    archive::create_archive_with_password(
                        &work_path, &sources, options, Some(password.as_str()),
                    )?;
                } else {
                    archive::create_archive(&work_path, &sources, options)?;
                }
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                reporter.set(total_sources.max(1), total_sources.max(1));
                archive::list_archive_with_password(
                    &work_path,
                    write_password.as_ref().map(|password| password.as_str()),
                )
            },
            move |this, result, _, cx| {
                this.creation_task = None;
                match result {
                    Ok(entries) => {
                        this.creation.sources.clear();
                        this.creation.password.clear();
                        this.creation.confirmation.clear();
                        this.install_archive(output.clone(), entries, password, cx);
                        this.status_message = Some(format!(
                            "{} {}",
                            this.language.text("Created"),
                            output.display()
                        ));
                    }
                    Err(crate::task_queue::TaskFailure::Cancelled) => {},
                    Err(crate::task_queue::TaskFailure::Failed(error)) => {
                        let message = format!(
                            "{}: {error:#}",
                            this.language.text("Failed to create archive")
                        );
                        this.creation.error = Some(message.clone());
                        this.status_message = Some(message);
                    }
                }
                cx.notify();
            },
            cx,
        );
        self.creation_task = Some(task_id);
        cx.notify();
    }
}

/// Renders archive sources, format-specific controls, and validation feedback.
pub(crate) fn view(
    manager: &ArchiveManager,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    let form = &manager.creation;
    let mut sources = div()
        .id("archive-sources")
        .flex()
        .flex_col()
        .max_h(px(144.0))
        .overflow_y_scroll();
    if form.sources.is_empty() {
        sources = sources.child(
            div()
                .py_2()
                .text_xs()
                .text_color(rgb(theme.muted_text))
                .child(language.text("No files or folders selected")),
        );
    }
    for (index, path) in form.sources.iter().enumerate() {
        sources = sources.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .h(px(20.0))
                .flex_shrink_0()
                .border_b_1()
                .border_color(rgb(theme.subtle_border))
                .child(app_icon(
                    if path.is_dir() { "folder" } else { "file" },
                    13.0,
                    theme.icon,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .text_xs()
                        .child(path.display().to_string()),
                )
                .child(
                    Button::new(("remove-source", index))
                        .ghost()
                        .xsmall()
                        .label(language.text("Remove"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.creation.sources.remove(index);
                            cx.notify();
                        })),
                ),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .on_drop(
            cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                cx.stop_propagation();
                this.receive_external_paths(paths.paths(), window, cx);
            }),
        )
        .child(language.text("Sources"))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    toolbar_button(
                        "add-archive-files",
                        "documents",
                        language.text("Add files"),
                        theme,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.add_archive_sources(false, cx))),
                )
                .child(
                    toolbar_button(
                        "add-archive-folders",
                        "folder",
                        language.text("Add folders"),
                        theme,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.add_archive_sources(true, cx))),
                ),
        )
        .child(sources)
        .child(language.text("Format"))
        .child(choice_view(ChoiceField::Format, manager, theme, cx))
        .when(
            form.options.format == ResolvedFormat::Rar
                && !form.options.format.capabilities().can_create,
            |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.muted_text))
                        .child(language.text(
                            "Install the rar command-line tool to create RAR archives",
                        )),
                )
            },
        )
        .when(
            form.options.format.archive == Some(ArchiveFormat::Tar),
            |this| {
                this.child(language.text("Compression method"))
                    .child(choice_view(
                        ChoiceField::CompressionFormat,
                        manager,
                        theme,
                        cx,
                    ))
                    .when(
                        form.options.format.compression != CompressionFormat::None,
                        |this| {
                            this.child(language.text("Compression level"))
                                .child(choice_view(ChoiceField::Compression, manager, theme, cx))
                        },
                    )
            },
        )
        .when(
            form.options.format.archive != Some(ArchiveFormat::Tar),
            |this| {
                this.child(language.text("Compression")).child(choice_view(
                    ChoiceField::Compression,
                    manager,
                    theme,
                    cx,
                ))
            },
        )
        .when(
            form.options.format.archive == Some(ArchiveFormat::Zip),
            |this| {
                this.child(toggle(
                    "force-zip64",
                    language.text("Force ZIP64"),
                    form.options.zip64,
                    theme,
                    cx,
                    |form| form.options.zip64 = !form.options.zip64,
                ))
            },
        )
        .when(form.options.format == ResolvedFormat::SevenZip, |this| {
            this.child(language.text("Dictionary size"))
                .child(choice_view(ChoiceField::Dictionary, manager, theme, cx))
                .child(toggle(
                    "solid-compression",
                    language.text("Solid compression"),
                    form.options.solid,
                    theme,
                    cx,
                    |form| form.options.solid = !form.options.solid,
                ))
        })
        .when(form.options.format.is_tar(), |this| {
            this.child(toggle(
                "deterministic-tar",
                language.text("Deterministic metadata"),
                form.options.deterministic,
                theme,
                cx,
                |form| form.options.deterministic = !form.options.deterministic,
            ))
        })
        .when(form.options.format.supports_password(), |this| {
            this.child(toggle(
                "encrypt-archive",
                language.text("Encrypt with AES-256"),
                form.encrypt,
                theme,
                cx,
                |form| {
                    form.encrypt = !form.encrypt;
                    if !form.encrypt {
                        form.password.clear();
                        form.confirmation.clear();
                    }
                },
            ))
            .when(form.encrypt, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(language.text("Password"))
                        .child(
                            Button::new("generate-archive-password")
                                .debug_selector(|| "generate-archive-password".to_owned())
                                .icon(gpui::assets::IconName::KeyRound)
                                .label(language.text("Generate password"))
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.creation.generate_password();
                                    crate::secret::prepare(this, window, cx);
                                    window.focus(&this.creation.password.focus, cx);
                                    cx.notify();
                                })),
                        ),
                )
                    .child(crate::secret::field(
                        "creation-password",
                        &form.password,
                        crate::secret::SecretField::Password,
                        true,
                        language,
                        cx,
                    ))
                    .child(language.text("Confirm password"))
                    .child(crate::secret::field(
                        "creation-confirmation",
                        &form.confirmation,
                        crate::secret::SecretField::Confirmation,
                        true,
                        language,
                        cx,
                    ))
                    .when(form.options.format == ResolvedFormat::SevenZip, |this| {
                        this.child(toggle(
                            "encrypt-filenames",
                            language.text("Encrypt file names"),
                            form.options.encrypt_headers,
                            theme,
                            cx,
                            |form| form.options.encrypt_headers = !form.options.encrypt_headers,
                        ))
                    })
            })
        })
        .child(
            Checkbox::new("include-hidden-files")
                .debug_selector(|| "include-hidden-files".to_owned())
                .label(language.text("Include hidden files"))
                .checked(form.options.include_hidden)
                .on_change(cx.listener(|this, checked, _, cx| {
                    this.creation.options.include_hidden = *checked;
                    cx.notify();
                })),
        )
        .when_some(form.error.as_ref(), |this, error| {
            this.child(
                div()
                    .text_xs()
                    .text_color(rgb(theme.text))
                    .child(language.error_message(error)),
            )
        })
}

/// Renders cancel and create actions for the archive-creation dialog.
pub fn footer(manager: &ArchiveManager, cx: &mut Context<ArchiveManager>) -> impl IntoElement {
    let language = manager.language;
    div()
        .id("creation-actions")
        .debug_selector(|| "creation-actions".to_owned())
        .flex()
        .justify_end()
        .gap_2()
        .child(
            Button::new("cancel-archive-creation")
                .ghost()
                .small()
                .label(language.text("Cancel"))
                .on_click(cx.listener(|this, _, window, cx| this.close_modal(window, cx))),
        )
        .child(
            Button::new("create-archive-submit")
                .debug_selector(|| "create-archive-submit".to_owned())
                .primary()
                .small()
                .disabled(
                    manager.creation.sources.is_empty()
                        || !manager.creation.options.format.capabilities().can_create,
                )
                .label(language.text("Create"))
                .on_click(cx.listener(|this, _, window, cx| this.submit_archive(window, cx))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Modal;
    use gpui::Modifiers;

    #[gpui::test]
    fn generated_passwords_are_random_confirmed_and_cleared(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut form = ArchiveCreationForm::new(cx);
            form.generate_password();
            assert!(form.password.text.is_empty());
            form.encrypt = true;
            for format in [ResolvedFormat::Zip, ResolvedFormat::SevenZip] {
                form.options.format = format;
                form.generate_password();
                assert!(form.validate_password().is_ok());
                assert_eq!(form.password.text.len(), 24);
                assert!(form.password.text.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
                }));
                let previous = form.password.text.clone();
                form.generate_password();
                assert!(form.password.text.as_str() != previous.as_str());
                assert!(form.validate_password().is_ok());
            }
            form.apply(ChoiceField::Format, 1);
            form.generate_password();
            assert!(form.password.text.is_empty());
            assert!(form.confirmation.text.is_empty());
        });
    }

    #[gpui::test]
    /// Keeps the action footer visible and within the dialog at compact sizes.
    fn creation_actions_stay_in_footer_when_form_overflows(cx: &mut gpui::TestAppContext) {
        let (_, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.creation.sources = (0..30)
                .map(|index| PathBuf::from(format!("/test/source-{index:02}.txt")))
                .collect();
            manager.creation.options.format = ResolvedFormat::SevenZip;
            manager.creation.encrypt = true;
            manager.show_modal(Modal::CreateArchive, window, cx);
            manager
        });
        for viewport in [
            gpui::size(px(1100.0), px(720.0)),
            gpui::size(px(560.0), px(440.0)),
        ] {
            cx.simulate_resize(viewport);
            cx.run_until_parked();
            let panel = cx.debug_bounds("dialog-0").unwrap();
            let actions = cx.debug_bounds("creation-actions").unwrap();
            let submit = cx.debug_bounds("create-archive-submit").unwrap();
            assert!(actions.origin.y >= panel.origin.y);
            assert!(actions.bottom() <= panel.bottom());
            assert!(submit.origin.y >= actions.origin.y);
            assert!(submit.bottom() <= actions.bottom());
            assert!(submit.right() <= panel.right());
            assert!(panel.bottom() <= viewport.height);
        }
    }

    #[gpui::test]
    /// Ensures format changes retain valid options and clear unsupported secrets.
    fn creation_format_changes_preserve_supported_options_and_clear_passwords(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let mut form = ArchiveCreationForm::new(cx);
            form.apply(ChoiceField::Format, 1);
            assert_eq!(form.options.format, ResolvedFormat::Tar);
            assert_eq!(form.options.compression, CompressionLevel::Stored);
            form.apply(ChoiceField::Format, 0);
            assert_eq!(form.options.compression, CompressionLevel::Stored);
            let seven_zip_index = ResolvedFormat::CREATION_FORMATS
                .iter()
                .position(|format| *format == ResolvedFormat::SevenZip)
                .unwrap();
            form.apply(ChoiceField::Format, seven_zip_index);
            assert_eq!(form.options.format, ResolvedFormat::SevenZip);
            assert_eq!(form.options.compression, CompressionLevel::Normal);
            form.apply(ChoiceField::Compression, 2);
            assert_eq!(form.options.compression, CompressionLevel::Best);
            form.apply(ChoiceField::Dictionary, 2);
            assert_eq!(form.options.dictionary_mib, 32);
            form.encrypt = true;
            form.password.text.push_str("test-password");
            form.confirmation.text.push_str("test-password");
            form.apply(ChoiceField::Format, 1);
            assert!(!form.encrypt);
            assert!(form.password.text.is_empty());
            assert!(form.confirmation.text.is_empty());

            form.apply(ChoiceField::CompressionFormat, 1);
            assert_eq!(form.options.output_format(), ResolvedFormat::TarGz);
            assert_eq!(form.options.compression, CompressionLevel::Normal);
            form.apply(ChoiceField::CompressionFormat, 0);
            assert_eq!(form.options.output_format(), ResolvedFormat::Tar);
            assert_eq!(form.options.compression, CompressionLevel::Stored);
        });
    }

    #[gpui::test]
    /// Shows a separate compression-method selector only for the logical TAR format.
    fn tar_creation_form_shows_methods_only_for_tar(cx: &mut gpui::TestAppContext) {
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.show_modal(Modal::CreateArchive, window, cx);
            manager
        });
        assert!(cx.debug_bounds("creation-tar-compression").is_none());

        view.update(cx, |manager, cx| {
            manager.creation.apply(ChoiceField::Format, 1);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("creation-tar-compression").is_some());
        assert!(cx.debug_bounds("creation-compression").is_none());

        view.update(cx, |manager, cx| {
            manager.creation.apply(ChoiceField::CompressionFormat, 1);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("creation-tar-compression").is_some());
        assert!(cx.debug_bounds("creation-compression").is_some());
    }

    #[gpui::test]
    /// Validates password confirmation and verifies an encrypted archive can be reopened.
    fn encrypted_creation_validates_confirmation_and_reopens_with_session_password(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("private.txt");
        std::fs::write(&source, b"Private data").unwrap();
        let output = directory.path().join("protected.zip");
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.entries.clear();
            manager.creation.sources.push(source.clone());
            manager.show_modal(Modal::CreateArchive, window, cx);
            manager
        });
        let toggle = cx.debug_bounds("encrypt-archive").unwrap();
        cx.simulate_mouse_move(toggle.center(), None, Modifiers::default());
        cx.simulate_click(toggle.center(), Modifiers::default());
        let field = cx.debug_bounds("creation-password").unwrap();
        cx.simulate_mouse_move(field.center(), None, Modifiers::default());
        cx.simulate_click(field.center(), Modifiers::default());
        cx.simulate_input("创建🙂test-password");
        let field = cx.debug_bounds("creation-confirmation").unwrap();
        cx.simulate_mouse_move(field.center(), None, Modifiers::default());
        cx.simulate_click(field.center(), Modifiers::default());
        cx.simulate_input("different-password");
        cx.update(|window, cx| view.update(cx, |manager, cx| {
            manager.start_archive_creation(output.clone(), manager.creation.options, window, cx);
            assert!(manager.creation_task.is_none());
            assert!(manager.creation.error.is_some());
        }));
        assert!(!output.exists());
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("创建🙂test-password");
        cx.update(|window, cx| view.update(cx, |manager, cx| {
            manager.start_archive_creation(output.clone(), manager.creation.options, window, cx);
            assert!(manager.creation_task.is_some());
            manager.modal = None;
        }));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.creation_task.is_none());
            assert!(manager.creation.error.is_none());
            assert!(manager.creation.password.text.is_empty());
            assert!(manager.creation.confirmation.text.is_empty());
            assert_eq!(manager.archive_path.as_ref(), Some(&output));
            assert!(manager.archive_password.is_some());
        });
        assert!(archive::list_archive(&output).is_err());
        assert!(archive::list_archive_with_password(&output, Some("创建🙂test-password")).is_ok());
    }

    #[gpui::test]
    fn creation_rejects_ar_folder_sources(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        cx.update(|window, cx| view.update(cx, |manager, cx| {
            manager.creation.sources = vec![root.path().to_path_buf()];
            let options = CreateOptions {
                format: ResolvedFormat::Ar,
                compression: CompressionLevel::Stored,
                ..CreateOptions::default()
            };
            manager.start_archive_creation(root.path().join("output.ar"), options, window, cx);
            assert!(manager.creation_task.is_none());
            assert_eq!(
                manager.creation.error.as_deref(),
                Some("AR archives support files only. Select individual files instead of folders.")
            );
        }));
    }

    #[gpui::test]
    /// Exercises format selectors, hidden-file options, background creation, and failures.
    fn creation_form_selects_formats_and_creates_archive_in_background(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("file.txt"), b"Archive contents").unwrap();
        std::fs::write(source.join(".hidden"), b"Hidden contents").unwrap();
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.entries.clear();
            manager.creation.sources.push(source.clone());
            manager.show_modal(Modal::CreateArchive, window, cx);
            manager
        });
        for (index, format) in ResolvedFormat::CREATION_FORMATS.into_iter().enumerate() {
            let trigger = cx.debug_bounds("creation-format").unwrap();
            cx.simulate_mouse_move(trigger.center(), None, Modifiers::default());
            cx.simulate_click(trigger.center(), Modifiers::default());
            if index > 0 {
                cx.simulate_keystrokes("down");
            }
            cx.simulate_keystrokes("enter");
            cx.read(|cx| {
                let manager = view.read(cx);
                assert_eq!(manager.creation.options.format, format);
                assert!(
                    format
                        .compression_levels()
                        .contains(&manager.creation.options.compression)
                );
            });
        }
        let trigger = cx.debug_bounds("creation-format").unwrap();
        cx.simulate_mouse_move(trigger.center(), None, Modifiers::default());
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.simulate_input("7z");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).creation.options.format,
                ResolvedFormat::SevenZip
            )
        });
        let trigger = cx.debug_bounds("creation-format").unwrap();
        cx.simulate_mouse_move(trigger.center(), None, Modifiers::default());
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.simulate_input("gzip");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(view.read(cx).creation.options.format, ResolvedFormat::Gzip));
        let trigger = cx.debug_bounds("creation-format").unwrap();
        cx.simulate_mouse_move(trigger.center(), None, Modifiers::default());
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.simulate_input("7z");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).creation.options.format,
                ResolvedFormat::SevenZip
            )
        });
        let trigger = cx.debug_bounds("creation-compression").unwrap();
        cx.simulate_mouse_move(trigger.center(), None, Modifiers::default());
        cx.simulate_click(trigger.center(), Modifiers::default());
        let selected = cx.read(|cx| view.read(cx).creation.selected(ChoiceField::Compression));
        for _ in selected..ResolvedFormat::SevenZip.compression_levels().len() - 1 {
            cx.simulate_keystrokes("down");
        }
        cx.simulate_keystrokes("enter");
        let hidden = cx.debug_bounds("include-hidden-files").unwrap();
        cx.simulate_mouse_move(hidden.center(), None, Modifiers::default());
        cx.simulate_click(hidden.center(), Modifiers::default());
        let output = directory.path().join("created.7z");
        cx.update(|window, cx| view.update(cx, |manager, cx| {
            assert_eq!(manager.creation.options.compression, CompressionLevel::Best);
            assert!(!manager.creation.options.include_hidden);
            let options = manager.creation.options;
            manager.start_archive_creation(output.clone(), options, window, cx);
            assert!(manager.creation_task.is_some());
            manager.modal = None;
        }));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.creation_task.is_none());
            assert!(
                manager.creation.error.is_none(),
                "format {:?}: {:?}",
                manager.creation.options.format,
                manager.creation.error
            );
            assert_eq!(manager.archive_path.as_ref(), Some(&output));
            assert!(manager.creation.sources.is_empty());
            assert!(
                manager
                    .entries
                    .iter()
                    .any(|entry| entry.name == "source/file.txt")
            );
            assert!(
                !manager
                    .entries
                    .iter()
                    .any(|entry| entry.name.ends_with(".hidden"))
            );
        });
        cx.update(|window, cx| view.update(cx, |manager, cx| {
            manager.creation.sources.push(source.clone());
            manager.start_archive_creation(
                directory.path().join("missing/output.7z"),
                manager.creation.options,
                window,
                cx,
            );
        }));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.creation_task.is_none());
            assert!(manager.creation.error.is_some());
            assert_eq!(manager.creation.sources, [source]);
            assert_eq!(manager.archive_path.as_ref(), Some(&output));
        });
    }
}

/// Maps a creation choice to its retained settings selector.
fn choice_view(
    field: ChoiceField,
    manager: &ArchiveManager,
    _theme: Theme,
    _cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    crate::ui::select(
        manager,
        match field {
            ChoiceField::Format => crate::ui::Selector::Format,
            ChoiceField::Compression => crate::ui::Selector::Compression,
            ChoiceField::CompressionFormat => crate::ui::Selector::CompressionFormat,
            ChoiceField::Dictionary => crate::ui::Selector::Dictionary,
        },
    )
}

/// Renders a checkbox that updates the creation form and clears stale errors.
fn toggle(
    id: &'static str,
    label: &'static str,
    checked: bool,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
    change: impl Fn(&mut ArchiveCreationForm) + 'static,
) -> impl IntoElement {
    let _ = theme;
    Checkbox::new(id)
        .debug_selector(move || id.to_owned())
        .label(label)
        .checked(checked)
        .on_change(cx.listener(move |this, _, _, cx| {
            change(&mut this.creation);
            this.creation.error = None;
            cx.notify();
        }))
}
