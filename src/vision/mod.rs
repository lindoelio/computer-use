//! Vision utilities: pure-image marks overlay and the OCR backend selector.
//!
//! The marks renderer in [`marks`] is platform-independent. The OCR backend in
//! [`ocr`] is `VNRecognizeTextRequest` on macOS (P11) and
//! `Windows.Media.Ocr.OcrEngine` on Windows (P12b); Linux returns
//! [`crate::error::ComputerError::Unsupported`]. Both platforms produce the
//! same [`ocr::OcrBlock`] shape, so callers never branch.

pub mod marks;
pub mod ocr;

pub use marks::{draw_marks, ocr_label, tree_label, MarkBox};
pub use ocr::{recognize, redact_blocks, ImageRect as OcrImageRect, OcrBlock, OcrResult};
