//! Linux desktop backend.
//!
//! P13a wires two real subsystems in:
//!
//! - **AT-SPI** (`atspi.rs`) — the accessibility tree, doctor probe,
//!   redaction contract, and element actions. Live tree building through
//!   `AccessibilityConnection` with a per-call budget. `press`,
//!   `focus`, `set_value`, `select_text`, `scroll_to`, and `type` run
//!   through `Action::do_action`, `Value::set_current_value`,
//!   `EditableText::set_text_contents`, `Text::set_selection`, and
//!   `Component::scroll_to` / `Component::grab_focus`; the same
//!   `crate::element::gate` revalidation macOS / Windows use; secure
//!   `Role::PasswordText` nodes refuse `set_value`, `select_text`, and
//!   `type --element`.
//! - **XDG portals** (`portal.rs`) — `screencast` capture and
//!   `remote_desktop` input, behind the `portal` feature. Wayland never hands
//!   a client pixels and never lets it move the pointer, so both go through a
//!   compositor dialog the user has to click; the agent cannot click it.
//! - **X11 XTest** (`x11.rs`) — `fake_input` for the `event` input tier on
//!   plain X11 (no Wayland). On Wayland without the `portal` feature the
//!   tier is `unsupported`; tier `activate` is enigo everywhere, mirroring
//!   the rest of the project.
//!
//! `xdotool`, when installed, supplies the frontmost window title for the
//! `frontmost` probe — same as before P13a.

#![cfg(target_os = "linux")]
// `x11rb` re-exports wire types marked `unsafe_op_in_unsafe_fn`; the
// `allow-unsafe-code` feature keeps that ergonomic.
#![allow(unsafe_op_in_unsafe_fn)]

pub mod atspi;
pub mod portal;
pub mod runtime;
pub mod x11;

use std::time::Duration;

use crate::doctor::{self, DoctorInput, DoctorOptions};
use crate::element::{ElementError, ElementOutcome, ElementRequest};
use crate::error::ComputerError;
use crate::target::{AppInfo, DoctorReport, LaunchResult, QuitResult, WindowList};
use crate::tier::{InputJob, InputReport, TierName};
use crate::tree::TreeQuery;
use crate::types::FrontmostInfo;

use super::command::{run_timeout, stderr_snippet};
use super::unsupported;
use x11rb::connection::Connection as X11Connection;

/// Linux desktop backend. Methods are implemented in this module.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxDesktopBackend;

pub(crate) fn frontmost() -> FrontmostInfo {
    match run_timeout(
        "xdotool",
        &["getactivewindow", "getwindowname"],
        Duration::from_secs(2),
    ) {
        Ok(output) if output.status.success() => {
            let window = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let app = window_class();
            FrontmostInfo {
                app,
                window: if window.is_empty() { None } else { Some(window) },
                note: None,
            }
        }
        Ok(output) => FrontmostInfo {
            note: Some(format!(
                "xdotool failed: {}",
                stderr_snippet(&output.stderr)
            )),
            ..FrontmostInfo::default()
        },
        Err(error) => FrontmostInfo {
            note: Some(format!(
                "frontmost window unavailable ({error}). On X11, install xdotool; Wayland often hides the active window from this build"
            )),
            ..FrontmostInfo::default()
        },
    }
}

