//! Per-window capture on Windows through `windows-capture` 2.0.1.
//!
//! One `Windows.Graphics.Capture` frame is pulled from the resolved `HWND`
//! and the pixel-to-screen map is built from the window rect plus the DPI scale
//! reported by `GetDpiForWindow`. When the graphics-capture path cannot
//! produce a frame the module crops the xcap desktop image instead and labels
//! it `capture_source: "xcap"`, exactly like the P4 macOS fallback.
//!
//! # Why `logical_*` is the window rect and not the frame buffer
//!
//! On Windows the whole crate speaks one screen-point space: `apps.rs` reports
//! `GetWindowRect` bounds, `uia.rs` reports UIA bounding rectangles, and the
//! enigo click path reports `SetCursorPos` coordinates. `FrameBuffer` pixels
//! are device pixels, so they can differ from that space when the process is
//! DPI-unaware (Windows virtualizes coordinates) or on a mixed-DPI desktop.
//!
//! The screenshot map already absorbs that ratio — `image_to_screen` scales by
//! `logical_* / content_*` — so the rect is the correct `logical_*` and
//! `GetDpiForWindow` is recorded as the capture backing `scale`. Using
//! `logical_* = frame / scale` instead would mix a physical origin with a
//! logical size and break every click. Process DPI awareness is deliberately
//! *not* switched here: `SetProcessDpiAwarenessContext` only succeeds before
//! the first window exists, and flipping it mid-process would desynchronize
//! the bounds `apps.rs` already read for this same capture.

// `windows-capture` hands out the raw `HWND` pointer and the frame buffer, so
// this module needs `unsafe` for the handle round-trip. There is no safe
// wrapper for `GetDpiForWindow` taking a `u32` id.
#![allow(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use image::RgbaImage;
use windows_capture::capture::{
    CaptureControlError, Context, GraphicsCaptureApiError, GraphicsCaptureApiHandler,
};
use windows_capture::frame::Frame as WgcFrame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window as CaptureWindow;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::GetDpiForWindow;

use crate::backend::capture as xcap_capture;
use crate::backend::WindowTarget;
use crate::error::ComputerError;
use crate::image_util;
use crate::types::{Frame, RawCapture};

/// Label for a real `Windows.Graphics.Capture` frame.
pub(crate) const SOURCE_WGC: &str = "wgc";

/// How long one frame may take before the capture is abandoned. The graphics
/// capture API only pushes a frame while the window repaints, so a window that
/// never repaints would otherwise hang the CLI forever.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the waiter re-checks the shared slot while polling for a frame.
const FRAME_POLL: Duration = Duration::from_millis(10);

/// Capture `target` without activating it.
///
/// Tries one `Windows.Graphics.Capture` frame first. Any failure falls back to
/// a crop of the xcap desktop image, which is correct geometry but may show
/// whatever is covering the window.
pub(crate) fn capture_window(target: &WindowTarget) -> Result<RawCapture, ComputerError> {
    if target.bounds.width == 0 || target.bounds.height == 0 {
        return Err(ComputerError::InvalidArguments(
            "window bounds are empty".into(),
        ));
    }
    match capture_wgc(target) {
        Ok(raw) => Ok(raw),
        Err(error) => fallback_crop(target, &error),
    }
}

/// Crop the desktop image to the window rect and explain why.
fn fallback_crop(
    target: &WindowTarget,
    cause: &ComputerError,
) -> Result<RawCapture, ComputerError> {
    let mut raw = xcap_capture::crop_window(target.bounds)?;
    raw.warnings.push(format!(
        "Windows.Graphics.Capture failed ({cause}); cropped the desktop image instead, so covered \
         pixels may be wrong"
    ));
    Ok(raw)
}

