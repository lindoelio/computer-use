//! Condition waits and verification.
//!
//! The engine is shared by `wait` (poll until a condition holds or the
//! timeout expires) and `verify` (a short assertion with a dedicated error
//! code). Evaluation itself lives in the dispatcher, where window resolution
//! and the tree cache are available; this module owns the types, defaults,
//! validation, and text matching.

use crate::error::ComputerError;

/// Default `wait` timeout.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// Longest accepted timeout.
pub const MAX_TIMEOUT_MS: u64 = 300_000;
/// Default poll interval.
pub const DEFAULT_POLL_MS: u64 = 100;
/// Fastest accepted poll interval.
pub const MIN_POLL_MS: u64 = 25;
/// Longest accepted poll interval.
pub const MAX_POLL_MS: u64 = 10_000;
/// Default `verify` timeout. Verification is an assertion, not a wait.
pub const VERIFY_TIMEOUT_MS: u64 = 2_000;

/// What to check about one element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementState {
    /// The index resolves to an element.
    Exists,
    /// The index does not resolve.
    Gone,
    /// The element exists and has a non-empty visible frame.
    Visible,
    /// The element exists and is enabled.
    Enabled,
}

impl ElementState {
    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exists => "exists",
            Self::Gone => "gone",
            Self::Visible => "visible",
            Self::Enabled => "enabled",
        }
    }

    /// Parse `exists`, `gone`, `visible`, or `enabled`.
    pub fn parse(value: &str) -> Result<Self, ComputerError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "exists" => Ok(Self::Exists),
            "gone" => Ok(Self::Gone),
            "visible" => Ok(Self::Visible),
            "enabled" => Ok(Self::Enabled),
            other => Err(ComputerError::InvalidArguments(format!(
                "element state must be exists, gone, visible, or enabled (got {other})"
            ))),
        }
    }
}

/// What to check about a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    /// The window selector resolves.
    Exists,
    /// The window selector does not resolve.
    Gone,
}

impl WindowState {
    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exists => "exists",
            Self::Gone => "gone",
        }
    }

    /// Parse `exists` or `gone`.
    pub fn parse(value: &str) -> Result<Self, ComputerError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "exists" => Ok(Self::Exists),
            "gone" => Ok(Self::Gone),
            other => Err(ComputerError::InvalidArguments(format!(
                "window state must be exists or gone (got {other})"
            ))),
        }
    }
}

/// One wait/verify condition.
#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    /// Element index and expected state.
    Element {
        /// Element index from the latest tree.
        index: u32,
        /// What must be true about it.
        state: ElementState,
    },
    /// Text inside the target window (or one element subtree).
    Text {
        /// Substring to find.
        needle: String,
        /// Case-insensitive comparison.
        ignore_case: bool,
        /// Restrict the search to this element's subtree.
        within_element: Option<u32>,
    },
    /// An element's value contains a substring.
    Value {
        /// Element index.
        index: u32,
        /// Substring to find.
        needle: String,
        /// Case-insensitive comparison.
        ignore_case: bool,
    },
    /// A window selector exists or is gone.
    Window {
        /// Expected state.
        state: WindowState,
        /// Selector override. `None` uses the context `--window`.
        selector: Option<String>,
    },
    /// The tree changed against the session baseline.
    TreeChange,
    /// An application is running.
    AppRunning {
        /// Name, bundle id, or path fragment.
        name: String,
        /// Case-insensitive comparison.
        ignore_case: bool,
    },
    /// The resolved window title contains a substring.
    Title {
        /// Substring to find.
        needle: String,
        /// Case-insensitive comparison.
        ignore_case: bool,
    },
}

