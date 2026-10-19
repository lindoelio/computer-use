//! Screen capture through xcap.
//!
//! Click coordinates and [`crate::Region`] are logical pixels (the same space
//! `xcap` reports for display origin and size). Captured buffers are device
//! pixels, so a region is scaled by the display's scale factor before cropping.

use crate::error::ComputerError;
use crate::types::{normalize_scale, DisplayInfo, Frame, Region};

const MAX_EDGE: i64 = 32_768;
const MAX_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) fn list_displays() -> Result<Vec<DisplayInfo>, ComputerError> {
    let monitors = monitors()?;
    Ok(monitors.iter().map(display_from_monitor).collect())
}

/// Geometry of one xcap capture, in screen points.
struct Shot {
    frame: Frame,
    origin_x: i32,
    origin_y: i32,
    logical_width: u32,
    logical_height: u32,
}

pub(crate) fn capture(region: Option<Region>) -> Result<Frame, ComputerError> {
    Ok(capture_described(region)?.frame)
}

/// Full desktop or region, labeled `xcap`. The origin is the top-left of the
/// stitched virtual desktop, or the region origin.
pub(crate) fn capture_screen(
    region: Option<Region>,
) -> Result<crate::types::RawCapture, ComputerError> {
    let shot = capture_described(region)?;
    Ok(described_to_raw(shot, "xcap", false, Vec::new()))
}

/// Crop the desktop image to `bounds`. Covered pixels may belong to another window.
pub(crate) fn crop_window(
    bounds: crate::target::WindowBounds,
) -> Result<crate::types::RawCapture, ComputerError> {
    if bounds.width == 0 || bounds.height == 0 {
        return Err(ComputerError::InvalidArguments(
            "window bounds are empty".into(),
        ));
    }
    let region = Region::new(bounds.x, bounds.y, bounds.width, bounds.height)?;
    let shot = capture_described(Some(region))?;
    Ok(described_to_raw(
        shot,
        "xcap",
        true,
        vec!["cropped the desktop image; covered pixels may be wrong".into()],
    ))
}

fn described_to_raw(
    shot: Shot,
    source: &str,
    occluded: bool,
    warnings: Vec<String>,
) -> crate::types::RawCapture {
    crate::types::RawCapture {
        frame: shot.frame,
        origin_x: shot.origin_x,
        origin_y: shot.origin_y,
        logical_width: shot.logical_width,
        logical_height: shot.logical_height,
        capture_source: source.to_string(),
        occluded,
        warnings,
    }
}

fn capture_described(region: Option<Region>) -> Result<Shot, ComputerError> {
    let monitors = monitors()?;
    if monitors.is_empty() {
        return Err(ComputerError::ExecutionFailed(
            "no display available on this system".into(),
        ));
    }
    match region {
        Some(region) => capture_region(&monitors, region),
        None if monitors.len() == 1 => capture_monitor(&monitors[0]),
        None => capture_all(&monitors),
    }
}

fn monitors() -> Result<Vec<xcap::Monitor>, ComputerError> {
    xcap::Monitor::all().map_err(|error| {
        ComputerError::ExecutionFailed(format!("failed to enumerate monitors: {error}"))
    })
}

fn display_from_monitor(monitor: &xcap::Monitor) -> DisplayInfo {
    DisplayInfo {
        id: monitor.id(),
        name: monitor.name().to_string(),
        x: monitor.x(),
        y: monitor.y(),
        width: monitor.width(),
        height: monitor.height(),
        scale: normalize_scale(f64::from(monitor.scale_factor())),
        is_primary: monitor.is_primary(),
    }
}

fn capture_monitor(monitor: &xcap::Monitor) -> Result<Shot, ComputerError> {
    let image = capture_image(monitor)?;
    let scale = normalize_scale(f64::from(monitor.scale_factor()));
    Ok(Shot {
        frame: Frame::from_image(image, scale),
        origin_x: monitor.x(),
        origin_y: monitor.y(),
        logical_width: monitor.width().max(1),
        logical_height: monitor.height().max(1),
    })
}

