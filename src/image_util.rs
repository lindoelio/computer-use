//! Encode a [`Frame`] as PNG or JPEG, optionally limiting the width.

use std::io::Cursor;

use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::ImageEncoder;

use crate::error::ComputerError;
use crate::types::{Frame, ImageFormat};

const JPEG_QUALITY: u8 = 80;

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

/// Resize to `max_width` when the frame is wider, then encode.
pub fn encode_image(
    frame: &Frame,
    format: ImageFormat,
    max_width: Option<u32>,
) -> Result<EncodedImage, ComputerError> {
    let image = limit_width(frame.image(), max_width)?;
    let bytes = match format {
        ImageFormat::Png => encode_png(&image)?,
        ImageFormat::Jpeg => encode_jpeg(&image)?,
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

fn limit_width(
    image: &image::RgbaImage,
    max_width: Option<u32>,
) -> Result<image::RgbaImage, ComputerError> {
    let Some(max_width) = max_width else {
        return Ok(image.clone());
    };
    if max_width == 0 {
        return Err(ComputerError::InvalidArguments(
            "max_width must be greater than 0".into(),
        ));
    }
    if image.width() <= max_width {
        return Ok(image.clone());
    }
    let height = u32::try_from(
        (u64::from(image.height()) * u64::from(max_width)) / u64::from(image.width()),
    )
    .unwrap_or(1)
    .max(1);
    Ok(image::imageops::resize(
        image,
        max_width,
        height,
        image::imageops::FilterType::Lanczos3,
    ))
}

fn encode_png(image: &image::RgbaImage) -> Result<Vec<u8>, ComputerError> {
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

fn encode_jpeg(image: &image::RgbaImage) -> Result<Vec<u8>, ComputerError> {
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

fn rgba_to_rgb(image: &image::RgbaImage) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(image.as_raw().len() / 4 * 3);
    for pixel in image.pixels() {
        rgb.extend_from_slice(&pixel.0[..3]);
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32) -> Frame {
        let mut rgba = Vec::new();
        for _ in 0..(width * height) {
            rgba.extend_from_slice(&[255, 0, 0, 255]);
        }
        Frame::new(width, height, 2.0, rgba).unwrap()
    }

    #[test]
    fn png_and_jpeg_magic_bytes() {
        let frame = solid(2, 2);
        let png = encode_image(&frame, ImageFormat::Png, None).unwrap();
        assert_eq!(&png.bytes[..4], &[0x89, b'P', b'N', b'G']);
        assert_eq!((png.width, png.height), (2, 2));
        let jpeg = encode_image(&frame, ImageFormat::Jpeg, None).unwrap();
        assert_eq!(&jpeg.bytes[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn max_width_scales_height_and_rejects_zero() {
        let frame = solid(4, 2);
        let encoded = encode_image(&frame, ImageFormat::Png, Some(2)).unwrap();
        assert_eq!((encoded.width, encoded.height), (2, 1));
        assert!(encode_image(&frame, ImageFormat::Png, Some(0)).is_err());
        let same = encode_image(&frame, ImageFormat::Png, Some(8)).unwrap();
        assert_eq!((same.width, same.height), (4, 2));
    }

    #[test]
    fn base64_roundtrip() {
        let text = encode_base64(b"hi");
        assert_eq!(text, "aGk=");
    }
}