impl Condition {
    /// Stable token used in responses and error messages.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Element { .. } => "element",
            Self::Text { .. } => "text",
            Self::Value { .. } => "value",
            Self::Window { .. } => "window",
            Self::TreeChange => "tree_change",
            Self::AppRunning { .. } => "app_running",
            Self::Title { .. } => "title",
        }
    }

    /// Human description for the timeout message.
    pub fn describe(&self) -> String {
        match self {
            Self::Element { index, state } => format!("element {index} {}", state.as_str()),
            Self::Text {
                needle,
                within_element,
                ..
            } => match within_element {
                Some(index) => format!("text {needle:?} inside element {index}"),
                None => format!("text {needle:?}"),
            },
            Self::Value { index, needle, .. } => format!("value {needle:?} on element {index}"),
            Self::Window { state, selector } => match selector {
                Some(selector) => format!("window {selector:?} {}", state.as_str()),
                None => format!("window {}", state.as_str()),
            },
            Self::TreeChange => "tree change".into(),
            Self::AppRunning { name, .. } => format!("app {name:?} running"),
            Self::Title { needle, .. } => format!("window title containing {needle:?}"),
        }
    }
}

/// A validated wait/verify request.
#[derive(Debug, Clone, PartialEq)]
pub struct WaitSpec {
    /// What must become true.
    pub condition: Condition,
    /// Total time budget in milliseconds.
    pub timeout_ms: u64,
    /// Interval between observations.
    pub poll_ms: u64,
}

impl WaitSpec {
    /// Validate and build a spec.
    pub fn new(condition: Condition, timeout_ms: u64, poll_ms: u64) -> Result<Self, ComputerError> {
        if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
            return Err(ComputerError::InvalidArguments(format!(
                "timeout-ms must be between 1 and {MAX_TIMEOUT_MS}"
            )));
        }
        if !(MIN_POLL_MS..=MAX_POLL_MS).contains(&poll_ms) {
            return Err(ComputerError::InvalidArguments(format!(
                "poll-ms must be between {MIN_POLL_MS} and {MAX_POLL_MS}"
            )));
        }
        Ok(Self {
            condition,
            timeout_ms,
            poll_ms,
        })
    }
}

/// Case-aware substring check.
pub fn contains(haystack: &str, needle: &str, ignore_case: bool) -> bool {
    if needle.is_empty() {
        return true;
    }
    if ignore_case {
        haystack
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    } else {
        haystack.contains(needle)
    }
}

/// A short window around the match, for the response observation.
///
/// `to_ascii_lowercase` rewrites only ASCII bytes, so a match offset found in
/// the folded copy is a valid byte offset in `text`. The bounds are snapped to
/// character boundaries before slicing: tree text routinely carries 3- and
/// 4-byte characters (curly quotes, ellipses, arrows, emoji, CJK), and a raw
/// `position - 40` would land inside one and panic.
pub fn snippet(text: &str, needle: &str, ignore_case: bool) -> String {
    if needle.is_empty() {
        return String::new();
    }
    let (haystack, wanted) = if ignore_case {
        (text.to_ascii_lowercase(), needle.to_ascii_lowercase())
    } else {
        (text.to_string(), needle.to_string())
    };
    let Some(position) = haystack.find(&wanted) else {
        return String::new();
    };
    let start = floor_char_boundary(text, position.saturating_sub(40));
    let end = ceil_char_boundary(text, (position + wanted.len() + 40).min(text.len()));
    let mut slice = text[start..end].to_string();
    slice = slice.replace('\n', " ");
    if start > 0 {
        slice.insert(0, '…');
    }
    if end < text.len() {
        slice.push('…');
    }
    slice
}

