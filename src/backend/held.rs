#![cfg_attr(test, allow(dead_code))]
//! Buttons and keys this process still holds.
//!
//! The registry is process-global outside tests so `serve` can press in one
//! request and release in a later one. Tests do not share it: the stub keeps
//! its own list, and these functions are no-ops so a unit test cannot post.

use crate::error::ComputerError;
use crate::types::{Modifier, MouseButton};

/// Where the down event was posted. The matching up must use the same path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeldWhere {
    /// `CGEventPostToPid` into `pid`.
    Event {
        /// Target pid.
        pid: i32,
        /// Target window id.
        window_id: u32,
        /// Window origin x, so the up uses the same local point.
        origin_x: i32,
        /// Window origin y.
        origin_y: i32,
    },
    /// Foreground enigo. Dropping enigo does not release mouse buttons.
    Activate,
}

#[derive(Debug, Clone)]
pub(crate) struct HeldButton {
    /// Button that is down.
    pub button: MouseButton,
    /// Screen x of the down.
    pub x: i32,
    /// Screen y of the down.
    pub y: i32,
    /// How to release it.
    pub where_: HeldWhere,
}

#[derive(Debug, Clone)]
pub(crate) struct HeldKey {
    /// Modifiers that are down.
    pub modifiers: Vec<Modifier>,
    /// Canonical key that is down.
    pub key: String,
    /// How to release it.
    pub where_: HeldWhere,
}

/// Remember a button. A second down for the same button replaces the first.
pub(crate) fn note_button(button: HeldButton) {
    #[cfg(not(test))]
    {
        let mut state = state().lock().unwrap_or_else(|poison| poison.into_inner());
        state.buttons.retain(|held| held.button != button.button);
        state.buttons.push(button);
    }
    #[cfg(test)]
    {
        let _ = button;
    }
}

/// Forget a button after a successful up.
pub(crate) fn clear_button(button: MouseButton) {
    #[cfg(not(test))]
    {
        let mut state = state().lock().unwrap_or_else(|poison| poison.into_inner());
        state.buttons.retain(|held| held.button != button);
    }
    #[cfg(test)]
    {
        let _ = button;
    }
}

/// Remember keys that are down. A new hold replaces the previous one.
pub(crate) fn note_key(key: HeldKey) {
    #[cfg(not(test))]
    {
        let mut state = state().lock().unwrap_or_else(|poison| poison.into_inner());
        state.keys.clear();
        state.keys.push(key);
    }
    #[cfg(test)]
    {
        let _ = key;
    }
}

/// Forget held keys after a successful up.
pub(crate) fn clear_keys() {
    #[cfg(not(test))]
    {
        let mut state = state().lock().unwrap_or_else(|poison| poison.into_inner());
        state.keys.clear();
    }
}

/// Release everything still held. An empty registry is success.
pub(crate) fn release_all() -> Result<(), ComputerError> {
    #[cfg(test)]
    {
        Ok(())
    }
    #[cfg(not(test))]
    {
        let taken = {
            let mut state = state().lock().unwrap_or_else(|poison| poison.into_inner());
            std::mem::take(&mut *state)
        };
        let mut first: Option<ComputerError> = None;
        for button in taken.buttons.into_iter().rev() {
            if let Err(error) = release_button(&button) {
                first.get_or_insert(error);
            }
        }
        for key in taken.keys.into_iter().rev() {
            if let Err(error) = release_key(&key) {
                first.get_or_insert(error);
            }
        }
        match first {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(not(test))]
#[derive(Default)]
struct State {
    buttons: Vec<HeldButton>,
    keys: Vec<HeldKey>,
}

#[cfg(not(test))]
fn state() -> &'static std::sync::Mutex<State> {
    static STATE: std::sync::OnceLock<std::sync::Mutex<State>> = std::sync::OnceLock::new();
    STATE.get_or_init(|| std::sync::Mutex::new(State::default()))
}

#[cfg(not(test))]
fn release_button(button: &HeldButton) -> Result<(), ComputerError> {
    match button.where_ {
        HeldWhere::Activate => super::input::button_up(button.x, button.y, button.button),
        HeldWhere::Event {
            pid,
            window_id,
            origin_x,
            origin_y,
        } => {
            #[cfg(target_os = "macos")]
            {
                super::macos::input::release_posted_button(
                    pid,
                    window_id,
                    origin_x,
                    origin_y,
                    button.x,
                    button.y,
                    button.button,
                )
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (pid, window_id, origin_x, origin_y);
                Err(ComputerError::Unsupported(
                    "event-tier button release is only available on macOS".into(),
                ))
            }
        }
    }
}

#[cfg(not(test))]
fn release_key(key: &HeldKey) -> Result<(), ComputerError> {
    match key.where_ {
        HeldWhere::Activate => super::input::key_up(&key.modifiers, &key.key),
        HeldWhere::Event {
            pid,
            window_id,
            origin_x,
            origin_y,
        } => {
            #[cfg(target_os = "macos")]
            {
                super::macos::input::release_posted_key(
                    pid,
                    window_id,
                    origin_x,
                    origin_y,
                    &key.modifiers,
                    &key.key,
                )
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (pid, window_id, origin_x, origin_y);
                Err(ComputerError::Unsupported(
                    "event-tier key release is only available on macOS".into(),
                ))
            }
        }
    }
}
