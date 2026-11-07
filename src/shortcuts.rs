use gpui::{Action, App, KeyBinding, Keystroke, OperatingSystem};
use std::collections::{BTreeMap, BTreeSet};

gpui::actions!(zarchive_shortcuts, [CancelRecording, RecordEnter]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Command {
    Open,
    Create,
    ExtractHere,
    ExtractTo,
    Integrity,
    Audit,
    Previous,
    Next,
    EnterFolder,
    LeaveFolder,
    Preview,
}

impl Command {
    pub(crate) const ALL: [Self; 11] = [
        Self::Open,
        Self::Create,
        Self::ExtractHere,
        Self::ExtractTo,
        Self::Integrity,
        Self::Audit,
        Self::Previous,
        Self::Next,
        Self::EnterFolder,
        Self::LeaveFolder,
        Self::Preview,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Create => "create",
            Self::ExtractHere => "extract_here",
            Self::ExtractTo => "extract_to",
            Self::Integrity => "integrity",
            Self::Audit => "audit",
            Self::Previous => "previous",
            Self::Next => "next",
            Self::EnterFolder => "enter_folder",
            Self::LeaveFolder => "leave_folder",
            Self::Preview => "preview",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Open => "Open archive...",
            Self::Create => "New archive...",
            Self::ExtractHere => "Extract here",
            Self::ExtractTo => "Extract to...",
            Self::Integrity => "Test integrity",
            Self::Audit => "Audit archive",
            Self::Previous => "Previous item",
            Self::Next => "Next item",
            Self::EnterFolder => "Enter folder",
            Self::LeaveFolder => "Parent folder",
            Self::Preview => "Inspector",
        }
    }

    fn browser_only(self) -> bool {
        matches!(
            self,
            Self::Previous | Self::Next | Self::EnterFolder | Self::LeaveFolder | Self::Preview
        )
    }

    pub(crate) fn default_key(self) -> &'static str {
        match self {
            Self::Previous => "up",
            Self::Next => "down",
            Self::EnterFolder => "right",
            Self::LeaveFolder => "left",
            Self::Preview => "space",
            _ if cfg!(target_os = "macos") => match self {
                Self::Open => "cmd-o",
                Self::Create => "cmd-n",
                Self::ExtractHere => "cmd-e",
                Self::ExtractTo => "cmd-shift-e",
                Self::Integrity => "cmd-i",
                _ => "cmd-shift-i",
            },
            _ => match self {
                Self::Open => "ctrl-o",
                Self::Create => "ctrl-n",
                Self::ExtractHere => "ctrl-e",
                Self::ExtractTo => "ctrl-shift-e",
                Self::Integrity => "ctrl-i",
                _ => "ctrl-shift-i",
            },
        }
    }

    fn action(self) -> Box<dyn Action> {
        use crate::toolbar::key_actions::*;
        match self {
            Self::Open => Box::new(OpenFile),
            Self::Create => Box::new(CreateArchive),
            Self::ExtractHere => Box::new(ExtractHere),
            Self::ExtractTo => Box::new(ExtractTo),
            Self::Integrity => Box::new(TestIntegrity),
            Self::Audit => Box::new(AuditArchive),
            Self::Previous => Box::new(SelectPrevious),
            Self::Next => Box::new(SelectNext),
            Self::EnterFolder => Box::new(EnterFolder),
            Self::LeaveFolder => Box::new(LeaveFolder),
            Self::Preview => Box::new(PreviewSelection),
        }
    }
}

