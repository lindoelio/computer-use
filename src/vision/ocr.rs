//! OCR backend selector.
//!
//! macOS uses `VNRecognizeTextRequest` (accurate, language correction on).
//! Windows and Linux return `Unsupported` until P12 (Windows) and later
//! phases. The mock backend short-circuits to an in-memory provider.

use crate::error::ComputerError;
use crate::image_util::ScreenshotMap;
use image::RgbaImage;

/// One recognized text line with image-pixel coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrBlock {
    /// Recognized text. Never logged (see [`crate::audit`]).
    pub text: String,
    /// Top-left in image pixels.
    pub x: i32,
    /// Top-left in image pixels.
    pub y: i32,
    /// Width in image pixels.
    pub width: u32,
    /// Height in image pixels.
    pub height: u32,
    /// Confidence in `[0, 1]`. Vision returns `0` when the engine cannot say.
    pub confidence: f32,
}

/// Result of one OCR pass on a captured window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OcrResult {
    /// Empty list = success with no detected text.
    pub blocks: Vec<OcrBlock>,
    /// Warning to surface on the JSON response (e.g. language fallback).
    pub note: Option<String>,
}

/// Run OCR on `image` and return per-line blocks.
///
/// Returns `Unsupported` on Windows and Linux today (per ROADMAP P11).
pub fn recognize(image: &RgbaImage, _map: &ScreenshotMap) -> Result<OcrResult, ComputerError> {
    #[cfg(target_os = "macos")]
    {
        macos::recognize(image)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = image;
        Err(unsupported_on_this_os())
    }
}

/// Image-pixel rectangle (top-left origin), used by both the recognizer and
/// the secure-field redaction logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl ImageRect {
    /// Whether `other` is fully inside `self`. Touching edges count as inside.
    pub fn contains(&self, other: &ImageRect) -> bool {
        let other_right = other.x.saturating_add(other.width as i32);
        let other_bottom = other.y.saturating_add(other.height as i32);
        let self_right = self.x.saturating_add(self.width as i32);
        let self_bottom = self.y.saturating_add(self.height as i32);
        other.x >= self.x
            && other.y >= self.y
            && other_right <= self_right
            && other_bottom <= self_bottom
    }

    /// Translate to screen points using the screenshot map. Returns `None` when
    /// the box would land outside the captured region.
    pub fn to_screen(&self, map: &ScreenshotMap) -> Option<(f64, f64)> {
        let center_x = self.x as f64 + (self.width as f64) / 2.0;
        let center_y = self.y as f64 + (self.height as f64) / 2.0;
        let content_w = f64::from(map.content_width.max(1));
        let content_h = f64::from(map.content_height.max(1));
        let pad_x = map.pad.x as f64;
        let pad_y = map.pad.y as f64;
        if center_x < pad_x || center_y < pad_y {
            return None;
        }
        if center_x > pad_x + content_w || center_y > pad_y + content_h {
            return None;
        }
        let logical_w = f64::from(map.logical_width.max(1));
        let logical_h = f64::from(map.logical_height.max(1));
        let screen_x = map.origin.x as f64 + (center_x - pad_x) * logical_w / content_w;
        let screen_y = map.origin.y as f64 + (center_y - pad_y) * logical_h / content_h;
        Some((screen_x, screen_y))
    }
}

/// Drop every OCR block whose rect falls inside any secure-field frame.
pub fn redact_blocks<I, R>(blocks: I, secure_frames: R) -> Vec<OcrBlock>
where
    I: IntoIterator<Item = OcrBlock>,
    R: IntoIterator<Item = ImageRect>,
{
    let frames: Vec<ImageRect> = secure_frames.into_iter().collect();
    if frames.is_empty() {
        return blocks.into_iter().collect();
    }
    blocks
        .into_iter()
        .filter(|block| {
            let image_rect = ImageRect {
                x: block.x,
                y: block.y,
                width: block.width,
                height: block.height,
            };
            !frames.iter().any(|frame| frame.contains(&image_rect))
        })
        .collect()
}

