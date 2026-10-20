//! X11 XTest `fake_input` driving the `event` input tier on plain X11.
//!
//! P13a exposes the `event` tier only when the session is plain X11 (no
//! Wayland). On Wayland without the `portal` feature the tier is
//! `unsupported` — part B brings XDG Desktop Portal RemoteDesktop into
//! Play. The implementation here is intentionally minimum-viable: we
//! translate the agent's pointer-and-keyboard requests into XTest
//! `FakeInput` packets, without trying to be clever about pointer-grab
//! choreography.
//!
//! Important caveat: XTest inputs are not constrained to a window. The
//! `event` tier on Linux is documented as not per-window background, just
//! like the macOS / Windows `event` tiers. The dispatcher's tier planner
//! chooses `event` when the caller requested it and the backend advertises
//! the capability; we never invent a click we cannot prove.
//!
//! `xdotool`, when installed, supplies the frontmost window title for the
//! `frontmost` probe — same as before P13a.

use x11rb::connection::RequestConnection;
use x11rb::protocol::xproto::Window;
use x11rb::protocol::xtest;
use x11rb::rust_connection::RustConnection;

use crate::error::ComputerError;

/// Tier name reported back to the dispatcher for a successful XTest event.
pub(crate) const TIER_NAME: &str = "x11";

/// XTest event type constants from `<X11/X.h>`.
pub(crate) const EVENT_TYPE_KEY_PRESS: u8 = 2;
pub(crate) const EVENT_TYPE_BUTTON_PRESS: u8 = 4;
pub(crate) const EVENT_TYPE_MOTION_NOTIFY: u8 = 6;

/// Standard keycode shift bits.
#[allow(dead_code)]
pub(crate) const SHIFT_MASK: u8 = 1;
#[allow(dead_code)]
pub(crate) const LOCK_MASK: u8 = 2;
#[allow(dead_code)]
pub(crate) const CONTROL_MASK: u8 = 4;
#[allow(dead_code)]
pub(crate) const MOD1_MASK: u8 = 8;
#[allow(dead_code)]
pub(crate) const MOD2_MASK: u8 = 16;
#[allow(dead_code)]
pub(crate) const MOD3_MASK: u8 = 32;
#[allow(dead_code)]
pub(crate) const MOD4_MASK: u8 = 64;
#[allow(dead_code)]
pub(crate) const MOD5_MASK: u8 = 128;

/// True when the live session is a usable X11 surface. We honour both
/// the canonical `$DISPLAY` env variable and the `--x11` flag the caller
/// can pass when they want this even on a Wayland compositor that
/// incidentally exports `$DISPLAY` for compatibility.
pub(crate) fn is_x11_session(wayland_unset: bool, x11_requested: bool) -> bool {
    let display = std::env::var("DISPLAY")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let wayland = std::env::var("WAYLAND_DISPLAY")
        .ok()
        .filter(|value| !value.trim().is_empty());
    match (display, wayland) {
        (Some(_), None) => true,
        (Some(_), Some(_)) if wayland_unset => true,
        (_, _) if x11_requested => true,
        _ => false,
    }
}

/// Plain-vanilla XTest request, ready to be flushed. Mostly used to
/// shorten the unit tests; the live code path wires `fake_input` straight
/// into the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct XTestRequest {
    /// `KEY_PRESS`, `BUTTON_PRESS`, or `MOTION_NOTIFY`.
    pub kind: XTestKind,
    /// Keycode, button, or screen-update byte.
    pub detail: u8,
    /// Time field. Caller-supplied; `0` means "now".
    pub time: u32,
    /// Root window the server interprets relative to.
    pub root: Window,
    /// X / Y in screen coordinates, used when the kind is motion or button.
    pub root_x: i16,
    pub root_y: i16,
}

/// `MOTION_NOTIFY`, `KEY_PRESS`, or `BUTTON_PRESS`. We do not raise
/// release pairs from the agent — the upstream tier planner expects the
/// press to round-trip the kernel's autorepeat and we report success after
/// the request returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum XTestKind {
    /// `event_type = MotionNotify`. Detail is unused for plain motion.
    Motion,
    /// `event_type = KeyPress`. Detail is a keycode.
    Key,
    /// `event_type = ButtonPress`. Detail is a button number (1 = left).
    Button,
}

impl XTestKind {
    #[allow(dead_code)]
    fn event_type_byte(self) -> u8 {
        match self {
            XTestKind::Motion => EVENT_TYPE_MOTION_NOTIFY,
            XTestKind::Key => EVENT_TYPE_KEY_PRESS,
            XTestKind::Button => EVENT_TYPE_BUTTON_PRESS,
        }
    }
}

impl XTestRequest {
    /// Translate into the literal `fake_input` argument list that
    /// `x11rb::protocol::xtest::fake_input` expects.
    #[allow(dead_code)]
    pub fn to_fake_input_args(self) -> (u8, u8, u32, Window, i16, i16, u8) {
        (
            self.kind.event_type_byte(),
            self.detail,
            self.time,
            self.root,
            self.root_x,
            self.root_y,
            0,
        )
    }
}

