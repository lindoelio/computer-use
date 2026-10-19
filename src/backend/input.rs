//! Pointer and keyboard input through enigo.
//!
//! Adapted from the Bezalel macOS desktop backend. The same enigo calls run
//! on macOS, Windows, and Linux (X11 via libxdo).

use enigo::{Axis, Button, Coordinate, Direction, Keyboard, Mouse};

use crate::error::ComputerError;
use crate::types::{Modifier, MouseButton};

pub(crate) fn move_mouse(x: i32, y: i32) -> Result<(), ComputerError> {
    let mut session = session()?;
    session
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|error| ComputerError::ExecutionFailed(format!("mouse move failed: {error}")))
}

pub(crate) fn click(x: i32, y: i32, button: MouseButton) -> Result<(), ComputerError> {
    let mut session = session()?;
    session
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|error| ComputerError::ExecutionFailed(format!("mouse move failed: {error}")))?;
    session
        .button(map_button(button), Direction::Click)
        .map_err(|error| ComputerError::ExecutionFailed(format!("click failed: {error}")))
}

pub(crate) fn type_text(text: &str) -> Result<(), ComputerError> {
    if text.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "text must not be empty".into(),
        ));
    }
    if text.contains('\0') {
        return Err(ComputerError::InvalidArguments(
            "text must not contain a null byte".into(),
        ));
    }
    let mut session = session()?;
    session
        .text(text)
        .map_err(|error| ComputerError::ExecutionFailed(format!("type failed: {error}")))
}

pub(crate) fn press_key(modifiers: &[Modifier], key: &str) -> Result<(), ComputerError> {
    let enigo_key = map_key(key)?;
    let mut session = session()?;
    let mapped: Vec<enigo::Key> = modifiers.iter().copied().map(map_modifier).collect();
    for modifier in &mapped {
        session.key(*modifier, Direction::Press).map_err(|error| {
            ComputerError::ExecutionFailed(format!("modifier press failed: {error}"))
        })?;
    }
    let typed = session.key(enigo_key, Direction::Click);
    for modifier in mapped.iter().rev() {
        let _ = session.key(*modifier, Direction::Release);
    }
    typed.map_err(|error| ComputerError::ExecutionFailed(format!("key press failed: {error}")))
}

pub(crate) fn scroll(dx: i32, dy: i32) -> Result<(), ComputerError> {
    if dx == 0 && dy == 0 {
        return Ok(());
    }
    let mut session = session()?;
    if dy != 0 {
        session.scroll(dy, Axis::Vertical).map_err(|error| {
            ComputerError::ExecutionFailed(format!("vertical scroll failed: {error}"))
        })?;
    }
    if dx != 0 {
        session.scroll(dx, Axis::Horizontal).map_err(|error| {
            ComputerError::ExecutionFailed(format!("horizontal scroll failed: {error}"))
        })?;
    }
    Ok(())
}

pub(crate) fn cursor_position() -> Result<(i32, i32), ComputerError> {
    let session = session()?;
    session
        .location()
        .map_err(|error| ComputerError::ExecutionFailed(format!("cursor position failed: {error}")))
}

/// Return an error when `key` is not a known name or a single character.
pub fn validate_key(key: &str) -> Result<(), ComputerError> {
    map_key(key).map(|_| ())
}

fn session() -> Result<enigo::Enigo, ComputerError> {
    // Agents should fail with an error instead of blocking on a permission dialog.
    let settings = enigo::Settings {
        open_prompt_to_get_permissions: false,
        release_keys_when_dropped: true,
        ..enigo::Settings::default()
    };
    enigo::Enigo::new(&settings).map_err(|error| {
        ComputerError::ExecutionFailed(format!("failed to initialize input: {error}"))
    })
}

fn map_button(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
    }
}

fn map_modifier(modifier: Modifier) -> enigo::Key {
    match modifier {
        Modifier::Command | Modifier::Meta => enigo::Key::Meta,
        Modifier::Control => enigo::Key::Control,
        Modifier::Shift => enigo::Key::Shift,
        Modifier::Alt => enigo::Key::Alt,
    }
}

fn map_key(key: &str) -> Result<enigo::Key, ComputerError> {
    if key.chars().count() != 1 {
        return map_named(key).or_else(|_| map_named(&key.to_ascii_lowercase()));
    }
    // A single character is typed as itself, including digits and letters.
    // Named keys (enter, f1, left, ...) are more than one character.
    match key.chars().next() {
        Some(ch) => Ok(enigo::Key::Unicode(ch)),
        None => Err(ComputerError::InvalidArguments("unknown key \"\"".into())),
    }
}

