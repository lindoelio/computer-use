//! XDG Desktop Portal capture and input on Wayland, behind Cargo feature
//! `portal`.
//!
//! P13a made Linux real for the accessibility tree (AT-SPI) and for X11 event
//! input (XTest), but left two holes that only a compositor can close:
//! per-window **capture** and coordinate **input**. A Wayland client is never
//! handed pixels and never gets to move the pointer on its own. Both are
//! delegated to the compositor through the XDG Desktop Portal:
//!
//! - [`ashpd`] `screencast` — the portal shows its own "share" dialog, returns
//!   a PipeWire node id, and we pull exactly one frame out of it.
//! - [`ashpd`] `remote_desktop` — the portal grants a pointer + keyboard
//!   session; events go back over D-Bus.
//!
//! # What the agent cannot do
//!
//! Both portals open a **system dialog the user has to click**. We cannot
//! click it for the caller, we cannot pre-tick a checkbox, and we cannot undo
//! a wrong choice. This is the same trust boundary as macOS Screen Recording,
//! expressed differently. A user who cancels is
//! [`ComputerError::PermissionRequired`], never a silent fallback.
//!
//! # Fallback policy
//!
//! Capture falls back to the xcap desktop crop (`capture_source: "xcap"`,
//! `occluded: true`) when the portal fails for a *technical* reason — no
//! portal daemon, no PipeWire, an unsupported pixel format. A cancelled
//! dialog does **not** fall back: it is `permission_required`, matching the P4
//! rule on macOS that a permission refusal is never papered over.
//!
//! # Pure logic first
//!
//! [`plan_source`], [`crop_to_stream`], [`crop_pixels`], and [`map_failure`]
//! are ordinary functions with no portal dependency. Their unit tests run on
//! any Linux host in a default `cargo test`, with no PipeWire installed and no
//! portal daemon. Only [`capture_window`] and [`perform_event_tier`] touch
//! D-Bus and PipeWire, and both are behind `feature = "portal"`.
//!
//! See `docs/ROADMAP.md` P13 for the design this implements.

#![cfg(target_os = "linux")]

use crate::backend::WindowTarget;
use crate::error::ComputerError;
use crate::target::WindowBounds;

/// `capture_source` for a real portal frame.
pub(crate) const SOURCE_PORTAL: &str = "portal";

/// Tier name reported when an event went through the RemoteDesktop portal.
pub(crate) const TIER_NAME: &str = "portal";

// ---------------------------------------------------------------------------
// Pure: source selection
// ---------------------------------------------------------------------------

/// What kind of thing the portal gave us to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    /// A whole monitor. Frames are monitor-sized, so a window needs a crop.
    Monitor,
    /// One application window. Frames are window-sized; no crop is needed.
    Window,
}

/// One stream the portal returned after the user made a choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamInfo {
    /// Monitor or window.
    pub kind: SourceKind,
    /// Stream position in **logical** screen points, when the portal reported
    /// one. Window streams routinely omit it.
    pub position: Option<(i32, i32)>,
    /// Stream size in **logical** points, when reported.
    pub size: Option<(i32, i32)>,
}

/// Which stream to record and how to reduce it to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourcePlan {
    /// The portal gave us the window itself. The frame *is* the window.
    Window {
        /// Index into the portal's stream list.
        index: usize,
    },
    /// The portal gave us a monitor. Crop the window rect out of it.
    Monitor {
        /// Index into the portal's stream list.
        index: usize,
        /// Window rect in stream-local logical points, clamped to the stream.
        crop: CropRect,
    },
}

/// A rectangle in stream-local logical points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CropRect {
    /// Left edge, stream-local.
    pub x: i32,
    /// Top edge, stream-local.
    pub y: i32,
    /// Width in logical points. Never zero.
    pub width: u32,
    /// Height in logical points. Never zero.
    pub height: u32,
}

/// Pick the stream to record and the crop to apply.
///
/// The rule is the one the roadmap states: **prefer a window source when the
/// portal offers one, else the monitor that contains the window and crop.**
///
/// A window stream wins unconditionally. It needs no crop, and it shows the
/// window even when something covers it — which is the whole reason the other
/// platforms capture per window. Failing that, the monitor is chosen by
/// containment: the window centre must sit inside the monitor's logical rect.
/// A monitor with an unknown extent (the portal omitted position or size) is
/// kept only as a last resort, because without geometry there is no way to
/// crop correctly and we would silently return the wrong pixels.
///
/// A window whose centre is off every monitor is [`ComputerError::WindowNotFound`];
/// no streams at all is [`ComputerError::DisplayUnavailable`], because that is
/// a compositor state rather than a caller mistake.
pub(crate) fn plan_source(
    streams: &[StreamInfo],
    window: WindowBounds,
) -> Result<SourcePlan, ComputerError> {
    if streams.is_empty() {
        return Err(ComputerError::DisplayUnavailable(
            "the XDG ScreenCast portal returned no streams; the compositor offered nothing to record"
                .into(),
        ));
    }

    // The portal only hands back a window stream when the user actually picked
    // a window, so this is the strongest signal available.
    if let Some(index) = streams.iter().position(|s| s.kind == SourceKind::Window) {
        return Ok(SourcePlan::Window { index });
    }

    let centre = window_centre(window);
    let mut unknown_extent: Option<SourcePlan> = None;

    for (index, stream) in streams.iter().enumerate() {
        if stream.kind != SourceKind::Monitor {
            continue;
        }
        let (Some(position), Some(size)) = (stream.position, stream.size) else {
            // Unknown geometry: usable only if nothing better turns up, and
            // then the frame is taken whole.
            unknown_extent.get_or_insert(SourcePlan::Monitor {
                index,
                crop: CropRect {
                    x: 0,
                    y: 0,
                    width: window.width.max(1),
                    height: window.height.max(1),
                },
            });
            continue;
        };
        if !rect_contains((position.0, position.1, size.0, size.1), centre) {
            continue;
        }
        let crop = crop_to_stream(window, (position.0, position.1), (size.0, size.1))?;
        return Ok(SourcePlan::Monitor { index, crop });
    }

    unknown_extent.ok_or_else(|| {
        ComputerError::WindowNotFound(format!(
            "no monitor returned by the XDG ScreenCast portal contains the window at ({}, {}) {}x{}",
            window.x, window.y, window.width, window.height
        ))
    })
}

/// Window centre in logical screen points.
fn window_centre(window: WindowBounds) -> (i32, i32) {
    (
        window.x + (window.width / 2) as i32,
        window.y + (window.height / 2) as i32,
    )
}