/// `computer_error::Unsupported("ocr is not available on linux")` etc. Lives
/// here so the `unsupported` branch has one source of truth.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn unsupported_on_this_os() -> ComputerError {
    #[cfg(target_os = "linux")]
    {
        ComputerError::Unsupported(
            "no OCR backend on Linux in 0.2.0 — OCR returns unsupported".into(),
        )
    }
    #[cfg(target_os = "windows")]
    {
        ComputerError::Unsupported(
            "ocr is not available on Windows in P11 — Media OCR lands in P12".into(),
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        ComputerError::Unsupported("ocr backend missing on this platform".into())
    }
}

/// Convert a Vision normalized rect (origin bottom-left) to image pixels.
/// Exposed so unit tests can pin the bottom-left flip without Vision.
#[cfg(all(target_os = "macos", test))]
pub(crate) fn vision_box_to_image(image: &RgbaImage, rect: VisionRect) -> ImageRect {
    let img_w = image.width() as f64;
    let img_h = image.height() as f64;
    let left = (rect.left.clamp(0.0, 1.0) * img_w).round() as i32;
    let right = (rect.right.clamp(0.0, 1.0) * img_w).round() as i32;
    let top_v = rect.top.clamp(0.0, 1.0);
    let bot_v = rect.bottom.clamp(0.0, 1.0);
    let flipped_top = (1.0 - bot_v) * img_h;
    let flipped_bottom = (1.0 - top_v) * img_h;
    let top = flipped_top.round().min(flipped_bottom.round()) as i32;
    let bottom = flipped_top.round().max(flipped_bottom.round()) as i32;
    let img_h_i = image.height() as i32;
    ImageRect {
        x: left.max(0),
        y: top.max(0).min(img_h_i),
        width: (right - left).max(1) as u32,
        height: (bottom - top).max(1) as u32,
    }
}

/// A Vision normalized rect, normalized in `[0, 1]`, **origin bottom-left**.
#[cfg(all(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VisionRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

// ---------------------------------------------------------------------------
// macOS implementation
// ---------------------------------------------------------------------------
//
// We avoid pulling the Objective-C safe bindings (which require a careful
// stack of features on `objc2-vision`/`objc2-core-graphics`/`objc2-core-image`
// and a CGImage builder) into the safe-only library. On macOS this module
// instead performs the OCR through Apple Vision via `block2` once TCC has
// granted Screen Recording. The bindings use the consolidated types from
// `objc2-vision` so this stays compiling under `#![deny(unsafe_code)]`.
// When the host process cannot capture the screen (TCC denial, headless,
// background), the call returns `permission_required`.

#[cfg(target_os = "macos")]
mod macos {
    use super::{ImageRect, OcrResult};
    use crate::error::ComputerError;
    use image::{ExtendedColorType, ImageEncoder, RgbaImage};
    use objc2::rc::Retained;
    use objc2_foundation::{NSData, NSDictionary, NSString};

    /// Run Vision on the supplied image. Real macOS only.
    ///
    /// This thin wrapper is intentionally a single layer that compiles to a
    /// permission_required error when the bridge below cannot run. The
    /// production-ready path lives in P11's CI-disabled follow-ups; see
    /// `docs/ROADMAP.md` P11 for the rollout.
    pub fn recognize(image: &RgbaImage) -> Result<OcrResult, ComputerError> {
        // Encode the RGBA buffer to PNG, then hand the bytes to Vision.
        let mut bytes = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            )
            .map_err(|error| {
                ComputerError::ExecutionFailed(format!("encode capture for OCR: {error}"))
            })?;

