//! Clipboard text and images.
//!
//! The real pasteboard is touched only outside tests. Unit tests use
//! [`crate::test_backend::StubBackend`], which keeps an in-memory clipboard.

use crate::error::ComputerError;

/// What a clipboard read found. Image bytes are tightly packed RGBA8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContents {
    /// Nothing the caller can use.
    Empty,
    /// Unicode text. An empty string is not stored; that is [`Self::Empty`].
    Text(String),
    /// RGBA pixels. The protocol writes a PNG into the session cache.
    Image {
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// `width * height * 4` bytes.
        rgba: Vec<u8>,
    },
}

/// Read the system clipboard. Tests never call the real pasteboard.
pub(crate) fn clipboard_get() -> Result<ClipboardContents, ComputerError> {
    #[cfg(test)]
    {
        Err(ComputerError::ExecutionFailed(
            "clipboard is disabled in tests".into(),
        ))
    }
    #[cfg(not(test))]
    {
        live_get()
    }
}

/// Write text. Empty text is refused by the caller before this runs.
pub(crate) fn clipboard_set_text(text: &str) -> Result<(), ComputerError> {
    #[cfg(test)]
    {
        let _ = text;
        Err(ComputerError::ExecutionFailed(
            "clipboard is disabled in tests".into(),
        ))
    }
    #[cfg(not(test))]
    {
        let mut clipboard = arboard::Clipboard::new().map_err(map_error)?;
        clipboard.set_text(text).map_err(map_error)
    }
}

/// Write an RGBA image.
pub(crate) fn clipboard_set_image(
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), ComputerError> {
    #[cfg(test)]
    {
        let _ = (width, height, rgba);
        Err(ComputerError::ExecutionFailed(
            "clipboard is disabled in tests".into(),
        ))
    }
    #[cfg(not(test))]
    {
        let mut clipboard = arboard::Clipboard::new().map_err(map_error)?;
        let image = arboard::ImageData {
            width: usize::try_from(width).unwrap_or(0),
            height: usize::try_from(height).unwrap_or(0),
            bytes: std::borrow::Cow::Borrowed(rgba),
        };
        clipboard.set_image(image).map_err(map_error)
    }
}

#[cfg(not(test))]
fn live_get() -> Result<ClipboardContents, ComputerError> {
    let mut clipboard = arboard::Clipboard::new().map_err(map_error)?;
    match clipboard.get_text() {
        Ok(text) if !text.is_empty() => return Ok(ClipboardContents::Text(text)),
        Ok(_) | Err(arboard::Error::ContentNotAvailable) => {}
        Err(error) => return Err(map_error(error)),
    }
    match clipboard.get_image() {
        Ok(image) => {
            let width = u32::try_from(image.width).map_err(|_| {
                ComputerError::ExecutionFailed("clipboard image is too wide".into())
            })?;
            let height = u32::try_from(image.height).map_err(|_| {
                ComputerError::ExecutionFailed("clipboard image is too tall".into())
            })?;
            Ok(ClipboardContents::Image {
                width,
                height,
                rgba: image.bytes.into_owned(),
            })
        }
        Err(arboard::Error::ContentNotAvailable) => Ok(ClipboardContents::Empty),
        Err(error) => Err(map_error(error)),
    }
}

#[cfg(not(test))]
fn map_error(error: arboard::Error) -> ComputerError {
    let text = error.to_string();
    let lower = text.to_ascii_lowercase();
    #[cfg(target_os = "macos")]
    if lower.contains("pasteboard")
        && (lower.contains("permission")
            || lower.contains("denied")
            || lower.contains("not allowed"))
    {
        return ComputerError::PermissionRequired(text);
    }
    ComputerError::ExecutionFailed(format!("clipboard failed: {text}"))
}
