//! Keeps password values synchronized with masked GPUI inputs and zeroizes them on clear.

use crate::{ArchiveManager, i18n::Language};
use gpui::component::{
    Sizable,
    button::{Button, ButtonVariants},
    input::{Copy, Cut, Input, InputContentType, InputEvent, InputState},
};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, Subscription, Window, div,
    prelude::*,
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Copy)]
/// Identifies which password-bearing form field is being prepared or updated.
pub enum SecretField {
    /// Password entered to unlock an existing archive.
    Unlock,
    /// Password entered while creating an archive.
    Password,
    /// Confirmation value for an archive-creation password.
    Confirmation,
    Recovery,
    Conversion,
}

/// Stores a zeroizing secret alongside its masked input and focus handle.
pub struct SecretInput {
    pub text: Zeroizing<String>,
    pub focus: FocusHandle,
    visible: bool,
    state: Option<Entity<InputState>>,
    subscription: Option<Subscription>,
}

impl SecretInput {
    /// Creates an empty secret field with a focus handle owned by the app.
    pub fn new(cx: &mut App) -> Self {
        Self {
            text: Zeroizing::new(String::new()),
            focus: cx.focus_handle(),
            visible: false,
            state: None,
            subscription: None,
        }
    }
    /// Erases the secret and drops input state, subscriptions, and edit history.
    pub fn clear(&mut self) {
        self.text.zeroize();
        self.visible = false;
        self.subscription = None;
        self.state = None;
    }
}

pub fn generate_password() -> Result<Zeroizing<String>, getrandom::Error> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut random = Zeroizing::new([0u8; 24]);
    getrandom::fill(random.as_mut())?;
    let mut password = Zeroizing::new(String::with_capacity(random.len()));
    for byte in random.iter() {
        password.push(ALPHABET[usize::from(byte & 63)] as char);
    }
    Ok(password)
}

/// Selects the password input associated with a field kind.
fn input(manager: &mut ArchiveManager, field: SecretField) -> &mut SecretInput {
    match field {
        SecretField::Unlock => &mut manager.password_prompt.input,
        SecretField::Password => &mut manager.creation.password,
        SecretField::Confirmation => &mut manager.creation.confirmation,
        SecretField::Recovery => &mut manager.recovery.password,
        SecretField::Conversion => &mut manager.conversion.password,
    }
}

/// Creates or synchronizes a masked GPUI input for a secret value.
fn prepare_input(
    value: &mut SecretInput,
    field: SecretField,
    window: &mut Window,
    cx: &mut Context<ArchiveManager>,
) {
    if let Some(state) = &value.state {
        if state.read(cx).value().as_ref() != value.text.as_str() {
            state.update(cx, |state, cx| {
                state.set_value(value.text.as_str(), window, cx)
            });
        }
        return;
    }
    let state = cx.new(|cx| {
        InputState::new(window, cx)
            .masked(!value.visible)
            .default_value(value.text.as_str())
    });
    value.focus = state.focus_handle(cx);
    let subscription = cx.subscribe_in(
        &state,
        window,
        move |this, state, event, window, cx| match event {
            InputEvent::Change => {
                input(this, field).text = Zeroizing::new(state.read(cx).value().to_string());
                if matches!(field, SecretField::Conversion) { this.invalidate_conversion_analysis(); }
                cx.notify();
            }
            InputEvent::PressEnter { .. } if matches!(field, SecretField::Unlock) => {
                this.unlock_archive(window, cx)
            }
            _ => {}
        },
    );
    value.state = Some(state);
    value.subscription = Some(subscription);
}

/// Prepares all password inputs owned by the archive manager.
pub fn prepare(
    manager: &mut ArchiveManager,
    window: &mut Window,
    cx: &mut Context<ArchiveManager>,
) {
    prepare_input(
        &mut manager.password_prompt.input,
        SecretField::Unlock,
        window,
        cx,
    );
    prepare_input(
        &mut manager.creation.password,
        SecretField::Password,
        window,
        cx,
    );
    prepare_input(
        &mut manager.creation.confirmation,
        SecretField::Confirmation,
        window,
        cx,
    );
    prepare_input(&mut manager.recovery.password, SecretField::Recovery, window, cx);
    prepare_input(&mut manager.conversion.password, SecretField::Conversion, window, cx);
}