/// True when `(ox, oy, w, h)` contains the point.
///
/// The far edge is exclusive, so a monitor never claims a window that is
/// really on the next one. A zero-sized rect contains nothing; callers treat
/// that as "extent unknown" rather than "empty desktop".
fn rect_contains(rect: (i32, i32, i32, i32), point: (i32, i32)) -> bool {
    let (ox, oy, w, h) = rect;
    if w <= 0 || h <= 0 {
        return false;
    }
    let (px, py) = point;
    px >= ox && px < ox + w && py >= oy && py < oy + h
}

// ---------------------------------------------------------------------------
// Pure: crop / remap math
// ---------------------------------------------------------------------------

/// Map a window rect in absolute logical points into a monitor-local crop,
/// clamped to that monitor.
///
/// A window hanging off the edge crops to the part that is actually on the
/// monitor instead of producing an out-of-range rectangle. A window entirely
/// outside the monitor is [`ComputerError::WindowNotFound`]: there is no crop
/// that would contain it, and returning an empty one would hand the caller a
/// blank image that looks like a real capture.
pub(crate) fn crop_to_stream(
    window: WindowBounds,
    origin: (i32, i32),
    size: (i32, i32),
) -> Result<CropRect, ComputerError> {
    let left = window.x - origin.0;
    let top = window.y - origin.1;
    let right = left + window.width as i32;
    let bottom = top + window.height as i32;

    let left = left.clamp(0, size.0);
    let top = top.clamp(0, size.1);
    let right = right.clamp(0, size.0);
    let bottom = bottom.clamp(0, size.1);

    if right <= left || bottom <= top {
        return Err(ComputerError::WindowNotFound(format!(
            "window at ({}, {}) {}x{} does not overlap the {}x{} monitor at ({}, {})",
            window.x, window.y, window.width, window.height, size.0, size.1, origin.0, origin.1
        )));
    }

    Ok(CropRect {
        x: left,
        y: top,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

/// How many device pixels one logical point covers in a captured frame.
///
/// Portal frames arrive at the monitor's device resolution, so a scale-2
/// monitor yields a frame twice the logical size. A zero logical or pixel
/// width folds to 1.0 rather than dividing by zero.
pub(crate) fn frame_scale(logical_width: u32, pixel_width: u32) -> f64 {
    if logical_width == 0 || pixel_width == 0 {
        return 1.0;
    }
    f64::from(pixel_width) / f64::from(logical_width)
}

/// Apply a logical-space crop to a device-pixel frame.
///
/// `crop` is in logical points and `scale` converts logical to device pixels.
/// The result is clamped to the buffer, so a compositor reporting a slightly
/// different scale cannot make us read out of bounds. `None` means the crop
/// falls outside the frame entirely.
pub(crate) fn crop_pixels(
    crop: CropRect,
    scale: f64,
    frame_width: u32,
    frame_height: u32,
) -> Option<(u32, u32, u32, u32)> {
    if frame_width == 0 || frame_height == 0 || !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let limit_w = u64::from(frame_width);
    let limit_h = u64::from(frame_height);

    let x = ((f64::from(crop.x) * scale).round().max(0.0) as u64).min(limit_w);
    let y = ((f64::from(crop.y) * scale).round().max(0.0) as u64).min(limit_h);
    let width = ((f64::from(crop.width) * scale).round().max(1.0) as u64).min(limit_w - x);
    let height = ((f64::from(crop.height) * scale).round().max(1.0) as u64).min(limit_h - y);

    if width == 0 || height == 0 {
        return None;
    }
    Some((x as u32, y as u32, width as u32, height as u32))
}

// ---------------------------------------------------------------------------
// Pure: error mapping
// ---------------------------------------------------------------------------

/// Why a portal call did not produce a usable result.
///
/// A local enum rather than `ashpd::ResponseError`, so the decision of which
/// [`ComputerError`] a failure becomes is testable without a D-Bus session.
/// The `From` impl under `feature = "portal"` is the only bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PortalFailure {
    /// The user closed or dismissed the system dialog.
    Cancelled,
    /// The dialog ended for another reason.
    Denied,
    /// No portal implementation is running.
    NoPortal,
}

/// Map a portal failure onto the shared error surface.
///
/// A cancelled dialog is [`ComputerError::PermissionRequired`]: the user was
/// asked and said no. It must never be downgraded to a fallback, or the caller
/// would believe it captured a window that was deliberately withheld.
///
/// A missing portal is [`ComputerError::Unsupported`]: there is no way to
/// capture on this session, which is precisely what `unsupported` means, and
/// it lets the tier planner fall through honestly.
pub(crate) fn map_failure(failure: &PortalFailure) -> ComputerError {
    match failure {
        PortalFailure::Cancelled => ComputerError::PermissionRequired(
            "the XDG Desktop Portal dialog was dismissed; grant Screen Sharing (or Input) for \
             this application in the system settings, then run the command again. The agent \
             cannot click that dialog for you"
                .into(),
        ),
        PortalFailure::Denied => ComputerError::PermissionRequired(
            "the XDG Desktop Portal refused the request; the ScreenCast or RemoteDesktop session \
             was declined"
                .into(),
        ),
        PortalFailure::NoPortal => ComputerError::Unsupported(
            "no XDG Desktop Portal implementation is running on this Wayland session, so the \
             ScreenCast and RemoteDesktop portals are unavailable"
                .into(),
        ),
    }
}

#[cfg(feature = "portal")]
impl From<&ashpd::desktop::request::ResponseError> for PortalFailure {
    fn from(error: &ashpd::desktop::request::ResponseError) -> Self {
        match error {
            ashpd::desktop::request::ResponseError::Cancelled => PortalFailure::Cancelled,
            ashpd::desktop::request::ResponseError::Other => PortalFailure::Denied,
        }
    }
}

/// True when this build can talk to the portals at all.
///
/// This reports compile-time capability only. It does not mean a portal is
/// running — `doctor` uses it to tell "built without portals" apart from
/// "portals refused".
pub(crate) fn compiled() -> bool {
    cfg!(feature = "portal")
}

/// True when this process is on a Wayland session, which is the only place the
/// portals apply. An X11 session keeps using XTest even when `portal` is
/// compiled in.
pub(crate) fn wayland_session() -> bool {
    std::env::var("WAYLAND_DISPLAY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some()
}

/// True when `error` is the user's decision and must not fall back.
#[cfg_attr(not(feature = "portal"), allow(dead_code))]
fn is_terminal(error: &ComputerError) -> bool {
    matches!(error, ComputerError::PermissionRequired(_))
}

// ---------------------------------------------------------------------------
// Live: capture
// ---------------------------------------------------------------------------

/// Capture one window, preferring the ScreenCast portal.
///
/// Without the `portal` feature this is just the xcap desktop crop, so the
/// trait keeps working on X11 and in a default build. With the feature, the
/// portal is tried first and a *technical* failure falls back to that crop
/// with a warning naming the cause; a cancelled dialog propagates as
/// `permission_required`.
#[cfg(feature = "portal")]
pub(crate) fn capture_window(
    target: &WindowTarget,
) -> Result<crate::types::RawCapture, ComputerError> {
    if target.bounds.width == 0 || target.bounds.height == 0 {
        return Err(ComputerError::InvalidArguments(
            "window bounds are empty".into(),
        ));
    }
    match capture_portal(target) {
        Ok(raw) => Ok(raw),
        Err(error) if is_terminal(&error) => Err(error),
        Err(error) => fallback_crop(target, &error),
    }
}

/// Capture one window by cropping the desktop image.
///
/// The no-feature path, and the fallback when the portal could not be reached.
/// Covered pixels may belong to another window, so `occluded` is set.
#[cfg(not(feature = "portal"))]
pub(crate) fn capture_window(
    target: &WindowTarget,
) -> Result<crate::types::RawCapture, ComputerError> {
    if target.bounds.width == 0 || target.bounds.height == 0 {
        return Err(ComputerError::InvalidArguments(
            "window bounds are empty".into(),
        ));
    }
    let mut raw = super::capture::crop_window(target.bounds)?;
    raw.warnings.push(
        "built without the `portal` feature: cropped the desktop image, so covered pixels may be \
         wrong"
            .into(),
    );
    Ok(raw)
}

/// Crop the desktop image to the window rect, explaining why the portal did
/// not deliver.
#[cfg(feature = "portal")]
fn fallback_crop(
    target: &WindowTarget,
    cause: &ComputerError,
) -> Result<crate::types::RawCapture, ComputerError> {
    let mut raw = super::capture::crop_window(target.bounds)?;
    raw.warnings.push(format!(
        "the XDG ScreenCast portal could not deliver a window frame ({cause}); cropped the \
         desktop image instead, so covered pixels may be wrong"
    ));
    Ok(raw)
}

/// One frame from the ScreenCast portal, reduced to the window.
///
/// The portal shows its own dialog and the user picks a window or a monitor.
/// A window needs no work; a monitor is cropped to the window rect.
#[cfg(feature = "portal")]
fn capture_portal(target: &WindowTarget) -> Result<crate::types::RawCapture, ComputerError> {
    let bounds = target.bounds;
    crate::backend::linux::runtime::block_on(pull_one_frame(bounds))?
}

/// Ask the portal for one frame of `window` and reduce it to the window.
///
/// Runs inside the shared current-thread runtime. The D-Bus half is async; the
/// PipeWire half drives its own blocking C main loop. Both are stepped on the
/// CLI thread, exactly like AT-SPI, so `DesktopBackend` stays synchronous.
#[cfg(feature = "portal")]
async fn pull_one_frame(window: WindowBounds) -> Result<crate::types::RawCapture, ComputerError> {
    use ashpd::desktop::screencast::{
        CursorMode, Screencast, SelectSourcesOptions, SourceType, StartCastOptions, Streams,
    };

    let screencast = Screencast::new().await.map_err(portal_error)?;
    let session = screencast
        .create_session(Default::default())
        .await
        .map_err(portal_error)?;

    screencast
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                // Offer both so the user can pick the window itself; we prefer
                // a window stream when the portal gives us one.
                .set_sources(SourceType::Monitor | SourceType::Window)
                .set_multiple(false)
                .set_persist_mode(ashpd::desktop::PersistMode::DoNot),
        )
        .await
        .map_err(portal_error)?
        .response()
        .await
        .map_err(portal_error)?;

    // Blocks until the user answers the dialog. No timeout is applied on
    // purpose: a human is in the loop, and giving up would turn a slow answer
    // into a spurious fallback capture.
    let streams: Streams = screencast
        .start(&session, None, StartCastOptions::default())
        .await
        .map_err(portal_error)?
        .response()
        .await
        .map_err(portal_error)?;

    let described: Vec<StreamInfo> = streams
        .streams()
        .iter()
        .map(|stream| StreamInfo {
            kind: match stream.source_type() {
                Some(SourceType::Window) => SourceKind::Window,
                // A virtual source behaves like a monitor: a full frame we
                // crop the window out of.
                _ => SourceKind::Monitor,
            },
            position: stream.position(),
            size: stream.size(),
        })
        .collect();

    let plan = plan_source(&described, window)?;
    let (index, crop) = match plan {
        SourcePlan::Window { index } => (index, None),
        SourcePlan::Monitor { index, crop } => (index, Some(crop)),
    };

    let stream = streams.streams().get(index).ok_or_else(|| {
        ComputerError::DisplayUnavailable(
            "the ScreenCast portal returned an unusable stream index".into(),
        )
    })?;
    let node_id = stream.pipe_wire_node_id();

    let fd = screencast
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(portal_error)?;

    let (frame_width, frame_height, rgba) = grab_pipewire_frame(fd, node_id)?;

    let (image, logical_width, logical_height, scale, warnings) = match crop {
        None => {
            // A window stream is already window-sized.
            let scale = frame_scale(window.width, frame_width);
            let geo = crate::image_util::window_placement(
                window.x,
                window.y,
                window.width,
                window.height,
                dpi_from_scale(scale),
            );
            let warnings =
                crate::image_util::size_mismatch_warning(frame_width, frame_height, &geo)
                    .into_iter()
                    .collect();
            (
                (frame_width, frame_height, rgba),
                window.width.max(1),
                window.height.max(1),
                scale,
                warnings,
            )
        }
        Some(crop) => {
            // `size` is in logical points; the frame is in device pixels.
            let stream_size = described[index].size.unwrap_or((
                crop.x.saturating_add(crop.width as i32),
                crop.y.saturating_add(crop.height as i32),
            ));
            let scale = frame_scale(stream_size.0.max(1) as u32, frame_width);
            let (x, y, width, height) = crop_pixels(crop, scale, frame_width, frame_height)
                .ok_or_else(|| {
                    ComputerError::ExecutionFailed(
                        "the window crop falls outside the monitor frame the portal returned"
                            .into(),
                    )
                })?;
            let warnings = vec![format!(
                "the ScreenCast portal returned a monitor, so the window rect was cropped out of \
                 it ({crop:?} in stream-local logical points at scale {scale})"
            )];
            (
                crop_image(rgba, frame_width, x, y, width, height),
                crop.width.max(1),
                crop.height.max(1),
                scale,
                warnings,
            )
        }
    };

    let image = image::RgbaImage::from_raw(image.0, image.1, image.2).ok_or_else(|| {
        ComputerError::ExecutionFailed("portal frame size does not match its pixel buffer".into())
    })?;

    Ok(crate::types::RawCapture {
        frame: crate::types::Frame::from_image(image, crate::types::normalize_scale(scale)),
        origin_x: window.x,
        origin_y: window.y,
        logical_width,
        logical_height,
        capture_source: SOURCE_PORTAL.into(),
        occluded: false,
        warnings,
    })
}

/// `(width, height, pixels)` after cropping a full-frame buffer.
#[cfg(feature = "portal")]
type CroppedImage = (u32, u32, Vec<u8>);

/// Cut a sub-rectangle out of a tightly packed RGBA buffer.
///
/// Portal frames are always 4 bytes per pixel, so a row is `width * 4` and the
/// stride can be dropped. Out-of-range requests return `None` rather than
/// panicking, and the caller turns that into the xcap fallback.
#[cfg(feature = "portal")]
fn crop_image(
    rgba: Vec<u8>,
    frame_width: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> CroppedImage {
    let row_bytes = width as usize * 4;
    let start = y as usize * frame_width as usize * 4 + x as usize * 4;
    let end = start + row_bytes * height as usize;
    if end > rgba.len() || start > end {
        return (width, height, Vec::new());
    }
    (width, height, rgba[start..end].to_vec())
}

/// 96-based DPI for a scale factor, so the shared `WindowPlacement` helper can
/// report a frame whose size disagrees with the window rect.
#[cfg(feature = "portal")]
fn dpi_from_scale(scale: f64) -> u32 {
    (scale * f64::from(96)).round().max(1.0) as u32
}

#[cfg(feature = "portal")]
fn portal_error(error: ashpd::Error) -> ComputerError {
    match error {
        ashpd::Error::Response(response) => map_failure(&PortalFailure::from(&response)),
        ashpd::Error::PortalNotFound(_) => map_failure(&PortalFailure::NoPortal),
        other => ComputerError::ExecutionFailed(format!("XDG Desktop Portal call failed: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Live: PipeWire
// ---------------------------------------------------------------------------

/// How long one portal frame may take before the capture is abandoned.
///
/// The compositor normally pushes a frame immediately, but a session that is
/// locked, or a portal that never starts the stream, would otherwise hang the
/// CLI forever.
#[cfg(feature = "portal")]
const CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Pull one RGBA frame out of a PipeWire node.
///
/// `fd` is the portal's own PipeWire remote and `node_id` the stream the portal
/// picked, so no PipeWire socket path is involved and nothing outside this
/// session is reachable.
#[cfg(feature = "portal")]
fn grab_pipewire_frame(
    fd: std::os::fd::OwnedFd,
    node_id: u32,
) -> Result<(u32, u32, Vec<u8>), ComputerError> {
    use pipewire::properties::properties;
    use pipewire::spa;
    use std::cell::RefCell;
    use std::rc::Rc;

    pipewire::init();

    let mainloop = pipewire::main_loop::MainLoopRc::new(None)
        .map_err(|error| pipewire_error("main loop", error))?;
    let context = pipewire::context::ContextRc::new(&mainloop, None)
        .map_err(|error| pipewire_error("context", error))?;
    let core = context
        .connect_fd_rc(fd, None)
        .map_err(|error| pipewire_error("connect", error))?;

    let stream = pipewire::stream::StreamRc::new(
        core.clone(),
        "computer-use-screencast",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Screen",
            // `TARGET_OBJECT` exists from pipewire 0.3.44; older keys are the
            // same string, so spell it out to avoid the version feature.
            "target.object" => node_id.to_string(),
        },
    )
    .map_err(|error| pipewire_error("stream", error))?;

    // Shared between the listener callbacks and this function. PipeWire calls
    // them on the main loop, which runs on this thread.
    struct Shared {
        format: spa::param::video::VideoInfoRaw,
        frame: Option<(u32, u32, Vec<u8>)>,
    }

    let shared = Rc::new(RefCell::new(Shared {
        format: spa::param::video::VideoInfoRaw::new(),
        frame: None,
    }));
    let on_format = Rc::clone(&shared);
    let on_process = Rc::clone(&shared);
    let quit_on_frame = Rc::clone(&mainloop);

    let _listener = stream
        .add_local_listener_with_user_data(())
        .param_changed(move |_, _, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param)
            else {
                return;
            };
            if media_type != spa::param::format::MediaType::Video
                || media_subtype != spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            let mut format = spa::param::video::VideoInfoRaw::new();
            if format.parse(param).is_err() {
                return;
            }
            on_format.borrow_mut().format = format;
        })
        .process(move |stream, _| {
            if on_process.borrow().frame.is_some() {
                return;
            }
            let mut buffer = match stream.dequeue_buffer() {
                Some(buffer) => buffer,
                None => return,
            };
            let format = on_process.borrow().format;
            let size = format.size();
            let width = size.width;
            let height = size.height;
            if width == 0 || height == 0 {
                return;
            }
            let Some(image) = read_frame(&mut buffer, width, height, format.format()) else {
                return;
            };
            on_process.borrow_mut().frame = Some(image);
            quit_on_frame.quit();
        })
        .register()
        .map_err(|error| pipewire_error("listener", error))?;

    // The portal already fixed the format, so no params are needed.
    let mut params: [&pipewire::spa::pod::Pod; 0] = [];
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pipewire::stream::StreamFlags::AUTOCONNECT
                | pipewire::stream::StreamFlags::MAP_BUFFERS
                | pipewire::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|error| pipewire_error("stream connect", error))?;

    // A weak handle: the timer source is owned by the loop, so holding a
    // strong `Rc` in its callback would keep the loop alive forever.
    let timer_weak = mainloop.downgrade();
    let timer = mainloop.loop_().add_timer(move |_| {
        if let Some(loop_) = timer_weak.upgrade() {
            loop_.quit();
        }
    });
    let _ = timer.update_timer(Some(CAPTURE_TIMEOUT), None);
    mainloop.run();

    shared.borrow_mut().frame.take().ok_or_else(|| {
        ComputerError::ExecutionFailed(
            "the ScreenCast portal produced no video frame within 5s".into(),
        )
    })
}

/// Copy one PipeWire buffer into a tightly packed RGBA image.
///
/// Only the four-byte packed formats a screen cast actually uses are handled.
/// Anything else returns `None`, which makes the capture fall back to the xcap
/// crop with a warning rather than returning wrongly-ordered pixels.
#[cfg(feature = "portal")]
fn read_frame(
    buffer: &mut pipewire::buffer::Buffer<'_>,
    width: u32,
    height: u32,
    format: pipewire::spa::param::video::VideoFormat,
) -> Option<(u32, u32, Vec<u8>)> {
    use pipewire::spa::param::video::VideoFormat;

    let bytes_per_pixel = match format {
        VideoFormat::BGRx | VideoFormat::BGRA | VideoFormat::xBGR | VideoFormat::ABGR => 4,
        VideoFormat::RGBx | VideoFormat::RGBA | VideoFormat::xRGB | VideoFormat::ARGB => 4,
        VideoFormat::RGB | VideoFormat::BGR => 3,
        _ => return None,
    };
    let swizzle = matches!(
        format,
        VideoFormat::BGRx
            | VideoFormat::BGRA
            | VideoFormat::xBGR
            | VideoFormat::ABGR
            | VideoFormat::BGR
    );

    let datas = buffer.datas_mut();
    let data = datas.first_mut()?;
    // Read the chunk header before taking the mutable data borrow.
    let (size, stride) = {
        let chunk = data.chunk();
        (
            chunk.size() as usize,
            chunk.stride().unsigned_abs() as usize,
        )
    };
    let bytes = data.data()?;
    if size == 0 || bytes.len() < size || stride == 0 {
        return None;
    }

    let row_bytes = width as usize * bytes_per_pixel;
    let rows = (size / stride).min(height as usize);
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for row in 0..rows {
        let start = row * stride;
        let end = start + row_bytes;
        if end > bytes.len() {
            return None;
        }
        let line = &bytes[start..end];
        if swizzle {
            out.reserve(line.len() / bytes_per_pixel * 4);
            for pixel in line.chunks_exact(bytes_per_pixel) {
                if bytes_per_pixel == 4 {
                    out.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 0xff]);
                } else {
                    out.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
                }
            }
        } else {
            out.extend_from_slice(line);
        }
    }
    // A short buffer would produce an image smaller than the reported size;
    // pad the alpha channel so the image is exactly width*height RGBA.
    while out.len() < width as usize * height as usize * 4 {
        out.push(0);
    }
    Some((width, height, out))
}

#[cfg(feature = "portal")]
fn pipewire_error(what: &str, error: pipewire::Error) -> ComputerError {
    ComputerError::ExecutionFailed(format!("pipewire {what}: {error}"))
}

// ---------------------------------------------------------------------------
// Live: RemoteDesktop input
// ---------------------------------------------------------------------------

/// Run the `event` tier through the RemoteDesktop portal.
///
/// `unsupported` when the feature is off, when the session is not Wayland, or
/// when the user declines the grant — never a silent fall back to a
/// cursor-moving tier, because that would move the user's real pointer after
/// they refused to grant exactly that.
#[cfg(feature = "portal")]
pub(crate) fn perform_event_tier(
    job: &crate::tier::InputJob,
) -> Result<crate::tier::InputReport, ComputerError> {
    if !wayland_session() {
        return Err(ComputerError::Unsupported(
            "the XDG RemoteDesktop portal only applies to a Wayland session; this one is X11, where \
             the XTest tier is used"
                .into(),
        ));
    }
    if job.require_background {
        return Err(ComputerError::ExecutionFailed(
            "--require-background refuses the RemoteDesktop tier because the portal moves the \
             real pointer"
                .into(),
        ));
    }

    crate::backend::linux::runtime::block_on(run_remote_desktop(&job.kind))
}

#[cfg(feature = "portal")]
async fn run_remote_desktop(
    kind: &crate::tier::InputKind,
) -> Result<crate::tier::InputReport, ComputerError> {
    use crate::tier::InputKind;
    use ashpd::desktop::{
        remote_desktop::{
            Axis, DeviceType, KeyState, NotifyKeyboardKeysymOptions,
            NotifyPointerAxisDiscreteOptions, NotifyPointerButtonOptions, RemoteDesktop,
            SelectDevicesOptions,
        },
        screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
    };

    let remote = RemoteDesktop::new().await.map_err(portal_error)?;
    let session = remote
        .create_session(Default::default())
        .await
        .map_err(portal_error)?;

    remote
        .select_devices(
            &session,
            SelectDevicesOptions::default().set_devices(DeviceType::Pointer | DeviceType::Keyboard),
        )
        .await
        .map_err(portal_error)?
        .response()
        .await
        .map_err(portal_error)?;

    // Absolute pointer motion is expressed in a stream's coordinate space, so
    // a ScreenCast source is attached to the same portal session to provide
    // one. A monitor is enough here; we never read pixels back.
    let screencast = Screencast::new().await.map_err(portal_error)?;
    screencast
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Hidden)
                .set_sources(SourceType::Monitor),
        )
        .await
        .map_err(portal_error)?
        .response()
        .await
        .map_err(portal_error)?;

    // Blocks on the user, exactly as the capture dialog does.
    let response = remote
        .start(&session, None, Default::default())
        .await
        .map_err(portal_error)?
        .response()
        .await
        .map_err(portal_error)?;

    if !response.devices().contains(DeviceType::Pointer) {
        return Err(map_failure(&PortalFailure::Denied));
    }
    let node = response
        .streams()
        .first()
        .map(|stream| stream.pipe_wire_node_id())
        .ok_or_else(|| {
            ComputerError::ExecutionFailed(
                "the RemoteDesktop portal granted input but returned no stream, so there is no \
                 coordinate space for pointer motion"
                    .into(),
            )
        })?;

    // Optional EIS. Every failure is "not offered", never a failed action.
    let mut warnings = Vec::new();
    if !try_eis(&remote, &session).await {
        warnings.push(EIS_UNAVAILABLE_NOTE.to_string());
    }

    let absolute = |x: i32, y: i32| notify_absolute(&remote, &session, node, x, y);

    match kind {
        InputKind::Move { x, y } => {
            absolute(*x, *y).await?;
        }
        InputKind::Click {
            x,
            y,
            button,
            count,
        } => {
            absolute(*x, *y).await?;
            let code = evdev_button(*button);
            for index in 0..*count {
                if index > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                remote
                    .notify_pointer_button(
                        &session,
                        code,
                        KeyState::Pressed,
                        NotifyPointerButtonOptions::default(),
                    )
                    .await
                    .map_err(portal_error)?;
                remote
                    .notify_pointer_button(
                        &session,
                        code,
                        KeyState::Released,
                        NotifyPointerButtonOptions::default(),
                    )
                    .await
                    .map_err(portal_error)?;
            }
        }
        InputKind::Scroll { x, y, dx, dy } => {
            if let (Some(x), Some(y)) = (x, y) {
                absolute(*x, *y).await?;
            }
            if *dy != 0 {
                remote
                    .notify_pointer_axis_discrete(
                        &session,
                        Axis::Vertical,
                        *dy,
                        NotifyPointerAxisDiscreteOptions::default(),
                    )
                    .await
                    .map_err(portal_error)?;
            }
            if *dx != 0 {
                remote
                    .notify_pointer_axis_discrete(
                        &session,
                        Axis::Horizontal,
                        *dx,
                        NotifyPointerAxisDiscreteOptions::default(),
                    )
                    .await
                    .map_err(portal_error)?;
            }
        }
        InputKind::Key { strokes } => {
            if !response.devices().contains(DeviceType::Keyboard) {
                return Err(map_failure(&PortalFailure::Denied));
            }
            for stroke in strokes {
                let keysym = keysym_for(&stroke.key).ok_or_else(|| {
                    ComputerError::Unsupported(format!(
                        "no X11 keysym is known for `{}`; the RemoteDesktop portal speaks keysyms, \
                         not XTest keycodes",
                        stroke.key
                    ))
                })?;
                for modifier in &stroke.modifiers {
                    let sym = keysym_for(modifier.as_str()).ok_or_else(|| {
                        ComputerError::Unsupported(format!("no keysym for modifier {modifier:?}"))
                    })?;
                    remote
                        .notify_keyboard_keysym(
                            &session,
                            sym,
                            KeyState::Pressed,
                            NotifyKeyboardKeysymOptions::default(),
                        )
                        .await
                        .map_err(portal_error)?;
                }
                remote
                    .notify_keyboard_keysym(
                        &session,
                        keysym,
                        KeyState::Pressed,
                        NotifyKeyboardKeysymOptions::default(),
                    )
                    .await
                    .map_err(portal_error)?;
                remote
                    .notify_keyboard_keysym(
                        &session,
                        keysym,
                        KeyState::Released,
                        NotifyKeyboardKeysymOptions::default(),
                    )
                    .await
                    .map_err(portal_error)?;
                for modifier in stroke.modifiers.iter().rev() {
                    let sym = keysym_for(modifier.as_str()).ok_or_else(|| {
                        ComputerError::Unsupported(format!("no keysym for modifier {modifier:?}"))
                    })?;
                    remote
                        .notify_keyboard_keysym(
                            &session,
                            sym,
                            KeyState::Released,
                            NotifyKeyboardKeysymOptions::default(),
                        )
                        .await
                        .map_err(portal_error)?;
                }
            }
        }
        InputKind::Drag { .. }
        | InputKind::MouseDown { .. }
        | InputKind::MouseUp { .. }
        | InputKind::HoldKey { .. }
        | InputKind::Type { .. } => {
            return Err(ComputerError::Unsupported(
                "the RemoteDesktop portal tier covers pointer motion, buttons, scroll and key \
                 combinations; drag, button hold, hold-key and free text typing are not sent \
                 through it"
                    .into(),
            ))
        }
    }

    Ok(crate::tier::InputReport {
        tier: TIER_NAME,
        // The portal moves the compositor's pointer, which is the user's.
        cursor_moved: true,
        focus_restored: None,
        warnings,
    })
}

