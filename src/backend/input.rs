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
    let mapped = map_button(button)?;
    let mut session = session()?;
    session
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|error| ComputerError::ExecutionFailed(format!("mouse move failed: {error}")))?;
    session
        .button(mapped, Direction::Click)
        .map_err(|error| ComputerError::ExecutionFailed(format!("click failed: {error}")))
}

/// Press, move through every point, then release. A failed move releases the button.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn drag(points: &[(i32, i32)], button: MouseButton) -> Result<(), ComputerError> {
    if points.len() < 2 {
        return Err(ComputerError::InvalidArguments(
            "drag needs at least 2 points".into(),
        ));
    }
    let mapped = map_button(button)?;
    let mut session = session()?;
    session
        .move_mouse(points[0].0, points[0].1, Coordinate::Abs)
        .map_err(|error| ComputerError::ExecutionFailed(format!("mouse move failed: {error}")))?;
    session
        .button(mapped, Direction::Press)
        .map_err(|error| ComputerError::ExecutionFailed(format!("button press failed: {error}")))?;
    let mut released = false;
    let moved = (|| {
        for point in &points[1..] {
            session
                .move_mouse(point.0, point.1, Coordinate::Abs)
                .map_err(|error| {
                    ComputerError::ExecutionFailed(format!("mouse move failed: {error}"))
                })?;
        }
        session
            .button(mapped, Direction::Release)
            .map_err(|error| {
                ComputerError::ExecutionFailed(format!("button release failed: {error}"))
            })?;
        released = true;
        Ok(())
    })();
    if !released {
        let _ = session.button(mapped, Direction::Release);
    }
    moved
}

/// Button down. The process registry releases it if no later up runs.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn button_down(x: i32, y: i32, button: MouseButton) -> Result<(), ComputerError> {
    let mapped = map_button(button)?;
    let mut session = session()?;
    session
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|error| ComputerError::ExecutionFailed(format!("mouse move failed: {error}")))?;
    session
        .button(mapped, Direction::Press)
        .map_err(|error| ComputerError::ExecutionFailed(format!("button press failed: {error}")))?;
    super::held::note_button(super::held::HeldButton {
        button,
        x,
        y,
        where_: super::held::HeldWhere::Activate,
    });
    Ok(())
}

/// Button up. Clears the registry after a successful release.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn button_up(x: i32, y: i32, button: MouseButton) -> Result<(), ComputerError> {
    let mapped = map_button(button)?;
    let mut session = session()?;
    session
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|error| ComputerError::ExecutionFailed(format!("mouse move failed: {error}")))?;
    session
        .button(mapped, Direction::Release)
        .map_err(|error| {
            ComputerError::ExecutionFailed(format!("button release failed: {error}"))
        })?;
    super::held::clear_button(button);
    Ok(())
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

