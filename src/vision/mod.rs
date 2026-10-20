//! Vision utilities: pure-image marks overlay and the OCR backend selector.
//!
//! The marks renderer in [`marks`] is platform-independent. The OCR backend in
//! [`ocr`] is macOS-only today (`VNRecognizeTextRequest`); Windows and Linux
//! return [`crate::error::ComputerError::Unsupported`]. The markup is kept
//! inside `src/vision` so future P12 work can extend the same module without
//! reshaping callers.

pub mod marks;
pub mod ocr;

pub use marks::{draw_marks, ocr_label, tree_label, MarkBox};
pub use ocr::{recognize, redact_blocks, ImageRect as OcrImageRect, OcrBlock, OcrResult};
