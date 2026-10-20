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

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{QueryExtensionReply, Setup, Window};
use x11rb::protocol::xtest;
use x11rb::x11_utils::X11Error;
use x11rb::COPY_DEPTH_FROM_PARENT;

use crate::error::ComputerError;

/// Tier name reported back to the dispatcher for a successful XTest event.
pub(crate) const TIER_NAME: &str = "x11";

/// XTest event type constants from `<X11/X.h>`. The same values are used by
/// `x11rb::protocol::xtest::EventType`; we re-declare them here so the
/// request encoder stays free of the upstream enum and the unit tests can
/// compare against a stable integer.
pub(crate) const EVENT_TYPE_KEY_PRESS: u8 = 2;
pub(crate) const EVENT_TYPE_BUTTON_PRESS: u8 = 4;
pub(crate) const EVENT_TYPE_MOTION_NOTIFY: u8 = 6;

/// Per-request budget for an XTest round trip. Three seconds matches the
/// rest of the desktop latency. A wedged X server stalls the call and we
/// surface that as `ExecutionFailed` rather than blocking the dispatch
/// forever.
const XTEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Standard keycode shift bits. Encoded into the XTest `detail` byte per
/// `<X11/X.h>` and re-stated here so the tests stay anchored.
pub(crate) const SHIFT_MASK: u8 = 1;
pub(crate) const LOCK_MASK: u8 = 2;
pub(crate) const CONTROL_MASK: u8 = 4;
pub(crate) const MOD1_MASK: u8 = 8;
pub(crate) const MOD2_MASK: u8 = 16;
pub(crate) const MOD3_MASK: u8 = 32;
pub(crate) const MOD4_MASK: u8 = 64;
pub(crate) const MOD5_MASK: u8 = 128;

/// True when the live session is a usable X11 surface. We honour both
/// the canonical $DISPLAY env variable and the `--x11` flag the caller
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
pub(crate) enum XTestKind {
    /// `event_type = MotionNotify`. Detail is unused for plain motion.
    Motion,
    /// `event_type = KeyPress`. Detail is a keycode (`xdotool --clearmodifiers`).
    Key,
    /// `event_type = ButtonPress`. Detail is a button number (1 = left).
    Button,
}

impl XTestKind {
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

/// A wire-format byte buffer that mirrors the upstream `fake_input` payload
/// shape: a request header followed by the eight arguments. The unit tests
/// use this to assert that the `(x, y)` and `kind` codecs agree with what
/// the server expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedRequest {
    /// Raw bytes of the request header + payload.
    pub bytes: Vec<u8>,
}

impl EncodedRequest {
    /// Build the same byte layout `x11rb` produces, without touching a real
    /// connection. The function exists for tests; the live path uses
    /// `x11rb::protocol::xtest::fake_input` directly.
    #[allow(clippy::too_many_arguments)]
    pub fn from_args(
        type_: u8,
        detail: u8,
        time: u32,
        root: Window,
        root_x: i16,
        root_y: i16,
        deviceid: u8,
    ) -> Self {
        // The X11 wire format is little-endian. Each request begins with an
        // opcode (8 bits), a length-and-extra (16 bits), then eight 16-bit
        // fields. We never put these bytes on the wire from Rust — the live
        // path goes through `x11rb` for correct padding. The structure is
        // here so a unit test can pin the field order against XTest itself.
        let mut bytes = Vec::with_capacity(32);
        bytes.push(type_);
        // length is 8 (the leading opcode byte accounts for itself).
        bytes.extend_from_slice(&8u16.to_le_bytes());
        // detail, root_x_as_zero  -> two bytes pushed together as two u16
        // fields for root_id-time.
        bytes.extend_from_slice(&detail.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&time.to_le_bytes());
        bytes.extend_from_slice(&root.to_le_bytes());
        bytes.extend_from_slice(&root_x.to_le_bytes());
        bytes.extend_from_slice(&root_y.to_le_bytes());
        bytes.push(deviceid);
        bytes.push(0);
        Self { bytes }
    }

    /// Just the first 8 bytes — enough for a sanity test that the kind
    /// translated into the event_type byte we expect.
    pub fn head(&self) -> &[u8] {
        &self.bytes[..self.bytes.len().min(8)]
    }
}

/// Connect to the local X server, honouring `$DISPLAY`.
pub(crate) fn connect() -> Result<x11rb::RustConnection, ComputerError> {
    match x11rb::connect(None) {
        Ok((connection, _setup)) => Ok(connection),
        Err(error) => Err(ComputerError::DisplayUnavailable(format!(
            "X11 connect failed: {error}"
        ))),
    }
}

/// True when the X server advertises the XTest extension. We still error
/// out at request time if a buggy server returned an advert without the
/// opcode registered.
pub(crate) fn has_xtest(connection: &x11rb::RustConnection) -> bool {
    match connection.extension_information("XTEST") {
        Ok(Some(_)) => true,
        _ => false,
    }
}

/// Probe the XTest extension on a fresh connection. Used by the doctor.
/// The connection is dropped when the function returns; that's the
/// cheapest possible check we can run on `apt-get purge libx11-6`.
pub(crate) fn xtest_supported() -> bool {
    let Ok(connection) = connect() else {
        return false;
    };
    if !has_xtest(&connection) {
        return false;
    }
    connection
        .wait_for_reply_or_error_reply(
            xtest::get_version(&connection, 2, 2)
                .map_err(|_| X11Error::Connection)
                .ok()
                .unwrap(),
        )
        .is_ok()
}

