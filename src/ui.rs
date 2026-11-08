//! Synchronizes GPUI controls, themes, and dialogs with application state.

use crate::{
    ArchiveManager,
    archive::{CompressionFormat, ResolvedFormat},
    creation::ChoiceField,
    i18n::Language,
};
use gpui::component::{
    IndexPath, Sizable, WindowExt,
    select::{SearchableVec, Select, SelectEvent, SelectGroup, SelectState},
};
use gpui::{App, Context, Entity, IntoElement, SharedString, Subscription, Window, prelude::*, rgb};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// Identifies a retained selector managed by the application.
pub enum Selector {
    /// Application language selector.
    Language,
    /// Archive output format selector.
    Format,
    /// Archive compression-level selector.
    Compression,
    /// Outer compression method selector for TAR archives.
    CompressionFormat,
    /// 7z dictionary-size selector.
    Dictionary,
    VolumeSize,
    VolumeUnit,
    RecoveryPercent,
    ConversionFormat,
}

pub(crate) fn sidebar_button_style(
    theme: crate::Theme,
    selected: bool,
    cx: &App,
) -> gpui::component::button::ButtonCustomVariant {
    gpui::component::button::ButtonCustomVariant::new(cx)
        .color(if selected {
            gpui::rgba((theme.accent << 8) | 0x20).into()
        } else {
            gpui::transparent_black().into()
        })
        .foreground(rgb(theme.text).into())
        .hover(gpui::rgba((theme.text << 8) | 0x0c).into())
        .active(gpui::rgba((theme.accent << 8) | 0x30).into())
}

/// Retains selector options, selected index, and its event subscription.
struct SelectControl {
    state: SelectStateHandle,
    items: Vec<SharedString>,
    selected: usize,
    _subscription: Subscription,
}

enum SelectStateHandle {
    Flat(Entity<SelectState<Vec<SharedString>>>),
    Format(Entity<SelectState<SearchableVec<SelectGroup<SharedString>>>>),
}

fn format_index_path(index: usize) -> Option<IndexPath> {
    let formats = crate::archive::ResolvedFormat::CREATION_FORMATS;
    let selected = *formats.get(index)?;
    let section = usize::from(selected.archive.is_none());
    let row = formats[..index]
        .iter()
        .filter(|format| usize::from(format.archive.is_none()) == section)
        .count();
    Some(IndexPath::default().section(section).row(row))
}

#[derive(Default)]
/// Owns retained controls and synchronization state for modals and global themes.
pub struct Controls {
    selects: BTreeMap<Selector, SelectControl>,
    modal: Option<crate::Modal>,
    modal_generation: u64,
    dark: Option<bool>,
    accent_color: Option<crate::AccentColor>,
    language: Option<Language>,
}