fn capture_image(monitor: &xcap::Monitor) -> Result<image::RgbaImage, ComputerError> {
    monitor.capture_image().map_err(|error| {
        ComputerError::ExecutionFailed(format!("screenshot failed for {}: {error}", monitor.name()))
    })
}

fn capture_all(monitors: &[xcap::Monitor]) -> Result<Shot, ComputerError> {
    let min_x = monitors
        .iter()
        .map(|monitor| i64::from(monitor.x()))
        .min()
        .unwrap_or(0);
    let min_y = monitors
        .iter()
        .map(|monitor| i64::from(monitor.y()))
        .min()
        .unwrap_or(0);
    let mut shots = Vec::with_capacity(monitors.len());
    for monitor in monitors {
        let image = capture_image(monitor)?;
        let scale = normalize_scale(f64::from(monitor.scale_factor()));
        let px = ((i64::from(monitor.x()) - min_x) as f64 * scale).round() as i64;
        let py = ((i64::from(monitor.y()) - min_y) as f64 * scale).round() as i64;
        shots.push((image, px, py));
    }
    let width = shots
        .iter()
        .map(|(image, px, _)| *px + i64::from(image.width()))
        .max()
        .unwrap_or(0);
    let height = shots
        .iter()
        .map(|(image, _, py)| *py + i64::from(image.height()))
        .max()
        .unwrap_or(0);
    let (width, height) = checked_canvas(width, height)?;
    let mut canvas = image::RgbaImage::new(width, height);
    for (image, px, py) in &shots {
        image::imageops::overlay(&mut canvas, image, *px, *py);
    }
    let scale = primary_scale(monitors);
    let max_x = monitors
        .iter()
        .map(|monitor| i64::from(monitor.x()) + i64::from(monitor.width()))
        .max()
        .unwrap_or(min_x);
    let max_y = monitors
        .iter()
        .map(|monitor| i64::from(monitor.y()) + i64::from(monitor.height()))
        .max()
        .unwrap_or(min_y);
    Ok(Shot {
        frame: Frame::from_image(canvas, scale),
        origin_x: i32::try_from(min_x).unwrap_or(i32::MIN),
        origin_y: i32::try_from(min_y).unwrap_or(i32::MIN),
        logical_width: u32::try_from(max_x - min_x).unwrap_or(1).max(1),
        logical_height: u32::try_from(max_y - min_y).unwrap_or(1).max(1),
    })
}

fn capture_region(monitors: &[xcap::Monitor], region: Region) -> Result<Shot, ComputerError> {
    let region_box = region_bounds(region);
    if let Some(monitor) = monitors
        .iter()
        .find(|monitor| contains_rect(monitor_bounds(monitor), region_box))
    {
        let image = capture_image(monitor)?;
        let scale = normalize_scale(f64::from(monitor.scale_factor()));
        let x = logical_to_px(i64::from(region.x) - i64::from(monitor.x()), scale)?;
        let y = logical_to_px(i64::from(region.y) - i64::from(monitor.y()), scale)?;
        let width = logical_len_to_px(i64::from(region.width), scale)?;
        let height = logical_len_to_px(i64::from(region.height), scale)?;
        let cropped = crop_clamped(&image, x, y, width, height)?;
        return Ok(Shot {
            frame: Frame::from_image(cropped, scale),
            origin_x: region.x,
            origin_y: region.y,
            logical_width: region.width,
            logical_height: region.height,
        });
    }

    let intersecting: Vec<&xcap::Monitor> = monitors
        .iter()
        .filter(|monitor| intersects_rect(monitor_bounds(monitor), region_box))
        .collect();
    if intersecting.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "region is outside the visible desktop".into(),
        ));
    }
    stitch_region(&intersecting, region)
}