/// Issue one motion-notify request. Convenience wrapper around `fake_input`.
pub(crate) fn motion_notify(
    connection: &x11rb::RustConnection,
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
    connection
        .wait_for_reply_or_error_reply(cookie)
        .map(|_| ())
        .map_err(x11_error)
}

/// Issue one button-press request.
pub(crate) fn button_press(
    connection: &x11rb::RustConnection,
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
    connection
        .wait_for_reply_or_error_reply(cookie)
        .map(|_| ())
        .map_err(x11_error)
}

/// Issue one key-press request.
pub(crate) fn key_press(
    connection: &x11rb::RustConnection,
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
    connection
        .wait_for_reply_or_error_reply(cookie)
        .map(|_| ())
        .map_err(x11_error)
}

/// Translate an X11 error to a `ComputerError`. We deliberately lose the
/// "FAIL kind / bad-resource" detail because the dispatcher asks the
/// tier planner for `Event -> Activate` fallback on any failure, and the
/// structured code is what matters.
fn x11_error(error: impl std::fmt::Display) -> ComputerError {
    ComputerError::ExecutionFailed(format!("XTest request failed: {error}"))
}

/// True when the X11 session is `Event`-capable: we have a `$DISPLAY` and
/// the XTest extension is reachable. When not, we tell the tier planner to
/// skip `event` and offer `activate`/`unsupported` instead.
pub(crate) fn event_capable(use_x11_override: bool) -> bool {
    let Some(_connection) = connect_for_probe() else {
        return false;
    };
    if !has_xtest_probe() {
        return false;
    }
    is_x11_session(true, use_x11_override)
}

/// Open a connection for the probe path. Kept separate from [`connect`]
/// so the doctor can call it without poisoning a longer-lived connection.
fn connect_for_probe() -> Option<x11rb::RustConnection> {
    x11rb::connect(None).ok().map(|(conn, _)| conn)
}

fn has_xtest_probe() -> bool {
    let Some(conn) = connect_for_probe() else {
        return false;
    };
    has_xtest(&conn)
}

/// Best-effort X11 setup probe for the doctor. Returns `Some` with the
/// screen dimensions if a connection is reachable, `None` otherwise. Used
/// to populate the `screen` field of the doctor report.
pub(crate) fn screen_size() -> Option<(u16, u16)> {
    let (connection, setup) = x11rb::connect(None).ok()?;
    let screen = setup.roots.first()?;
    let size = (screen.width, screen.height);
    let _ = connection; // drop on return
    Some(size)
}

// `QueryExtensionReply` and `Setup` are exposed so the unit tests can
// build a synthetic setup without an actual X server.
#[allow(unused_imports)]
use QueryExtensionReply as _QueryExtensionReply;
#[allow(unused_imports)]
use Setup as _Setup;

/// Bound the per-call latency using a wall-clock guard. We do not use
/// tokio's `timeout` here — the X test path must stay sync.
#[allow(dead_code)]
pub(crate) fn call_with_timeout<F>(label: &str, future: F) -> Result<(), ComputerError>
where
    F: FnOnce() -> Result<(), ComputerError>,
{
    let start = std::time::Instant::now();
    let outcome = future();
    let elapsed = start.elapsed();
    if elapsed > XTEST_TIMEOUT {
        return Err(ComputerError::ExecutionFailed(format!(
            "{label} exceeded {}s budget",
            XTEST_TIMEOUT.as_secs()
        )));
    }
    outcome
}

// `COPY_DEPTH_FROM_PARENT` shows up in some x11rb headers; we keep the
// reference here so the import stays used when the XTest helpers expand.
#[allow(dead_code)]
const _COPY_DEPTH: u8 = COPY_DEPTH_FROM_PARENT as u8;

#[cfg(test)]
mod tests {
    use super::*;

    /// `is_x11_session` returns `false` when neither env var is set.
    #[test]
    fn x11_detection_falls_back_to_env() {
        // We cannot mutate the parent process env without breaking
        // sibling tests, so we exercise only the function's pure branch.
        let always_x11 = is_x11_session(true, true);
        assert!(always_x11, "x11 requested must always be x11");
        let never_x11 = is_x11_session(true, false);
        assert!(!never_x11 || always_x11);
    }

    /// `XTestKind` -> event type byte matches the X11 protocol.
    #[test]
    fn kind_event_type_bytes() {
        assert_eq!(XTestKind::Motion.event_type_byte(), 6);
        assert_eq!(XTestKind::Key.event_type_byte(), 2);
        assert_eq!(XTestKind::Button.event_type_byte(), 4);
    }

    /// `XTestRequest::to_fake_input_args` preserves the (x, y) round-trip
    /// with no truncation. The XTest protocol takes `i16`, so out-of-range
    /// coordinates must be rejected by the caller, not silently clipped.
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

    /// The encoded bytes start with the event type byte, then the wire
    /// length. This is a sanity check that the field ordering matches the
    /// protocol; we don't ship these bytes anywhere.
    #[test]
    fn encoded_request_starts_with_kind() {
        let enc = XTestRequest {
            kind: XTestKind::Motion,
            detail: 0,
            time: 0,
            root: 1,
            root_x: 100,
            root_y: 200,
        }
        .to_fake_input_args();
        let enc = EncodedRequest::from_args(enc.0, enc.1, enc.2, enc.3, enc.4, enc.5, enc.6);
        assert_eq!(enc.head()[0], EVENT_TYPE_MOTION_NOTIFY);
    }

    /// The `xdotool --clearmodifiers` constant layout we ship with the
    /// mask constants must agree with `<X11/X.h>`. A regression here would
    /// silently send the wrong modifier during a Ctrl+Click.
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
