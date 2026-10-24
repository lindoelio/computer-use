//! Shared geometry, input, and image types.

use serde::de::{self, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::ComputerError;

const MAX_REGION_EDGE: u32 = 100_000;

/// Rectangle in logical screen pixels. The origin is the top-left of the
/// virtual desktop, matching click coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Region {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in logical pixels. Must be non-zero.
    pub width: u32,
    /// Height in logical pixels. Must be non-zero.
    pub height: u32,
}

impl Region {
    /// Build a region after checking size limits.
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Result<Self, ComputerError> {
        validate_region(x, y, width, height)?;
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }
}

/// Parse `x,y,width,height`. Whitespace around commas is ignored.
pub fn parse_region(text: &str) -> Result<Region, ComputerError> {
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(ComputerError::InvalidArguments(format!(
            "region must be x,y,width,height (got {text:?})"
        )));
    }
    let x = parse_i32(parts[0], "x")?;
    let y = parse_i32(parts[1], "y")?;
    let width = parse_u32(parts[2], "width")?;
    let height = parse_u32(parts[3], "height")?;
    Region::new(x, y, width, height)
}

fn parse_i32(text: &str, name: &str) -> Result<i32, ComputerError> {
    text.parse::<i32>().map_err(|_| {
        ComputerError::InvalidArguments(format!("region {name} is not an integer: {text:?}"))
    })
}

fn parse_u32(text: &str, name: &str) -> Result<u32, ComputerError> {
    text.parse::<u32>().map_err(|_| {
        ComputerError::InvalidArguments(format!(
            "region {name} is not a non-negative integer: {text:?}"
        ))
    })
}

fn validate_region(x: i32, y: i32, width: u32, height: u32) -> Result<(), ComputerError> {
    if width == 0 || height == 0 {
        return Err(ComputerError::InvalidArguments(
            "region width and height must be greater than 0".into(),
        ));
    }
    if width > MAX_REGION_EDGE || height > MAX_REGION_EDGE {
        return Err(ComputerError::InvalidArguments(format!(
            "region width and height must be at most {MAX_REGION_EDGE}"
        )));
    }
    let _ = (x, y);
    Ok(())
}

impl<'de> Deserialize<'de> for Region {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RegionVisitor;

        impl<'de> Visitor<'de> for RegionVisitor {
            type Value = Region;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a \"x,y,width,height\" string or {x,y,width,height} object")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Region, E> {
                parse_region(value).map_err(E::custom)
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Region, E> {
                self.visit_str(&value)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Region, A::Error> {
                let mut x = None;
                let mut y = None;
                let mut width = None;
                let mut height = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "x" => x = Some(map.next_value()?),
                        "y" => y = Some(map.next_value()?),
                        "width" => width = Some(map.next_value()?),
                        "height" => height = Some(map.next_value()?),
                        _ => {
                            let _ = map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                let x = x.ok_or_else(|| de::Error::missing_field("x"))?;
                let y = y.ok_or_else(|| de::Error::missing_field("y"))?;
                let width = width.ok_or_else(|| de::Error::missing_field("width"))?;
                let height = height.ok_or_else(|| de::Error::missing_field("height"))?;
                Region::new(x, y, width, height).map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_any(RegionVisitor)
    }
}

/// Mouse button for click actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    /// Primary button.
    #[default]
    Left,
    /// Secondary button.
    Right,
    /// Middle button or wheel click.
    Middle,
    /// Button 4. The event tier posts it as other-mouse button number 3.
    Back,
    /// Button 5. The event tier posts it as other-mouse button number 4.
    Forward,
}

impl MouseButton {
    /// JSON and CLI name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
            Self::Back => "back",
            Self::Forward => "forward",
        }
    }
}

/// One logical point. Drag paths are a list of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    /// Logical x.
    pub x: i32,
    /// Logical y.
    pub y: i32,
}

/// Parse a flat `x,y,x,y,...` list. At least two points, and an even count.
pub fn parse_path(text: &str) -> Result<Vec<Point>, ComputerError> {
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    if parts.len() < 4 || !parts.len().is_multiple_of(2) || parts.iter().any(|part| part.is_empty())
    {
        return Err(ComputerError::InvalidArguments(format!(
            "path must be at least two x,y points (got {text:?})"
        )));
    }
    let mut points = Vec::with_capacity(parts.len() / 2);
    for pair in parts.chunks(2) {
        let x = pair[0].parse::<i32>().map_err(|_| {
            ComputerError::InvalidArguments(format!("path x is not an integer: {:?}", pair[0]))
        })?;
        let y = pair[1].parse::<i32>().map_err(|_| {
            ComputerError::InvalidArguments(format!("path y is not an integer: {:?}", pair[1]))
        })?;
        points.push(Point { x, y });
    }
    Ok(points)
}