impl Controls {
    /// Creates a selector once, then updates its options and selection as needed.
    fn sync(
        &mut self,
        selector: Selector,
        items: Vec<SharedString>,
        selected: usize,
        window: &mut Window,
        cx: &mut Context<ArchiveManager>,
    ) {
        if let Some(control) = self.selects.get_mut(&selector) {
            let SelectStateHandle::Flat(state) = &control.state else {
                unreachable!("format selector is synchronized through sync_format")
            };
            if control.items != items || control.selected != selected {
                state.update(cx, |state, cx| {
                    if control.items != items {
                        state.set_items(items.clone(), window, cx);
                    }
                    state.set_selected_index(Some(IndexPath::new(selected)), window, cx);
                });
                control.items = items;
                control.selected = selected;
            }
            return;
        }
        let state = cx
            .new(|cx| SelectState::new(items.clone(), Some(IndexPath::new(selected)), window, cx));
        let subscription = cx.subscribe_in(
            &state,
            window,
            move |this, _, event: &SelectEvent<Vec<SharedString>>, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    let Some(index) =
                        this.controls.selects.get(&selector).and_then(|control| {
                            control.items.iter().position(|item| item == value)
                        })
                    else {
                        return;
                    };
                    match selector {
                        Selector::Language => {
                            this.language = Language::ALL[index];
                            this.status_message = None;
                            this.save_preferences();
                        }
                        Selector::Format => this.creation.apply(ChoiceField::Format, index),
                        Selector::Compression => {
                            this.creation.apply(ChoiceField::Compression, index)
                        }
                        Selector::CompressionFormat => {
                            this.creation.apply(ChoiceField::CompressionFormat, index)
                        }
                        Selector::Dictionary => this.creation.apply(ChoiceField::Dictionary, index),
                        Selector::VolumeSize => this.creation.apply(ChoiceField::VolumeSize, index),
                        Selector::VolumeUnit => this.creation.apply(ChoiceField::VolumeUnit, index),
                        Selector::RecoveryPercent => {
                            this.creation.apply(ChoiceField::RecoveryPercent, index)
                        }
                        Selector::ConversionFormat => this.conversion.set_target(index),
                    }
                    cx.notify();
                }
            },
        );
        self.selects.insert(
            selector,
            SelectControl {
                state: SelectStateHandle::Flat(state),
                items,
                selected,
                _subscription: subscription,
            },
        );
    }

    fn sync_format(
        &mut self,
        language: Language,
        items: Vec<SharedString>,
        selected: usize,
        window: &mut Window,
        cx: &mut Context<ArchiveManager>,
    ) {
        let archive_items = items
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                crate::archive::ResolvedFormat::CREATION_FORMATS[*index]
                    .archive
                    .is_some()
            })
            .map(|(_, item)| item.clone())
            .collect::<Vec<_>>();
        let stream_items = items
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                crate::archive::ResolvedFormat::CREATION_FORMATS[*index]
                    .archive
                    .is_none()
            })
            .map(|(_, item)| item.clone())
            .collect::<Vec<_>>();
        let groups = SearchableVec::new(vec![
            SelectGroup::new(language.text("Archives")).items(archive_items),
            SelectGroup::new(language.text("Compression streams")).items(stream_items),
        ]);
        if let Some(control) = self.selects.get_mut(&Selector::Format) {
            let SelectStateHandle::Format(state) = &control.state else {
                unreachable!("format selector must use grouped state")
            };
            if control.items != items || control.selected != selected {
                state.update(cx, |state, cx| {
                    if control.items != items {
                        state.set_items(groups, window, cx);
                    }
                    if let Some(value) = items.get(selected) {
                        state.set_selected_value(value, window, cx);
                    }
                });
                control.items = items;
                control.selected = selected;
            }
            return;
        }
        let selected_value = items.get(selected).cloned();
        let selected_index = selected_value
            .as_ref()
            .and_then(|value| items.iter().position(|item| item == value))
            .and_then(format_index_path);
        let state =
            cx.new(|cx| SelectState::new(groups, selected_index, window, cx).searchable(true));
        let subscription = cx.subscribe_in(
            &state,
            window,
            |this, _, event: &SelectEvent<SearchableVec<SelectGroup<SharedString>>>, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    if let Some(index) = this
                        .controls
                        .selects
                        .get(&Selector::Format)
                        .and_then(|control| control.items.iter().position(|item| item == value))
                    {
                        this.creation.apply(ChoiceField::Format, index);
                        cx.notify();
                    }
                }
            },
        );
        self.selects.insert(
            Selector::Format,
            SelectControl {
                state: SelectStateHandle::Format(state),
                items,
                selected,
                _subscription: subscription,
            },
        );
    }
}

