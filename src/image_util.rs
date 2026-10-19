//! Encode a screenshot, fit it to a max width or canvas, and remap pixels.

use std::io::Cursor;

use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ImageEncoder, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::error::ComputerError;
use crate::types::{normalize_scale, Frame, ImageFormat};

const JPEG_QUALITY: u8 = 80;
const MAX_EDGE: u32 = 32_768;
const MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Default screenshot width after P4. `0` means do not scale.
pub const DEFAULT_SCREENSHOT_MAX_WIDTH: u32 = 1440;

const IMAGE_TO_SCREEN: &str = "screen = origin + (image - pad) * logical_size / image_content_size";
const SCREEN_TO_IMAGE: &str = "image = pad + (screen - origin) * image_content_size / logical_size";

/// How a ScreenCaptureKit failure should be treated. Pure; no system call.
///
/// Compiled on macOS and in tests. Other platforms do not call it.
#[cfg(any(test, target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureFault {
    /// Screen Recording permission or a TCC denial.
    Permission,
    /// The CG window id was absent from shareable content.
    MissingWindow,
    /// Any other capture failure. Callers may fall back.
    Other,
}

/// Classify a fake or real ScreenCaptureKit error without calling the framework.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn classify_sck_error(domain: &str, code: i64, message: &str) -> CaptureFault {
    let domain = domain.to_ascii_lowercase();
    let message = message.to_ascii_lowercase();
    if code == -3801 || code == -3803 {
        return CaptureFault::Permission;
    }
    if domain.contains("tcc")
        || message.contains("screen recording")
        || message.contains("declined")
        || message.contains("not authorized")
    {
        return CaptureFault::Permission;
    }
    if message.contains("window not found") {
        return CaptureFault::MissingWindow;
    }
    CaptureFault::Other
}

/// True when neither shareable-content query contained the CG window id.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn window_absent(on_screen_found: bool, off_screen_found: bool) -> bool {
    !on_screen_found && !off_screen_found
}

/// Integer point stored on the screenshot map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapPoint {
    /// Horizontal component.
    pub x: i32,
    /// Vertical component.
    pub y: i32,
}

/// The two formulas from the screenshot-map contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapFormulas {
    /// Image pixels to screen points.
    pub image_to_screen: String,
    /// Screen points to image pixels.
    pub screen_to_image: String,
}

/// Map attached to every screenshot and stored in the session cache.
///
/// `content_width` and `content_height` are the drawn image inside any
/// letterbox. `pad` is only the top-left inset, so an odd leftover pixel
/// stays on the right or bottom and the formula stays exact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotMap {
    /// Output pixel width, including letterbox padding.
    pub width: u32,
    /// Output pixel height, including letterbox padding.
    pub height: u32,
    /// Capture backing scale. This is not the downscale ratio.
    pub scale: f64,
    /// Source width in screen points.
    pub logical_width: u32,
    /// Source height in screen points.
    pub logical_height: u32,
    /// Top-left of the captured area in screen points.
    pub origin: MapPoint,
    /// `width`, `none`, or `canvas`.
    pub fit: String,
    /// Top-left letterbox inset in output pixels.
    pub pad: MapPoint,
    /// Drawn width inside the letterbox.
    pub content_width: u32,
    /// Drawn height inside the letterbox.
    pub content_height: u32,
    /// Remap formulas.
    pub map: MapFormulas,
}

/// Encoded screenshot bytes and the final pixel size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedImage {
    /// File bytes.
    pub bytes: Vec<u8>,
    /// Pixel width after an optional max-width resize.
    pub width: u32,
    /// Pixel height after an optional max-width resize.
    pub height: u32,
}

/// Image plus the map that describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedShot {
    /// Pixels written to disk.
    pub image: RgbaImage,
    /// Remap metadata.
    pub map: ScreenshotMap,
}