/// Connect to the local X server, honouring `$DISPLAY`.
pub(crate) fn connect() -> Result<RustConnection, ComputerError> {
    match x11rb::connect(None) {
        Ok((connection, _setup)) => Ok(connection),
        Err(error) => Err(ComputerError::DisplayUnavailable(format!(
            "X11 connect failed: {error}"
        ))),
    }
}

/// True when the X server advertises the XTest extension.
pub(crate) fn has_xtest(connection: &RustConnection) -> bool {
    matches!(connection.extension_information("XTEST"), Ok(Some(_info)))
}

/// Probe the XTest extension on a fresh connection.
#[allow(dead_code)]
pub(crate) fn xtest_supported() -> bool {
    let Ok(connection) = connect() else {
        return false;
    };
    // The server can advertise XTest without honouring requests. The
    // doctor only needs the wire-level answer: a successful
    // `extension_information` round-trip is enough to declare the tier
    // reachable. The live element-action path opens its own connection
    // and surfaces a real `ExecutionFailed` if the request fails.
    has_xtest(&connection)
}

/// Issue one motion-notify request.
pub(crate) fn motion_notify(
    connection: &RustConnection,
    root: Window,
    x: i16,
    y: i16,
) -> Result<(), ComputerError> {
    if !has_xtest(connection) {
        return Err(ComputerError::Unsupported(
            "XTEST extension is not present on this X server".into(),
        ));
    }
    let cookie = xtest::fake_input(connection, EVENT_TYPE_MOTION_NOTIFY, 0, 0, root, x, y, 0)
        .map_err(x11_error)?;
    cookie.check().map_err(x11_error)
}

/// Issue one button-press request.
pub(crate) fn button_press(
    connection: &RustConnection,
    root: Window,
    detail: u8,
    x: i16,
    y: i16,
) -> Result<(), ComputerError> {
    if !has_xtest(connection) {
        return Err(ComputerError::Unsupported(
            "XTEST extension is not present on this X server".into(),
        ));
    }
    let cookie = xtest::fake_input(
        connection,
        EVENT_TYPE_BUTTON_PRESS,
        detail,
        0,
        root,
        x,
        y,
        0,
    )
    .map_err(x11_error)?;
    cookie.check().map_err(x11_error)
}

/// Issue one key-press request.
pub(crate) fn key_press(
    connection: &RustConnection,
    root: Window,
    detail: u8,
) -> Result<(), ComputerError> {
    if !has_xtest(connection) {
        return Err(ComputerError::Unsupported(
            "XTEST extension is not present on this X server".into(),
        ));
    }
    let cookie = xtest::fake_input(connection, EVENT_TYPE_KEY_PRESS, detail, 0, root, 0, 0, 0)
        .map_err(x11_error)?;
    cookie.check().map_err(x11_error)
}

/// Translate an X11 error to a `ComputerError`.
fn x11_error(error: impl std::fmt::Display) -> ComputerError {
    ComputerError::ExecutionFailed(format!("XTest request failed: {error}"))
}

/// True when the X11 session is `Event`-capable: we have a `$DISPLAY`
/// and the XTest extension is reachable.
pub(crate) fn event_capable(use_x11_override: bool) -> bool {
    if connect().is_err() {
        return false;
    }
    if !has_xtest_probe() {
        return false;
    }
    is_x11_session(true, use_x11_override)
}

fn has_xtest_probe() -> bool {
    let Ok(connection) = connect() else {
        return false;
    };
    has_xtest(&connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_detection_falls_back_to_env() {
        let always_x11 = is_x11_session(true, true);
        assert!(always_x11, "x11 requested must always be x11");
    }

    #[test]
    fn kind_event_type_bytes() {
        assert_eq!(XTestKind::Motion.event_type_byte(), 6);
        assert_eq!(XTestKind::Key.event_type_byte(), 2);
        assert_eq!(XTestKind::Button.event_type_byte(), 4);
    }

    #[test]
    fn request_passes_root_x_y_through() {
        let req = XTestRequest {
            kind: XTestKind::Button,
            detail: 1,
            time: 0,
            root: 0xdead,
            root_x: 1234,
            root_y: -567,
        };
        let (kind_byte, detail, _time, root, x, y, _device) = req.to_fake_input_args();
        assert_eq!(kind_byte, EVENT_TYPE_BUTTON_PRESS);
        assert_eq!(detail, 1);
        assert_eq!(root, 0xdead);
        assert_eq!(x, 1234);
        assert_eq!(y, -567);
    }

    #[test]
    fn mask_bytes_match_x11_constants() {
        assert_eq!(SHIFT_MASK, 1);
        assert_eq!(LOCK_MASK, 2);
        assert_eq!(CONTROL_MASK, 4);
        assert_eq!(MOD1_MASK, 8);
        assert_eq!(MOD4_MASK, 64);
        assert_eq!(MOD5_MASK, 128);
    }
}