fn map_named(key: &str) -> Result<enigo::Key, ComputerError> {
    let mapped = match key {
        "space" => enigo::Key::Space,
        "enter" | "return" => enigo::Key::Return,
        "tab" => enigo::Key::Tab,
        "esc" | "escape" => enigo::Key::Escape,
        "backspace" => enigo::Key::Backspace,
        "delete" => enigo::Key::Delete,
        "left" => enigo::Key::LeftArrow,
        "right" => enigo::Key::RightArrow,
        "up" => enigo::Key::UpArrow,
        "down" => enigo::Key::DownArrow,
        "home" => enigo::Key::Home,
        "end" => enigo::Key::End,
        "pageup" => enigo::Key::PageUp,
        "pagedown" => enigo::Key::PageDown,
        "f1" => enigo::Key::F1,
        "f2" => enigo::Key::F2,
        "f3" => enigo::Key::F3,
        "f4" => enigo::Key::F4,
        "f5" => enigo::Key::F5,
        "f6" => enigo::Key::F6,
        "f7" => enigo::Key::F7,
        "f8" => enigo::Key::F8,
        "f9" => enigo::Key::F9,
        "f10" => enigo::Key::F10,
        "f11" => enigo::Key::F11,
        "f12" => enigo::Key::F12,
        "shift" => enigo::Key::Shift,
        "control" | "ctrl" => enigo::Key::Control,
        "alt" | "option" => enigo::Key::Alt,
        "meta" | "cmd" | "command" | "super" => enigo::Key::Meta,
        _ => {
            return Err(ComputerError::InvalidArguments(format!(
                "unknown key {key:?}"
            )))
        }
    };
    Ok(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_named_function_and_arrow_keys() {
        assert!(matches!(map_key("space").unwrap(), enigo::Key::Space));
        assert!(matches!(map_key("enter").unwrap(), enigo::Key::Return));
        assert!(matches!(map_key("tab").unwrap(), enigo::Key::Tab));
        assert!(matches!(map_key("esc").unwrap(), enigo::Key::Escape));
        assert!(matches!(
            map_key("backspace").unwrap(),
            enigo::Key::Backspace
        ));
        assert!(matches!(map_key("delete").unwrap(), enigo::Key::Delete));
        assert!(matches!(map_key("left").unwrap(), enigo::Key::LeftArrow));
        assert!(matches!(map_key("right").unwrap(), enigo::Key::RightArrow));
        assert!(matches!(map_key("up").unwrap(), enigo::Key::UpArrow));
        assert!(matches!(map_key("down").unwrap(), enigo::Key::DownArrow));
        assert!(matches!(map_key("home").unwrap(), enigo::Key::Home));
        assert!(matches!(map_key("end").unwrap(), enigo::Key::End));
        assert!(matches!(map_key("pageup").unwrap(), enigo::Key::PageUp));
        assert!(matches!(map_key("pagedown").unwrap(), enigo::Key::PageDown));
        assert!(matches!(map_key("f1").unwrap(), enigo::Key::F1));
        assert!(matches!(map_key("F12").unwrap(), enigo::Key::F12));
        assert!(matches!(map_key("meta").unwrap(), enigo::Key::Meta));
        assert!(matches!(map_key("shift").unwrap(), enigo::Key::Shift));
    }

    #[test]
    fn maps_single_characters_and_rejects_unknown_names() {
        match map_key("a").unwrap() {
            enigo::Key::Unicode(ch) => assert_eq!(ch, 'a'),
            other => panic!("expected Unicode a, got {other:?}"),
        }
        match map_key("Z").unwrap() {
            enigo::Key::Unicode(ch) => assert_eq!(ch, 'Z'),
            other => panic!("expected Unicode Z, got {other:?}"),
        }
        match map_key("4").unwrap() {
            enigo::Key::Unicode(ch) => assert_eq!(ch, '4'),
            other => panic!("expected Unicode 4, got {other:?}"),
        }
        assert!(map_key("not-a-key").is_err());
        assert!(map_key("").is_err());
        assert!(validate_key("enter").is_ok());
        assert!(validate_key("nope").is_err());
    }

    #[test]
    fn modifiers_use_meta_for_command_and_super() {
        assert!(matches!(map_modifier(Modifier::Command), enigo::Key::Meta));
        assert!(matches!(map_modifier(Modifier::Meta), enigo::Key::Meta));
        assert!(matches!(
            map_modifier(Modifier::Control),
            enigo::Key::Control
        ));
        assert!(matches!(map_modifier(Modifier::Shift), enigo::Key::Shift));
        assert!(matches!(map_modifier(Modifier::Alt), enigo::Key::Alt));
        assert!(matches!(map_button(MouseButton::Left), Button::Left));
        assert!(matches!(map_button(MouseButton::Right), Button::Right));
        assert!(matches!(map_button(MouseButton::Middle), Button::Middle));
    }
}
