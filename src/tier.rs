//! Input tier selection. Pure: no events are posted from this module.
//!
//! Auto uses Accessibility for presses and value changes. Coordinate and
//! keyboard input uses the event tier, then `activate` when that returns
//! `execution_failed` and another session does not hold `foreground.lock`.
//! `synthetic` runs only when the caller asks for it.

use crate::error::ComputerError;
use crate::policy::SYNTHETIC_EVENT_USER_DATA;
use crate::types::{Modifier, MouseButton};

/// `kCGEventTargetUnixProcessID`.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub const FIELD_TARGET_UNIX_PROCESS_ID: u32 = 40;
/// `kCGEventSourceUserData`. The listen-only tap reads this field.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub const FIELD_EVENT_SOURCE_USER_DATA: u32 = 42;
/// `kCGMouseEventWindowUnderMousePointer`.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub const FIELD_MOUSE_WINDOW_UNDER_POINTER: u32 = 91;
/// `kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent`.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub const FIELD_MOUSE_WINDOW_THAT_CAN_HANDLE: u32 = 92;

/// SkyLight front-process symbols, in the order this crate tries them.
///
/// Live code reads this only on macOS with the `skylight` feature. Tests read it everywhere.
#[cfg_attr(not(all(feature = "skylight", target_os = "macos")), allow(dead_code))]
pub const SKYLIGHT_FRONT_SYMBOLS: [&str; 3] = [
    "SLPSSetFrontProcessWithOptions",
    "SLSSetFrontProcessWithOptions",
    "CPSSetFrontProcess",
];

/// One concrete tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierName {
    /// Accessibility action or value. Does not activate the app.
    Ax,
    /// `CGEventPostToPid`. Does not move the system cursor.
    Event,
    /// Private SkyLight front-process call, then the event tier.
    Synthetic,
    /// Activate, run foreground input, then restore the previous app.
    Activate,
}

impl TierName {
    /// Stable JSON name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ax => "ax",
            Self::Event => "event",
            Self::Synthetic => "synthetic",
            Self::Activate => "activate",
        }
    }
}

/// What the caller asked for. Absent means [`RequestedTier::Auto`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedTier {
    /// Pick from the action class.
    Auto,
    /// One named tier, with no fallback.
    Named(TierName),
}

/// Which family of action is being tiered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputClass {
    /// Press, focus, set value, select text, perform, scroll-to, or type into an element.
    ///
    /// Element actions take this path directly. Coordinate dispatch does not construct it.
    #[allow(dead_code)]
    Accessibility,
    /// Coordinate click, move, scroll, or keyboard input that is not an element value.
    Background,
}

/// What this process can post. The stub turns `event` on so fallback tests run everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// `CGEventPostToPid` exists on this backend.
    pub event: bool,
}

/// True when this binary was built with `--features skylight`.
pub fn skylight_compiled() -> bool {
    cfg!(feature = "skylight")
}

/// Parse `--tier`. Empty is auto.
pub fn parse_tier(raw: Option<&str>) -> Result<RequestedTier, ComputerError> {
    let Some(raw) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(RequestedTier::Auto);
    };
    let named = match raw {
        "ax" => TierName::Ax,
        "event" => TierName::Event,
        "synthetic" => TierName::Synthetic,
        "activate" => TierName::Activate,
        other => {
            return Err(ComputerError::InvalidArguments(format!(
                "unknown tier {other:?}; expected ax, event, synthetic, or activate"
            )))
        }
    };
    Ok(RequestedTier::Named(named))
}

/// Ordered attempts. `skylight` is the compile-time feature, passed in so tests can force it off.
pub fn plan(
    requested: RequestedTier,
    class: InputClass,
    caps: Capabilities,
    skylight: bool,
) -> Result<Vec<TierName>, ComputerError> {
    match requested {
        RequestedTier::Named(TierName::Synthetic) if !skylight => Err(ComputerError::Unsupported(
            "built without skylight feature".into(),
        )),
        RequestedTier::Named(TierName::Synthetic) => Ok(vec![TierName::Synthetic]),
        RequestedTier::Named(TierName::Event) if !caps.event => Err(ComputerError::Unsupported(
            format!("tier event is not available on {}", std::env::consts::OS),
        )),
        RequestedTier::Named(TierName::Event) => Ok(vec![TierName::Event]),
        RequestedTier::Named(TierName::Ax) if matches!(class, InputClass::Background) => {
            Err(ComputerError::Unsupported(
                "tier ax does not post coordinate or keyboard events; pass --element to use an accessibility action".into(),
            ))
        }
        RequestedTier::Named(tier) => Ok(vec![tier]),
        RequestedTier::Auto => match class {
            InputClass::Accessibility => Ok(vec![TierName::Ax]),
            InputClass::Background if caps.event => Ok(vec![TierName::Event, TierName::Activate]),
            InputClass::Background => Ok(vec![TierName::Activate]),
        },
    }
}