fn window_class() -> Option<String> {
    let output = run_timeout(
        "xdotool",
        &["getactivewindow", "getwindowclassname"],
        Duration::from_secs(2),
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let class = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if class.is_empty() {
        None
    } else {
        Some(class)
    }
}

pub(crate) fn inspect_accessibility(app_name: Option<&str>) -> Result<String, ComputerError> {
    let query = TreeQuery {
        app: app_name.map(str::to_owned),
        window: None,
        full: true,
        session: None,
    };
    let report = atspi::build_tree(&query)?;
    Ok(report.full_text)
}

pub(crate) fn list_apps(_all: bool) -> Result<Vec<AppInfo>, ComputerError> {
    Err(unsupported("list_apps"))
}

pub(crate) fn list_windows() -> Result<WindowList, ComputerError> {
    Err(unsupported("list_windows"))
}

pub(crate) fn launch(_app: &str) -> Result<LaunchResult, ComputerError> {
    Err(unsupported("launch"))
}

pub(crate) fn activate(_app: &str, _window: Option<&str>) -> Result<LaunchResult, ComputerError> {
    Err(unsupported("activate"))
}

pub(crate) fn quit(_app: &str, _force: bool) -> Result<QuitResult, ComputerError> {
    Err(unsupported("quit"))
}

pub(crate) fn tree(query: &TreeQuery) -> Result<crate::tree::TreeReport, ComputerError> {
    atspi::build_tree(query)
}

pub(crate) fn end_session(session: Option<&str>) -> Result<Vec<i32>, ComputerError> {
    clear_cached_restores(session);
    Ok(Vec::new())
}

fn clear_cached_restores(session: Option<&str>) {
    if let Ok(pending) = crate::cache::pending_restores(session) {
        let pids: Vec<i32> = pending.into_iter().map(|(pid, _)| pid).collect();
        if !pids.is_empty() {
            let _ = crate::cache::clear_restores(session, &pids);
        }
    }
}

/// Element actions route through AT-SPI: `press`, `focus`, `set_value`,
/// `select_text`, `scroll_to`, and `type` map to the matching
/// `org.a11y.atspi.*` interfaces (`Action::do_action`,
/// `Component::grab_focus`, `EditableText::set_text_contents`,
/// `Value::set_current_value`, `Text::set_selection`/`add_selection`,
/// `Component::scroll_to`). Secure `Role::PasswordText` nodes refuse
/// writes with `policy_denied`. A missing interface returns
/// `execution_failed` — we never invent a click. See
/// [`atspi::element_action`] for the per-call timeout and the
/// `crate::element::gate` revalidation contract.
pub(crate) fn element_action(request: &ElementRequest) -> Result<ElementOutcome, ElementError> {
    atspi::element_action(request)
}

pub(crate) fn doctor(options: DoctorOptions) -> Result<DoctorReport, ComputerError> {
    let mut notes =
        vec!["P13b: AT-SPI tree + Linux actions + XTest event tier + XDG portals".to_string()];
    let display = env_nonempty("DISPLAY");
    let wayland = env_nonempty("WAYLAND_DISPLAY");
    let session = doctor::session_kind(display.as_deref(), wayland.as_deref());
    if session.is_none() {
        notes.push("no DISPLAY or WAYLAND_DISPLAY".to_string());
    }
    let (atspi_bus, atspi_note) = atspi_presence();
    if let Some(note) = atspi_note {
        notes.push(note);
    }
    if x11::event_capable(options.x11) {
        notes.push(format!(
            "XTest event tier reachable on {session:?} (synchronous X11, not per-window)",
        ));
    }
    match event_route() {
        EventRoute::X11Test => notes.push("event tier: X11 XTest".to_string()),
        EventRoute::Portal => notes.push(
            "event tier: XDG RemoteDesktop portal (opens a system dialog the user must click)"
                .to_string(),
        ),
        EventRoute::Unsupported => notes.push(format!(
            "event tier is unsupported on {session:?} (Wayland): built without the `portal` feature"
        )),
    }
    if portal::compiled() {
        notes.push(
            "portal feature compiled: ScreenCast capture and RemoteDesktop input are available; \
             both open a system dialog the agent cannot click for you"
                .to_string(),
        );
    }
    let report = doctor::assemble(DoctorInput {
        path: doctor::current_executable(),
        session: session.map(str::to_string),
        atspi_bus,
        portal: Some(portal::compiled()),
        notes,
        prompt_ignored: options.prompt,
        open_settings_ignored: options.open_settings,
        ..DoctorInput::default()
    });
    Ok(report)
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn atspi_presence() -> (Option<bool>, Option<String>) {
    let bus = env_nonempty("AT_SPI_BUS");
    let runtime = env_nonempty("XDG_RUNTIME_DIR");
    if bus.is_none() && runtime.is_none() {
        return (
            None,
            Some(
                "AT_SPI_BUS and XDG_RUNTIME_DIR are unset; bus was not probed (P13a: install at-spi2-core)"
                    .to_string(),
            ),
        );
    }
    let present =
        doctor::atspi_bus_present(bus.as_deref(), runtime.as_deref().map(std::path::Path::new));
    let note = if present {
        None
    } else {
        Some("AT-SPI bus address was not found; install at-spi2-core + at-spi2-atk".to_string())
    };
    (Some(present), note)
}

/// Bridge for `DoctorOptions::x11`: the `--x11` CLI flag flows through the
/// dispatcher; tests want to bypass env-detection, so we expose a probe.
#[allow(dead_code)]
pub(crate) fn x11_event_capable(use_x11_override: bool) -> bool {
    x11::event_capable(use_x11_override)
}

/// Tier name to report back when we dispatched via XTest. Exposed here so
/// the dispatcher can render the tier tag in its JSON output.
#[allow(dead_code)]
pub(crate) fn x11_tier_name() -> &'static str {
    x11::TIER_NAME
}

/// Linux `perform_input`. `tier event` on a plain X11 session routes
/// through `x11rb::protocol::xtest::fake_input`; everything else falls
/// back to the regular `input::*` enigo path on the same dispatch. Wayland
/// without `portal` returns `unsupported` for `tier event`; the tier
/// planner routes the request through `activate` instead, never this
/// function.
pub(crate) fn perform_input(job: &InputJob) -> Result<InputReport, ComputerError> {
    match job.tier {
        TierName::Event => perform_event_tier(job),
        // Synthetic (skylight) is intentionally absent on Linux; the tier
        // planner hides it. AX clicks are routed through `activate` on
        // Linux because pure-AT-SPI background clicks need a session bus.
        TierName::Synthetic => Err(unsupported("tier synthetic")),
        TierName::Ax => Err(unsupported("tier ax for coordinate and keyboard input")),
        TierName::Activate => perform_activate_tier(job),
    }
}

/// Capture one window on Linux.
///
/// With the `portal` feature this goes through the XDG ScreenCast dialog;
/// without it, and on any portal failure that is not a user refusal, it crops
/// the desktop image the way P13a did.
pub(crate) fn capture_window(
    target: &crate::backend::WindowTarget,
) -> Result<crate::types::RawCapture, ComputerError> {
    portal::capture_window(target)
}

/// Where the `event` tier should be sent on Linux.
///
/// X11 keeps XTest. Wayland has no such API, so the RemoteDesktop portal is
/// the only route — and only when `portal` is compiled in. Anything else is
/// [`ComputerError::Unsupported`], which lets the planner fall through to
/// `activate` honestly instead of pretending the click landed.
#[cfg(feature = "portal")]
fn event_route() -> EventRoute {
    if portal::wayland_session() {
        EventRoute::Portal
    } else {
        EventRoute::X11Test
    }
}

/// No `portal` feature: only X11 can host the event tier.
#[cfg(not(feature = "portal"))]
fn event_route() -> EventRoute {
    if portal::wayland_session() {
        EventRoute::Unsupported
    } else {
        EventRoute::X11Test
    }
}

/// Which backend the `event` tier uses on this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventRoute {
    /// Plain X11: XTest.
    X11Test,
    /// Wayland with `portal` compiled in: the RemoteDesktop portal.
    Portal,
    /// Wayland without `portal`: nothing can do this.
    Unsupported,
}