fn stitch_region(monitors: &[&xcap::Monitor], region: Region) -> Result<Shot, ComputerError> {
    let scale = monitors
        .iter()
        .copied()
        .find(|monitor| monitor.is_primary())
        .map(|monitor| normalize_scale(f64::from(monitor.scale_factor())))
        .unwrap_or_else(|| primary_scale_ref(monitors));
    let out_w = logical_len_to_px(i64::from(region.width), scale)?;
    let out_h = logical_len_to_px(i64::from(region.height), scale)?;
    let _ = checked_canvas(i64::from(out_w), i64::from(out_h))?;
    let mut canvas = image::RgbaImage::new(out_w, out_h);
    let mut painted = false;
    for monitor in monitors {
        let image = capture_image(monitor)?;
        let monitor_scale = normalize_scale(f64::from(monitor.scale_factor()));
        let (mx0, my0, mx1, my1) = monitor_bounds(monitor);
        let (rx0, ry0, rx1, ry1) = region_bounds(region);
        let ix0 = rx0.max(mx0);
        let iy0 = ry0.max(my0);
        let ix1 = rx1.min(mx1);
        let iy1 = ry1.min(my1);
        if ix1 <= ix0 || iy1 <= iy0 {
            continue;
        }
        let sx = logical_to_px(ix0 - mx0, monitor_scale)?;
        let sy = logical_to_px(iy0 - my0, monitor_scale)?;
        let sw = logical_len_to_px(ix1 - ix0, monitor_scale)?;
        let sh = logical_len_to_px(iy1 - iy0, monitor_scale)?;
        let Ok((sx, sy, sw, sh)) = clamp_rect(image.width(), image.height(), sx, sy, sw, sh) else {
            continue;
        };
        let crop = image::imageops::crop_imm(&image, sx, sy, sw, sh).to_image();
        let dx = logical_to_px(ix0 - rx0, scale)? as i64;
        let dy = logical_to_px(iy0 - ry0, scale)? as i64;
        let dw = logical_len_to_px(ix1 - ix0, scale)?;
        let dh = logical_len_to_px(iy1 - iy0, scale)?;
        let piece = if crop.width() == dw && crop.height() == dh {
            crop
        } else {
            image::imageops::resize(&crop, dw, dh, image::imageops::FilterType::Triangle)
        };
        image::imageops::overlay(&mut canvas, &piece, dx, dy);
        painted = true;
    }
    if !painted {
        return Err(ComputerError::ExecutionFailed(
            "region did not overlap a captured display".into(),
        ));
    }
    Ok(Shot {
        frame: Frame::from_image(canvas, scale),
        origin_x: region.x,
        origin_y: region.y,
        logical_width: region.width,
        logical_height: region.height,
    })
}

fn primary_scale(monitors: &[xcap::Monitor]) -> f64 {
    monitors
        .iter()
        .find(|monitor| monitor.is_primary())
        .map(|monitor| normalize_scale(f64::from(monitor.scale_factor())))
        .unwrap_or(1.0)
}

fn primary_scale_ref(monitors: &[&xcap::Monitor]) -> f64 {
    monitors
        .first()
        .map(|monitor| normalize_scale(f64::from(monitor.scale_factor())))
        .unwrap_or(1.0)
}

fn monitor_bounds(monitor: &xcap::Monitor) -> (i64, i64, i64, i64) {
    let x = i64::from(monitor.x());
    let y = i64::from(monitor.y());
    (
        x,
        y,
        x + i64::from(monitor.width()),
        y + i64::from(monitor.height()),
    )
}

fn region_bounds(region: Region) -> (i64, i64, i64, i64) {
    (
        i64::from(region.x),
        i64::from(region.y),
        i64::from(region.x) + i64::from(region.width),
        i64::from(region.y) + i64::from(region.height),
    )
}