/// Whether an `execution_failed` from the event tier may try `activate`.
pub fn fallback_to_activate(lock_held: bool, require_background: bool) -> bool {
    !lock_held && !require_background
}

/// Screen point minus the window origin. Both are top-left logical points.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub fn window_local_point(
    screen_x: i32,
    screen_y: i32,
    origin_x: i32,
    origin_y: i32,
) -> (i32, i32) {
    (
        screen_x.saturating_sub(origin_x),
        screen_y.saturating_sub(origin_y),
    )
}

/// Integer fields stamped on a synthetic mouse or scroll event.
///
/// Field 42 carries [`SYNTHETIC_EVENT_USER_DATA`] so the P6 tap ignores the event.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub fn event_integer_fields(window_id: u32, pid: i32) -> [(u32, i64); 4] {
    [
        (FIELD_EVENT_SOURCE_USER_DATA, SYNTHETIC_EVENT_USER_DATA),
        (FIELD_TARGET_UNIX_PROCESS_ID, i64::from(pid)),
        (FIELD_MOUSE_WINDOW_UNDER_POINTER, i64::from(window_id)),
        (FIELD_MOUSE_WINDOW_THAT_CAN_HANDLE, i64::from(window_id)),
    ]
}

/// True when the cursor moved by more than one logical point.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub fn cursor_moved(before_x: f64, before_y: f64, after_x: f64, after_y: f64) -> bool {
    (after_x - before_x).abs() > 1.0 || (after_y - before_y).abs() > 1.0
}

/// First SkyLight candidate whose lookup succeeded.
#[cfg_attr(not(test), allow(dead_code))]
pub fn first_present<'a>(names: &'a [&'a str], present: &[bool]) -> Option<&'a str> {
    names
        .iter()
        .zip(present.iter())
        .find_map(|(name, found)| found.then_some(*name))
}

/// Hold longer than this is `invalid_arguments`.
pub const MAX_HOLD_MS: u64 = 30_000;

/// One key tap inside a sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStroke {
    /// Modifiers in combo order.
    pub modifiers: Vec<Modifier>,
    /// Canonical key.
    pub key: String,
}

/// Half of one click in [`click_state_sequence`].
///
/// The macOS event tier posts these pairs. Other platforms keep the helper for
/// unit tests and do not post events.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickHalf {
    /// Button down. The click state is the click number.
    Down,
    /// Button up with the same click state.
    Up,
}

/// Down/up pairs with click states 1, 2, … `count`.
///
/// A triple click is `(down, 1), (up, 1), (down, 2), (up, 2), (down, 3), (up, 3)`.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub fn click_state_sequence(count: u8) -> Vec<(ClickHalf, u8)> {
    let clicks = count.max(1);
    let mut pairs = Vec::with_capacity(usize::from(clicks) * 2);
    for state in 1..=clicks {
        pairs.push((ClickHalf::Down, state));
        pairs.push((ClickHalf::Up, state));
    }
    pairs
}

/// Pointer or keyboard work passed to [`crate::backend::DesktopBackend::perform_input`].
#[derive(Debug, Clone, PartialEq)]
pub struct InputJob {
    /// Tier to run. The caller does not ask the backend to fall back.
    pub tier: TierName,
    /// What to do.
    pub kind: InputKind,
    /// Target application, when the caller named one.
    pub app: Option<String>,
    /// Target window, when the caller named one.
    pub window: Option<String>,
    /// Skip `foreground.lock` for tier `activate`.
    pub allow_foreground: bool,
    /// Treat cursor motion as `execution_failed`.
    pub require_background: bool,
}