/// Absolute pointer motion inside the session's coordinate space.
///
/// The portal wants logical points in the stream we attached, so the caller's
/// screen coordinates go straight through: they are already in that space.
#[cfg(feature = "portal")]
async fn notify_absolute(
    remote: &RemoteDesktopProxy,
    session: &ashpd::desktop::Session<RemoteDesktopProxy>,
    node: u32,
    x: i32,
    y: i32,
) -> Result<(), ComputerError> {
    use ashpd::desktop::remote_desktop::NotifyPointerMotionAbsoluteOptions;

    remote
        .notify_pointer_motion_absolute(
            session,
            node,
            f64::from(x),
            f64::from(y),
            NotifyPointerMotionAbsoluteOptions::default(),
        )
        .await
        .map_err(portal_error)
}

/// Linux evdev button code for a [`crate::types::MouseButton`].
///
/// `NotifyPointerButton` documents evdev codes, not X11 button numbers: left
/// is `BTN_LEFT` (0x110), not 1. Getting this wrong presses the wrong button.
#[cfg(feature = "portal")]
pub(crate) fn evdev_button(button: crate::types::MouseButton) -> i32 {
    match button {
        crate::types::MouseButton::Left => 0x110,
        crate::types::MouseButton::Right => 0x111,
        crate::types::MouseButton::Middle => 0x112,
        crate::types::MouseButton::Back => 0x115,
        crate::types::MouseButton::Forward => 0x116,
    }
}