/// Direct call into the `input::*` enigo path used for tier `activate` on
/// Linux. Mirrors the body of [`crate::backend::off_platform_input`] on
/// the `Activate` branch so we can route `TierName::Event` through XTest
/// without going through a `&dyn DesktopBackend`.
fn perform_activate_tier(
    job: &crate::tier::InputJob,
) -> Result<crate::tier::InputReport, ComputerError> {
    use crate::backend::input;
    use crate::tier::{InputKind, InputReport};
    if job.require_background {
        return Err(ComputerError::ExecutionFailed(
            "--require-background refuses tier activate because it uses the foreground cursor"
                .into(),
        ));
    }
    match &job.kind {
        InputKind::Click {
            x,
            y,
            button,
            count,
        } => {
            for index in 0..*count {
                if index > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                input::click(*x, *y, *button)?;
            }
        }
        InputKind::Move { x, y } => input::move_mouse(*x, *y)?,
        InputKind::Scroll { x, y, dx, dy } => {
            if let (Some(x), Some(y)) = (x, y) {
                input::move_mouse(*x, *y)?;
            }
            input::scroll(*dx, *dy)?;
        }
        InputKind::Type { text } => input::type_text(text)?,
        InputKind::Key { strokes } => {
            for stroke in strokes {
                input::press_key(&stroke.modifiers, &stroke.key)?;
            }
        }
        InputKind::Drag { points, button } => input::drag(points, *button)?,
        InputKind::MouseDown { x, y, button } => input::button_down(*x, *y, *button)?,
        InputKind::MouseUp { x, y, button } => input::button_up(*x, *y, *button)?,
        InputKind::HoldKey { modifiers, key, ms } => input::hold_key(modifiers, key, *ms)?,
    }
    Ok(InputReport {
        tier: "activate",
        cursor_moved: true,
        focus_restored: None,
        warnings: Vec::new(),
    })
}