/// True when the region sits inside `width` by `height`.
///
/// The far edge may equal the width or height. A point on that edge is outside.
pub fn region_inside(region: Region, width: u32, height: u32) -> Result<(), ComputerError> {
    if region.x < 0 || region.y < 0 {
        return Err(ComputerError::InvalidArguments(
            "region is outside the capture".into(),
        ));
    }
    let right = i64::from(region.x) + i64::from(region.width);
    let bottom = i64::from(region.y) + i64::from(region.height);
    if right > i64::from(width) || bottom > i64::from(height) {
        return Err(ComputerError::InvalidArguments(
            "region is outside the capture".into(),
        ));
    }
    Ok(())
}

/// Modifier held while another key is pressed.
///
/// `Command` and `Meta` both synthesize the platform meta key (Command on
/// macOS, Super on Linux, Windows key on Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    /// Command key. Serialized as `command`.
    #[serde(rename = "command")]
    Command,
    /// Control key.
    Control,
    /// Shift key.
    Shift,
    /// Alt or Option key.
    Alt,
    /// Meta or Super key.
    Meta,
}

impl Modifier {
    /// Name used in JSON responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Control => "control",
            Self::Shift => "shift",
            Self::Alt => "alt",
            Self::Meta => "meta",
        }
    }
}

/// Image file format for screenshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    /// PNG, lossless.
    #[default]
    Png,
    /// JPEG.
    Jpeg,
}

impl ImageFormat {
    /// Canonical format name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
        }
    }

    /// File extension without a dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

/// Scroll direction used with an amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDirection {
    /// Toward the top of the screen.
    Up,
    /// Toward the bottom of the screen.
    Down,
    /// Toward the left.
    Left,
    /// Toward the right.
    Right,
}

/// One connected display, in logical pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// Platform display id.
    pub id: u32,
    /// Human-readable name. May be empty when the platform has none.
    pub name: String,
    /// Left edge in the virtual desktop.
    pub x: i32,
    /// Top edge in the virtual desktop.
    pub y: i32,
    /// Logical width.
    pub width: u32,
    /// Logical height.
    pub height: u32,
    /// Device pixels per logical pixel. `1.0` when the platform does not report one.
    pub scale: f64,
    /// Whether this is the primary display.
    pub is_primary: bool,
}

/// Best-effort frontmost application and window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FrontmostInfo {
    /// Application or process name.
    pub app: Option<String>,
    /// Front window title.
    pub window: Option<String>,
    /// Why the lookup was incomplete.
    pub note: Option<String>,
}

/// Decoded screenshot pixels.
#[derive(Clone)]
pub struct Frame {
    width: u32,
    height: u32,
    scale: f64,
    image: image::RgbaImage,
}

impl Frame {
    /// Build a frame from tightly packed RGBA8 pixels.
    pub fn new(width: u32, height: u32, scale: f64, rgba: Vec<u8>) -> Result<Self, ComputerError> {
        if width == 0 || height == 0 {
            return Err(ComputerError::InvalidArguments(
                "frame width and height must be greater than 0".into(),
            ));
        }
        let image = image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
            ComputerError::InvalidArguments(
                "rgba buffer length does not match width*height*4".into(),
            )
        })?;
        Ok(Self::from_image(image, scale))
    }

    pub(crate) fn from_image(image: image::RgbaImage, scale: f64) -> Self {
        Self {
            width: image.width(),
            height: image.height(),
            scale: normalize_scale(scale),
            image,
        }
    }

    /// Pixel width of the buffer.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Pixel height of the buffer.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Device scale captured with the frame.
    pub fn scale(&self) -> f64 {
        self.scale
    }

    pub(crate) fn image(&self) -> &image::RgbaImage {
        &self.image
    }
}

impl fmt::Debug for Frame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("scale", &self.scale)
            .finish()
    }
}