fn contains_rect(outer: (i64, i64, i64, i64), inner: (i64, i64, i64, i64)) -> bool {
    outer.0 <= inner.0 && outer.1 <= inner.1 && outer.2 >= inner.2 && outer.3 >= inner.3
}

fn intersects_rect(a: (i64, i64, i64, i64), b: (i64, i64, i64, i64)) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

fn logical_to_px(delta: i64, scale: f64) -> Result<u32, ComputerError> {
    if delta < 0 {
        return Err(ComputerError::InvalidArguments(
            "region is outside the display".into(),
        ));
    }
    px_from_f64(delta as f64 * scale)
}

fn logical_len_to_px(len: i64, scale: f64) -> Result<u32, ComputerError> {
    if len <= 0 {
        return Err(ComputerError::InvalidArguments(
            "region size must be greater than 0".into(),
        ));
    }
    let px = px_from_f64(len as f64 * scale)?;
    Ok(px.max(1))
}

fn px_from_f64(value: f64) -> Result<u32, ComputerError> {
    if !value.is_finite() || value < 0.0 || value > u32::MAX as f64 {
        return Err(ComputerError::ExecutionFailed(
            "region pixel coordinate is out of range".into(),
        ));
    }
    Ok(value.round() as u32)
}

fn crop_clamped(
    image: &image::RgbaImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<image::RgbaImage, ComputerError> {
    let (x, y, width, height) = clamp_rect(image.width(), image.height(), x, y, width, height)?;
    Ok(image::imageops::crop_imm(image, x, y, width, height).to_image())
}

fn clamp_rect(
    image_width: u32,
    image_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(u32, u32, u32, u32), ComputerError> {
    if width == 0 || height == 0 || x >= image_width || y >= image_height {
        return Err(ComputerError::InvalidArguments(
            "region is outside the captured display".into(),
        ));
    }
    let width = width.min(image_width - x);
    let height = height.min(image_height - y);
    Ok((x, y, width, height))
}

fn checked_canvas(width: i64, height: i64) -> Result<(u32, u32), ComputerError> {
    if width <= 0 || height <= 0 || width > MAX_EDGE || height > MAX_EDGE {
        return Err(ComputerError::ExecutionFailed(format!(
            "invalid capture size {width}x{height}"
        )));
    }
    let bytes = (width as u64)
        .checked_mul(height as u64)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| ComputerError::ExecutionFailed("capture size overflow".into()))?;
    if bytes > MAX_BYTES {
        return Err(ComputerError::ExecutionFailed(
            "capture is too large".into(),
        ));
    }
    Ok((width as u32, height as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_and_intersects() {
        let screen = (0, 0, 100, 80);
        assert!(contains_rect(screen, (0, 0, 10, 10)));
        assert!(contains_rect(screen, (90, 70, 100, 80)));
        assert!(!contains_rect(screen, (90, 70, 110, 80)));
        assert!(intersects_rect(screen, (90, 70, 120, 90)));
        assert!(!intersects_rect(screen, (100, 0, 110, 10)));
        assert!(!intersects_rect(screen, (200, 200, 210, 210)));
    }

    #[test]
    fn logical_pixels_honor_scale_and_reject_negatives() {
        assert_eq!(logical_to_px(10, 2.0).unwrap(), 20);
        assert_eq!(logical_len_to_px(10, 2.0).unwrap(), 20);
        assert_eq!(logical_len_to_px(1, 0.1).unwrap(), 1);
        assert!(logical_to_px(-1, 2.0).is_err());
        assert!(checked_canvas(0, 10).is_err());
        assert!(checked_canvas(10, 10).is_ok());
    }

    #[test]
    fn clamp_rect_trims_the_overhang() {
        assert_eq!(clamp_rect(10, 10, 8, 8, 5, 5).unwrap(), (8, 8, 2, 2));
        assert!(clamp_rect(10, 10, 10, 0, 1, 1).is_err());
    }
}