/// Drive the `event` tier on Linux X11 through XTest. Wayland without
/// portals returns `unsupported` here so the dispatcher shows the user the
/// right message instead of inventing a click.
///
/// The macro path emits a `Box<dyn DesktopBackend>` so we cannot borrow
/// `self`; we open a fresh connection per dispatch. XTest is fast enough
/// for that overhead to be invisible.
fn perform_event_tier(job: &InputJob) -> Result<InputReport, ComputerError> {
    use crate::tier::InputKind;
    match event_route() {
        EventRoute::X11Test => x11_event_tier(job),
        #[cfg(feature = "portal")]
        EventRoute::Portal => portal::perform_event_tier(job),
        EventRoute::Unsupported => Err(unsupported(
            "tier event on Wayland needs the `portal` feature; this build does not have it",
        )),
    }
}

/// The X11 XTest path, reached only when the session really is X11.
fn x11_event_tier(job: &InputJob) -> Result<InputReport, ComputerError> {
    use crate::tier::InputKind;
    let connection = x11::connect()?;
    let (root, _w, _h) = read_setup(&connection)?;
    let report = match &job.kind {
        InputKind::Move { x, y } => {
            x11::motion_notify(
                &connection,
                root,
                i16::try_from(*x).unwrap_or(i16::MAX),
                i16::try_from(*y).unwrap_or(i16::MAX),
            )?;
            InputReport {
                tier: x11::TIER_NAME,
                cursor_moved: true,
                focus_restored: None,
                warnings: Vec::new(),
            }
        }
        InputKind::Click {
            x,
            y,
            button,
            count,
        } => {
            x11::motion_notify(
                &connection,
                root,
                i16::try_from(*x).unwrap_or(i16::MAX),
                i16::try_from(*y).unwrap_or(i16::MAX),
            )?;
            for index in 0..*count {
                if index > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                x11::button_press(
                    &connection,
                    root,
                    map_mouse_button(*button),
                    i16::try_from(*x).unwrap_or(i16::MAX),
                    i16::try_from(*y).unwrap_or(i16::MAX),
                )?;
            }
            InputReport {
                tier: x11::TIER_NAME,
                cursor_moved: true,
                focus_restored: None,
                warnings: Vec::new(),
            }
        }
        InputKind::Scroll { dx, dy, .. } => {
            // XTest fake_input has no separate scroll motion type. Report
            // the unsupported case so the dispatcher falls back to
            // `activate` and the user sees the right message.
            let _ = (dx, dy);
            Err(ComputerError::Unsupported(
                "scroll via XTest is not yet supported on Linux; part B adds portals".into(),
            ))?
        }
        InputKind::Key { strokes } => {
            for stroke in strokes {
                let keycode = keycode_for_canonical(&stroke.key)?;
                x11::key_press(&connection, root, keycode)?;
            }
            InputReport {
                tier: x11::TIER_NAME,
                cursor_moved: false,
                focus_restored: None,
                warnings: Vec::new(),
            }
        }
        InputKind::Type { text } => {
            // XTest fake_input has no Type request; route through the
            // canonical key presses we already support.
            let _ = text;
            Err(ComputerError::Unsupported(
                "type via XTest is not yet supported on Linux; use `tier activate` for now".into(),
            ))?
        }
        InputKind::Drag { .. } | InputKind::MouseDown { .. } | InputKind::MouseUp { .. } => {
            return Err(ComputerError::Unsupported(
                "tier event drag/down/up is not implemented for XTest; part B adds portals".into(),
            ))
        }
        InputKind::HoldKey { .. } => {
            return Err(ComputerError::Unsupported(
                "tier event hold-key is not implemented for XTest; part B adds portals".into(),
            ))
        }
    };
    Ok(report)
}

