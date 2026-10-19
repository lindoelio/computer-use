//! Parse key combinations such as `cmd+shift+4`.

use crate::error::ComputerError;
use crate::types::Modifier;

/// Modifiers plus the final key from a combo string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCombo {
    /// Modifiers, in the order they appeared, without duplicates.
    pub modifiers: Vec<Modifier>,
    /// Canonical key name. Single characters keep their case.
    pub key: String,
}

/// Parse `cmd+shift+4`, `ctrl+alt+delete`, or a bare key such as `enter`.
///
/// Modifier names: `cmd`, `command`, `meta`, `super`, `ctrl`, `control`,
/// `alt`, `option`, and `shift`. The last segment is the key.
pub fn parse_key_combo(combo: &str) -> Result<KeyCombo, ComputerError> {
    let trimmed = combo.trim();
    if trimmed.is_empty() {
        return Err(ComputerError::InvalidArguments("key combo is empty".into()));
    }
    let parts: Vec<&str> = trimmed.split('+').map(str::trim).collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(ComputerError::InvalidArguments(format!(
            "key combo has an empty segment: {combo:?}"
        )));
    }
    let (modifier_parts, key_part) = parts.split_at(parts.len() - 1);
    let mut modifiers = Vec::new();
    for part in modifier_parts {
        let modifier = parse_modifier(part).ok_or_else(|| {
            ComputerError::InvalidArguments(format!("unknown modifier {part:?} in combo {combo:?}"))
        })?;
        if !modifiers.contains(&modifier) {
            modifiers.push(modifier);
        }
    }
    let key = canonical_key(key_part[0]);
    if key.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "key combo is missing a key".into(),
        ));
    }
    if !is_known_key(&key) {
        return Err(ComputerError::InvalidArguments(format!(
            "unknown key {key:?}"
        )));
    }
    Ok(KeyCombo { modifiers, key })
}

/// Split a space-separated sequence such as `ctrl+a Delete` into combos.
pub fn parse_key_sequence(text: &str) -> Result<Vec<KeyCombo>, ComputerError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ComputerError::InvalidArguments("key combo is empty".into()));
    }
    let mut combos = Vec::new();
    for token in trimmed.split_whitespace() {
        combos.push(parse_key_combo(token)?);
    }
    if combos.is_empty() {
        return Err(ComputerError::InvalidArguments("key combo is empty".into()));
    }
    Ok(combos)
}

/// A canonical name or a single character. Unknown words are not keys.
pub fn is_known_key(key: &str) -> bool {
    if key.chars().count() == 1 {
        return true;
    }
    let lower = key.trim().to_ascii_lowercase().replace('-', "_");
    canonical_named(&lower).is_some()
}