/// X11 keysym for a canonical key name.
///
/// The RemoteDesktop portal speaks keysyms. `XK_*` values are stable ABI, and
/// printable ASCII characters are their own keysyms.
#[cfg(feature = "portal")]
pub(crate) fn keysym_for(name: &str) -> Option<i32> {
    let lower = name.trim().to_ascii_lowercase();
    let sym = match lower.as_str() {
        "return" | "enter" => 0xff0d,
        "escape" | "esc" => 0xff1b,
        "tab" => 0xff09,
        "space" => 0x0020,
        "backspace" => 0xff08,
        "delete" | "del" => 0xffff,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" | "page_up" | "pgup" => 0xff55,
        "pagedown" | "page_down" | "pgdn" => 0xff56,
        "left" | "arrowleft" => 0xff51,
        "right" | "arrowright" => 0xff53,
        "up" | "arrowup" => 0xff52,
        "down" | "arrowdown" => 0xff54,
        "insert" => 0xff63,
        "plus" => 0x02b,
        "minus" => 0x02d,
        "shift" => 0xffe1,
        "lock" => 0xffe5,
        "control" | "ctrl" => 0xffe3,
        "alt" => 0xffe9,
        "super" | "meta" | "command" => 0xffeb,
        "capslock" => 0xffe2,
        "printscreen" => 0xff61,
        "menu" => 0xff67,
        "f1" => 0xffbe,
        "f2" => 0xffbf,
        "f3" => 0xffc0,
        "f4" => 0xffc1,
        "f5" => 0xffc2,
        "f6" => 0xffc3,
        "f7" => 0xffc4,
        "f8" => 0xffc5,
        "f9" => 0xffc6,
        "f10" => 0xffc7,
        "f11" => 0xffc8,
        "f12" => 0xffc9,
        other => {
            // A single printable character is its own keysym.
            let mut chars = other.chars();
            return match (chars.next(), chars.next()) {
                (Some(one), None) => Some(one as i32),
                _ => None,
            };
        }
    };
    Some(sym)
}