/// Applies the current language and appearance to GPUI and synchronizes selectors/modals.
pub fn prepare(
    manager: &mut ArchiveManager,
    window: &mut Window,
    cx: &mut Context<ArchiveManager>,
) {
    if manager.controls.language != Some(manager.language) {
        gpui::component::set_locale(match manager.language {
            Language::English => "en",
            Language::Polish => "pl",
            Language::ChineseSimplified => "zh-CN",
            Language::Japanese => "ja",
            Language::Spanish => "es",
            Language::German => "de",
        });
        manager.controls.language = Some(manager.language);
    }
    let dark = manager.theme_mode.is_dark(window.appearance());
    let dark_changed = manager.controls.dark != Some(dark);
    let accent_changed = manager.controls.accent_color != Some(manager.accent_color);
    if dark_changed {
        gpui::component::Theme::change(
            if dark {
                gpui::component::ThemeMode::Dark
            } else {
                gpui::component::ThemeMode::Light
            },
            Some(window),
            cx,
        );
    }
    if dark_changed || accent_changed {
        let palette = crate::Theme::new(dark).with_accent(manager.accent_color.color(dark));
        gpui::component::Theme::update(cx, |theme| {
            let primary: gpui::Hsla = gpui::rgb(palette.accent).into();
            let foreground = gpui::rgb(if dark { 0x111820 } else { 0xffffff }).into();
            let hover = gpui::Hsla {
                l: (primary.l + if dark { 0.06 } else { -0.05 }).clamp(0.0, 1.0),
                ..primary
            };
            let active = gpui::Hsla {
                l: (primary.l - 0.10).max(0.0),
                ..primary
            };
            theme.colors.background = gpui::rgb(palette.window).into();
            let editor = std::sync::Arc::make_mut(&mut theme.highlight_theme);
            editor.style.editor_background = Some(gpui::rgb(palette.surface).into());
            editor.style.editor_gutter_background = Some(gpui::rgb(palette.surface).into());
            editor.style.editor_foreground = Some(gpui::rgb(palette.text).into());
            editor.style.editor_line_number = Some(gpui::rgb(palette.faint_text).into());
            editor.style.editor_active_line_number = Some(gpui::rgb(palette.muted_text).into());
            editor.style.editor_active_line = Some(gpui::rgb(palette.hover).into());
            theme.colors.foreground = gpui::rgb(palette.text).into();
            theme.colors.popover = gpui::rgb(palette.surface).into();
            theme.colors.popover_foreground = gpui::rgb(palette.text).into();
            theme.colors.muted = gpui::rgb(palette.sidebar).into();
            theme.colors.muted_foreground = gpui::rgb(palette.muted_text).into();
            theme.colors.border = gpui::rgb(palette.border).into();
            theme.colors.input = gpui::rgb(palette.border).into();
            theme.colors.accent = gpui::rgb(palette.hover).into();
            theme.colors.accent_foreground = gpui::rgb(palette.text).into();
            theme.colors.sidebar_accent = gpui::rgb(palette.selection).into();
            theme.colors.sidebar_accent_foreground = gpui::rgb(palette.text).into();
            theme.colors.button_active = gpui::rgb(palette.selection).into();
            theme.colors.secondary_active = gpui::rgb(palette.selection).into();
            theme.colors.primary = primary;
            theme.colors.primary_hover = hover;
            theme.colors.primary_active = active;
            theme.colors.primary_foreground = foreground;
            theme.colors.button_primary = primary;
            theme.colors.button_primary_hover = hover;
            theme.colors.button_primary_active = active;
            theme.colors.button_primary_foreground = foreground;
            theme.colors.ring = gpui::rgb(palette.soft_accent()).into();
        });
        manager.controls.dark = Some(dark);
        manager.controls.accent_color = Some(manager.accent_color);
    }
    if manager.controls.modal != manager.modal {
        if manager.controls.modal.is_some() {
            window.close_dialog(cx);
            manager.password_blur.clear();
        }
        manager.controls.modal_generation += 1;
        let generation = manager.controls.modal_generation;
        manager.controls.modal = manager.modal;
        if let Some(modal) = manager.modal {
            let view = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                let _ = view.update(cx, |manager, cx| {
                    if manager.modal != Some(modal)
                        || manager.controls.modal_generation != generation
                    {
                        return;
                    }
                    if !manager.password_blur.has_capture() {
                        manager.password_blur.capture(window, cx);
                    }
                    open_dialog(modal, window, cx);
                    cx.notify();
                });
            });
        }
    }
    let language = manager.language;
    let language_index = Language::ALL
        .iter()
        .position(|value| *value == language)
        .unwrap_or(0);
    manager.controls.sync(
        Selector::Language,
        Language::ALL
            .iter()
            .map(|language| language.name().into())
            .collect(),
        language_index,
        window,
        cx,
    );
    manager.controls.sync_format(
        language,
        ResolvedFormat::CREATION_FORMATS
            .iter()
            .map(|format| format.label().into())
            .collect(),
        manager.creation.selected(ChoiceField::Format),
        window,
        cx,
    );
    manager.controls.sync(
        Selector::Compression,
        manager
            .creation
            .options
            .output_format()
            .compression_levels()
            .iter()
            .map(|level| language.text(level.label()).into())
            .collect(),
        manager.creation.selected(ChoiceField::Compression),
        window,
        cx,
    );
    manager.controls.sync(
        Selector::CompressionFormat,
        CompressionFormat::ALL
            .iter()
            .map(|method| language.text(method.label()).into())
            .collect(),
        manager.creation.selected(ChoiceField::CompressionFormat),
        window,
        cx,
    );
    manager.controls.sync(
        Selector::Dictionary,
        ["1 MiB", "8 MiB", "32 MiB", "64 MiB"]
            .map(SharedString::from)
            .to_vec(),
        manager.creation.selected(ChoiceField::Dictionary),
        window,
        cx,
    );
    manager.controls.sync(
        Selector::VolumeSize,
        ["100 MB", "250 MB", "500 MB", "1 GB", "2 GB", "4 GB"].into_iter()
            .map(SharedString::from).chain(std::iter::once(language.text("Custom size").into())).collect(),
        manager.creation.selected(ChoiceField::VolumeSize), window, cx,
    );
    manager.controls.sync(
        Selector::VolumeUnit, ["MB", "GB"].map(SharedString::from).to_vec(),
        manager.creation.selected(ChoiceField::VolumeUnit), window, cx,
    );
    manager.controls.sync(
        Selector::RecoveryPercent,
        ["5%", "10%", "20%", "30%", "50%"]
            .map(SharedString::from)
            .to_vec(),
        manager.creation.selected(ChoiceField::RecoveryPercent),
        window,
        cx,
    );
    manager.controls.sync(
        Selector::ConversionFormat,
        crate::conversion::FORMATS.iter().map(|format| format.label().into()).collect(),
        manager.conversion.target_index(), window, cx,
    );
}