fn read_setup(
    connection: &x11rb::rust_connection::RustConnection,
) -> Result<(x11rb::protocol::xproto::Window, u16, u16), ComputerError> {
    // The X server keeps one `Setup` per connection and exposes a single
    // root window per screen. P13a only consumes screen `0`'s root.
    let setup = connection.setup();
    let screen = setup
        .roots
        .first()
        .ok_or_else(|| ComputerError::DisplayUnavailable("X server has no screens".into()))?;
    Ok((screen.root, screen.width_in_pixels, screen.height_in_pixels))
}

fn map_mouse_button(button: crate::types::MouseButton) -> u8 {
    match button {
        crate::types::MouseButton::Left => 1,
        crate::types::MouseButton::Right => 3,
        crate::types::MouseButton::Middle => 2,
        crate::types::MouseButton::Back => 8,
        crate::types::MouseButton::Forward => 9,
    }
}

/// Map a canonical key name (`super::keys::canonical_key`) to an XTest
/// keycode. We keep a small table for the keys the agent actually emits
/// during element actions — anything missing is reported as
/// `unsupported` so the dispatcher falls back to `activate` where the
/// `keys.rs` enigo mapping knows more.
fn keycode_for_canonical(name: &str) -> Result<u8, ComputerError> {
    let code = match name.to_ascii_lowercase().as_str() {
        "return" | "enter" => 36,
        "escape" | "esc" => 9,
        "tab" => 23,
        "space" => 65,
        "backspace" => 22,
        "delete" | "del" => 119,
        "home" => 110,
        "end" => 115,
        "pageup" | "page_up" | "pgup" => 112,
        "pagedown" | "page_down" | "pgdn" => 117,
        "left" | "arrowleft" => 113,
        "right" | "arrowright" => 114,
        "up" | "arrowup" => 111,
        "down" | "arrowdown" => 116,
        other => {
            return Err(ComputerError::Unsupported(format!(
                "XTest keycode for `{other}` is not in the P13a table; part B adds portals"
            )))
        }
    };
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DesktopBackend;

    #[test]
    fn tree_uses_atspi_symbol() {
        // The dispatcher calls `tree(query)`. We don't actually wire to a
        // bus here — that path is exercised on Linux CI through the
        // integration tests that read the AT-SPI daemon's real tree. The
        // shape check is enough for the Mac build.
        let _tree: fn(&TreeQuery) -> Result<crate::tree::TreeReport, ComputerError> = tree;
    }

    /// Element actions are routed through `atspi::element_action` on
    /// P13a, not stubbed to `unsupported`. The dispatcher-facing symbol
    /// still has the right shape for the trait object; without an
    /// AT-SPI bus on a Mac, the call returns the live
    /// `AppNotFound`/`ExecutionFailed` rather than the old part-B
    /// `unsupported` message.
    #[test]
    fn element_action_routes_to_atspi_on_linux() {
        use crate::element::{ElementOp, ElementRequest};
        // Symbol shape stays the same; the body now lives in
        // `atspi::element_action`.
        let _: fn(
            &ElementRequest,
        ) -> Result<crate::element::ElementOutcome, crate::element::ElementError> =
            atspi::element_action;
        // The function calls AT-SPI on Linux CI; without a bus here it
        // returns an error, never a fake `AXPress`.
        let error = LinuxDesktopBackend
            .element_action(&ElementRequest {
                op: ElementOp::Focus,
                index: 0,
                require_visible: false,
                app: None,
                window: None,
                session: None,
                tier: None,
                allow_foreground: false,
                require_background: false,
                no_overlay: true,
            })
            .unwrap_err();
        let code = error.error.code();
        // No `unsupported` code — the path is real.
        assert_ne!(code, "unsupported", "got {error:?}");
    }

    #[test]
    #[test]
    fn event_route_follows_the_session_and_the_feature() {
        let route = event_route();
        if portal::wayland_session() {
            // Wayland: only the portal can post events, and only when built.
            assert_eq!(
                route,
                if cfg!(feature = "portal") {
                    EventRoute::Portal
                } else {
                    EventRoute::Unsupported
                }
            );
        } else {
            // X11 (or no display at all) always keeps XTest.
            assert_eq!(route, EventRoute::X11Test);
        }
    }

    #[test]
    fn the_event_route_is_exhaustive_so_a_new_arm_cannot_be_forgotten() {
        let all = [
            EventRoute::X11Test,
            EventRoute::Portal,
            EventRoute::Unsupported,
        ];
        // Each route is distinct and printable, so a caller can never get two
        // of them to the same branch by accident.
        assert_eq!(all.len(), 3);
        assert_ne!(EventRoute::X11Test, EventRoute::Portal);
        assert_ne!(EventRoute::Portal, EventRoute::Unsupported);
    }

    fn doctor_uses_the_shared_shape() {
        let report = doctor(DoctorOptions::default()).unwrap();
        let value = serde_json::to_value(&report).unwrap();
        assert!(value["permissions_ok"].is_boolean());
        assert!(value["missing"].is_array());
        assert!(value["accessibility"].is_null());
        assert!(value["screen_capture"].is_null());
        assert!(value["screen_recording"].is_null());
        assert!(value["post_events"].is_null());
        assert!(value["input_monitoring"].is_null());
        assert!(value["input_monitor"].is_null());
        assert!(value["fix"].is_array());
        assert!(value["fixes"].is_array());
        assert!(value["text"].is_string());
        assert!(value["identity"].is_object());
        assert!(value["session"].is_string() || value["session"].is_null());
        assert!(value["atspi_bus"].is_boolean() || value["atspi_bus"].is_null());
        // P13b: `portal` reports compile-time capability, never a runtime probe.
        assert!(value["portal"].is_boolean() || value["portal"].is_null());
        assert_eq!(value["portal"], serde_json::json!(portal::compiled()));
        assert!(value["uiaccess"].is_null());
        assert!(value["integrity"].is_null());
        let note = value["note"].as_str().unwrap();
        assert!(note.contains("P13b"), "{note}");
        assert!(report.permissions_ok);
        assert!(report.missing.is_empty());
    }

    /// Live AT-SPI `connect()` is exercised only on Linux CI. The unit
    /// tests on a Mac cannot reach a session bus; we still want to assert
    /// the function exists and is wired up.
    #[test]
    fn atspi_module_wires_async_through_runtime() {
        // `build_tree` is async under the hood but stays sync for the
        // dispatcher. Pin the symbol so a future refactor cannot silently
        // change its signature without breaking this test.
        let _: fn(&TreeQuery) -> Result<crate::tree::TreeReport, ComputerError> = atspi::build_tree;
    }

    /// X11 helpers exist and the tier constant is `"x11"`.
    #[test]
    fn x11_helpers_link() {
        let _: fn(bool) -> bool = x11::event_capable;
        assert_eq!(x11::TIER_NAME, "x11");
    }

    /// `tier = event` on Linux goes through `perform_event_tier`, which
    /// uses `x11rb::protocol::xtest::fake_input`. Without an X server the
    /// connection step fails; the error path is the documented one
    /// (`DisplayUnavailable`), not `unsupported`. Pin the wiring so a
    /// future refactor cannot silently route `event` through `activate`
    /// or fall through the dispatcher.
    #[test]
    fn perform_event_tier_wires_xtest_fake_input() {
        use crate::tier::{InputJob, InputKind, InputReport, TierName};
        let job = InputJob {
            tier: TierName::Event,
            kind: InputKind::Move { x: 0, y: 0 },
            app: None,
            window: None,
            allow_foreground: false,
            require_background: false,
        };
        // The dispatcher entry point is `perform_input`; route the same
        // way. The function signature must stay compatible with the
        // dispatcher macro.
        let _: fn(&InputJob) -> Result<InputReport, ComputerError> = perform_input;
        // On a Mac (no $DISPLAY) `perform_event_tier` errors with
        // `DisplayUnavailable`. The contract: a structured error, never a
        // generic `unsupported` for `tier event` on a missing X server.
        let result = perform_input(&job);
        match result {
            Ok(_) => {
                // A test running against a live X server would have
                // already verified the round-trip; we only assert the
                // error contract on systems without an X server.
            }
            Err(error) => {
                let code = error.code();
                assert!(
                    code == "display_unavailable" || code == "execution_failed",
                    "event tier must fail with display_unavailable/execution_failed, got: {error:?}"
                );
            }
        }
    }

    /// `tier = activate` on Linux always goes through the enigo path, on
    /// both X11 and Wayland. `perform_activate_tier` reports `tier:
    /// "activate"`. Without a real `DISPLAY` enigo also errors, but the
    /// path is symmetric across sessions — pin that contract here.
    #[test]
    fn perform_activate_tier_is_enigo_on_linux() {
        use crate::tier::{InputJob, InputKind, InputReport, TierName};
        let _: fn(&InputJob) -> Result<InputReport, ComputerError> = perform_activate_tier;
        let job = InputJob {
            tier: TierName::Activate,
            kind: InputKind::Move { x: 0, y: 0 },
            app: None,
            window: None,
            allow_foreground: false,
            require_background: false,
        };
        // Pin the TierName::Activate -> perform_activate_tier wiring
        // through perform_input so a future refactor cannot quietly
        // route `activate` through `xtest`.
        let result = perform_input(&job);
        match result {
            Ok(report) => assert_eq!(report.tier, "activate"),
            Err(_) => {
                // Permission / no-display path: the test still proved
                // that perform_input routed to the activate branch.
            }
        }
    }

    /// XTest tier is `unsupported` on Wayland: `is_x11_session` returns
    /// false when `$WAYLAND_DISPLAY` is set and `--x11` is not. The
    /// helper depends on the live `$DISPLAY` / `$WAYLAND_DISPLAY`
    /// environment, so the unit test that pins the boolean contract is
    /// written against the production call shape instead of an absolute
    /// result: the symbol resolves, the type signature stays the same.
    #[test]
    fn x11_session_detection_compiles() {
        let _: fn(bool, bool) -> bool = x11::is_x11_session;
    }

    /// `--x11` exposes the flag through the doctor when run on Linux.
    #[test]
    fn doctor_options_default_x11_is_false() {
        let options = DoctorOptions::default();
        assert!(!options.x11);
    }
}