/// Largest character boundary at or before `index`.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut index = index;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Smallest character boundary at or after `index`.
fn ceil_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut index = index;
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_validate() {
        assert_eq!(
            ElementState::parse("visible").unwrap(),
            ElementState::Visible
        );
        assert_eq!(WindowState::parse("gone").unwrap(), WindowState::Gone);
        assert!(WaitSpec::new(Condition::TreeChange, 0, 100).is_err());
        assert!(WaitSpec::new(Condition::TreeChange, 1_000, 1).is_err());
        assert!(WaitSpec::new(Condition::TreeChange, 1_000, 100).is_ok());
    }

    #[test]
    fn contains_and_snippet_are_case_aware() {
        assert!(contains("Saved!", "saved", true));
        assert!(!contains("Saved!", "saved", false));
        let window = snippet("the quick brown fox jumped", "brown", false);
        assert!(window.contains("brown"));
        let long = "x".repeat(120) + "needle";
        assert!(snippet(&long, "needle", false).starts_with('…'));
    }

    #[test]
    fn snippet_keeps_the_original_case_when_folding() {
        // The needle match is found in the folded copy, but the window is
        // sliced from the original text.
        let text = "Save “Draft” Before You Leave";
        let window = snippet(text, "before", true);
        assert!(window.contains("Before"), "{window:?}");
        assert!(window.contains('“'), "{window:?}");
    }

    #[test]
    fn snippet_never_slices_inside_a_multibyte_character() {
        for fill in ["é", "€", "😀"] {
            let text = format!("{}Saved", fill.repeat(30));
            let window = snippet(&text, "Saved", false);
            assert!(window.contains("Saved"), "{window:?}");
            // No leading filler is needed for the 2-byte case; the 3- and
            // 4-byte cases must still produce a slice, not a panic.
            assert!(window.chars().count() <= 41, "{window:?}");
        }
        let mixed = "a€é😀bneedle".to_string();
        assert!(snippet(&mixed, "needle", false).contains("needle"));
    }

    #[test]
    fn snippet_handles_empty_and_missing_needles() {
        assert_eq!(snippet("anything", "", false), "");
        assert_eq!(snippet("anything", "absent", false), "");
        assert_eq!(snippet("", "x", false), "");
    }

    #[test]
    fn state_parsers_reject_unknown_tokens() {
        let error = ElementState::parse("maybe").unwrap_err();
        assert!(error
            .to_string()
            .contains("exists, gone, visible, or enabled"));
        let error = WindowState::parse("maybe").unwrap_err();
        assert!(error.to_string().contains("exists or gone"));
        assert_eq!(ElementState::parse("  GONE  ").unwrap(), ElementState::Gone);
        assert_eq!(ElementState::Exists.as_str(), "exists");
        assert_eq!(ElementState::Gone.as_str(), "gone");
        assert_eq!(ElementState::Visible.as_str(), "visible");
        assert_eq!(ElementState::Enabled.as_str(), "enabled");
        assert_eq!(WindowState::Exists.as_str(), "exists");
        assert_eq!(WindowState::Gone.as_str(), "gone");
    }

    #[test]
    fn spec_bounds_and_descriptions() {
        assert!(WaitSpec::new(Condition::TreeChange, MAX_TIMEOUT_MS + 1, 100).is_err());
        assert!(WaitSpec::new(Condition::TreeChange, 1_000, MAX_POLL_MS + 1).is_err());
        assert!(WaitSpec::new(Condition::TreeChange, 1_000, MIN_POLL_MS).is_ok());

        let conditions = vec![
            Condition::Element {
                index: 4,
                state: ElementState::Visible,
            },
            Condition::Text {
                needle: "hi".into(),
                ignore_case: false,
                within_element: None,
            },
            Condition::Text {
                needle: "hi".into(),
                ignore_case: false,
                within_element: Some(2),
            },
            Condition::Value {
                index: 1,
                needle: "v".into(),
                ignore_case: false,
            },
            Condition::Window {
                state: WindowState::Gone,
                selector: Some("id:9".into()),
            },
            Condition::Window {
                state: WindowState::Exists,
                selector: None,
            },
            Condition::TreeChange,
            Condition::AppRunning {
                name: "Notes".into(),
                ignore_case: false,
            },
            Condition::Title {
                needle: "Save".into(),
                ignore_case: false,
            },
        ];
        for condition in &conditions {
            assert!(!condition.as_str().is_empty());
            assert!(!condition.describe().is_empty(), "{condition:?}");
        }
        assert_eq!(conditions[0].as_str(), "element");
        assert_eq!(conditions[1].as_str(), "text");
        assert_eq!(conditions[3].as_str(), "value");
        assert_eq!(conditions[4].as_str(), "window");
        assert_eq!(conditions[6].as_str(), "tree_change");
        assert_eq!(conditions[7].as_str(), "app_running");
        assert_eq!(conditions[8].as_str(), "title");
        assert!(conditions[2].describe().contains("inside element 2"));
        assert!(conditions[4].describe().contains("id:9"));

        assert!(contains("anything", "", false), "an empty needle matches");
        assert!(contains("anything", "", true));
    }
}