/// One window or sheet placed into a union image.
#[cfg(any(test, target_os = "macos"))]
#[derive(Debug, Clone)]
pub(crate) struct Layer {
    /// Captured pixels.
    pub image: RgbaImage,
    /// Left edge in screen points.
    pub origin_x: i32,
    /// Top edge in screen points.
    pub origin_y: i32,
    /// Width in screen points.
    pub logical_width: u32,
    /// Height in screen points.
    pub logical_height: u32,
}

/// Union of composited layers.
#[cfg(any(test, target_os = "macos"))]
#[derive(Debug, Clone)]
pub(crate) struct Composite {
    /// Sheet drawn above the parent.
    pub image: RgbaImage,
    /// Union left edge in screen points.
    pub origin_x: i32,
    /// Union top edge in screen points.
    pub origin_y: i32,
    /// Union width in screen points.
    pub logical_width: u32,
    /// Union height in screen points.
    pub logical_height: u32,
}

/// Resize to `max_width` when the frame is wider, then encode.
///
/// `None` and `Some(0)` both keep the source width.
pub fn encode_image(
    frame: &Frame,
    format: ImageFormat,
    max_width: Option<u32>,
) -> Result<EncodedImage, ComputerError> {
    let image = limit_width(frame.image(), max_width.unwrap_or(0))?;
    encode_rgba(&image, format)
}

/// Encode pixels that [`prepare_shot`] already fitted.
pub fn encode_rgba(image: &RgbaImage, format: ImageFormat) -> Result<EncodedImage, ComputerError> {
    let bytes = match format {
        ImageFormat::Png => encode_png(image)?,
        ImageFormat::Jpeg => encode_jpeg(image)?,
    };
    Ok(EncodedImage {
        width: image.width(),
        height: image.height(),
        bytes,
    })
}

/// Standard base64, without line wraps.
pub fn encode_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Parse `1440x900`, `1440X900`, or `1440×900`.
pub fn parse_canvas(text: &str) -> Result<(u32, u32), ComputerError> {
    let normalized = text.trim().replace(['×', 'X'], "x");
    let Some((width, height)) = normalized.split_once('x') else {
        return Err(ComputerError::InvalidArguments(format!(
            "canvas must be WxH (got {text:?})"
        )));
    };
    let width = parse_canvas_edge(width, "width")?;
    let height = parse_canvas_edge(height, "height")?;
    Ok((width, height))
}

/// Geometry and fit policy for one prepared screenshot.
pub struct ShotFit {
    /// Capture backing scale.
    pub scale: f64,
    /// Left edge of the captured area in screen points.
    pub origin_x: i32,
    /// Top edge of the captured area in screen points.
    pub origin_y: i32,
    /// Source width in screen points.
    pub logical_width: u32,
    /// Source height in screen points.
    pub logical_height: u32,
    /// Maximum output width. `0` does not scale.
    pub max_width: u32,
    /// Letterbox canvas. When set, it wins over `max_width`.
    pub canvas: Option<(u32, u32)>,
}