/// Renders one retained selector with its localized accessibility label.
pub fn select(manager: &ArchiveManager, selector: Selector) -> impl IntoElement {
    let control = &manager.controls.selects[&selector];
    let (id, label) = match selector {
        Selector::Language => ("language-select", "Language"),
        Selector::Format => ("creation-format", "Format"),
        Selector::Compression => ("creation-compression", "Compression"),
        Selector::CompressionFormat => ("creation-tar-compression", "Compression method"),
        Selector::Dictionary => ("creation-dictionary", "Dictionary size"),
        Selector::VolumeSize => ("creation-volume-size", "Volume size"),
        Selector::VolumeUnit => ("creation-volume-unit", "Size unit"),
        Selector::RecoveryPercent => ("creation-parity-percent", "Recovery data"),
        Selector::ConversionFormat => ("conversion-target-format", "Target format"),
    };
    let selected = match &control.state {
        SelectStateHandle::Flat(state) => Select::new(state)
            .id("control")
            .small()
            .w_full()
            .accessibility_label(manager.language.text(label))
            .disabled(control.items.len() < 2)
            .into_any_element(),
        SelectStateHandle::Format(state) => Select::new(state)
            .id("control")
            .small()
            .w_full()
            .accessibility_label(manager.language.text(label))
            .search_placeholder(manager.language.text("Search formats"))
            .disabled(control.items.len() < 2)
            .into_any_element(),
    };
    gpui::div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .w_full()
        .child(selected)
}

/// Centers a dialog using measured title/footer geometry cached for the current modal.
pub fn center_dialog(
    dialog: gpui::component::dialog::Dialog,
    title: &'static str,
    footer: gpui::AnyElement,
) -> gpui::component::dialog::Dialog {
    dialog
        .p(gpui::px(16.0))
        .entrance(gpui::component::dialog::DialogEntrance::Fade)
        .centered()
        .title(title)
        .footer(footer)
}

