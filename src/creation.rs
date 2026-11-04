//! Holds archive-creation form state and writes selected sources asynchronously.

use std::path::PathBuf;

use gpui::{App, Context, Entity, IntoElement, Subscription, Window, div, prelude::*, px, rgb};

use crate::archive::{
    self, ArchiveFormat, CompressionFormat, CompressionLevel, CreateOptions, ResolvedFormat,
};
use crate::{ArchiveManager, Theme, app_icon, toolbar_button};
use gpui::component::checkbox::Checkbox;
use gpui::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};

pub(crate) const VOLUME_PRESETS: [u64; 6] = [100_000_000, 250_000_000, 500_000_000, 1_000_000_000, 2_000_000_000, 4_000_000_000];
pub(crate) const PARITY_PRESETS: [u32; 5] = [5, 10, 20, 30, 50];

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
    VolumeSize,
    VolumeUnit,
    RecoveryPercent,
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
    pub create_parity: bool,
    pub parity_percent: usize,
    split: bool,
    volume_preset: usize,
    volume_unit: usize,
    custom_volume_size: String,
    volume_input: Option<Entity<InputState>>,
    volume_subscription: Option<Subscription>,
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
            create_parity: false,
            parity_percent: 1,
            split: false,
            volume_preset: 0,
            volume_unit: 0,
            custom_volume_size: "100".to_owned(),
            volume_input: None,
            volume_subscription: None,
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
            ChoiceField::VolumeSize => self.volume_preset,
            ChoiceField::VolumeUnit => self.volume_unit,
            ChoiceField::RecoveryPercent => self.parity_percent,
        }
    }

    /// Applies a selector choice and clears options that the new format cannot support.
    pub(crate) fn apply(&mut self, field: ChoiceField, index: usize) {
        match field {
            ChoiceField::Format => {
                self.options.format = ResolvedFormat::CREATION_FORMATS[index];
                if !self.options.format.capabilities().volumes.can_create {
                    self.split = false;
                    self.options.volume_size = None;
                }
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
            ChoiceField::VolumeSize => self.volume_preset = index,
            ChoiceField::VolumeUnit => self.volume_unit = index,
            ChoiceField::RecoveryPercent => self.parity_percent = index,
        }
        self.error = None;
    }

    fn recovery_percent(&self) -> u32 {
        PARITY_PRESETS.get(self.parity_percent).copied().unwrap_or(10)
    }

    fn resolved_options(&self) -> Result<CreateOptions, &'static str> {
        let mut options = self.options;
        options.volume_size = if self.split {
            if !options.format.capabilities().volumes.can_create {
                return Err("This format cannot create volumes");
            }
            Some(if let Some(size) = VOLUME_PRESETS.get(self.volume_preset) {
                *size
            } else if self.volume_preset == VOLUME_PRESETS.len() {
                parse_volume_size(&self.custom_volume_size, self.volume_unit)?
            } else {
                return Err("Enter a valid volume size");
            })
        } else { None };
        Ok(options)
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

fn parse_volume_size(text: &str, unit: usize) -> Result<u64, &'static str> {
    let error = "Enter a valid volume size";
    let multiplier = *[1_000_000u128, 1_000_000_000].get(unit).ok_or(error)?;
    let text = text.trim();
    if text.len() > 32 { return Err(error); }
    let (whole, fraction) = text.split_once(['.', ',']).unwrap_or((text, ""));
    if !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit()) || fraction.len() > 9 { return Err(error); }
    let whole = if whole.is_empty() { 0 } else { whole.parse::<u128>().map_err(|_| error)? };
    let fraction_value = if fraction.is_empty() { 0 } else { fraction.parse::<u128>().map_err(|_| error)? };
    let denominator = 10u128.pow(fraction.len() as u32);
    let fraction_bytes = fraction_value.checked_mul(multiplier).ok_or(error)?;
    if fraction_bytes % denominator != 0 { return Err(error); }
    let bytes = whole.checked_mul(multiplier).and_then(|bytes| bytes.checked_add(fraction_bytes / denominator)).ok_or(error)?;
    u64::try_from(bytes).ok().filter(|bytes| *bytes > 0).ok_or(error)
}