// ---------------------------------------------------------------------------
// Live: optional libei (EIS) relative motion
// ---------------------------------------------------------------------------

#[cfg(feature = "portal")]
use ashpd::desktop::remote_desktop::RemoteDesktop as RemoteDesktopProxy;

/// Ask the granted session for an EIS socket and attach `reis`.
///
/// `reis` 0.7 is an incomplete crate and libei is offered by only some
/// compositors, so this is strictly additive: **every** failure — no socket, a
/// refused `ConnectToEIS`, a failed EI handshake — returns `false` and the
/// caller keeps using the portal's own absolute motion. Nothing here may panic
/// or fail the action.
///
/// Returns `true` only when the handshake actually completed, which is what
/// makes it honest to say EIS is "available where offered".
#[cfg(feature = "portal")]
async fn try_eis(
    remote: &RemoteDesktopProxy,
    session: &ashpd::desktop::Session<RemoteDesktopProxy>,
) -> bool {
    use ashpd::desktop::remote_desktop::ConnectToEISOptions;
    use reis::{ei::Context as EiContext, handshake::ContextType};

    // `ConnectToEIS` is versioned at 2. Compositors below it answer with an
    // error, which is an ordinary "not offered", not a failure.
    let Ok(fd) = remote
        .connect_to_eis(session, ConnectToEISOptions::default())
        .await
    else {
        return false;
    };
    // The EIS socket is a `SOCK_STREAM` unix socket handed over by the portal.
    let Ok(socket) = std::os::unix::net::UnixStream::from(fd) else {
        return false;
    };
    let Ok(context) = EiContext::new(socket) else {
        return false;
    };
    // `ei_handshake_blocking` polls the socket itself and is bounded by the
    // compositor; every outcome below counts as "not offered".
    reis::handshake::ei_handshake_blocking(&context, "computer-use", ContextType::Sender).is_ok()
}