/// Key down, sleep `ms`, key up. The registry keeps the key if the up fails.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn hold_key(modifiers: &[Modifier], key: &str, ms: u64) -> Result<(), ComputerError> {
    if ms > crate::tier::MAX_HOLD_MS {
        return Err(ComputerError::InvalidArguments(format!(
            "hold_key ms must be at most {}",
            crate::tier::MAX_HOLD_MS
        )));
    }
    key_down(modifiers, key)?;
    if ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    key_up(modifiers, key)
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) fn key_down(modifiers: &[Modifier], key: &str) -> Result<(), ComputerError> {
    let enigo_key = map_key(key)?;
    let mut session = session()?;
    let mapped: Vec<enigo::Key> = modifiers.iter().copied().map(map_modifier).collect();
    for modifier in &mapped {
        session.key(*modifier, Direction::Press).map_err(|error| {
            ComputerError::ExecutionFailed(format!("modifier press failed: {error}"))
        })?;
    }
    session
        .key(enigo_key, Direction::Press)
        .map_err(|error| ComputerError::ExecutionFailed(format!("key press failed: {error}")))?;
    super::held::note_key(super::held::HeldKey {
        modifiers: modifiers.to_vec(),
        key: key.to_string(),
        where_: super::held::HeldWhere::Activate,
    });
    Ok(())
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) fn key_up(modifiers: &[Modifier], key: &str) -> Result<(), ComputerError> {
    let enigo_key = map_key(key)?;
    let mut session = session()?;
    let released = session.key(enigo_key, Direction::Release);
    for modifier in modifiers.iter().rev() {
        let _ = session.key(map_modifier(*modifier), Direction::Release);
    }
    released
        .map_err(|error| ComputerError::ExecutionFailed(format!("key release failed: {error}")))?;
    super::held::clear_keys();
    Ok(())
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
    crate::keys::macos_function_gap(key)?;
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

fn map_button(button: MouseButton) -> Result<Button, ComputerError> {
    match button {
        MouseButton::Left => Ok(Button::Left),
        MouseButton::Right => Ok(Button::Right),
        MouseButton::Middle => Ok(Button::Middle),
        #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
        MouseButton::Back => Ok(Button::Back),
        #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
        MouseButton::Forward => Ok(Button::Forward),
        #[cfg(target_os = "macos")]
        MouseButton::Back | MouseButton::Forward => Err(ComputerError::Unsupported(
            "enigo on macOS has no back or forward button; use tier event".into(),
        )),
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
        // enigo's macOS `Key` enum names this Help. It is the Insert key.
        #[cfg(target_os = "macos")]
        "insert" => enigo::Key::Help,
        #[cfg(not(target_os = "macos"))]
        "insert" => enigo::Key::Insert,
        "plus" => enigo::Key::Unicode('+'),
        "minus" => enigo::Key::Unicode('-'),
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
        "f13" => enigo::Key::F13,
        "f14" => enigo::Key::F14,
        "f15" => enigo::Key::F15,
        "f16" => enigo::Key::F16,
        "f17" => enigo::Key::F17,
        "f18" => enigo::Key::F18,
        "f19" => enigo::Key::F19,
        "f20" => enigo::Key::F20,
        // enigo omits F21–F24 on macOS. The event tier has no virtual keycode either.
        #[cfg(not(target_os = "macos"))]
        "f21" => enigo::Key::F21,
        #[cfg(not(target_os = "macos"))]
        "f22" => enigo::Key::F22,
        #[cfg(not(target_os = "macos"))]
        "f23" => enigo::Key::F23,
        #[cfg(not(target_os = "macos"))]
        "f24" => enigo::Key::F24,
        #[cfg(target_os = "macos")]
        "f21" | "f22" | "f23" | "f24" => {
            return Err(ComputerError::Unsupported(format!(
                "{key} has no macOS virtual keycode; F13 through F20 are available"
            )));
        }
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
        assert!(matches!(
            map_button(MouseButton::Left).unwrap(),
            Button::Left
        ));
        assert!(matches!(
            map_button(MouseButton::Right).unwrap(),
            Button::Right
        ));
        assert!(matches!(
            map_button(MouseButton::Middle).unwrap(),
            Button::Middle
        ));
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                map_button(MouseButton::Back).unwrap_err().code(),
                "unsupported"
            );
            assert_eq!(validate_key("f24").unwrap_err().code(), "unsupported");
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(matches!(
                map_button(MouseButton::Back).unwrap(),
                Button::Back
            ));
            assert!(matches!(
                map_button(MouseButton::Forward).unwrap(),
                Button::Forward
            ));
            assert!(validate_key("f24").is_ok());
        }
        assert!(matches!(map_key("plus").unwrap(), enigo::Key::Unicode('+')));
        #[cfg(target_os = "macos")]
        assert!(matches!(map_key("insert").unwrap(), enigo::Key::Help));
        #[cfg(not(target_os = "macos"))]
        assert!(matches!(map_key("insert").unwrap(), enigo::Key::Insert));
        assert!(validate_key("f13").is_ok());
    }
}