/// Fit `image` and build the screenshot map.
///
/// `max_width == 0` does not scale. A canvas letterboxes and wins over
/// `max_width`. The image is never scaled up.
pub fn prepare_shot(image: &RgbaImage, fit: ShotFit) -> Result<PreparedShot, ComputerError> {
    let ShotFit {
        scale,
        origin_x,
        origin_y,
        logical_width,
        logical_height,
        max_width,
        canvas,
    } = fit;
    let scale = normalize_scale(scale);
    let logical_width = logical_width.max(1);
    let logical_height = logical_height.max(1);
    let (fitted, fit, pad_x, pad_y, content_width, content_height) = match canvas {
        Some((canvas_w, canvas_h)) => {
            checked_size(canvas_w, canvas_h)?;
            let (content_w, content_h) =
                fit_inside(image.width(), image.height(), canvas_w, canvas_h);
            let content = resize_to(image, content_w, content_h);
            let pad_x = (canvas_w - content.width()) / 2;
            let pad_y = (canvas_h - content.height()) / 2;
            let mut canvas_image = RgbaImage::from_pixel(canvas_w, canvas_h, Rgba([0, 0, 0, 255]));
            image::imageops::overlay(
                &mut canvas_image,
                &content,
                i64::from(pad_x),
                i64::from(pad_y),
            );
            (
                canvas_image,
                "canvas",
                pad_x,
                pad_y,
                content.width(),
                content.height(),
            )
        }
        None => {
            let fitted = limit_width(image, max_width)?;
            let fit = if max_width == 0 { "none" } else { "width" };
            (fitted, fit, 0, 0, 0, 0)
        }
    };
    let content_width = if canvas.is_some() {
        content_width
    } else {
        fitted.width()
    };
    let content_height = if canvas.is_some() {
        content_height
    } else {
        fitted.height()
    };
    checked_size(fitted.width(), fitted.height())?;
    Ok(PreparedShot {
        map: ScreenshotMap {
            width: fitted.width(),
            height: fitted.height(),
            scale,
            logical_width,
            logical_height,
            origin: MapPoint {
                x: origin_x,
                y: origin_y,
            },
            fit: fit.to_string(),
            pad: MapPoint {
                x: i32::try_from(pad_x).unwrap_or(i32::MAX),
                y: i32::try_from(pad_y).unwrap_or(i32::MAX),
            },
            content_width,
            content_height,
            map: MapFormulas {
                image_to_screen: IMAGE_TO_SCREEN.to_string(),
                screen_to_image: SCREEN_TO_IMAGE.to_string(),
            },
        },
        image: fitted,
    })
}

/// Image pixel to screen point.
pub fn image_to_screen(map: &ScreenshotMap, image_x: f64, image_y: f64) -> (f64, f64) {
    let content_w = f64::from(map.content_width.max(1));
    let content_h = f64::from(map.content_height.max(1));
    let logical_w = f64::from(map.logical_width.max(1));
    let logical_h = f64::from(map.logical_height.max(1));
    let screen_x = map.origin.x as f64 + (image_x - map.pad.x as f64) * logical_w / content_w;
    let screen_y = map.origin.y as f64 + (image_y - map.pad.y as f64) * logical_h / content_h;
    (screen_x, screen_y)
}

/// Screen point to image pixel.
pub fn screen_to_image(map: &ScreenshotMap, screen_x: f64, screen_y: f64) -> (f64, f64) {
    let content_w = f64::from(map.content_width.max(1));
    let content_h = f64::from(map.content_height.max(1));
    let logical_w = f64::from(map.logical_width.max(1));
    let logical_h = f64::from(map.logical_height.max(1));
    let image_x = map.pad.x as f64 + (screen_x - map.origin.x as f64) * content_w / logical_w;
    let image_y = map.pad.y as f64 + (screen_y - map.origin.y as f64) * content_h / logical_h;
    (image_x, image_y)
}

/// Draw `layers` into one image. Later layers paint over earlier ones.
///
/// The pixel grid is the union of the logical rectangles times `scale`.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn composite_layers(layers: &[Layer], scale: f64) -> Result<Composite, ComputerError> {
    if layers.is_empty() {
        return Err(ComputerError::ExecutionFailed(
            "composite has no layers".into(),
        ));
    }
    let scale = normalize_scale(scale);
    let mut min_x = i64::MAX;
    let mut min_y = i64::MAX;
    let mut max_x = i64::MIN;
    let mut max_y = i64::MIN;
    for layer in layers {
        let x = i64::from(layer.origin_x);
        let y = i64::from(layer.origin_y);
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + i64::from(layer.logical_width.max(1)));
        max_y = max_y.max(y + i64::from(layer.logical_height.max(1)));
    }
    let logical_width = u32::try_from(max_x - min_x).unwrap_or(u32::MAX).max(1);
    let logical_height = u32::try_from(max_y - min_y).unwrap_or(u32::MAX).max(1);
    let pixel_width = points_to_px(logical_width, scale);
    let pixel_height = points_to_px(logical_height, scale);
    checked_size(pixel_width, pixel_height)?;
    let mut image = RgbaImage::from_pixel(pixel_width, pixel_height, Rgba([0, 0, 0, 255]));
    for layer in layers {
        let layer_w = points_to_px(layer.logical_width.max(1), scale);
        let layer_h = points_to_px(layer.logical_height.max(1), scale);
        let placed = resize_to(&layer.image, layer_w, layer_h);
        let dx = ((layer.origin_x as f64 - min_x as f64) * scale).round() as i64;
        let dy = ((layer.origin_y as f64 - min_y as f64) * scale).round() as i64;
        image::imageops::overlay(&mut image, &placed, dx, dy);
    }
    Ok(Composite {
        image,
        origin_x: i32::try_from(min_x).unwrap_or(i32::MIN),
        origin_y: i32::try_from(min_y).unwrap_or(i32::MIN),
        logical_width,
        logical_height,
    })
}