/// One background or foreground input.
#[derive(Debug, Clone, PartialEq)]
pub enum InputKind {
    /// Click `count` times. `count` is 1, 2, or 3.
    Click {
        /// Screen x.
        x: i32,
        /// Screen y.
        y: i32,
        /// Mouse button.
        button: MouseButton,
        /// 1 for a click, 2 for a double click, 3 for a triple click.
        count: u8,
    },
    /// Move. The event tier posts a move to the pid. `activate` moves the system cursor.
    Move {
        /// Screen x.
        x: i32,
        /// Screen y.
        y: i32,
    },
    /// Scroll. Optional `x` and `y` are the wheel position.
    Scroll {
        /// Screen x, when the caller set both coordinates.
        x: Option<i32>,
        /// Screen y.
        y: Option<i32>,
        /// Horizontal steps. Positive is right.
        dx: i32,
        /// Vertical steps. Positive is down.
        dy: i32,
    },
    /// Type Unicode text.
    Type {
        /// Text. Not written to the audit log.
        text: String,
    },
    /// Press one or more key combinations, in order.
    Key {
        /// Strokes. A space-separated sequence is more than one.
        strokes: Vec<KeyStroke>,
    },
    /// Button down at the first point, dragged events through the rest, up at the last.
    Drag {
        /// At least two screen points.
        points: Vec<(i32, i32)>,
        /// Mouse button.
        button: MouseButton,
    },
    /// Button down. A later `MouseUp`, or process exit, releases it.
    MouseDown {
        /// Screen x.
        x: i32,
        /// Screen y.
        y: i32,
        /// Mouse button.
        button: MouseButton,
    },
    /// Button up.
    MouseUp {
        /// Screen x.
        x: i32,
        /// Screen y.
        y: i32,
        /// Mouse button.
        button: MouseButton,
    },
    /// Key down, sleep `ms`, key up. `ms` above [`MAX_HOLD_MS`] is refused before this runs.
    HoldKey {
        /// Modifiers held for the whole interval.
        modifiers: Vec<Modifier>,
        /// Canonical key.
        key: String,
        /// Milliseconds to hold. Zero is a press and release with no sleep.
        ms: u64,
    },
}

/// Result of one tier attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputReport {
    /// Tier that ran.
    pub tier: &'static str,
    /// True when the system cursor moved by more than one point.
    pub cursor_moved: bool,
    /// `Some` when this tier tried to put the previous app back in front.
    pub focus_restored: Option<bool>,
    /// Non-fatal notes.
    pub warnings: Vec<String>,
}

