//! Parse `"File > Save"`-style menu paths into a typed list of titles.
//!
//! The path separator is `>` or `→`, with optional whitespace around it. Each
//! segment is trimmed. Empty segments are rejected. A single title such as
//! `Save` parses as a one-segment path.
//!
//! Paths are case-folded on the macOS backend so callers may pass either
//! `File > Save` or `file > save`.

use serde::Serialize;

use crate::error::ComputerError;

/// Separator between menu titles. `>` is the documented form. `→` (U+2192)
/// is accepted as a Unicode-friendly alias.
pub const SEPARATORS: &[char] = &['>', '→'];

/// One menu path. Each entry is a title that the macOS backend matches
/// case-insensitively against `AXTitle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MenuPath {
    /// Title segments in order.
    pub segments: Vec<String>,
}

impl MenuPath {
    /// Borrow the segments.
    pub fn as_slice(&self) -> &[String] {
        &self.segments
    }
}

/// Split a single title path. Empty input and empty segments are rejected.
pub fn parse(input: &str) -> Result<MenuPath, ComputerError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ComputerError::InvalidArguments("menu path is empty".into()));
    }
    let mut segments = Vec::new();
    for raw in trimmed.split(SEPARATORS) {
        let piece = raw.trim();
        if piece.is_empty() {
            return Err(ComputerError::InvalidArguments(format!(
                "menu path has an empty segment in {input:?}"
            )));
        }
        segments.push(piece.to_string());
    }
    if segments.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "menu path has no segments".into(),
        ));
    }
    Ok(MenuPath { segments })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_segment_parses() {
        let path = parse("Save").unwrap();
        assert_eq!(path.segments, vec!["Save".to_string()]);
    }

    #[test]
    fn standard_separator_with_padding() {
        let path = parse("File > Save As > Save").unwrap();
        assert_eq!(
            path.segments,
            vec![
                "File".to_string(),
                "Save As".to_string(),
                "Save".to_string()
            ]
        );
    }

    #[test]
    fn arrow_separator_is_accepted() {
        let path = parse("File → Save").unwrap();
        assert_eq!(path.segments, vec!["File".to_string(), "Save".to_string()]);
    }

    #[test]
    fn empty_input_is_rejected() {
        let error = parse("").unwrap_err();
        assert!(matches!(error, ComputerError::InvalidArguments(_)));
    }

    #[test]
    fn empty_segment_is_rejected() {
        let error = parse("File > > Save").unwrap_err();
        assert!(matches!(error, ComputerError::InvalidArguments(_)));
    }

    #[test]
    fn fixture_menu_path_round_trip() {
        // A representative Finder menu path. The macOS backend matches titles
        // case-insensitively, so any case combination here is fine.
        let path = parse("File > New Finder Window").unwrap();
        assert_eq!(path.segments.len(), 2);
        assert_eq!(path.segments[1], "New Finder Window");
    }

    #[test]
    fn trailing_separator_is_rejected() {
        let error = parse("File > ").unwrap_err();
        assert!(matches!(error, ComputerError::InvalidArguments(_)));
    }
}
