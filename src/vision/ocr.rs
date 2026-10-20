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
#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
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
// Real `VNRecognizeTextRequest` call through `objc2-vision` 0.3.2. Pipeline:
// encode the captured RGBA window as PNG → `NSData` →
// `VNImageRequestHandler::initWithData_options` → run a `VNRecognizeTextRequest`
// (accurate + language correction on) → walk `VNRecognizedTextObservation`
// results → top candidate text + normalized bounding box → flip the box
// (Vision origin is bottom-left) into image pixels → `OcrBlock`.
//
// Only `boundingBox` requires `unsafe`; all other Vision methods in this call
// graph are safe per the `objc2-vision` Rust bindings. The `#[allow(unsafe_code)]`
// keeps the audit surface tight.

#[cfg(target_os = "macos")]
mod macos {
    use super::{vision_box_to_image, OcrBlock, OcrResult, VisionRect};
    use crate::error::ComputerError;
    use image::{ExtendedColorType, ImageEncoder, RgbaImage};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::AnyThread;
    use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};
    use objc2_vision::{
        VNDetectedObjectObservation, VNImageRequestHandler, VNRecognizeTextRequest,
        VNRecognizedText, VNRecognizedTextObservation, VNRequest, VNRequestTextRecognitionLevel,
    };

    /// Run Vision on the supplied image. Real macOS only.
    #[allow(unsafe_code)]
    pub fn recognize(image: &RgbaImage) -> Result<OcrResult, ComputerError> {
        let (img_w, img_h) = (image.width(), image.height());

        // 1. Encode the RGBA buffer to PNG; Vision reads the image bytes from a
        // PNG `NSData` via `initWithData:options:`.
        let mut bytes = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder
            .write_image(image.as_raw(), img_w, img_h, ExtendedColorType::Rgba8)
            .map_err(|error| {
                ComputerError::ExecutionFailed(format!("encode capture for OCR: {error}"))
            })?;
        let png = NSData::with_bytes(&bytes);

        // 2. Empty options dictionary is the canonical "let Vision infer"
        // configuration. `VNImageOption` is a type alias for `NSString`, so
        // `NSDictionary<NSString, AnyObject>` is the right parameter type.
        let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();

        // 3. Build the handler. `initWithData:options:` is a safe method in the
        // `objc2-vision` bindings; the `png` Retained is kept alive until the
        // handler drops, which keeps the byte buffer pinned during
        // `performRequests`.
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &png,
            &options,
        );

        // 4. Build the request. Accurate + language correction on is the
        // recognizable-text default Apple recommends for screen content.
        // Setters are safe Rust functions.
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);

        // 5. Upcast to `VNRequest` for the `NSArray<VNRequest>` argument.
        // `into_super()` re-retains the same object under its superclass
        // type. `VNRecognizeTextRequest -> VNImageBasedRequest -> VNRequest`,
        // so we call it twice. We keep a separate clone of the original
        // request intact so we can call `results()` on the concrete subclass
        // after `performRequests` finishes.
        let upcast: Retained<VNRequest> = request.clone().into_super().into_super();
        let requests: Retained<NSArray<VNRequest>> = NSArray::from_retained_slice(&[upcast]);

        // 6. Run. `performRequests:error:_` returns a `Result` where the
        // `Err(NSError)` carries the localized description. On `Err` we map
        // to `ExecutionFailed` with the description.
        if let Err(error) = handler.performRequests_error(&requests) {
            return Err(ComputerError::ExecutionFailed(format!(
                "VNImageRequestHandler performRequests failed: {error}"
            )));
        }

        // 7. Pull the observations. `results()` may be `None` if the request
        // returned nothing — treat that as "no text recognized" and return an
        // empty block list rather than an error.
        let observations = match request.results() {
            Some(obs) => obs,
            None => {
                return Ok(OcrResult {
                    blocks: Vec::new(),
                    note: None,
                })
            }
        };

        // 8. Walk the observations; build `OcrBlock`s with the top candidate
        // and an image-pixel rectangle. `objectAtIndex` returns a fresh
        // `Retained<VNRecognizedTextObservation>`; we deref to call methods.
        let observations_ref: &NSArray<VNRecognizedTextObservation> = &observations;
        let count = observations_ref.len();
        let mut blocks = Vec::with_capacity(count);

        for i in 0..count {
            let observation_retained: Retained<VNRecognizedTextObservation> =
                observations_ref.objectAtIndex(i);

            // SAFETY: `boundingBox` is declared by the bindings on the
            // `VNDetectedObjectObservation` superclass. The `objc2-vision`
            // Rust surface re-exposes that method only on the superclass
            // impl, so we re-type the same Objective-C instance to the
            // superclass via `Retained::cast_unchecked` and call it there.
            // `cast_unchecked` is documented for Objective-C ISA-consistent
            // reinterpretation; this is exactly that case because
            // `VNRecognizedTextObservation` IS-A `VNDetectedObjectObservation`
            // in the Vision framework. `cast_unchecked` consumes the clone
            // and yields an extra strong reference that we drop immediately
            // after reading the rect; the `observation_retained` below
            // remains the surviving refcount and is reused by
            // `topCandidates`.
            let rect = unsafe {
                let obs_super: Retained<VNDetectedObjectObservation> =
                    Retained::cast_unchecked(observation_retained.clone());
                (*obs_super).boundingBox()
            };

            let vision_rect = VisionRect {
                left: rect.origin.x,
                bottom: rect.origin.y,
                right: rect.origin.x + rect.size.width,
                top: rect.origin.y + rect.size.height,
            };

            // `topCandidates(1)` is the canonical helper on
            // `VNRecognizedTextObservation`. It returns the top-1 array of
            // `VNRecognizedText` candidates; each has a `string()` and
            // `confidence()`. All three are safe methods in the bindings.
            let candidates = observation_retained.topCandidates(1);
            let candidate: Retained<VNRecognizedText> = match candidates.into_iter().next() {
                Some(c) => c,
                None => continue,
            };
            let text_ns = candidate.string();
            let confidence: f32 = candidate.confidence();

            let image_rect = vision_box_to_image(image, vision_rect);
            blocks.push(OcrBlock {
                text: text_ns.to_string(),
                x: image_rect.x,
                y: image_rect.y,
                width: image_rect.width,
                height: image_rect.height,
                confidence,
            });
        }

        Ok(OcrResult { blocks, note: None })
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

    /// End-to-end smoke: render a known label into an RGBA buffer with a
    /// small embedded 5x7 bitmap font, run the real Vision pipeline through
    /// the public entry point, and assert the top candidate contains the
    /// label text. Skipped on CI / headless (no display context, Vision may
    /// still work but text rendering and disk read of fixtures is enough —
    /// run locally with `cargo test --release`).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires a live Vision framework; run locally with `cargo test --release vision_recognizes_drawn_text -- --ignored --nocapture`"]
    fn vision_recognizes_drawn_text() {
        use image::{Rgba, RgbaImage};

        // Tiny 5x7 ASCII font covering letters that spell `HELLO`.
        const FONT_W: usize = 5;
        const FONT_H: usize = 7;
        const FONT: &[(char, [u8; FONT_H])] = &[
            (
                'H',
                [
                    0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
                ],
            ),
            (
                'E',
                [
                    0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
                ],
            ),
            (
                'L',
                [
                    0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
                ],
            ),
            (
                'O',
                [
                    0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
                ],
            ),
            (' ', [0, 0, 0, 0, 0, 0, 0]),
        ];
        fn put_pixel(image: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>) {
            if x < 0 || y < 0 {
                return;
            }
            let w = image.width() as i32;
            let h = image.height() as i32;
            if x >= w || y >= h {
                return;
            }
            image.put_pixel(x as u32, y as u32, color);
        }

        // Build a 200x60 white image and render `HELLO` in black at scale 4x.
        let mut image = RgbaImage::from_pixel(200, 60, Rgba([255, 255, 255, 255]));
        let ink = Rgba([0, 0, 0, 255]);
        let scale = 4_u32;
        let text = "HELLO";
        let origin_x: i32 = 10;
        let origin_y: i32 = 10;
        let cell_w = (FONT_W as u32) * scale;
        let cell_h = (FONT_H as u32) * scale;
        let mut pen_x = origin_x;
        for ch in text.chars() {
            let glyph = match FONT.iter().find(|(key, _)| *key == ch) {
                Some((_, g)) => *g,
                None => {
                    pen_x += cell_w as i32;
                    continue;
                }
            };
            for (row, byte) in glyph.iter().enumerate() {
                for col in 0..FONT_W {
                    if (byte >> (FONT_W - 1 - col)) & 1 == 1 {
                        let px0 = pen_x + (col as i32) * scale as i32;
                        let py0 = origin_y + (row as i32) * scale as i32;
                        for dy in 0..scale as i32 {
                            for dx in 0..scale as i32 {
                                put_pixel(&mut image, px0 + dx, py0 + dy, ink);
                            }
                        }
                    }
                }
            }
            pen_x += cell_w as i32;
            if pen_x + cell_w as i32 > image.width() as i32 {
                break;
            }
            if origin_y + cell_h as i32 > image.height() as i32 {
                break;
            }
        }

        let map = make_stub_map();
        let result =
            recognize(&image, &map).expect("recognize should not error on a synthetic image");

        assert!(
            !result.blocks.is_empty(),
            "Vision must return at least one block for a drawn text buffer; got {:?}",
            result
        );
        let joined: String = result
            .blocks
            .iter()
            .map(|block| block.text.to_uppercase())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.contains("HELLO"),
            "Vision output should contain HELLO; got: {joined:?}"
        );
    }

    #[cfg(any(test, target_os = "linux", target_os = "windows"))]
    #[allow(dead_code)]
    fn make_stub_map() -> crate::image_util::ScreenshotMap {
        crate::image_util::ScreenshotMap {
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