/// Renders one prepared password field and controls whether it can be edited.
pub fn field(
    id: &'static str,
    value: &SecretInput,
    kind: SecretField,
    enabled: bool,
    language: Language,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let label = language.text(if value.visible {
        "Hide password"
    } else {
        "Show password"
    });
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .w_full()
        .capture_action(|_: &Copy, _, cx| cx.stop_propagation())
        .capture_action(|_: &Cut, _, cx| cx.stop_propagation())
        .child(
            Input::new(value.state.as_ref().expect("password input initialized"))
                .small()
                .disabled(!enabled)
                .content_type(InputContentType::NewPassword)
                .context_menu(|menu, _, _| menu)
                .when(matches!(kind, SecretField::Password), |this| {
                    this.suffix(
                        Button::new("toggle-creation-password")
                            .debug_selector(|| "toggle-creation-password".to_owned())
                            .icon(if value.visible {
                                gpui::assets::IconName::EyeOff
                            } else {
                                gpui::assets::IconName::Eye
                            })
                            .ghost()
                            .xsmall()
                            .tab_stop(false)
                            .accessibility_label(label)
                            .tooltip(label)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let value = input(this, kind);
                                value.visible = !value.visible;
                                if let Some(state) = &value.state {
                                    state.update(cx, |state, cx| {
                                        state.set_masked(!value.visible, window, cx);
                                    });
                                }
                                cx.notify();
                            })),
                    )
                }),
        )
}

#[cfg(test)]
mod tests {
    use gpui::{ClipboardItem, Modifiers};

    #[gpui::test]
    fn generated_password_preview_stays_inside_input_and_clears(cx: &mut gpui::TestAppContext) {
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.creation.encrypt = true;
            manager.show_modal(crate::Modal::CreateArchive, window, cx);
            manager
        });
        let generate = cx.debug_bounds("generate-archive-password").unwrap();
        cx.simulate_mouse_move(generate.center(), None, Modifiers::default());
        cx.simulate_click(generate.center(), Modifiers::default());
        let first = cx.read(|cx| {
            let form = &view.read(cx).creation;
            assert!(!form.password.visible);
            assert!(form.password.text.as_str() == form.confirmation.text.as_str());
            form.password.text.clone()
        });
        assert_eq!(first.len(), 24);

        let field = cx.debug_bounds("creation-password").unwrap();
        let eye = cx.debug_bounds("toggle-creation-password").unwrap();
        assert!(eye.origin.x >= field.origin.x);
        assert!(eye.right() <= field.right());
        assert!(eye.origin.y >= field.origin.y);
        assert!(eye.bottom() <= field.bottom());
        cx.simulate_mouse_move(eye.center(), None, Modifiers::default());
        cx.simulate_click(eye.center(), Modifiers::default());
        cx.read(|cx| assert!(view.read(cx).creation.password.visible));

        cx.simulate_mouse_move(field.center(), None, Modifiers::default());
        cx.simulate_click(field.center(), Modifiers::default());
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".to_owned()));
        cx.simulate_keystrokes("cmd-a cmd-c");
        assert!(cx.read_from_clipboard().unwrap().text().as_deref() == Some("unchanged"));
        cx.simulate_keystrokes("cmd-x");
        assert!(cx.read_from_clipboard().unwrap().text().as_deref() == Some("unchanged"));
        cx.read(|cx| assert!(view.read(cx).creation.password.text.as_str() == first.as_str()));

        cx.simulate_mouse_move(generate.center(), None, Modifiers::default());
        cx.simulate_click(generate.center(), Modifiers::default());
        cx.read(|cx| {
            let form = &view.read(cx).creation;
            assert!(!form.password.visible);
            assert!(form.password.text.as_str() != first.as_str());
        });
        let second = cx.read(|cx| view.read(cx).creation.password.text.clone());
        cx.simulate_mouse_move(field.center(), None, Modifiers::default());
        cx.simulate_click(field.center(), Modifiers::default());
        cx.simulate_keystrokes("cmd-z");
        cx.read(|cx| assert!(view.read(cx).creation.password.text.as_str() == second.as_str()));

        let toggle = cx.debug_bounds("encrypt-archive").unwrap();
        cx.simulate_mouse_move(toggle.center(), None, Modifiers::default());
        cx.simulate_click(toggle.center(), Modifiers::default());
        cx.read(|cx| {
            let form = &view.read(cx).creation;
            assert!(form.password.text.is_empty());
            assert!(form.confirmation.text.is_empty());
            assert!(!form.password.visible);
        });
        assert!(cx.debug_bounds("generate-archive-password").is_none());

        let toggle = cx.debug_bounds("encrypt-archive").unwrap();
        cx.simulate_mouse_move(toggle.center(), None, Modifiers::default());
        cx.simulate_click(toggle.center(), Modifiers::default());
        let generate = cx.debug_bounds("generate-archive-password").unwrap();
        cx.simulate_mouse_move(generate.center(), None, Modifiers::default());
        cx.simulate_click(generate.center(), Modifiers::default());
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.read(|cx| {
            let form = &view.read(cx).creation;
            assert!(form.password.text.is_empty());
            assert!(form.confirmation.text.is_empty());
            assert!(!form.password.visible);
        });
    }
}