pub(crate) fn host_os() -> OperatingSystem {
    if cfg!(target_os = "macos") {
        OperatingSystem::Mac
    } else if cfg!(windows) {
        OperatingSystem::Windows
    } else {
        OperatingSystem::Linux
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShortcutSettings {
    overrides: BTreeMap<String, String>,
}

impl ShortcutSettings {
    pub(crate) fn combination(&self, command: Command) -> &str {
        self.overrides
            .get(command.key())
            .map(String::as_str)
            .unwrap_or(command.default_key())
    }

    fn normalize(command: Command, combination: &str) -> Result<String, &'static str> {
        if combination.is_empty() {
            return Ok(String::new());
        }
        let stroke = Keystroke::parse_with_os(combination, host_os())
            .map_err(|_| "Invalid keyboard shortcut")?;
        if combination.split_whitespace().count() != 1
            || stroke.key.is_empty()
            || matches!(
                stroke.key.as_str(),
                "escape" | "tab" | "shift" | "ctrl" | "alt" | "cmd" | "fn"
            )
        {
            return Err("Invalid keyboard shortcut");
        }
        if !command.browser_only()
            && !(stroke.modifiers.control || stroke.modifiers.platform || stroke.modifiers.alt)
            && !(stroke.key.starts_with('f')
                && stroke.key[1..]
                    .parse::<u8>()
                    .is_ok_and(|number| (1..=24).contains(&number)))
        {
            return Err("Use a modifier key for this action");
        }
        Ok(stroke.unparse(host_os()))
    }

    pub(crate) fn set(&mut self, command: Command, combination: &str) -> Result<(), &'static str> {
        let normalized = Self::normalize(command, combination)?;
        if !normalized.is_empty()
            && Command::ALL.into_iter().any(|other| {
                other != command
                    && Keystroke::parse_with_os(self.combination(other), host_os())
                        .is_ok_and(|value| value.unparse(host_os()) == normalized)
            })
        {
            return Err("This shortcut is already assigned");
        }
        if normalized
            == Keystroke::parse_with_os(command.default_key(), host_os())
                .unwrap()
                .unparse(host_os())
        {
            self.overrides.remove(command.key());
        } else {
            self.overrides.insert(command.key().into(), normalized);
        }
        Ok(())
    }

    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::json!(self.overrides)
    }

    pub(crate) fn from_json(value: Option<&serde_json::Value>) -> Self {
        let mut settings = Self::default();
        for command in Command::ALL {
            if let Some(combination) = value
                .and_then(|value| value.get(command.key()))
                .and_then(serde_json::Value::as_str)
            {
                if let Ok(normalized) = Self::normalize(command, combination) {
                    settings.overrides.insert(command.key().into(), normalized);
                }
            }
        }
        let mut combinations = BTreeSet::new();
        for command in Command::ALL {
            let normalized = Self::normalize(command, settings.combination(command)).unwrap();
            if !normalized.is_empty() && !combinations.insert(normalized) {
                return Self::default();
            }
        }
        settings
    }

    pub(crate) fn install(&self, recording: bool, cx: &mut App) {
        let retained: Vec<_> = cx
            .key_bindings()
            .borrow()
            .bindings()
            .filter(|binding| {
                !Command::ALL
                    .into_iter()
                    .any(|command| command.action().name() == binding.action().name())
                    && binding.action().name() != CancelRecording.name()
                    && binding.action().name() != RecordEnter.name()
                    && binding.action().name()
                        != crate::toolbar::key_actions::CloseBrowserTab.name()
            })
            .cloned()
            .collect();
        let mut bindings: Vec<_> = Command::ALL
            .into_iter()
            .filter(|command| !recording && !self.combination(*command).is_empty())
            .map(|command| {
                let context = if command.browser_only() {
                    "Browser"
                } else {
                    "ArchiveManager"
                };
                KeyBinding::load(
                    self.combination(command),
                    command.action(),
                    Some(
                        gpui::KeyBindingContextPredicate::parse(context)
                            .unwrap()
                            .into(),
                    ),
                    false,
                    None,
                    cx,
                )
                .expect("validated shortcut")
            })
            .collect();
        for context in ["Browser", "Inspector"] {
            bindings.push(KeyBinding::new(
                "ctrl-w",
                crate::toolbar::key_actions::CloseBrowserTab,
                Some(context),
            ));
            if cfg!(target_os = "macos") {
                bindings.push(KeyBinding::new(
                    "cmd-w",
                    crate::toolbar::key_actions::CloseBrowserTab,
                    Some(context),
                ));
            }
        }
        if recording {
            bindings.extend([
                KeyBinding::new("escape", CancelRecording, Some("ShortcutRecorder")),
                KeyBinding::new("enter", RecordEnter, Some("ShortcutRecorder")),
            ]);
        }
        cx.clear_key_bindings();
        cx.bind_keys(retained.into_iter().chain(bindings));
    }
}

#[derive(Default)]
pub(crate) struct Editor {
    pub(crate) recording: Option<Command>,
    pub(crate) error: Option<&'static str>,
    installed: Option<(ShortcutSettings, bool)>,
}

pub(crate) fn prepare(
    manager: &mut crate::ArchiveManager,
    cx: &mut gpui::Context<crate::ArchiveManager>,
) {
    if manager.modal != Some(crate::Modal::Settings)
        || manager.settings_category != crate::SettingsCategory::Keyboard
    {
        manager.shortcut_editor.recording = None;
    }
    let active = (
        manager.shortcut_settings.clone(),
        manager.shortcut_editor.recording.is_some(),
    );
    if manager.shortcut_editor.installed.as_ref() != Some(&active) {
        manager.shortcut_settings.install(active.1, cx);
        manager.shortcut_editor.installed = Some(active);
    }
}