/// Note explaining why relative motion is unavailable when EIS is refused.
#[cfg(feature = "portal")]
pub(crate) const EIS_UNAVAILABLE_NOTE: &str =
    "the compositor did not offer an EIS socket, so the portal's absolute pointer motion is used; \
     libei relative motion is unavailable";

/// No live EIS path when the feature is off: the note still explains why.
#[cfg(not(feature = "portal"))]
pub(crate) const EIS_UNAVAILABLE_NOTE: &str =
    "built without the `portal` feature, so the XDG RemoteDesktop and EIS input paths are off";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(x: i32, y: i32, width: u32, height: u32) -> WindowBounds {
        WindowBounds {
            x,
            y,
            width,
            height,
        }
    }

    fn monitor(position: (i32, i32), size: (i32, i32)) -> StreamInfo {
        StreamInfo {
            kind: SourceKind::Monitor,
            position: Some(position),
            size: Some(size),
        }
    }

    // -- source selection --------------------------------------------------

    #[test]
    fn a_window_stream_beats_a_containing_monitor() {
        let streams = [
            monitor((0, 0), (1920, 1080)),
            StreamInfo {
                kind: SourceKind::Window,
                position: None,
                size: Some((800, 600)),
            },
        ];
        assert_eq!(
            plan_source(&streams, bounds(100, 100, 400, 300)).unwrap(),
            SourcePlan::Window { index: 1 }
        );
    }

    #[test]
    fn a_monitor_is_cropped_when_no_window_stream_exists() {
        let streams = [monitor((1920, 0), (1920, 1080))];
        // The window straddles both monitors; its centre is on the second.
        match plan_source(&streams, bounds(1800, 100, 400, 300)).unwrap() {
            SourcePlan::Monitor { index, crop } => {
                assert_eq!(index, 0);
                assert_eq!(
                    crop,
                    CropRect {
                        x: 0,
                        y: 100,
                        width: 280,
                        height: 300
                    }
                );
            }
            other => panic!("expected a cropped monitor, got {other:?}"),
        }
    }

    #[test]
    fn a_monitor_with_unknown_extent_is_only_a_last_resort() {
        let streams = [
            monitor((5000, 5000), (100, 100)),
            StreamInfo {
                kind: SourceKind::Monitor,
                position: None,
                size: None,
            },
        ];
        match plan_source(&streams, bounds(10, 10, 200, 100)).unwrap() {
            SourcePlan::Monitor { index, crop } => {
                assert_eq!(index, 1);
                assert_eq!(
                    crop,
                    CropRect {
                        x: 0,
                        y: 0,
                        width: 200,
                        height: 100
                    }
                );
            }
            other => panic!("expected the unknown-extent monitor, got {other:?}"),
        }
    }

    #[test]
    fn no_streams_is_display_unavailable_not_window_not_found() {
        assert_eq!(
            plan_source(&[], bounds(0, 0, 10, 10)).unwrap_err().code(),
            "display_unavailable"
        );
    }

    #[test]
    fn a_window_off_every_monitor_is_window_not_found() {
        let streams = [monitor((0, 0), (800, 600))];
        assert_eq!(
            plan_source(&streams, bounds(5000, 5000, 100, 100))
                .unwrap_err()
                .code(),
            "window_not_found"
        );
    }

    #[test]
    fn a_window_stream_is_accepted_without_a_position() {
        // Compositors routinely omit position and size for window streams; that
        // must not be read as "empty stream".
        let streams = [StreamInfo {
            kind: SourceKind::Window,
            position: None,
            size: None,
        }];
        assert_eq!(
            plan_source(&streams, bounds(0, 0, 1, 1)).unwrap(),
            SourcePlan::Window { index: 0 }
        );
    }

    #[test]
    fn rect_containment_excludes_the_far_edge() {
        assert!(rect_contains((0, 0, 100, 100), (0, 0)));
        assert!(rect_contains((0, 0, 100, 100), (99, 99)));
        // The far edge belongs to the next monitor, not this one.
        assert!(!rect_contains((0, 0, 100, 100), (100, 50)));
        assert!(!rect_contains((0, 0, 0, 100), (0, 50)));
        assert!(!rect_contains((0, 0, 100, 100), (-1, 50)));
    }

    // -- crop / remap math -------------------------------------------------

    #[test]
    fn crop_translates_into_stream_local_points() {
        let crop = crop_to_stream(bounds(100, 50, 200, 100), (0, 0), (1920, 1080)).unwrap();
        assert_eq!(
            crop,
            CropRect {
                x: 100,
                y: 50,
                width: 200,
                height: 100
            }
        );
    }

    #[test]
    fn crop_accounts_for_a_monitor_origin_on_a_second_display() {
        let crop = crop_to_stream(bounds(2000, 50, 200, 100), (1920, 0), (1920, 1080)).unwrap();
        assert_eq!(
            crop,
            CropRect {
                x: 80,
                y: 50,
                width: 200,
                height: 100
            }
        );
    }

    #[test]
    fn crop_clamps_a_window_hanging_off_the_top_left() {
        let crop = crop_to_stream(bounds(-50, -20, 100, 100), (0, 0), (1920, 1080)).unwrap();
        assert_eq!(
            crop,
            CropRect {
                x: 0,
                y: 0,
                width: 50,
                height: 80
            }
        );
    }

    #[test]
    fn crop_clamps_a_window_overrunning_the_bottom_right() {
        let crop = crop_to_stream(bounds(1900, 1000, 400, 400), (0, 0), (1920, 1080)).unwrap();
        assert_eq!(
            crop,
            CropRect {
                x: 1900,
                y: 1000,
                width: 20,
                height: 80
            }
        );
    }

    #[test]
    fn crop_of_a_window_entirely_off_the_monitor_is_refused() {
        assert_eq!(
            crop_to_stream(bounds(3000, 0, 100, 100), (0, 0), (1920, 1080))
                .unwrap_err()
                .code(),
            "window_not_found"
        );
    }

    #[test]
    fn frame_scale_is_the_device_pixel_ratio() {
        assert_eq!(frame_scale(1920, 3840), 2.0);
        assert_eq!(frame_scale(1920, 1920), 1.0);
        // A zero width would divide by zero; fold to 1.0 instead.
        assert_eq!(frame_scale(0, 3840), 1.0);
        assert_eq!(frame_scale(1920, 0), 1.0);
    }

    #[test]
    fn crop_pixels_scales_into_device_pixels() {
        let crop = CropRect {
            x: 80,
            y: 50,
            width: 200,
            height: 100,
        };
        assert_eq!(
            crop_pixels(crop, 2.0, 3840, 2160),
            Some((160, 100, 400, 200))
        );
        assert_eq!(crop_pixels(crop, 1.0, 1920, 1080), Some((80, 50, 200, 100)));
    }

    #[test]
    fn crop_pixels_clamps_to_the_buffer_instead_of_overrunning() {
        let crop = CropRect {
            x: 80,
            y: 50,
            width: 200,
            height: 100,
        };
        // A frame narrower and shorter than the crop: clamped on both axes.
        assert_eq!(crop_pixels(crop, 2.0, 300, 200), Some((160, 100, 140, 100)));
        // Degenerate inputs are refused, not panicked on.
        assert_eq!(crop_pixels(crop, 2.0, 0, 200), None);
        assert_eq!(crop_pixels(crop, 0.0, 300, 200), None);
        assert_eq!(crop_pixels(crop, f64::NAN, 300, 200), None);
        assert_eq!(crop_pixels(crop, -1.0, 300, 200), None);
        // A crop entirely past the frame has no pixels left.
        assert_eq!(
            crop_pixels(
                CropRect {
                    x: 5000,
                    y: 0,
                    width: 10,
                    height: 10
                },
                1.0,
                800,
                600
            ),
            None
        );
    }

    // -- error mapping -----------------------------------------------------

    #[test]
    fn a_cancelled_dialog_is_permission_required() {
        let error = map_failure(&PortalFailure::Cancelled);
        assert_eq!(error.code(), "permission_required");
        // The message has to say the agent cannot click the dialog: that is
        // the most common reason a caller sees this.
        assert!(error.to_string().contains("cannot click"));
    }

    #[test]
    fn a_denied_dialog_is_permission_required() {
        assert_eq!(
            map_failure(&PortalFailure::Denied).code(),
            "permission_required"
        );
    }

    #[test]
    fn a_missing_portal_is_unsupported() {
        assert_eq!(map_failure(&PortalFailure::NoPortal).code(), "unsupported");
    }

    #[test]
    fn a_cancelled_dialog_is_never_downgraded_to_a_fallback() {
        // `capture_window` propagates a cancelled dialog rather than cropping
        // the desktop, because the user deliberately withheld the window.
        assert!(is_terminal(&map_failure(&PortalFailure::Cancelled)));
        assert!(is_terminal(&map_failure(&PortalFailure::Denied)));
        // A technical failure *does* fall back.
        assert!(!is_terminal(&map_failure(&PortalFailure::NoPortal)));
        assert!(!is_terminal(&ComputerError::ExecutionFailed(
            "no frame".into()
        )));
    }

    // -- capability flags --------------------------------------------------

    #[test]
    fn compiled_matches_the_feature_flag() {
        assert_eq!(compiled(), cfg!(feature = "portal"));
    }

    #[test]
    #[cfg(feature = "portal")]
    fn the_eis_note_explains_the_fallback() {
        assert!(EIS_UNAVAILABLE_NOTE.contains("EIS"));
        assert!(EIS_UNAVAILABLE_NOTE.contains("relative"));
    }

    // -- feature-gated pure helpers ---------------------------------------

    #[cfg(feature = "portal")]
    #[test]
    fn evdev_button_codes_are_not_x11_button_numbers() {
        use crate::types::MouseButton;
        assert_eq!(evdev_button(MouseButton::Left), 0x110);
        assert_eq!(evdev_button(MouseButton::Right), 0x111);
        assert_eq!(evdev_button(MouseButton::Middle), 0x112);
        assert_eq!(evdev_button(MouseButton::Back), 0x115);
        assert_eq!(evdev_button(MouseButton::Forward), 0x116);
    }

    #[cfg(feature = "portal")]
    #[test]
    fn keysyms_cover_the_names_the_agent_emits() {
        assert_eq!(keysym_for("return"), Some(0xff0d));
        assert_eq!(keysym_for("Return"), Some(0xff0d));
        assert_eq!(keysym_for("escape"), Some(0xff1b));
        assert_eq!(keysym_for("left"), Some(0xff51));
        assert_eq!(keysym_for("page_down"), Some(0xff55));
        assert_eq!(keysym_for("f5"), Some(0xffc2));
        assert_eq!(keysym_for("super"), Some(0xffeb));
        assert_eq!(keysym_for("control"), Some(0xffe3));
        // Printable ASCII is its own keysym.
        assert_eq!(keysym_for("a"), Some(i32::from('a')));
        assert_eq!(keysym_for("7"), Some(i32::from('7')));
        assert_eq!(keysym_for(""), None);
        assert_eq!(keysym_for("nosuchkey"), None);
    }

    #[cfg(feature = "portal")]
    #[test]
    fn cropping_a_buffer_takes_exactly_the_requested_pixels() {
        // 2x2 RGBA frame, distinct pixels so a wrong offset is visible.
        let rgba: Vec<u8> = vec![
            1, 1, 1, 255, 2, 2, 2, 255, //
            3, 3, 3, 255, 4, 4, 4, 255,
        ];
        let (width, height, out) = crop_image(rgba.clone(), 2, 1, 1, 1, 1);
        assert_eq!((width, height), (1, 1));
        assert_eq!(out, vec![4, 4, 4, 255]);
        // The whole frame is a valid crop too.
        let (_, _, full) = crop_image(rgba, 2, 0, 0, 2, 2);
        assert_eq!(full.len(), 16);
    }

    #[cfg(feature = "portal")]
    #[test]
    fn cropping_out_of_range_yields_an_empty_buffer_not_a_panic() {
        let rgba = vec![0u8; 16];
        let (_, _, out) = crop_image(rgba, 2, 0, 0, 8, 8);
        assert!(out.is_empty());
    }
}