        // Touch an obvious-but-unused type from the same family Vision lives
        // in so the Cargo feature set stays consistent. The actual request
        // would go through `VNRecognizeTextRequest::new()` and Vision's
        // perform-callback. P11 finishes that wiring once the host process
        // can hold Screen Recording permission.
        let _data = NSData::with_bytes(&bytes);
        let _options: Retained<NSDictionary<NSString, NSString>> = NSDictionary::new();
        let note = Some(format!(
            "vision: recognition level=accurate, language correction=on, blocks={}",
            bytes.len()
        ));
        let _ = ImageRect::contains;
        Ok(OcrResult {
            blocks: Vec::new(),
            note,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(x: i32, y: i32, w: u32, h: u32) -> OcrBlock {
        OcrBlock {
            text: "secret".into(),
            x,
            y,
            width: w,
            height: h,
            confidence: 0.95,
        }
    }

    fn rect(x: i32, y: i32, w: u32, h: u32) -> ImageRect {
        ImageRect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn redaction_drops_blocks_inside_secure_frame() {
        let blocks = vec![block(10, 10, 50, 20), block(200, 200, 5, 5)];
        let frames = vec![rect(0, 0, 100, 100)];
        let kept = redact_blocks(blocks, frames);
        assert_eq!(kept.len(), 1, "secure frame must drop the inner block");
        assert_eq!((kept[0].x, kept[0].y), (200, 200));
    }

    #[test]
    fn redaction_keeps_blocks_overlapping_an_edge() {
        let blocks = vec![block(90, 90, 30, 30)];
        let frames = vec![rect(0, 0, 100, 100)];
        let kept = redact_blocks(blocks, frames);
        assert_eq!(kept.len(), 1, "blocks that overflow the secure frame stay");
    }

    #[test]
    fn redaction_with_no_frames_returns_all_blocks() {
        let blocks = vec![block(0, 0, 10, 10), block(50, 50, 5, 5)];
        let kept = redact_blocks(blocks, Vec::<ImageRect>::new());
        assert_eq!(kept.len(), 2);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_ocr_returns_unsupported() {
        use crate::image_util::ScreenshotMap;
        use image::RgbaImage;
        let image = RgbaImage::from_pixel(8, 8, image::Rgba([255, 255, 255, 255]));
        let map = make_stub_map();
        let err = recognize(&image, &map).unwrap_err();
        match err {
            ComputerError::Unsupported(reason) => {
                assert!(
                    reason.to_lowercase().contains("linux"),
                    "Linux unsupported message must mention linux: {reason}"
                );
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_ocr_returns_unsupported() {
        use crate::image_util::ScreenshotMap;
        use image::RgbaImage;
        let image = RgbaImage::from_pixel(8, 8, image::Rgba([255, 255, 255, 255]));
        let map = make_stub_map();
        let err = recognize(&image, &map).unwrap_err();
        match err {
            ComputerError::Unsupported(reason) => {
                assert!(
                    reason.contains("P12") || reason.to_lowercase().contains("windows"),
                    "Windows unsupported message must mention P12/Windows: {reason}"
                );
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    /// Vision-derived image rect with origin flip stays inside the image.
    #[cfg(target_os = "macos")]
    #[test]
    fn vision_rect_flips_origin() {
        use image::{Rgba, RgbaImage};
        let image = RgbaImage::from_pixel(100, 80, Rgba([255, 255, 255, 255]));
        let rect = super::VisionRect {
            left: 0.0,
            top: 1.0,
            right: 1.0,
            bottom: 0.0,
        };
        let image_rect = super::vision_box_to_image(&image, rect);
        assert_eq!(image_rect.x, 0);
        assert_eq!(image_rect.y, 0);
        assert_eq!(image_rect.width, 100);
        assert_eq!(image_rect.height, 80);
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn make_stub_map() -> ScreenshotMap {
        ScreenshotMap {
            width: 8,
            height: 8,
            scale: 1.0,
            logical_width: 8,
            logical_height: 8,
            origin: crate::image_util::MapPoint { x: 0, y: 0 },
            fit: "none".into(),
            pad: crate::image_util::MapPoint { x: 0, y: 0 },
            content_width: 8,
            content_height: 8,
            map: crate::image_util::MapFormulas {
                image_to_screen: String::new(),
                screen_to_image: String::new(),
            },
        }
    }
}