/// One frame from the HWND in `target`.
fn capture_wgc(target: &WindowTarget) -> Result<RawCapture, ComputerError> {
    let raw_hwnd = target.window_id as *mut std::ffi::c_void;
    let dpi = unsafe { GetDpiForWindow(HWND(raw_hwnd)) };
    let geo = image_util::window_placement(
        target.bounds.x,
        target.bounds.y,
        target.bounds.width,
        target.bounds.height,
        dpi,
    );

    let window = CaptureWindow::from_raw_hwnd(raw_hwnd);
    let sink: Arc<Mutex<Option<RgbaImage>>> = Arc::new(Mutex::new(None));
    let settings = Settings::new(
        window,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Exclude,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        Arc::clone(&sink),
    );

    let image = grab_frame(settings, sink)?;
    let mut warnings = Vec::new();
    if let Some(warning) = image_util::size_mismatch_warning(image.width(), image.height(), &geo) {
        warnings.push(warning);
    }
    Ok(RawCapture {
        frame: Frame::from_image(image, geo.scale),
        origin_x: geo.origin_x,
        origin_y: geo.origin_y,
        logical_width: geo.logical_width,
        logical_height: geo.logical_height,
        capture_source: SOURCE_WGC.into(),
        occluded: false,
        warnings,
    })
}

/// Handler that keeps exactly one frame and then asks the session to stop.
///
/// The shared slot arrives through `Settings::flags` because the callback is
/// constructed on the capture thread: the session owns the handler, not us.
struct SingleFrame {
    slot: Arc<Mutex<Option<RgbaImage>>>,
}

impl GraphicsCaptureApiHandler for SingleFrame {
    type Flags = Arc<Mutex<Option<RgbaImage>>>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { slot: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WgcFrame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let image = copy_frame(frame)?;
        if let Ok(mut slot) = self.slot.lock() {
            if slot.is_none() {
                *slot = Some(image);
            }
        }
        capture_control.stop();
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Copy one frame out of the GPU-backed buffer, dropping the row pitch.
fn copy_frame(frame: &mut WgcFrame) -> Result<RgbaImage, Box<dyn std::error::Error + Send + Sync>> {
    let mut buffer = frame.buffer()?;
    let width = buffer.width();
    let height = buffer.height();
    let pixels = buffer.as_raw_buffer().to_vec();
    RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| "captured frame dimensions do not match its pixel buffer".into())
}

/// Start the session, wait for one frame, then shut the session down.
fn grab_frame(
    settings: Settings<Arc<Mutex<Option<RgbaImage>>>, CaptureWindow>,
    slot: Arc<Mutex<Option<RgbaImage>>>,
) -> Result<RgbaImage, ComputerError> {
    let control = SingleFrame::start_free_threaded(settings).map_err(start_error)?;
    let image = wait_for_frame(&slot);
    // The handler already asked the session to stop; this joins the capture
    // thread. A failure here leaves a worker behind, so it is reported.
    control.stop().map_err(stop_error)?;
    image
}

/// Poll the shared slot until a frame lands or the budget runs out.
fn wait_for_frame(slot: &Arc<Mutex<Option<RgbaImage>>>) -> Result<RgbaImage, ComputerError> {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    loop {
        if let Ok(mut guard) = slot.lock() {
            if let Some(image) = guard.take() {
                return Ok(image);
            }
        }
        if Instant::now() >= deadline {
            return Err(ComputerError::ExecutionFailed(format!(
                "Windows.Graphics.Capture produced no frame within {}s",
                FRAME_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(FRAME_POLL);
    }
}

fn start_error(
    error: GraphicsCaptureApiError<Box<dyn std::error::Error + Send + Sync>>,
) -> ComputerError {
    ComputerError::ExecutionFailed(format!("Windows.Graphics.Capture: {error}"))
}

fn stop_error(
    error: CaptureControlError<Box<dyn std::error::Error + Send + Sync>>,
) -> ComputerError {
    ComputerError::ExecutionFailed(format!("Windows.Graphics.Capture shutdown: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::WindowBounds;

    fn bounds(x: i32, y: i32, width: u32, height: u32) -> WindowBounds {
        WindowBounds {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn empty_bounds_are_refused_before_any_capture() {
        let target = WindowTarget {
            window_id: 1,
            pid: 1,
            bounds: bounds(0, 0, 0, 0),
        };
        let Err(error) = capture_window(&target) else {
            panic!("empty bounds must be refused");
        };
        assert!(
            matches!(error, ComputerError::InvalidArguments(_)),
            "{error:?}"
        );
    }
}