/// Pixels from one capture, before max-width and canvas fitting.
#[derive(Clone)]
pub struct RawCapture {
    /// Device-pixel buffer and its backing scale.
    pub frame: Frame,
    /// Left edge of the buffer in screen points.
    pub origin_x: i32,
    /// Top edge of the buffer in screen points.
    pub origin_y: i32,
    /// Width of the captured area in screen points.
    pub logical_width: u32,
    /// Height of the captured area in screen points.
    pub logical_height: u32,
    /// `sck`, `xcap`, or `stub`.
    pub capture_source: String,
    /// True when the image is a desktop crop and covered pixels may be wrong.
    pub occluded: bool,
    /// Non-fatal notes. Empty when the capture was clean.
    pub warnings: Vec<String>,
}

/// Replace missing or invalid scale factors with 1.
pub(crate) fn normalize_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_object_visitor_reports_missing_and_invalid_fields() {
        let missing_height = serde_json::from_str::<Region>(r#"{"x":1,"y":2,"width":3}"#);
        assert!(missing_height.is_err());
        let zero_width = serde_json::from_str::<Region>(r#"{"x":1,"y":2,"width":0,"height":4}"#);
        assert!(zero_width.is_err());
        let negative_y = serde_json::from_str::<Region>(r#"{"x":1,"y":-2,"width":3,"height":4}"#);
        assert!(negative_y.is_ok(), "negative origins are valid");
    }

    #[test]
    fn frame_debug_names_the_dimensions_not_the_pixels() {
        let frame = Frame::new(4, 2, 2.0, vec![0u8; 4 * 2 * 4]).unwrap();
        let text = format!("{frame:?}");
        assert!(text.contains("width: 4"), "{text}");
        assert!(text.contains("height: 2"), "{text}");
        assert!(text.contains("scale: 2.0"), "{text}");
        assert!(!text.contains("0, 0, 0"), "pixels stay out of Debug");
    }

    #[test]
    fn parse_region_accepts_whitespace_and_rejects_bad_shapes() {
        let region = parse_region(" 10, 20, 30, 40 ").unwrap();
        assert_eq!(
            region,
            Region {
                x: 10,
                y: 20,
                width: 30,
                height: 40
            }
        );
        assert_eq!(parse_region("-5,-8,2,3").unwrap().x, -5);

        assert!(parse_region("1,2,3").is_err());
        assert!(parse_region("1,2,3,4,5").is_err());
        assert!(parse_region("a,2,3,4").is_err());
        assert!(parse_region("1,2,-3,4").is_err());
        assert!(parse_region("0,0,0,10").is_err());
        assert!(parse_region("0,0,10,0").is_err());
        assert!(parse_region("").is_err());
    }

    #[test]
    fn region_deserializes_from_string_and_object() {
        let from_string: Region = serde_json::from_str("\"1,2,3,4\"").unwrap();
        let from_object: Region =
            serde_json::from_str(r#"{"x":1,"y":2,"width":3,"height":4,"ignored":true}"#).unwrap();
        assert_eq!(from_string, from_object);
        let json = serde_json::to_value(from_object).unwrap();
        assert_eq!(json["width"], 3);
        assert!(serde_json::from_str::<Region>(r#"{"x":1,"y":2,"width":0,"height":4}"#).is_err());
    }

    #[test]
    fn path_needs_two_points_and_region_may_touch_the_far_edge() {
        let path = parse_path("10, 20, 200, 40").unwrap();
        assert_eq!(path, vec![Point { x: 10, y: 20 }, Point { x: 200, y: 40 }]);
        assert!(parse_path("1,1").is_err());
        assert!(parse_path("1,2,3").is_err());
        let inside = Region::new(0, 0, 400, 300).unwrap();
        assert!(region_inside(inside, 400, 300).is_ok());
        let past = Region::new(0, 0, 401, 300).unwrap();
        assert!(region_inside(past, 400, 300).is_err());
        assert_eq!(MouseButton::Back.as_str(), "back");
        assert_eq!(MouseButton::Forward.as_str(), "forward");
    }

    #[test]
    fn frame_checks_buffer_length_and_normalizes_scale() {
        let frame = Frame::new(1, 1, 0.0, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(frame.width(), 1);
        assert_eq!(frame.scale(), 1.0);
        assert!(Frame::new(1, 1, 1.0, vec![0, 0, 0]).is_err());
        assert!(Frame::new(0, 1, 1.0, vec![]).is_err());
    }
}