/// macOS has virtual keycodes for F13–F20. F21–F24 are known names and still unsupported there.
pub fn macos_function_gap(key: &str) -> Result<(), ComputerError> {
    #[cfg(target_os = "macos")]
    if matches!(key, "f21" | "f22" | "f23" | "f24") {
        return Err(ComputerError::Unsupported(format!(
            "{key} has no macOS virtual keycode; F13 through F20 are available"
        )));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = key;
    Ok(())
}

fn parse_modifier(text: &str) -> Option<Modifier> {
    match text.to_ascii_lowercase().as_str() {
        "cmd" | "command" => Some(Modifier::Command),
        "meta" | "super" => Some(Modifier::Meta),
        "ctrl" | "control" => Some(Modifier::Control),
        "alt" | "option" => Some(Modifier::Alt),
        "shift" => Some(Modifier::Shift),
        _ => None,
    }
}

/// Normalize aliases (`esc`, `return`, `arrow_left`) to one spelling.
pub fn canonical_key(text: &str) -> String {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase().replace('-', "_");
    if let Some(name) = canonical_named(&lower) {
        return name.to_string();
    }
    if trimmed.chars().count() == 1 {
        return trimmed.to_string();
    }
    lower
}

fn canonical_named(lower: &str) -> Option<&'static str> {
    Some(match lower {
        "enter" | "return" => "enter",
        "tab" => "tab",
        "esc" | "escape" => "esc",
        "space" => "space",
        "backspace" | "bksp" => "backspace",
        "delete" | "del" => "delete",
        "left" | "leftarrow" | "left_arrow" | "arrowleft" | "arrow_left" => "left",
        "right" | "rightarrow" | "right_arrow" | "arrowright" | "arrow_right" => "right",
        "up" | "uparrow" | "up_arrow" | "arrowup" | "arrow_up" => "up",
        "down" | "downarrow" | "down_arrow" | "arrowdown" | "arrow_down" => "down",
        "home" => "home",
        "end" => "end",
        "pageup" | "page_up" | "prior" => "pageup",
        "pagedown" | "page_down" | "next" => "pagedown",
        "insert" => "insert",
        "plus" => "plus",
        "minus" => "minus",
        "f1" => "f1",
        "f2" => "f2",
        "f3" => "f3",
        "f4" => "f4",
        "f5" => "f5",
        "f6" => "f6",
        "f7" => "f7",
        "f8" => "f8",
        "f9" => "f9",
        "f10" => "f10",
        "f11" => "f11",
        "f12" => "f12",
        "f13" => "f13",
        "f14" => "f14",
        "f15" => "f15",
        "f16" => "f16",
        "f17" => "f17",
        "f18" => "f18",
        "f19" => "f19",
        "f20" => "f20",
        "f21" => "f21",
        "f22" => "f22",
        "f23" => "f23",
        "f24" => "f24",
        "shift" => "shift",
        "ctrl" | "control" => "control",
        "alt" | "option" => "alt",
        "cmd" | "command" | "meta" | "super" => "meta",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modifiers(combo: &str) -> (Vec<Modifier>, String) {
        let parsed = parse_key_combo(combo).unwrap();
        (parsed.modifiers, parsed.key)
    }

    #[test]
    fn parses_modifiers_and_named_keys() {
        assert_eq!(modifiers("space"), (vec![], "space".into()));
        assert_eq!(modifiers("Enter"), (vec![], "enter".into()));
        assert_eq!(modifiers("ESC"), (vec![], "esc".into()));
        assert_eq!(modifiers("F12"), (vec![], "f12".into()));
        assert_eq!(modifiers("arrow_left"), (vec![], "left".into()));
        assert_eq!(modifiers("page_up"), (vec![], "pageup".into()));

        assert_eq!(modifiers("cmd+c"), (vec![Modifier::Command], "c".into()));
        assert_eq!(
            modifiers("command+v"),
            (vec![Modifier::Command], "v".into())
        );
        assert_eq!(modifiers("meta+a"), (vec![Modifier::Meta], "a".into()));
        assert_eq!(modifiers("super+tab"), (vec![Modifier::Meta], "tab".into()));
        assert_eq!(
            modifiers("ctrl+shift+tab"),
            (vec![Modifier::Control, Modifier::Shift], "tab".into())
        );
        assert_eq!(
            modifiers("control+z"),
            (vec![Modifier::Control], "z".into())
        );
        assert_eq!(modifiers("alt+f4"), (vec![Modifier::Alt], "f4".into()));
        assert_eq!(modifiers("option+q"), (vec![Modifier::Alt], "q".into()));
        assert_eq!(modifiers("shift+A"), (vec![Modifier::Shift], "A".into()));
        assert_eq!(
            modifiers(" cmd + shift + 4 "),
            (vec![Modifier::Command, Modifier::Shift], "4".into())
        );
        assert_eq!(
            modifiers("Ctrl+Alt+Delete"),
            (vec![Modifier::Control, Modifier::Alt], "delete".into())
        );
    }

    #[test]
    fn dedups_repeated_modifiers() {
        let (mods, key) = modifiers("shift+shift+a");
        assert_eq!(mods, vec![Modifier::Shift]);
        assert_eq!(key, "a");
    }

    #[test]
    fn rejects_empty_unknown_and_dangling_plus() {
        assert!(parse_key_combo("").is_err());
        assert!(parse_key_combo("   ").is_err());
        assert!(parse_key_combo("cmd+").is_err());
        assert!(parse_key_combo("+c").is_err());
        assert!(parse_key_combo("foo+c").is_err());
        assert!(parse_key_combo("cmd++c").is_err());
        assert!(parse_key_combo("bogus").is_err());
    }

    #[test]
    fn xdotool_names_and_sequences() {
        assert_eq!(modifiers("super+c"), (vec![Modifier::Meta], "c".into()));
        assert_eq!(modifiers("Return"), (vec![], "enter".into()));
        assert_eq!(
            modifiers("ctrl+alt+Delete"),
            (vec![Modifier::Control, Modifier::Alt], "delete".into())
        );
        assert_eq!(modifiers("plus"), (vec![], "plus".into()));
        assert_eq!(
            modifiers("shift+plus"),
            (vec![Modifier::Shift], "plus".into())
        );
        assert_eq!(modifiers("minus"), (vec![], "minus".into()));
        assert_eq!(modifiers("F24"), (vec![], "f24".into()));
        assert_eq!(modifiers("Prior"), (vec![], "pageup".into()));
        assert_eq!(modifiers("Next"), (vec![], "pagedown".into()));
        assert_eq!(modifiers("Insert"), (vec![], "insert".into()));
        assert_eq!(modifiers("BackSpace"), (vec![], "backspace".into()));
        let sequence = parse_key_sequence("ctrl+a Delete").unwrap();
        assert_eq!(sequence.len(), 2);
        assert_eq!(sequence[0].key, "a");
        assert_eq!(sequence[1].key, "delete");
        assert!(parse_key_sequence("bogus").is_err());
        #[cfg(target_os = "macos")]
        assert_eq!(macos_function_gap("f24").unwrap_err().code(), "unsupported");
        #[cfg(not(target_os = "macos"))]
        assert!(macos_function_gap("f24").is_ok());
    }
}