/// Installs a modal and focuses its password field when the password dialog opens.
fn open_dialog(modal: crate::Modal, window: &mut Window, cx: &mut Context<ArchiveManager>) {
    let view = cx.entity().downgrade();
    window.open_dialog(cx, move |dialog, window, cx| {
        view.update(cx, |manager, cx| {
            let dark = manager.theme_mode.is_dark(window.appearance());
            let theme = crate::Theme::new(dark).with_accent(manager.accent_color.color(dark));
            crate::modal_view(dialog, modal, manager, theme, window, cx)
        })
        .unwrap_or_else(|_| gpui::component::dialog::Dialog::new(cx))
    });
    if modal == crate::Modal::Password {
        cx.defer_in(window, |manager, window, cx| {
            window.focus(&manager.password_prompt.input.focus, cx)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    /// Detects a visible painted dialog surface of the expected width.
    fn painted_dialog_surface(window: &Window, width: f32) -> bool {
        let scale = window.scale_factor();
        window.painted_quads().iter().any(|quad| {
            (quad.bounds.size.width.0 - width * scale).abs() <= 1.0
                && quad.bounds.size.height.0 > 96.0 * scale
                && quad.corner_radii.top_left.0 > 0.0
                && (!quad.background.is_transparent() || quad.border_color.a > 0.0)
        })
    }

    #[gpui::test]
    /// Keeps dialogs centered through content-driven height changes.
    fn dialogs_remain_centered_when_content_height_changes(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager
        });
        for modal in [
            crate::Modal::Settings,
            crate::Modal::About,
            crate::Modal::CreateArchive,
            crate::Modal::Password,
            crate::Modal::Delete,
        ] {
            let width = if modal == crate::Modal::Password {
                crate::blur::PASSWORD_WIDTH
            } else if modal == crate::Modal::Settings {
                620.0
            } else {
                460.0
            };
            cx.update(|window, cx| {
                view.update(cx, |manager, cx| manager.show_modal(modal, window, cx));
                let _ = window.draw(cx);
                window.simulate_next_frame(cx);
                let _ = window.draw(cx);
            });
            cx.run_until_parked();
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(250));
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            let panel = cx.debug_bounds("dialog-0").unwrap();
            let background = cx.debug_bounds("modal-blurred-background").unwrap();
            assert!((panel.center().y - background.center().y).abs() <= gpui::px(1.0));
            cx.update(|window, _| assert!(painted_dialog_surface(window, width)));
            for _ in 0..3 {
                cx.update(|window, cx| {
                    window.refresh();
                    let _ = window.draw(cx);
                });
                cx.run_until_parked();
                assert_eq!(cx.debug_bounds("dialog-0").unwrap(), panel);
            }
            if modal == crate::Modal::CreateArchive {
                let seven_zip_index = crate::archive::ResolvedFormat::CREATION_FORMATS
                    .iter()
                    .position(|format| *format == crate::archive::ResolvedFormat::SevenZip)
                    .unwrap();
                view.update(cx, |manager, cx| {
                    manager.creation.apply(crate::creation::ChoiceField::Format, seven_zip_index);
                    cx.notify();
                });
                cx.run_until_parked();
                let resized = cx.debug_bounds("dialog-0").unwrap();
                assert_ne!(resized.size.height, panel.size.height);
                assert!((resized.center().y - background.center().y).abs() <= gpui::px(1.0));
            }
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    /// Checks toolbar icon geometry and compact creation-dialog controls.
    fn titlebar_icons_match_toolbar_geometry_and_creation_icon_stays_compact(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.show_modal(crate::Modal::CreateArchive, window, cx);
            manager
        });
        let toolbar_icon = cx
            .debug_bounds("toolbar-icon:toolbar-files-dropdown")
            .expect("Files dropdown icon is rendered");
        assert_eq!(toolbar_icon.size.width, gpui::px(20.0));
        assert_eq!(toolbar_icon.size.height, gpui::px(20.0));
        let extraction_icon = cx
            .debug_bounds("toolbar-icon:toolbar-extract-dropdown")
            .expect("Extraction dropdown icon is rendered");
        assert_eq!(extraction_icon.size.width, gpui::px(20.0));
        assert_eq!(extraction_icon.size.height, gpui::px(20.0));
        let inspect_icon = cx
            .debug_bounds("toolbar-icon:toolbar-inspect-dropdown")
            .expect("Inspect dropdown icon is rendered");
        assert_eq!(inspect_icon.size.width, gpui::px(20.0));
        assert_eq!(inspect_icon.size.height, gpui::px(20.0));
        let back_icon = cx
            .debug_bounds("toolbar-icon:navigate-back")
            .expect("back navigation icon is rendered");
        assert_eq!(back_icon.size.width, gpui::px(11.0));
        assert_eq!(back_icon.size.height, gpui::px(11.0));
        let forward_icon = cx
            .debug_bounds("toolbar-icon:navigate-forward")
            .expect("forward navigation icon is rendered");
        assert_eq!(forward_icon.size.width, gpui::px(11.0));
        assert_eq!(forward_icon.size.height, gpui::px(11.0));
        let create_icon = cx
            .debug_bounds("toolbar-icon:add-archive-files")
            .expect("creation modal icon is rendered");
        assert_eq!(create_icon.size.width, gpui::px(12.0));
        assert_eq!(create_icon.size.height, gpui::px(12.0));
    }

    #[gpui::test]
    /// Verifies modal backdrops render and are removed after the dialog closes.
    fn every_modal_has_blurred_background_and_removes_it_on_close(cx: &mut gpui::TestAppContext) {
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager
        });
        for modal in [
            crate::Modal::Settings,
            crate::Modal::About,
            crate::Modal::CreateArchive,
            crate::Modal::Password,
            crate::Modal::Delete,
        ] {
            cx.update(|window, cx| {
                view.update(cx, |manager, cx| manager.show_modal(modal, window, cx));
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
            });
            cx.run_until_parked();
            for viewport in [
                gpui::size(gpui::px(1100.0), gpui::px(720.0)),
                gpui::size(gpui::px(560.0), gpui::px(440.0)),
            ] {
                cx.simulate_resize(viewport);
                cx.run_until_parked();
                let background = cx
                    .debug_bounds("modal-blurred-background")
                    .expect("modal has blur layer");
                let dialog = cx
                    .debug_bounds("dialog-0")
                    .expect("dialog is rendered above blur");
                assert!(background.size.width > gpui::px(0.0));
                assert!(background.size.height > gpui::px(0.0));
                assert!(dialog.right() <= background.right());
                assert!(dialog.bottom() <= background.bottom());
                assert!((dialog.center().x - background.center().x).abs() <= gpui::px(1.0));
                assert!((dialog.center().y - background.center().y).abs() <= gpui::px(1.0));
            }
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
            cx.read(|cx| assert!(view.read(cx).modal.is_none()));
            assert!(cx.debug_bounds("modal-blurred-background").is_none());
        }
    }

    #[gpui::test]
    /// Checks that app palette colors propagate to GPUI and About content renders.
    fn component_palette_matches_application_and_about_shows_license(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.show_modal(crate::Modal::About, window, cx);
            manager
        });
        for (mode, dark) in [
            (crate::ThemeMode::Light, false),
            (crate::ThemeMode::Dark, true),
        ] {
            view.update(cx, |manager, cx| {
                manager.theme_mode = mode;
                cx.notify();
            });
            cx.run_until_parked();
            cx.read(|cx| {
                let palette = crate::Theme::new(dark);
                assert_eq!(palette.titlebar, if dark { 0x131a21 } else { 0xeaf0f6 });
                assert_eq!(palette.sidebar, if dark { 0x161e26 } else { 0xeef3f8 });
                assert_eq!(
                    palette.soft_accent(),
                    if dark { 0xb4d7ff } else { 0x3a72ed }
                );
                let kit = gpui::component::Theme::global(cx);
                assert_eq!(kit.colors.background, gpui::rgb(palette.window).into());
                assert_eq!(
                    kit.tokens.background.color,
                    gpui::rgb(palette.window).into()
                );
                assert_eq!(kit.colors.popover, gpui::rgb(palette.surface).into());
                assert_eq!(kit.colors.accent, gpui::rgb(palette.hover).into());
                assert_eq!(kit.tokens.accent.color, gpui::rgb(palette.hover).into());
                assert_eq!(
                    kit.colors.button_active,
                    gpui::rgb(palette.selection).into()
                );
                assert_eq!(
                    kit.colors.secondary_active,
                    gpui::rgb(palette.selection).into()
                );
                assert_eq!(kit.colors.primary, gpui::rgb(palette.accent).into());
                assert_eq!(kit.tokens.primary.color, gpui::rgb(palette.accent).into());
                assert_eq!(kit.colors.button_primary, gpui::rgb(palette.accent).into());
                assert_eq!(
                    kit.tokens.button_primary.color,
                    gpui::rgb(palette.accent).into()
                );
                assert_eq!(
                    kit.colors.button_primary_foreground,
                    kit.colors.primary_foreground
                );
                assert_ne!(kit.colors.button_primary_hover, kit.colors.button_primary);
                assert_ne!(kit.colors.button_primary_active, kit.colors.button_primary);
                assert_eq!(kit.colors.foreground, gpui::rgb(palette.text).into());
                assert_eq!(kit.colors.ring, gpui::rgb(palette.soft_accent()).into());
                assert_eq!(
                    kit.colors.sidebar_accent,
                    gpui::rgb(palette.selection).into()
                );
                assert_eq!(
                    kit.tokens.sidebar_accent.color,
                    gpui::rgb(palette.selection).into()
                );
                assert_eq!(
                    kit.colors.sidebar_accent_foreground,
                    gpui::rgb(palette.text).into()
                );
            });
            let license = cx
                .debug_bounds("application-license")
                .expect("license is rendered in About");
            let app_icon = cx
                .debug_bounds("about-app-icon")
                .expect("application icon is rendered in About");
            assert!(license.size.width > gpui::px(0.0));
            assert!(license.size.height > gpui::px(0.0));
            assert!(app_icon.size.width > gpui::px(0.0));
            assert!(app_icon.size.height > gpui::px(0.0));
        }
        assert_eq!(env!("CARGO_PKG_LICENSE"), "WTFPL");
    }

    #[gpui::test]
    /// Ensures changing the accent updates global colors without changing theme mode.
    fn changing_accent_updates_global_palette_without_changing_theme_mode(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.theme_mode = crate::ThemeMode::Light;
            manager
        });
        view.update(cx, |manager, cx| {
            manager.accent_color = crate::AccentColor::Teal;
            cx.notify();
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let palette =
                crate::Theme::new(false).with_accent(crate::AccentColor::Teal.color(false));
            let kit = gpui::component::Theme::global(cx);
            assert_eq!(kit.colors.primary, gpui::rgb(palette.accent).into());
            assert_eq!(kit.colors.button_primary, gpui::rgb(palette.accent).into());
        });
    }

    #[gpui::test]
    /// Covers settings selector updates while preserving the current archive state.
    fn settings_select_changes_language_and_theme_without_resetting_archive_state(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::test::TestWindowExt;

        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.show_modal(crate::Modal::Settings, window, cx);
            manager
        });
        let layout = cx
            .debug_bounds("settings-two-column-layout")
            .expect("settings columns are rendered");
        let categories = cx
            .debug_bounds("settings-category-nav")
            .expect("settings categories are rendered");
        let settings_category = cx
            .update(|window, _| window.find("settings-category-general").bounds());
        let place = cx
            .debug_bounds("place-drop-home")
            .expect("Home place button is rendered");
        let content = cx
            .debug_bounds("settings-category-content")
            .expect("settings content is rendered");
        assert!(categories.right() <= content.origin.x);
        assert!(content.right() <= layout.right());
        assert_eq!(settings_category.size.height, gpui::px(34.0));
        assert!(place.size.height > settings_category.size.height);
        let default_location = cx
            .debug_bounds("default-location-field")
            .expect("default location field is rendered");
        let language_select = cx
            .debug_bounds("language-select")
            .expect("language selector is rendered");
        assert!(default_location.size.width > gpui::px(0.0));
        assert_eq!(default_location.size.height, language_select.size.height);
        for expected in [Language::Polish, Language::ChineseSimplified] {
            let bounds = cx.debug_bounds("language-select").unwrap();
            cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.simulate_keystrokes("down enter");
            cx.read(|cx| {
                let manager = view.read(cx);
                assert_eq!(manager.language, expected);
                assert_eq!(manager.entries.len(), 4);
            });
        }
        cx.update(|window, cx| {
            use gpui::test::TestWindowExt;
            window.click("settings-category-appearance", cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            use gpui::test::TestWindowExt;
            window.click("theme-dark", cx);
        });
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.theme_mode, crate::ThemeMode::Dark);
            assert_eq!(manager.controls.dark, Some(true));
            assert_eq!(manager.entries.len(), 4);
        });
    }
}