fn parse_canvas_edge(text: &str, name: &str) -> Result<u32, ComputerError> {
    let value = text.trim().parse::<u32>().map_err(|_| {
        ComputerError::InvalidArguments(format!("canvas {name} is not an integer: {text:?}"))
    })?;
    if value == 0 {
        return Err(ComputerError::InvalidArguments(
            "canvas width and height must be greater than 0".into(),
        ));
    }
    Ok(value)
}

fn limit_width(image: &RgbaImage, max_width: u32) -> Result<RgbaImage, ComputerError> {
    if max_width == 0 || image.width() <= max_width {
        return Ok(image.clone());
    }
    let height = u32::try_from(
        (u64::from(image.height()) * u64::from(max_width)) / u64::from(image.width()),
    )
    .unwrap_or(1)
    .max(1);
    checked_size(max_width, height)?;
    Ok(resize_to(image, max_width, height))
}

fn fit_inside(src_w: u32, src_h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    if src_w <= box_w && src_h <= box_h {
        return (src_w.max(1), src_h.max(1));
    }
    let scale = (f64::from(box_w) / f64::from(src_w.max(1)))
        .min(f64::from(box_h) / f64::from(src_h.max(1)));
    let width = (f64::from(src_w) * scale).floor().max(1.0) as u32;
    let height = (f64::from(src_h) * scale).floor().max(1.0) as u32;
    (width.min(box_w).max(1), height.min(box_h).max(1))
}

fn resize_to(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    if image.width() == width && image.height() == height {
        image.clone()
    } else {
        image::imageops::resize(image, width, height, image::imageops::FilterType::Lanczos3)
    }
}

#[cfg(any(test, target_os = "macos"))]
fn points_to_px(points: u32, scale: f64) -> u32 {
    let value = (f64::from(points) * scale).round();
    if value < 1.0 {
        1
    } else if value > f64::from(u32::MAX) {
        u32::MAX
    } else {
        value as u32
    }
}

fn checked_size(width: u32, height: u32) -> Result<(), ComputerError> {
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
        return Err(ComputerError::ExecutionFailed(format!(
            "invalid capture size {width}x{height}"
        )));
    }
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| ComputerError::ExecutionFailed("capture size overflow".into()))?;
    if bytes > MAX_BYTES {
        return Err(ComputerError::ExecutionFailed(
            "capture is too large".into(),
        ));
    }
    Ok(())
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, ComputerError> {
    let mut buffer = Cursor::new(Vec::new());
    PngEncoder::new(&mut buffer)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| ComputerError::ExecutionFailed(format!("png encoding failed: {error}")))?;
    Ok(buffer.into_inner())
}

fn encode_jpeg(image: &RgbaImage) -> Result<Vec<u8>, ComputerError> {
    let rgb = rgba_to_rgb(image);
    let mut buffer = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut buffer, JPEG_QUALITY)
        .write_image(
            &rgb,
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|error| {
            ComputerError::ExecutionFailed(format!("jpeg encoding failed: {error}"))
        })?;
    Ok(buffer.into_inner())
}

