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
pub fn snippet(text: &str, needle: &str, ignore_case: bool) -> String {
    let (haystack, wanted) = if ignore_case {
        (text.to_ascii_lowercase(), needle.to_ascii_lowercase())
    } else {
        (text.to_string(), needle.to_string())
    };
    let Some(position) = haystack.find(&wanted) else {
        return String::new();
    };
    let start = position.saturating_sub(40);
    let end = (position + wanted.len() + 40).min(text.len());
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
}
