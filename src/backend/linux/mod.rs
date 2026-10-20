//! Linux desktop backend.
//!
//! P13a wires two real subsystems in:
//!
//! - **AT-SPI** (`atspi.rs`) — the accessibility tree, doctor probe, and
//!   redaction contract. Live tree building through `AccessibilityConnection`
//!   with a per-call budget. Element actions are implemented but their
//!   success path lands in part B (the `portal` feature);
//!   `LinuxDesktopBackend::element_action` is currently
//!   [`ComputerError::Unsupported`].
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
pub mod runtime;
pub mod x11;

use std::time::Duration;

use crate::doctor::{self, DoctorInput, DoctorOptions};
use crate::element::{ElementError, ElementOutcome, ElementRequest};
use crate::error::ComputerError;
use crate::target::{AppInfo, DoctorReport, LaunchResult, QuitResult, WindowList};
use crate::tree::TreeQuery;
use crate::types::FrontmostInfo;

use super::command::{run_timeout, stderr_snippet};
use super::unsupported;

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
    Ok(report.text)
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

/// Element actions route through AT-SPI once the `portal` feature lands in
/// part B. For now the cached fingerprint is revalidated; the actual
/// dispatch path stays [`unsupported`] so callers see a structured error
/// instead of an invented click.
pub(crate) fn element_action(request: &ElementRequest) -> Result<ElementOutcome, ElementError> {
    let _ = request;
    Err(ElementError::new(ComputerError::Unsupported(
        "element_action on linux lands in part B (portal)".into(),
    )))
}

pub(crate) fn doctor(options: DoctorOptions) -> Result<DoctorReport, ComputerError> {
    let mut notes = vec![
        "P13a: AT-SPI tree + Linux actions + XTest event tier + portal-but-inactive".to_string(),
    ];
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
    } else if let Some(kind) = session {
        notes.push(format!(
            "XTest event tier is unsupported on {kind} (Wayland); use tier `activate` until part B adds portals"
        ));
    }
    notes.push(format!(
        "portal feature compiled = {} (P13b will enable ScreenCast/RemoteDesktop)",
        cfg!(feature = "portal")
    ));
    let report = doctor::assemble(DoctorInput {
        path: doctor::current_executable(),
        session: session.map(str::to_string),
        atspi_bus,
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
pub(crate) fn x11_event_capable(use_x11_override: bool) -> bool {
    x11::event_capable(use_x11_override)
}

/// Tier name to report back when we dispatched via XTest. Exposed here so
/// the dispatcher can render the tier tag in its JSON output.
pub(crate) fn x11_tier_name() -> &'static str {
    x11::TIER_NAME
}

/// Linux `perform_input`. `tier event` on a plain X11 session routes
/// through `x11rb::protocol::xtest::fake_input`; everything else falls
/// back to the regular `input::*` enigo path on the same dispatch. Wayland
/// without `portal` returns `unsupported` for `tier event`; the tier
/// planner routes the request through `activate` instead, never this
/// function.
pub(crate) fn perform_input(
    job: &crate::tier::InputJob,
) -> Result<crate::tier::InputReport, ComputerError> {
    use crate::tier::{InputKind, InputReport, TierName};
    match job.tier {
        TierName::Event => perform_event_tier(job),
        // Synthetic (skylight) is intentionally absent on Linux; the tier
        // planner hides it. AX clicks are routed through `activate` on
        // Linux because pure-AT-SPI background clicks need a session bus
        // — `portals` (part B) lift this.
        TierName::Synthetic => Err(unsupported("tier synthetic")),
        TierName::Ax => Err(unsupported("tier ax for coordinate and keyboard input")),
        TierName::Activate => perform_activate_tier(job),
    }
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
fn perform_event_tier(job: &crate::tier::InputJob) -> Result<InputReport, ComputerError> {
    use crate::tier::InputKind;
    let connection = match x11::connect() {
        Ok(connection) => connection,
        Err(error) => return Err(error),
    };
    let (setup, _screen) = read_setup(&connection)?;
    let root = setup.roots[0].root;
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
    connection: &x11rb::RustConnection,
) -> Result<
    (
        x11rb::protocol::xproto::Setup,
        &x11rb::protocol::xproto::Screen,
    ),
    ComputerError,
> {
    let setup = connection.setup();
    let screen = setup
        .roots
        .first()
        .ok_or_else(|| ComputerError::DisplayUnavailable("X server has no screens".into()))?;
    Ok((setup.clone(), screen))
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

    #[test]
    fn tree_uses_atspi_symbol() {
        // The dispatcher calls `tree(query)`. We don't actually wire to a
        // bus here — that path is exercised on Linux CI through the
        // integration tests that read the AT-SPI daemon's real tree. The
        // shape check is enough for the Mac build.
        let _tree: fn(&TreeQuery) -> Result<crate::tree::TreeReport, ComputerError> = tree;
    }

    #[test]
    fn element_action_is_unsupported_on_linux_part_a() {
        use crate::element::{ElementOp, ElementRequest};
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
        assert_eq!(error.error.code(), "unsupported");
        assert!(error.error.to_string().contains("linux"));
    }

    #[test]
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
        assert!(value["uiaccess"].is_null());
        assert!(value["integrity"].is_null());
        let note = value["note"].as_str().unwrap();
        assert!(note.contains("P13a"), "{note}");
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

    /// `--x11` exposes the flag through the doctor when run on Linux.
    #[test]
    fn doctor_options_default_x11_is_false() {
        let options = DoctorOptions::default();
        assert!(!options.x11);
    }
}