fn rgba_to_rgb(image: &RgbaImage) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(image.as_raw().len() / 4 * 3);
    for pixel in image.pixels() {
        rgb.extend_from_slice(&pixel.0[..3]);
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba(rgba))
    }

    fn frame_solid(width: u32, height: u32) -> Frame {
        Frame::from_image(solid(width, height, [255, 0, 0, 255]), 2.0)
    }

    #[test]
    fn png_and_jpeg_magic_bytes() {
        let frame = frame_solid(2, 2);
        let png = encode_image(&frame, ImageFormat::Png, None).unwrap();
        assert_eq!(&png.bytes[..4], &[0x89, b'P', b'N', b'G']);
        assert_eq!((png.width, png.height), (2, 2));
        let jpeg = encode_image(&frame, ImageFormat::Jpeg, None).unwrap();
        assert_eq!(&jpeg.bytes[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn max_width_scales_height_and_zero_keeps_source_width() {
        let frame = frame_solid(4, 2);
        let encoded = encode_image(&frame, ImageFormat::Png, Some(2)).unwrap();
        assert_eq!((encoded.width, encoded.height), (2, 1));
        let full = encode_image(&frame, ImageFormat::Png, Some(0)).unwrap();
        assert_eq!((full.width, full.height), (4, 2));
        let same = encode_image(&frame, ImageFormat::Png, Some(8)).unwrap();
        assert_eq!((same.width, same.height), (4, 2));
    }

    #[test]
    fn base64_roundtrip() {
        let text = encode_base64(b"hi");
        assert_eq!(text, "aGk=");
    }

    #[test]
    fn retina_max_width_1440_remaps_origin() {
        let image = solid(1600, 1000, [10, 20, 30, 255]);
        let prepared = prepare_shot(
            &image,
            ShotFit {
                scale: 2.0,
                origin_x: 120,
                origin_y: 40,
                logical_width: 800,
                logical_height: 500,
                max_width: 1440,
                canvas: None,
            },
        )
        .unwrap();
        assert_eq!((prepared.map.width, prepared.map.height), (1440, 900));
        assert_eq!(prepared.map.scale, 2.0);
        assert_eq!(prepared.map.logical_width, 800);
        assert_eq!(prepared.map.logical_height, 500);
        assert_eq!(prepared.map.fit, "width");
        assert_eq!((prepared.map.pad.x, prepared.map.pad.y), (0, 0));
        assert_eq!(
            (prepared.map.content_width, prepared.map.content_height),
            (1440, 900)
        );
        assert_eq!(prepared.map.map.image_to_screen, IMAGE_TO_SCREEN);
        let (sx, sy) = image_to_screen(&prepared.map, 0.0, 0.0);
        assert_eq!((sx, sy), (120.0, 40.0));
        let (sx, sy) = image_to_screen(&prepared.map, 1440.0, 900.0);
        assert_eq!((sx, sy), (920.0, 540.0));
        let (ix, iy) = screen_to_image(&prepared.map, 520.0, 290.0);
        assert!((ix - 720.0).abs() < 0.001, "{ix}");
        assert!((iy - 450.0).abs() < 0.001, "{iy}");
        let (back_x, back_y) = image_to_screen(&prepared.map, ix, iy);
        assert!((back_x - 520.0).abs() < 1.0);
        assert!((back_y - 290.0).abs() < 1.0);
    }

    #[test]
    fn canvas_1440x900_records_padding() {
        let image = solid(800, 200, [1, 2, 3, 255]);
        let prepared = prepare_shot(
            &image,
            ShotFit {
                scale: 1.0,
                origin_x: 120,
                origin_y: 40,
                logical_width: 800,
                logical_height: 200,
                max_width: 1440,
                canvas: Some((1440, 900)),
            },
        )
        .unwrap();
        assert_eq!((prepared.map.width, prepared.map.height), (1440, 900));
        assert_eq!(prepared.map.fit, "canvas");
        assert_eq!(
            (prepared.map.content_width, prepared.map.content_height),
            (800, 200)
        );
        assert!(prepared.map.pad.x > 0);
        assert!(prepared.map.pad.y > 0);
        assert_eq!((prepared.map.pad.x, prepared.map.pad.y), (320, 350));
        let (sx, sy) = image_to_screen(
            &prepared.map,
            prepared.map.pad.x as f64,
            prepared.map.pad.y as f64,
        );
        assert_eq!((sx, sy), (120.0, 40.0));
        let (left, _) = image_to_screen(&prepared.map, 0.0, prepared.map.pad.y as f64);
        assert!(left < prepared.map.origin.x as f64);
        assert_eq!(prepared.image.get_pixel(0, 0).0, [0, 0, 0, 255]);
        assert_eq!(
            prepared
                .image
                .get_pixel(prepared.map.pad.x as u32, prepared.map.pad.y as u32)
                .0,
            [1, 2, 3, 255]
        );
    }

    #[test]
    fn known_pixel_round_trips_through_encode() {
        let mut image = solid(100, 50, [255, 0, 0, 255]);
        image.put_pixel(40, 20, Rgba([0, 255, 0, 255]));
        let prepared = prepare_shot(
            &image,
            ShotFit {
                scale: 2.0,
                origin_x: 120,
                origin_y: 40,
                logical_width: 50,
                logical_height: 25,
                max_width: 1440,
                canvas: None,
            },
        )
        .unwrap();
        assert_eq!((prepared.map.width, prepared.map.height), (100, 50));
        let encoded = encode_rgba(&prepared.image, ImageFormat::Png).unwrap();
        let decoded = image::load_from_memory(&encoded.bytes).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(40, 20).0, [0, 255, 0, 255]);
        let (sx, sy) = image_to_screen(&prepared.map, 40.0, 20.0);
        assert!((sx - 140.0).abs() <= 1.0, "{sx}");
        assert!((sy - 50.0).abs() <= 1.0, "{sy}");
        let (ix, iy) = screen_to_image(&prepared.map, sx, sy);
        assert!((ix - 40.0).abs() <= 1.0, "{ix}");
        assert!((iy - 20.0).abs() <= 1.0, "{iy}");
    }

    #[test]
    fn composite_places_sheet_above_parent() {
        let parent = Layer {
            image: solid(4, 4, [255, 0, 0, 255]),
            origin_x: 10,
            origin_y: 20,
            logical_width: 4,
            logical_height: 4,
        };
        let sheet = Layer {
            image: solid(2, 2, [0, 255, 0, 255]),
            origin_x: 11,
            origin_y: 21,
            logical_width: 2,
            logical_height: 2,
        };
        let union = composite_layers(&[parent, sheet], 1.0).unwrap();
        assert_eq!((union.origin_x, union.origin_y), (10, 20));
        assert_eq!((union.logical_width, union.logical_height), (4, 4));
        assert_eq!(union.image.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(union.image.get_pixel(1, 1).0, [0, 255, 0, 255]);
    }

    #[test]
    fn fake_sck_errors_map_to_faults() {
        assert_eq!(
            classify_sck_error("SCStreamErrorDomain", -3801, "declined"),
            CaptureFault::Permission
        );
        assert_eq!(
            classify_sck_error("com.apple.TCC", 0, "Screen Recording denied"),
            CaptureFault::Permission
        );
        assert_eq!(
            classify_sck_error("SCStreamErrorDomain", -3812, "window not found"),
            CaptureFault::MissingWindow
        );
        assert_eq!(
            classify_sck_error("SCStreamErrorDomain", -3802, "failed to start"),
            CaptureFault::Other
        );
        assert!(window_absent(false, false));
        assert!(!window_absent(true, false));
        assert!(!window_absent(false, true));
    }

    #[test]
    fn parse_canvas_accepts_separators() {
        assert_eq!(parse_canvas("1440x900").unwrap(), (1440, 900));
        assert_eq!(parse_canvas("1440X900").unwrap(), (1440, 900));
        assert_eq!(parse_canvas("1440×900").unwrap(), (1440, 900));
        assert!(parse_canvas("1440").is_err());
        assert!(parse_canvas("0x10").is_err());
    }
}
