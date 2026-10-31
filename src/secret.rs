//! Keeps password values synchronized with masked GPUI inputs and zeroizes them on clear.

use crate::{ArchiveManager, Theme};
use gpui::component::{
    Sizable,
    input::{Input, InputEvent, InputState},
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
}

/// Stores a zeroizing secret alongside its masked input and focus handle.
pub struct SecretInput {
    pub text: Zeroizing<String>,
    pub focus: FocusHandle,
    state: Option<Entity<InputState>>,
    subscription: Option<Subscription>,
}

impl SecretInput {
    /// Creates an empty secret field with a focus handle owned by the app.
    pub fn new(cx: &mut App) -> Self {
        Self {
            text: Zeroizing::new(String::new()),
            focus: cx.focus_handle(),
            state: None,
            subscription: None,
        }
    }
    /// Erases the secret and drops input state, subscriptions, and edit history.
    pub fn clear(&mut self) {
        self.text.zeroize();
        self.subscription = None;
        self.state = None;
    }
}

/// Selects the password input associated with a field kind.
fn input(manager: &mut ArchiveManager, field: SecretField) -> &mut SecretInput {
    match field {
        SecretField::Unlock => &mut manager.password_prompt.input,
        SecretField::Password => &mut manager.creation.password,
        SecretField::Confirmation => &mut manager.creation.confirmation,
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
            .masked(true)
            .default_value(value.text.as_str())
    });
    value.focus = state.focus_handle(cx);
    let subscription = cx.subscribe_in(
        &state,
        window,
        move |this, state, event, window, cx| match event {
            InputEvent::Change => {
                input(this, field).text = Zeroizing::new(state.read(cx).value().to_string());
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
}

/// Renders one prepared password field and controls whether it can be edited.
pub fn field(
    id: &'static str,
    value: &SecretInput,
    _kind: SecretField,
    enabled: bool,
    _theme: Theme,
    _cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .w_full()
        .child(
            Input::new(value.state.as_ref().expect("password input initialized"))
                .small()
                .disabled(!enabled),
        )
}