fn record_key(
    manager: &mut crate::ArchiveManager,
    event: &gpui::KeyDownEvent,
    _: &mut gpui::Window,
    cx: &mut gpui::Context<crate::ArchiveManager>,
) {
    let Some(command) = manager.shortcut_editor.recording else {
        return;
    };
    cx.stop_propagation();
    if event.keystroke.key == "escape" {
        manager.shortcut_editor.recording = None;
        manager.shortcut_editor.error = None;
    } else if !matches!(
        event.keystroke.key.as_str(),
        "shift" | "ctrl" | "alt" | "cmd" | "fn" | ""
    ) {
        match manager
            .shortcut_settings
            .set(command, &event.keystroke.unparse(host_os()))
        {
            Ok(()) => {
                manager.shortcut_editor.recording = None;
                manager.shortcut_editor.error = None;
                manager.save_preferences();
            }
            Err(error) => manager.shortcut_editor.error = Some(error),
        }
    }
    cx.notify();
}

pub(crate) fn view(
    manager: &crate::ArchiveManager,
    theme: crate::Theme,
    cx: &mut gpui::Context<crate::ArchiveManager>,
) -> impl gpui::IntoElement {
    use gpui::component::{
        Selectable, Sizable,
        button::{Button, ButtonVariants},
    };
    use gpui::{div, prelude::*, px, rgb};
    let language = manager.language;
    div()
        .id("keyboard-shortcuts")
        .debug_selector(|| "keyboard-shortcuts".into())
        .track_focus(&manager.modal_focus)
        .key_context(if manager.shortcut_editor.recording.is_some() {
            "ShortcutRecorder"
        } else {
            "ShortcutSettings"
        })
        .on_action(cx.listener(|manager, _: &CancelRecording, _, cx| {
            manager.shortcut_editor.recording = None;
            manager.shortcut_editor.error = None;
            cx.notify();
        }))
        .on_action(cx.listener(|manager, _: &RecordEnter, window, cx| {
            record_key(
                manager,
                &gpui::KeyDownEvent {
                    keystroke: Keystroke::parse("enter").unwrap(),
                    is_held: false,
                    prefer_character_input: false,
                },
                window,
                cx,
            );
        }))
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .capture_key_down(cx.listener(record_key))
        .child(
            Button::new("reset-all-shortcuts")
                .debug_selector(|| "reset-all-shortcuts".into())
                .ghost()
                .small()
                .icon(gpui::assets::IconName::RotateCcw)
                .label(language.text("Reset all shortcuts"))
                .on_click(cx.listener(|manager, _, _, cx| {
                    manager.shortcut_settings = ShortcutSettings::default();
                    manager.shortcut_editor.recording = None;
                    manager.shortcut_editor.error = None;
                    manager.save_preferences();
                    cx.notify();
                })),
        )
        .when_some(manager.shortcut_editor.error, |this, error| {
            this.child(
                div()
                    .text_xs()
                    .text_color(rgb(theme.text))
                    .child(language.text(error)),
            )
        })
        .children(Command::ALL.into_iter().map(|command| {
            let recording = manager.shortcut_editor.recording == Some(command);
            let combination = manager.shortcut_settings.combination(command);
            let label = if recording {
                language.text("Press shortcut...").to_owned()
            } else if combination.is_empty() {
                language.text("Unassigned").to_owned()
            } else {
                combination.to_owned()
            };
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .py_1()
                .border_b_1()
                .border_color(rgb(theme.subtle_border))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(95.0))
                        .text_sm()
                        .child(language.text(command.label())),
                )
                .child(
                    Button::new(gpui::SharedString::from(format!(
                        "shortcut-{}",
                        command.key()
                    )))
                    .debug_selector(move || format!("shortcut-{}", command.key()))
                    .ghost()
                    .small()
                    .text_xs()
                    .w(px(150.0))
                    .selected(recording)
                    .icon(gpui::assets::IconName::Keyboard)
                    .label(label)
                    .on_click(cx.listener(move |manager, _, window, cx| {
                        manager.shortcut_editor.recording =
                            if recording { None } else { Some(command) };
                        manager.shortcut_editor.error = None;
                        window.focus(&manager.modal_focus, cx);
                        cx.notify();
                    })),
                )
                .child(
                    Button::new(gpui::SharedString::from(format!(
                        "reset-shortcut-{}",
                        command.key()
                    )))
                    .debug_selector(move || format!("reset-shortcut-{}", command.key()))
                    .ghost()
                    .small()
                    .icon(gpui::assets::IconName::RotateCcw)
                    .tooltip(language.text("Restore default shortcut"))
                    .accessibility_label(language.text("Restore default shortcut"))
                    .on_click(cx.listener(move |manager, _, _, cx| {
                        manager.shortcut_editor.recording = None;
                        manager.shortcut_editor.error = manager
                            .shortcut_settings
                            .set(command, command.default_key())
                            .err();
                        if manager.shortcut_editor.error.is_none() {
                            manager.save_preferences();
                        }
                        cx.notify();
                    })),
                )
                .child(
                    Button::new(gpui::SharedString::from(format!(
                        "clear-shortcut-{}",
                        command.key()
                    )))
                    .debug_selector(move || format!("clear-shortcut-{}", command.key()))
                    .ghost()
                    .small()
                    .icon(gpui::assets::IconName::X)
                    .tooltip(language.text("Disable shortcut"))
                    .accessibility_label(language.text("Disable shortcut"))
                    .on_click(cx.listener(move |manager, _, _, cx| {
                        manager.shortcut_editor.recording = None;
                        manager.shortcut_editor.error =
                            manager.shortcut_settings.set(command, "").err();
                        manager.save_preferences();
                        cx.notify();
                    })),
                )
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_validate_conflicts_and_round_trip() {
        let mut settings = ShortcutSettings::default();
        assert!(
            settings
                .set(Command::Open, Command::Create.default_key())
                .is_err()
        );
        assert!(settings.set(Command::Open, "a").is_err());
        assert!(settings.set(Command::Preview, "escape").is_err());
        settings.set(Command::Open, "ctrl-shift-p").unwrap();
        assert_eq!(
            ShortcutSettings::from_json(Some(&settings.json())),
            settings
        );
        settings
            .set(Command::Open, Command::Open.default_key())
            .unwrap();
        assert_eq!(settings, ShortcutSettings::default());
        settings.set(Command::Preview, "").unwrap();
        assert_eq!(settings.combination(Command::Preview), "");
        assert_eq!(
            ShortcutSettings::from_json(Some(&settings.json())),
            settings
        );
        settings.set(Command::Create, "ctrl-shift-k").unwrap();
        settings
            .set(Command::Open, Command::Create.default_key())
            .unwrap();
        assert_eq!(
            ShortcutSettings::from_json(Some(&settings.json())),
            settings
        );
    }

    #[gpui::test]
    fn keyboard_settings_record_rebind_cancel_and_reset(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.show_modal(crate::Modal::Settings, window, cx);
            manager
        });
        cx.update(|window, cx| window.click("settings-category-keyboard", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("keyboard-shortcuts").is_some());
        cx.update(|window, cx| window.click("shortcut-create", cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-shift-k");
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(
                manager.shortcut_settings.combination(Command::Create),
                "ctrl-shift-k"
            );
            assert!(manager.shortcut_editor.recording.is_none());
        });
        cx.update(|window, cx| window.click("shortcut-open", cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-shift-k");
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(
                manager.shortcut_editor.error,
                Some("This shortcut is already assigned")
            );
            assert_eq!(
                manager.shortcut_settings.combination(Command::Open),
                Command::Open.default_key()
            );
        });
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.shortcut_editor.recording.is_none());
            assert!(manager.modal == Some(crate::Modal::Settings));
        });
        for language in crate::i18n::Language::ALL {
            view.update(cx, |manager, cx| {
                manager.language = language;
                cx.notify();
            });
            for viewport in [
                gpui::size(gpui::px(1100.0), gpui::px(720.0)),
                gpui::size(gpui::px(560.0), gpui::px(440.0)),
            ] {
                cx.simulate_resize(viewport);
                cx.run_until_parked();
                let panel = cx.debug_bounds("dialog-0").unwrap();
                let shortcuts = cx.debug_bounds("keyboard-shortcuts").unwrap();
                assert!(shortcuts.right() <= panel.right());
                assert!(panel.bottom() <= viewport.height);
            }
        }
        cx.update(|window, cx| view.update(cx, |manager, cx| manager.close_modal(window, cx)));
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| window.focus(&manager.content_focus, cx))
        });
        cx.run_until_parked();
        cx.simulate_keystrokes(Command::Create.default_key());
        cx.read(|cx| assert!(view.read(cx).modal.is_none()));
        cx.simulate_keystrokes("ctrl-shift-k");
        cx.run_until_parked();
        cx.read(|cx| assert!(view.read(cx).modal == Some(crate::Modal::CreateArchive)));
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.close_modal(window, cx);
                manager.settings_category = crate::SettingsCategory::Keyboard;
                manager.show_modal(crate::Modal::Settings, window, cx);
            })
        });
        cx.run_until_parked();
        for _ in 0..3 {
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        }
        cx.update(|window, cx| window.click("reset-all-shortcuts", cx));
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(view.read(cx).shortcut_settings, ShortcutSettings::default()));
        cx.update(|window, cx| window.click("clear-shortcut-create", cx));
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).shortcut_settings.combination(Command::Create),
                ""
            )
        });
        cx.update(|window, cx| window.click("reset-shortcut-create", cx));
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(view.read(cx).shortcut_settings, ShortcutSettings::default()));
    }
}