/// Screen point for the agent cursor, when this action has one.
pub fn input_point(kind: &InputKind) -> Option<(i32, i32)> {
    match kind {
        InputKind::Click { x, y, .. }
        | InputKind::Move { x, y }
        | InputKind::MouseDown { x, y, .. }
        | InputKind::MouseUp { x, y, .. } => Some((*x, *y)),
        InputKind::Drag { points, .. } => points.last().copied(),
        InputKind::Scroll {
            x: Some(x),
            y: Some(y),
            ..
        } => Some((*x, *y)),
        InputKind::Scroll { .. }
        | InputKind::Type { .. }
        | InputKind::Key { .. }
        | InputKind::HoldKey { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy;

    #[test]
    fn auto_plan_uses_ax_for_presses_and_event_then_activate_for_points() {
        let caps = Capabilities { event: true };
        assert_eq!(
            plan(RequestedTier::Auto, InputClass::Accessibility, caps, false).unwrap(),
            vec![TierName::Ax]
        );
        assert_eq!(
            plan(RequestedTier::Auto, InputClass::Background, caps, false).unwrap(),
            vec![TierName::Event, TierName::Activate]
        );
        assert_eq!(
            plan(
                RequestedTier::Auto,
                InputClass::Background,
                Capabilities { event: false },
                false
            )
            .unwrap(),
            vec![TierName::Activate]
        );
    }

    #[test]
    fn explicit_event_without_support_is_unsupported() {
        let error = plan(
            RequestedTier::Named(TierName::Event),
            InputClass::Background,
            Capabilities { event: false },
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "unsupported");
        assert!(error.to_string().contains("tier event"));
    }

    #[test]
    fn synthetic_without_the_feature_is_unsupported_and_never_selected_automatically() {
        let error = plan(
            RequestedTier::Named(TierName::Synthetic),
            InputClass::Background,
            Capabilities { event: true },
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "unsupported");
        assert!(error.to_string().contains("built without skylight feature"));
        let auto = plan(
            RequestedTier::Auto,
            InputClass::Background,
            Capabilities { event: true },
            true,
        )
        .unwrap();
        assert!(!auto.contains(&TierName::Synthetic));
        assert_eq!(
            plan(
                RequestedTier::Named(TierName::Synthetic),
                InputClass::Background,
                Capabilities { event: true },
                true
            )
            .unwrap(),
            vec![TierName::Synthetic]
        );
    }

    #[test]
    fn event_failure_falls_back_only_when_the_lock_is_free() {
        assert!(fallback_to_activate(false, false));
        assert!(!fallback_to_activate(true, false));
        assert!(!fallback_to_activate(false, true));
    }

    #[test]
    fn window_local_point_subtracts_the_origin() {
        assert_eq!(window_local_point(200, 150, 100, 100), (100, 50));
    }

    #[test]
    fn stamped_user_data_is_the_tap_marker() {
        let fields = event_integer_fields(88, 412);
        assert_eq!(fields[0].0, 42);
        assert_eq!(fields[1].0, 40);
        assert_eq!(fields[2].0, 91);
        assert_eq!(fields[3].0, 92);
        assert_eq!(fields[2].1, 88);
        assert_eq!(fields[1].1, 412);
        assert!(policy::event_is_synthetic(fields[0].1));
        assert!(!policy::event_is_synthetic(0));
        assert_eq!(fields[0].1, SYNTHETIC_EVENT_USER_DATA);
    }

    #[test]
    fn cursor_motion_threshold_is_one_point() {
        assert!(!cursor_moved(10.0, 10.0, 11.0, 10.0));
        assert!(cursor_moved(10.0, 10.0, 11.1, 10.0));
        assert!(cursor_moved(10.0, 10.0, 10.0, 8.5));
    }

    #[test]
    fn first_present_symbol_wins_and_a_total_miss_is_none() {
        let names = SKYLIGHT_FRONT_SYMBOLS.as_slice();
        assert_eq!(
            first_present(names, &[false, true, true]),
            Some("SLSSetFrontProcessWithOptions")
        );
        assert_eq!(first_present(names, &[false, false, false]), None);
        assert_eq!(
            first_present(names, &[true, false, false]),
            Some("SLPSSetFrontProcessWithOptions")
        );
    }

    #[test]
    fn ax_is_unsupported_for_coordinate_and_keyboard_input() {
        let error = plan(
            RequestedTier::Named(TierName::Ax),
            InputClass::Background,
            Capabilities { event: true },
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "unsupported");
        assert!(error.to_string().contains("pass --element"));
        assert_eq!(
            plan(
                RequestedTier::Named(TierName::Ax),
                InputClass::Accessibility,
                Capabilities { event: true },
                false
            )
            .unwrap(),
            vec![TierName::Ax]
        );
        let drag = InputKind::Drag {
            points: vec![(1, 2), (3, 4), (8, 9)],
            button: MouseButton::Left,
        };
        assert_eq!(input_point(&drag), Some((8, 9)));
        assert_eq!(
            input_point(&InputKind::MouseDown {
                x: 5,
                y: 6,
                button: MouseButton::Right,
            }),
            Some((5, 6))
        );
        assert!(input_point(&InputKind::HoldKey {
            modifiers: vec![],
            key: "shift".into(),
            ms: 10,
        })
        .is_none());
    }

    #[test]
    fn triple_click_states_are_three_down_up_pairs() {
        assert_eq!(
            click_state_sequence(3),
            vec![
                (ClickHalf::Down, 1),
                (ClickHalf::Up, 1),
                (ClickHalf::Down, 2),
                (ClickHalf::Up, 2),
                (ClickHalf::Down, 3),
                (ClickHalf::Up, 3),
            ]
        );
        assert_eq!(
            click_state_sequence(2),
            vec![
                (ClickHalf::Down, 1),
                (ClickHalf::Up, 1),
                (ClickHalf::Down, 2),
                (ClickHalf::Up, 2),
            ]
        );
    }

    #[test]
    fn unknown_tier_name_is_invalid_arguments() {
        let error = parse_tier(Some("hid")).unwrap_err();
        assert_eq!(error.code(), "invalid_arguments");
        assert!(matches!(parse_tier(None).unwrap(), RequestedTier::Auto));
        assert_eq!(
            parse_tier(Some("activate")).unwrap(),
            RequestedTier::Named(TierName::Activate)
        );
    }
}