pub(crate) fn prepare(manager: &mut ArchiveManager, window: &mut Window, cx: &mut Context<ArchiveManager>) {
    let form = &mut manager.creation;
    if let Some(state) = &form.volume_input {
        if state.read(cx).value().as_ref() != form.custom_volume_size {
            state.update(cx, |state, cx| state.set_value(form.custom_volume_size.clone(), window, cx));
        }
        return;
    }
    let state = cx.new(|cx| InputState::new(window, cx).default_value(form.custom_volume_size.clone()));
    let subscription = cx.subscribe_in(&state, window, |this, state, event, _, cx| {
        if matches!(event, InputEvent::Change) {
            this.creation.custom_volume_size = state.read(cx).value().to_string();
            this.creation.error = None;
            cx.notify();
        }
    });
    form.volume_input = Some(state);
    form.volume_subscription = Some(subscription);
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
        let options = match self.creation.resolved_options() {
            Ok(options) => options,
            Err(key) => {
                self.creation.error = Some(self.language.text(key).to_owned());
                cx.notify();
                return;
            }
        };
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
        let archive_path = options.first_output_path(&output);
        let read_path = archive_path.clone();
        let path = output.clone();
        let work_path = path.clone();
        let total_sources = sources.len() as u64;
        let create_parity = self.creation.create_parity;
        let parity_percent = self.creation.recovery_percent();
        self.creation.error = None;
        self.status_message = None;
        let task_id = self.background_queue.enqueue(
            "Creating archive...",
            crate::task_queue::TaskControl::default().for_paths([output.clone(), archive_path.clone()].into_iter().chain(sources.clone())),
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
                    &read_path,
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
                        this.install_archive(archive_path.clone(), entries, password, cx);
                        this.status_message = Some(format!(
                            "{} {}",
                            this.language.text("Created"),
                            archive_path.display()
                        ));
                        if create_parity {
                            this.start_parity_creation(
                                path.clone(),
                                archive_path.clone(),
                                options.volume_size.is_some(),
                                parity_percent,
                                cx,
                            );
                        }
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

    fn start_parity_creation(
        &mut self,
        archive_path: PathBuf,
        first_volume: PathBuf,
        split: bool,
        recovery_percent: u32,
        cx: &mut Context<Self>,
    ) {
        let mut sidecar_name = archive_path.file_name().unwrap_or_default().to_os_string();
        sidecar_name.push(".par2");
        let sidecar = archive_path.with_file_name(sidecar_name);
        let control_paths = [archive_path.clone(), first_volume.clone(), sidecar];
        let task = self.background_queue.enqueue(
            "Creating PAR2 recovery files...",
            crate::task_queue::TaskControl::default().for_paths(control_paths),
            move |control, reporter| async move {
                crate::parity::create_par2(
                    &archive_path,
                    &first_volume,
                    split,
                    recovery_percent,
                    control,
                    reporter,
                )
            },
            move |this, result, _, cx| {
                this.creation_task = None;
                match result {
                    Ok(paths) => {
                        this.status_message = Some(format!(
                            "{} {}",
                            this.language.text("Created PAR2 recovery files"),
                            paths.first().map_or_else(String::new, |path| path.display().to_string())
                        ));
                    }
                    Err(crate::task_queue::TaskFailure::Cancelled) => {
                        this.status_message = Some(this.language.text("PAR2 creation was cancelled; the archive was created").to_owned());
                    }
                    Err(crate::task_queue::TaskFailure::Failed(error)) => {
                        this.status_message = Some(format!(
                            "{}: {error:#}",
                            this.language.text("Failed to create PAR2 recovery files")
                        ));
                    }
                }
                cx.notify();
            },
            cx,
        );
        self.creation_task = Some(task);
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
        .when(form.options.format.capabilities().volumes.can_create, |this| {
            this.child(toggle("split-into-volumes", language.text("Split into volumes"), form.split, theme, cx, |form| {
                    form.split = !form.split;
                }))
                .when(form.split, |this| {
                    this.child(language.text("Volume size"))
                        .child(choice_view(ChoiceField::VolumeSize, manager, theme, cx))
                        .when(form.volume_preset == VOLUME_PRESETS.len(), |this| {
                            this.child(div().flex().gap_2().items_center()
                                .child(div().id("creation-custom-volume-size")
                                    .debug_selector(|| "creation-custom-volume-size".to_owned())
                                    .flex_1().min_w(px(0.0))
                                    .child(Input::new(form.volume_input.as_ref().expect("volume input initialized"))
                                        .id("custom-volume-size")
                                        .accessibility_id("custom-volume-size")
                                        .aria_label(language.text("Custom size"))
                                        .small()))
                                .child(div().w(px(92.0)).flex_shrink_0()
                                    .child(choice_view(ChoiceField::VolumeUnit, manager, theme, cx))))
                        })
                })
        })
        .child(
            Checkbox::new("create-par2-recovery")
                .debug_selector(|| "create-par2-recovery".to_owned())
                .label(language.text("Create PAR2 recovery files"))
                .checked(form.create_parity)
                .on_change(cx.listener(|this, checked, _, cx| {
                    this.creation.create_parity = *checked;
                    cx.notify();
                })),
        )
        .when(form.create_parity, |this| {
            this.child(language.text("Recovery data"))
                .child(crate::ui::select(manager, crate::ui::Selector::RecoveryPercent))
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

    #[test]
    fn custom_volume_sizes_are_exact_and_validated() {
        assert_eq!(parse_volume_size("100", 0), Ok(100_000_000));
        assert_eq!(parse_volume_size("4", 1), Ok(4_000_000_000));
        assert_eq!(parse_volume_size("1,5", 0), Ok(1_500_000));
        assert_eq!(parse_volume_size("0.000001", 0), Ok(1));
        assert_eq!(parse_volume_size("18446744073.709551615", 1), Ok(u64::MAX));
        for value in ["", "0", "-1", "abc", "1e3", "NaN", "1 MB", ".", "1.2.3", "0.0000001", "18446744073.709551616"] {
            assert!(parse_volume_size(value, 0).is_err(), "{value}");
        }
        assert!(parse_volume_size("1", 2).is_err());
    }

    #[gpui::test]
    fn volume_presets_resolve_and_unsupported_formats_clear_splitting(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut form = ArchiveCreationForm::new(cx);
            assert!(!form.split);
            assert!(form.resolved_options().unwrap().volume_size.is_none());
            form.split = true;
            for (index, size) in [100_000_000, 250_000_000, 500_000_000, 1_000_000_000, 2_000_000_000, 4_000_000_000].into_iter().enumerate() {
                form.apply(ChoiceField::VolumeSize, index);
                assert_eq!(form.resolved_options().unwrap().volume_size, Some(size));
            }
            form.apply(ChoiceField::VolumeSize, VOLUME_PRESETS.len());
            form.custom_volume_size = "0.001".to_owned();
            assert_eq!(form.resolved_options().unwrap().volume_size, Some(1000));
            form.apply(ChoiceField::VolumeUnit, 1);
            assert_eq!(form.resolved_options().unwrap().volume_size, Some(1_000_000));
            form.custom_volume_size = "invalid".to_owned();
            assert!(form.resolved_options().is_err());
            for (index, percent) in PARITY_PRESETS.into_iter().enumerate() {
                form.apply(ChoiceField::RecoveryPercent, index);
                assert_eq!(form.recovery_percent(), percent);
            }
            form.apply(ChoiceField::Format, ResolvedFormat::CREATION_FORMATS.iter().position(|format| *format == ResolvedFormat::Cpio).unwrap());
            assert!(!form.split);
            assert!(form.resolved_options().unwrap().volume_size.is_none());
        });
    }

    fn click_control(cx: &mut gpui::VisualTestContext, id: &'static str) {
        let bounds = cx.debug_bounds(id).unwrap_or_else(|| panic!("missing {id}"));
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
        cx.simulate_click(bounds.center(), Modifiers::default());
    }

    #[gpui::test]
    fn volume_checkbox_is_visible_and_accepts_custom_sizes(cx: &mut gpui::TestAppContext) {
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.show_modal(Modal::CreateArchive, window, cx);
            manager
        });
        assert!(cx.debug_bounds("split-into-volumes").is_some());
        assert!(cx.debug_bounds("creation-volume-size").is_none());
        assert!(cx.debug_bounds("create-par2-recovery").is_some());
        assert!(cx.debug_bounds("creation-parity-percent").is_none());
        click_control(cx, "create-par2-recovery");
        assert!(cx.debug_bounds("creation-parity-percent").is_some());
        cx.read(|cx| assert!(view.read(cx).creation.create_parity));
        click_control(cx, "create-par2-recovery");
        assert!(cx.debug_bounds("creation-parity-percent").is_none());
        click_control(cx, "split-into-volumes");
        click_control(cx, "creation-volume-size");
        for _ in 0..3 { cx.simulate_keystrokes("down"); }
        cx.simulate_keystrokes("enter");
        cx.read(|cx| assert_eq!(view.read(cx).creation.resolved_options().unwrap().volume_size, Some(1_000_000_000)));
        click_control(cx, "creation-volume-size");
        for _ in 0..3 { cx.simulate_keystrokes("down"); }
        cx.simulate_keystrokes("enter");
        click_control(cx, "creation-custom-volume-size");
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("0.001");
        cx.read(|cx| assert_eq!(view.read(cx).creation.resolved_options().unwrap().volume_size, Some(1000)));
        click_control(cx, "creation-volume-unit");
        cx.simulate_keystrokes("down enter");
        cx.read(|cx| assert_eq!(view.read(cx).creation.resolved_options().unwrap().volume_size, Some(1_000_000)));
        click_control(cx, "creation-custom-volume-size");
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("invalid");
        cx.read(|cx| assert!(view.read(cx).creation.resolved_options().is_err()));
        click_control(cx, "split-into-volumes");
        assert!(cx.debug_bounds("split-into-volumes").is_some());
        assert!(cx.debug_bounds("creation-volume-size").is_none());
        view.update(cx, |manager, cx| {
            manager.creation.apply(ChoiceField::Format, ResolvedFormat::CREATION_FORMATS.iter().position(|format| *format == ResolvedFormat::Cpio).unwrap());
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("split-into-volumes").is_none());
    }

    #[gpui::test]
    fn volume_creation_installs_first_part_after_background_completion(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("private.txt");
        let contents = b"private split archive payload".repeat(200);
        std::fs::write(&source, &contents).unwrap();
        let output = root.path().join("protected.zip");
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.creation.sources.push(source.clone());
            manager.creation.options.compression = CompressionLevel::Stored;
            manager.creation.split = true;
            manager.creation.create_parity = true;
            manager.creation.volume_preset = VOLUME_PRESETS.len();
            manager.creation.custom_volume_size = "0.001".to_owned();
            manager.creation.encrypt = true;
            manager.creation.password.text.push_str("volume-password");
            manager.creation.confirmation.text.push_str("volume-password");
            manager.show_modal(Modal::CreateArchive, window, cx);
            manager
        });
        cx.update(|window, cx| view.update(cx, |manager, cx| {
            let options = manager.creation.resolved_options().unwrap();
            manager.start_archive_creation(output.clone(), options, window, cx);
            manager.close_modal(window, cx);
        }));
        cx.run_until_parked();
        let first = crate::volumes::part_path(&output, 1);
        assert!(!output.exists());
        assert!(crate::volumes::part_path(&output, 2).is_file());
        let parity = output.with_file_name("protected.zip.par2");
        assert!(parity.is_file());
        let parity_set = rust_par2::parse(&parity).unwrap();
        assert!(parity_set.files.len() >= 2);
        let recovery_blocks = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    name.to_string_lossy().starts_with("protected.zip")
                }) && path.extension().is_some_and(|extension| extension == "par2")
            })
            .map(|path| rust_par2::parse(&path).unwrap().recovery_block_count)
            .sum::<u32>();
        assert!(recovery_blocks > 0);
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.creation_task.is_none());
            assert!(manager.creation.error.is_none(), "{:?}", manager.creation.error);
            assert_eq!(manager.archive_path.as_ref(), Some(&first));
            assert!(manager.archive_password.is_some());
            assert_eq!(manager.entries[0].name, "private.txt");
        });
        assert!(archive::list_archive(&first).is_err());
        assert!(archive::list_archive_with_password(&first, Some("volume-password")).is_ok());
    }

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
            manager.creation.split = true;
            manager.creation.volume_preset = VOLUME_PRESETS.len();
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
            ChoiceField::VolumeSize => crate::ui::Selector::VolumeSize,
            ChoiceField::VolumeUnit => crate::ui::Selector::VolumeUnit,
            ChoiceField::RecoveryPercent => crate::ui::Selector::RecoveryPercent,
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
