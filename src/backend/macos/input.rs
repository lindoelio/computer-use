//! Listen-only user-input tap and background event posting.
//!
//! The tap ignores events whose `CGEventField::EventSourceUserData` (field 42)
//! equals [`crate::policy::SYNTHETIC_EVENT_USER_DATA`]. Field 43 is
//! `kCGEventSourceUserID` and is not the marker. Event posting uses a private
//! event source stamped with that marker and `CGEventPostToPid`. Unit tests
//! never install the tap and never post.
//!
//! Tests never call [`ensure_installed`], so they never create a real event tap.

#![allow(unsafe_code)]

#[cfg(not(test))]
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::{AtomicU8, Ordering};

#[cfg(not(test))]
use crate::policy::{self, SYNTHETIC_EVENT_USER_DATA};

/// 0 = not attempted, 1 = installed, 2 = creation failed.
static STATUS: AtomicU8 = AtomicU8::new(0);
#[cfg(not(test))]
static PORT: AtomicPtr<objc2_core_foundation::CFMachPort> = AtomicPtr::new(std::ptr::null_mut());

/// `Some(true)` installed, `Some(false)` failed, `None` not attempted.
pub(crate) fn status() -> Option<bool> {
    match STATUS.load(Ordering::Acquire) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// Install the tap on a background thread. Returns the warning when it cannot.
///
/// A second call does not create another tap. Unit tests compile this and never call it.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn ensure_installed() -> Option<&'static str> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        install_outside_tests()
    }
}

#[cfg(not(test))]
fn install_outside_tests() -> Option<&'static str> {
    use std::sync::atomic::AtomicBool;
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return (status() == Some(false)).then_some("user-input pause unavailable");
    }
    let spawned = std::thread::Builder::new()
        .name("computer-use-input-tap".into())
        .spawn(install_on_this_thread)
        .is_ok();
    if !spawned {
        STATUS.store(2, Ordering::Release);
        STARTED.store(false, Ordering::Release);
        return Some("user-input pause unavailable");
    }
    for _ in 0..50 {
        if status().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    match status() {
        Some(true) => None,
        _ => Some("user-input pause unavailable"),
    }
}

#[cfg(not(test))]
fn install_on_this_thread() {
    if create_tap().is_err() {
        STATUS.store(2, Ordering::Release);
    }
}

#[cfg(not(test))]
fn create_tap() -> Result<(), ()> {
    use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRunLoop};
    use objc2_core_graphics::{
        CGEvent, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    };

    let mask = event_mask();
    // SAFETY: `tap_callback` only reads the event, ignores our marker, and returns
    // the same event pointer. `user_info` is null. Listen-only does not swallow events.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            mask,
            Some(tap_callback),
            std::ptr::null_mut(),
        )
    };
    let Some(port) = port else {
        return Err(());
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
        return Err(());
    };
    let Some(run_loop) = CFRunLoop::current() else {
        return Err(());
    };
    // SAFETY: `kCFRunLoopCommonModes` is a process-lifetime Apple constant.
    let mode = unsafe { kCFRunLoopCommonModes };
    run_loop.add_source(Some(&source), mode);
    let leaked_port = Box::leak(Box::new(port));
    let mach: &CFMachPort = leaked_port;
    PORT.store(std::ptr::from_ref(mach).cast_mut(), Ordering::Release);
    let _leaked_source = Box::leak(Box::new(source));
    STATUS.store(1, Ordering::Release);
    CFRunLoop::run();
    Ok(())
}

#[cfg(not(test))]
fn event_mask() -> u64 {
    use objc2_core_graphics::CGEventType;
    let types = [
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
        CGEventType::ScrollWheel,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::OtherMouseDragged,
    ];
    types
        .iter()
        .fold(0u64, |mask, event_type| mask | (1u64 << event_type.0))
}

#[cfg(not(test))]
unsafe extern "C-unwind" fn tap_callback(
    _proxy: objc2_core_graphics::CGEventTapProxy,
    event_type: objc2_core_graphics::CGEventType,
    event: std::ptr::NonNull<objc2_core_graphics::CGEvent>,
    _user_info: *mut std::ffi::c_void,
) -> *mut objc2_core_graphics::CGEvent {
    use objc2_core_graphics::{CGEvent, CGEventField, CGEventType};
    if event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
    {
        let port = PORT.load(Ordering::Acquire);
        if !port.is_null() {
            // SAFETY: `port` is the leaked mach port created for this tap and never freed.
            unsafe { CGEvent::tap_enable(&*port, true) };
        }
        return event.as_ptr();
    }
    // SAFETY: `event` is the non-null event the system passed to this callback.
    let user_data = CGEvent::integer_value_field(
        Some(unsafe { event.as_ref() }),
        CGEventField::EventSourceUserData,
    );
    if user_data != SYNTHETIC_EVENT_USER_DATA {
        let _ = policy::set_paused(true);
    }
    event.as_ptr()
}

use crate::error::ComputerError;
use crate::tier::{InputJob, InputReport};
#[cfg(not(test))]
use crate::tier::{InputKind, TierName};
#[cfg(not(test))]
use crate::types::Modifier;
use crate::types::MouseButton;

/// Run one tier. The test build returns an error and posts nothing.
pub(crate) fn perform_input(job: &InputJob) -> Result<InputReport, ComputerError> {
    #[cfg(test)]
    {
        let _ = job;
        Err(ComputerError::ExecutionFailed(
            "event posting is disabled in tests".into(),
        ))
    }
    #[cfg(not(test))]
    {
        perform_input_live(job)
    }
}

/// Post Unicode text to `pid`. The test build posts nothing.
pub(crate) fn post_text_to_pid(pid: i32, text: &str) -> Result<(), ComputerError> {
    #[cfg(test)]
    {
        let _ = (pid, text);
        Err(ComputerError::ExecutionFailed(
            "event posting is disabled in tests".into(),
        ))
    }
    #[cfg(not(test))]
    {
        let target = EventTarget {
            window_id: 0,
            pid,
            origin_x: 0,
            origin_y: 0,
        };
        let source = private_source()?;
        post_text(&source, &target, text)
    }
}

#[derive(Clone, Copy)]
enum PointerPhase {
    Down,
    Up,
    /// Mouse-dragged. P8 posts this. The unit test constructs it.
    #[allow(dead_code)]
    Drag,
    Move,
}

/// Event type for a button and phase. `Drag` is the mouse-dragged event P8 will post.
fn pointer_event(
    button: MouseButton,
    phase: PointerPhase,
) -> (
    objc2_core_graphics::CGEventType,
    objc2_core_graphics::CGMouseButton,
) {
    use objc2_core_graphics::{CGEventType, CGMouseButton};
    match (button, phase) {
        (MouseButton::Left, PointerPhase::Down) => {
            (CGEventType::LeftMouseDown, CGMouseButton::Left)
        }
        (MouseButton::Left, PointerPhase::Up) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
        (MouseButton::Left, PointerPhase::Drag) => {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        }
        (MouseButton::Left, PointerPhase::Move) => (CGEventType::MouseMoved, CGMouseButton::Left),
        (MouseButton::Right, PointerPhase::Down) => {
            (CGEventType::RightMouseDown, CGMouseButton::Right)
        }
        (MouseButton::Right, PointerPhase::Up) => (CGEventType::RightMouseUp, CGMouseButton::Right),
        (MouseButton::Right, PointerPhase::Drag) => {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        }
        (MouseButton::Right, PointerPhase::Move) => (CGEventType::MouseMoved, CGMouseButton::Right),
        (MouseButton::Middle, PointerPhase::Down) => {
            (CGEventType::OtherMouseDown, CGMouseButton::Center)
        }
        (MouseButton::Middle, PointerPhase::Up) => {
            (CGEventType::OtherMouseUp, CGMouseButton::Center)
        }
        (MouseButton::Middle, PointerPhase::Drag) => {
            (CGEventType::OtherMouseDragged, CGMouseButton::Center)
        }
        (MouseButton::Middle, PointerPhase::Move) => {
            (CGEventType::MouseMoved, CGMouseButton::Center)
        }
        (MouseButton::Back, PointerPhase::Down) | (MouseButton::Forward, PointerPhase::Down) => {
            (CGEventType::OtherMouseDown, CGMouseButton::Center)
        }
        (MouseButton::Back, PointerPhase::Up) | (MouseButton::Forward, PointerPhase::Up) => {
            (CGEventType::OtherMouseUp, CGMouseButton::Center)
        }
        (MouseButton::Back, PointerPhase::Drag) | (MouseButton::Forward, PointerPhase::Drag) => {
            (CGEventType::OtherMouseDragged, CGMouseButton::Center)
        }
        (MouseButton::Back, PointerPhase::Move) | (MouseButton::Forward, PointerPhase::Move) => {
            (CGEventType::MouseMoved, CGMouseButton::Center)
        }
    }
}

/// `kCGMouseEventButtonNumber`. Back is 3 and forward is 4. Left and right use their event type.
fn button_number(button: MouseButton) -> Option<i64> {
    match button {
        MouseButton::Middle => Some(2),
        MouseButton::Back => Some(3),
        MouseButton::Forward => Some(4),
        MouseButton::Left | MouseButton::Right => None,
    }
}

/// macOS virtual keycode. `None` means the key is posted as Unicode.
pub(crate) fn virtual_keycode(key: &str) -> Option<u16> {
    let owned = key.to_ascii_lowercase();
    Some(match owned.as_str() {
        "a" => 0x00,
        "s" => 0x01,
        "d" => 0x02,
        "f" => 0x03,
        "h" => 0x04,
        "g" => 0x05,
        "z" => 0x06,
        "x" => 0x07,
        "c" => 0x08,
        "v" => 0x09,
        "b" => 0x0b,
        "q" => 0x0c,
        "w" => 0x0d,
        "e" => 0x0e,
        "r" => 0x0f,
        "y" => 0x10,
        "t" => 0x11,
        "1" => 0x12,
        "2" => 0x13,
        "3" => 0x14,
        "4" => 0x15,
        "6" => 0x16,
        "5" => 0x17,
        "9" => 0x19,
        "7" => 0x1a,
        "8" => 0x1c,
        "0" => 0x1d,
        "enter" | "return" => 0x24,
        "tab" => 0x30,
        "space" => 0x31,
        "backspace" => 0x33,
        "escape" | "esc" => 0x35,
        "command" | "cmd" | "meta" | "super" => 0x37,
        "shift" => 0x38,
        "alt" | "option" => 0x3a,
        "control" | "ctrl" => 0x3b,
        "f1" => 0x7a,
        "f2" => 0x78,
        "f3" => 0x63,
        "f4" => 0x76,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f8" => 0x64,
        "f9" => 0x65,
        "f10" => 0x6d,
        "f11" => 0x67,
        "f12" => 0x6f,
        "f13" => 0x69,
        "f14" => 0x6b,
        "f15" => 0x71,
        "f16" => 0x6a,
        "f17" => 0x40,
        "f18" => 0x4f,
        "f19" => 0x50,
        "f20" => 0x5a,
        "home" => 0x73,
        "pageup" => 0x74,
        "delete" => 0x75,
        "end" => 0x77,
        "pagedown" => 0x79,
        "insert" => 0x72,
        "minus" => 0x1b,
        "plus" => 0x18,
        "left" => 0x7b,
        "right" => 0x7c,
        "down" => 0x7d,
        "up" => 0x7e,
        _ => return None,
    })
}

#[cfg(test)]
mod posting_math {
    use super::*;

    #[test]
    fn keycodes_and_drag_events_do_not_post() {
        assert_eq!(virtual_keycode("enter"), Some(0x24));
        assert_eq!(virtual_keycode("left"), Some(0x7b));
        assert_eq!(virtual_keycode("a"), Some(0x00));
        assert_eq!(virtual_keycode("é"), None);
        let (kind, button) = pointer_event(MouseButton::Left, PointerPhase::Drag);
        assert_eq!(kind, objc2_core_graphics::CGEventType::LeftMouseDragged);
        assert_eq!(button, objc2_core_graphics::CGMouseButton::Left);
        let (kind, _) = pointer_event(MouseButton::Right, PointerPhase::Down);
        assert_eq!(kind, objc2_core_graphics::CGEventType::RightMouseDown);
        let (kind, _) = pointer_event(MouseButton::Middle, PointerPhase::Up);
        assert_eq!(kind, objc2_core_graphics::CGEventType::OtherMouseUp);
        let (kind, _) = pointer_event(MouseButton::Left, PointerPhase::Move);
        assert_eq!(kind, objc2_core_graphics::CGEventType::MouseMoved);
        let (kind, _) = pointer_event(MouseButton::Back, PointerPhase::Down);
        assert_eq!(kind, objc2_core_graphics::CGEventType::OtherMouseDown);
        assert_eq!(button_number(MouseButton::Back), Some(3));
        assert_eq!(button_number(MouseButton::Forward), Some(4));
        assert_eq!(virtual_keycode("f13"), Some(0x69));
        assert_eq!(virtual_keycode("f20"), Some(0x5a));
        assert_eq!(virtual_keycode("f24"), None);
        assert_eq!(virtual_keycode("plus"), Some(0x18));
        assert_eq!(virtual_keycode("minus"), Some(0x1b));
        assert_eq!(virtual_keycode("insert"), Some(0x72));
    }
}

#[cfg(not(test))]
fn perform_input_live(job: &InputJob) -> Result<InputReport, ComputerError> {
    match job.tier {
        // A scroll has a real accessibility action, so `--tier ax` runs the
        // scroll-bar path directly without posting an event.
        TierName::Ax if crate::tier::is_scroll(&job.kind) => run_ax_scroll(job),
        TierName::Ax => Err(ComputerError::Unsupported(
            "tier ax does not post coordinate or keyboard events; pass --element to use an accessibility action".into(),
        )),
        TierName::Activate => run_activate(job),
        TierName::Event => run_events(job, false),
        TierName::Synthetic => {
            #[cfg(feature = "skylight")]
            {
                run_events(job, true)
            }
            #[cfg(not(feature = "skylight"))]
            {
                let _ = job;
                Err(ComputerError::Unsupported(
                    "built without skylight feature".into(),
                ))
            }
        }
    }
}

#[cfg(not(test))]
struct EventTarget {
    window_id: u32,
    pid: i32,
    origin_x: i32,
    origin_y: i32,
}

#[cfg(not(test))]
fn run_events(job: &InputJob, synthetic: bool) -> Result<InputReport, ComputerError> {
    use objc2_core_graphics::CGPreflightPostEventAccess;
    if !CGPreflightPostEventAccess() {
        return Err(ComputerError::PermissionRequired(
            "posting events needs Accessibility permission. Enable this terminal or computer-use in System Settings > Privacy & Security > Accessibility, then retry".into(),
        ));
    }
    let target = resolve_target(job, crate::tier::input_point(&job.kind))?;
    #[cfg(feature = "skylight")]
    if synthetic {
        super::focus::claim(target.pid, target.window_id)?;
    }
    #[cfg(not(feature = "skylight"))]
    {
        let _ = synthetic;
    }
    let before = cursor_xy();
    let source = private_source()?;
    let mut warnings = Vec::new();
    if let Some(warning) = type_focus_warning(matches!(job.kind, InputKind::Type { .. })) {
        match app_has_focused_element(target.pid) {
            HasFocus::Yes => {}
            HasFocus::No => warnings.push(warning.to_string()),
            // Permission or other transient failure: do not warn (the
            // caller already has the same permission context for the rest
            // of the run).
            HasFocus::Unknown => {}
        }
    }
    dispatch_kind(&source, &target, &job.kind)?;
    let after = cursor_xy();
    let moved = match (before, after) {
        (Some((bx, by)), Some((ax, ay))) => crate::tier::cursor_moved(bx, by, ax, ay),
        _ => {
            warnings.push("cursor position was not read".into());
            false
        }
    };
    if moved && job.require_background {
        return Err(ComputerError::ExecutionFailed(
            "cursor moved and --require-background is set".into(),
        ));
    }
    // A posted scroll is not proof of a scroll. Read the bar back and say what
    // actually happened; the AX scroll-bar path is the fallback when the wheel
    // event was not delivered to a background app.
    let scroll = match &job.kind {
        InputKind::Scroll { x, y, dx, dy } => Some(verified_scroll(&target, (*x, *y), *dx, *dy)?),
        _ => None,
    };
    if let Some(outcome) = &scroll {
        warnings.extend(outcome.warnings.iter().cloned());
    }
    Ok(InputReport {
        tier: job.tier.as_str(),
        cursor_moved: moved,
        focus_restored: None,
        warnings,
        scroll,
    })
}

/// Read the scroll bar before and after the post, then fall back to `AXValue`.
///
/// This is the whole point of the fix: `CGEventPostToPid` returns without
/// error whether or not the app acted on the event, and a non-frontmost app
/// does not act on it. So the only honest report is a measured one.
#[cfg(not(test))]
fn verified_scroll(
    target: &EventTarget,
    point: (Option<i32>, Option<i32>),
    dx: i32,
    dy: i32,
) -> Result<crate::scroll::Outcome, ComputerError> {
    let app = super::ax::application(target.pid).map_err(walk_error)?;
    let axes = active_axes(dx, dy);
    // `scroll --dx 0 --dy 0` (or `--amount 0`) asks for no movement at all.
    // Posting did nothing, so there is nothing to verify and nothing to report.
    let Some(primary) = axes.first().copied() else {
        return Ok(crate::scroll::Outcome::none());
    };
    let bars = find_bars(&app, point, &axes)?;
    // The reported values are the primary axis, which is the one the caller
    // scrolled. A scroll with only one non-zero delta has exactly one axis.
    let Some(bar) = bars.get(&primary) else {
        return Err(no_scroll_area(primary));
    };
    let before = bar.value().map_err(walk_error)?;
    // The wheel event was posted by the caller. Settle, then measure again.
    std::thread::sleep(super::scroll_ax::SETTLE);
    let after = bar.value().map_err(walk_error)?;
    let steps = primary.delta(dx, dy);
    match crate::scroll::decide(before, after, steps, bar.visible_fraction()) {
        crate::scroll::Decision::Event { before, after } => {
            Ok(crate::scroll::Outcome::event(before, after))
        }
        crate::scroll::Decision::AxScrollbar { before, next } => {
            // A wheel event was posted and did not deliver, so say so.
            write_bar(
                bar,
                primary,
                before,
                next,
                crate::scroll::AxCause::WheelEventNotDelivered,
            )
        }
        crate::scroll::Decision::AtLimit { value } => {
            Ok(crate::scroll::Outcome::at_limit(primary, steps, value))
        }
        crate::scroll::Decision::Unverifiable => Err(no_scroll_area(primary)),
    }
}

/// Write `AXValue` and re-read the bar to confirm it moved.
#[cfg(not(test))]
fn write_bar(
    bar: &super::scroll_ax::ScrollBar,
    axis: crate::scroll::Axis,
    before: f64,
    next: f64,
    cause: crate::scroll::AxCause,
) -> Result<crate::scroll::Outcome, ComputerError> {
    if bar.set_value(next).is_err() {
        return Err(walk_to_computer(
            crate::scroll::AxFailure::Refused.reason(axis),
        ));
    }
    let read_back = bar.value().ok().flatten();
    crate::scroll::ax_result(axis, before, next, read_back, cause)
        .map_err(|failure| walk_to_computer(failure.reason(axis)))
}

/// `--tier ax` for a scroll: drive the scroll bar without posting an event.
#[cfg(not(test))]
fn run_ax_scroll(job: &InputJob) -> Result<InputReport, ComputerError> {
    let InputKind::Scroll { x, y, dx, dy } = job.kind else {
        return Err(ComputerError::Unsupported(
            "tier ax does not post coordinate or keyboard events".into(),
        ));
    };
    let target = resolve_target(job, crate::tier::input_point(&job.kind))?;
    let app = super::ax::application(target.pid).map_err(walk_error)?;
    let axes = active_axes(dx, dy);
    let Some(primary) = axes.first().copied() else {
        return Ok(InputReport {
            tier: job.tier.as_str(),
            cursor_moved: false,
            focus_restored: None,
            warnings: Vec::new(),
            scroll: Some(crate::scroll::Outcome::none()),
        });
    };
    let bars = find_bars(&app, (x, y), &axes)?;
    let Some(bar) = bars.get(&primary) else {
        return Err(no_scroll_area(primary));
    };
    let steps = primary.delta(dx, dy);
    let before = bar.value().map_err(walk_error)?;
    let Some(before) = before else {
        return Err(walk_to_computer(no_bar_value(primary)));
    };
    if crate::scroll::at_limit(before, steps) {
        // Nothing to do is a true answer, and it is not the AX path's doing.
        return Ok(InputReport {
            tier: job.tier.as_str(),
            cursor_moved: false,
            focus_restored: None,
            warnings: Vec::new(),
            scroll: Some(crate::scroll::Outcome::at_limit(primary, steps, before)),
        });
    }
    let next = crate::scroll::next_value(before, steps, bar.visible_fraction());
    // The caller asked for the scroll bar, so no "wheel event" warning applies.
    let outcome = write_bar(
        bar,
        primary,
        before,
        next,
        crate::scroll::AxCause::ExplicitAxTier,
    )?;
    Ok(InputReport {
        tier: job.tier.as_str(),
        cursor_moved: false,
        focus_restored: None,
        warnings: outcome.warnings.clone(),
        scroll: Some(outcome),
    })
}

/// The axes this scroll actually moves, vertical first.
#[cfg(not(test))]
fn active_axes(dx: i32, dy: i32) -> Vec<crate::scroll::Axis> {
    crate::scroll::Axis::both()
        .into_iter()
        .filter(|axis| axis.delta(dx, dy) != 0)
        .collect()
}

/// Resolve a scroll bar per active axis. A missing bar is only fatal for the
/// primary axis, which is the one the response reports.
#[cfg(not(test))]
fn find_bars(
    app: &objc2_application_services::AXUIElement,
    point: (Option<i32>, Option<i32>),
    axes: &[crate::scroll::Axis],
) -> Result<
    std::collections::BTreeMap<crate::scroll::Axis, super::scroll_ax::ScrollBar>,
    ComputerError,
> {
    let mut bars = std::collections::BTreeMap::new();
    for axis in axes {
        let found = super::scroll_ax::find(app, point, *axis).map_err(walk_error)?;
        if let Some(bar) = found {
            bars.insert(*axis, bar);
        }
    }
    Ok(bars)
}

#[cfg(not(test))]
fn no_scroll_area(axis: crate::scroll::Axis) -> ComputerError {
    walk_to_computer(format!(
        "no {} AXScrollArea under the point or in the window, so the scroll cannot be verified",
        axis.as_str()
    ))
}

#[cfg(not(test))]
fn no_bar_value(axis: crate::scroll::Axis) -> String {
    format!(
        "the {} AXScrollBar has no numeric AXValue to read",
        axis.as_str()
    )
}

#[cfg(not(test))]
fn walk_error(error: super::ax::WalkError) -> ComputerError {
    error.into_computer()
}

#[cfg(not(test))]
fn walk_to_computer(message: String) -> ComputerError {
    ComputerError::ExecutionFailed(message)
}

/// Result of asking macOS whether an app currently has an AXFocusedUIElement.
#[cfg(not(test))]
enum HasFocus {
    Yes,
    No,
    Unknown,
}

/// Read AXFocusedUIElement from the AXUIElement for `pid`. A null/missing
/// attribute means no element inside the app has keyboard focus — keystrokes
/// would be dropped on the floor. The caller turns that into a warning rather
/// than a hard failure because the user can still retype after refocusing.
#[cfg(not(test))]
fn app_has_focused_element(pid: i32) -> HasFocus {
    let element = match super::ax::application(pid) {
        Ok(element) => element,
        Err(_) => return HasFocus::Unknown,
    };
    // `copy_cf` returns `Ok(None)` both when the attribute is missing and
    // when the AX call returned a null CFType (the documented way macOS
    // signals "no focused element"), so a missing value means no focus.
    match super::ax::copy_cf(&element, "AXFocusedUIElement") {
        Ok(Some(_)) => HasFocus::Yes,
        Ok(None) => HasFocus::No,
        Err(_) => HasFocus::Unknown,
    }
}

/// Pure helper: return the warning a `type` call should emit when no element
/// has keyboard focus. Returns `None` for non-Type jobs so callers can map
/// over the result without a separate branch.
fn type_focus_warning(is_type: bool) -> Option<&'static str> {
    if is_type {
        Some("no focused element; keystrokes may be dropped — click or pass --element first")
    } else {
        None
    }
}

#[cfg(not(test))]
fn run_activate(job: &InputJob) -> Result<InputReport, ComputerError> {
    if job.require_background {
        return Err(ComputerError::ExecutionFailed(
            "--require-background refuses tier activate because it uses the foreground cursor"
                .into(),
        ));
    }
    let _lock = acquire_lock(job.allow_foreground)?;
    let track = job.app.as_deref().is_some_and(|app| !app.trim().is_empty());
    let mut restore = FrontRestore::capture(track);
    if track {
        let target = resolve_target(job, crate::tier::input_point(&job.kind))?;
        if let Err(error) = activate_pid(target.pid) {
            let _ = restore.restore();
            return Err(error);
        }
    }
    let action = run_foreground(&job.kind);
    let focus_restored = restore.restore();
    action?;
    Ok(InputReport {
        tier: "activate",
        cursor_moved: true,
        focus_restored,
        warnings: Vec::new(),
        scroll: None,
    })
}

#[cfg(not(test))]
fn acquire_lock(
    allow_foreground: bool,
) -> Result<Option<crate::foreground::ForegroundLock>, ComputerError> {
    if allow_foreground {
        return Ok(None);
    }
    let Some(path) = crate::foreground::lock_path() else {
        return Ok(None);
    };
    crate::foreground::try_lock(&path)
        .map(Some)
        .map_err(crate::foreground::LockError::into_computer)
}

#[cfg(not(test))]
struct FrontRestore {
    previous: Option<objc2::rc::Retained<objc2_app_kit::NSRunningApplication>>,
    finished: bool,
}

#[cfg(not(test))]
impl FrontRestore {
    fn capture(track: bool) -> Self {
        let previous = if track {
            objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication()
        } else {
            None
        };
        Self {
            previous,
            finished: false,
        }
    }

    fn restore(&mut self) -> Option<bool> {
        if self.finished {
            return None;
        }
        self.finished = true;
        let previous = self.previous.take()?;
        // macOS 14 deprecates this flag and ignores it. The activate tier still
        // passes it so older systems raise the previous app.
        #[allow(deprecated)]
        Some(previous.activateWithOptions(
            objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
        ))
    }
}

#[cfg(not(test))]
impl Drop for FrontRestore {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(not(test))]
fn activate_pid(pid: i32) -> Result<(), ComputerError> {
    let Some(app) =
        objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
    else {
        return Err(ComputerError::ExecutionFailed(format!(
            "pid {pid} is not a running application"
        )));
    };
    // macOS 14 deprecates this flag and ignores it. Older systems still need it
    // to raise the target over the current frontmost app.
    #[allow(deprecated)]
    if app.activateWithOptions(
        objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
    ) {
        Ok(())
    } else {
        Err(ComputerError::ExecutionFailed(format!(
            "activate failed for pid {pid}"
        )))
    }
}

#[cfg(not(test))]
fn run_foreground(kind: &InputKind) -> Result<(), ComputerError> {
    match kind {
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
                super::super::input::click(*x, *y, *button)?;
            }
        }
        InputKind::Move { x, y } => super::super::input::move_mouse(*x, *y)?,
        InputKind::Scroll { x, y, dx, dy } => {
            if let (Some(x), Some(y)) = (x, y) {
                super::super::input::move_mouse(*x, *y)?;
            }
            super::super::input::scroll(*dx, *dy)?;
        }
        InputKind::Type { text } => super::super::input::type_text(text)?,
        InputKind::Key { strokes } => {
            for stroke in strokes {
                super::super::input::press_key(&stroke.modifiers, &stroke.key)?;
            }
        }
        InputKind::Drag { points, button } => super::super::input::drag(points, *button)?,
        InputKind::MouseDown { x, y, button } => super::super::input::button_down(*x, *y, *button)?,
        InputKind::MouseUp { x, y, button } => super::super::input::button_up(*x, *y, *button)?,
        InputKind::HoldKey { modifiers, key, ms } => {
            super::super::input::hold_key(modifiers, key, *ms)?
        }
    }
    Ok(())
}

#[cfg(not(test))]
fn resolve_target(job: &InputJob, point: Option<(i32, i32)>) -> Result<EventTarget, ComputerError> {
    use crate::target::{
        filter_windows_for_pid, parse_app_selector, parse_window_selector, resolve_app,
        resolve_window,
    };
    let windows = super::apps::on_screen_windows()?;
    let app = job
        .app
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let window_name = job
        .window
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if let Some(app) = app {
        let apps = super::list_apps(true)?;
        let info = resolve_app(&apps, &parse_app_selector(app)?)?;
        let owned = filter_windows_for_pid(&windows, info.pid);
        let chosen = if let Some(window_name) = window_name {
            resolve_window(&owned, &parse_window_selector(window_name)?)?.clone()
        } else if let Some((x, y)) = point {
            owned
                .into_iter()
                .find(|window| contains(&window.bounds, x, y))
                .ok_or_else(|| {
                    ComputerError::ExecutionFailed(format!("no window of {app} contains {x},{y}"))
                })?
        } else {
            owned.into_iter().next().ok_or_else(|| {
                ComputerError::ExecutionFailed(format!("{app} has no on-screen window"))
            })?
        };
        return Ok(event_target(&chosen));
    }
    if let Some(window_name) = window_name {
        let chosen = resolve_window(&windows, &parse_window_selector(window_name)?)?;
        return Ok(event_target(chosen));
    }
    if let Some((x, y)) = point {
        let chosen = windows
            .into_iter()
            .find(|window| window.on_screen && contains(&window.bounds, x, y))
            .ok_or_else(|| {
                ComputerError::ExecutionFailed(format!("no on-screen window contains {x},{y}"))
            })?;
        return Ok(event_target(&chosen));
    }
    let chosen = windows
        .into_iter()
        .find(|window| window.on_screen)
        .ok_or_else(|| {
            ComputerError::ExecutionFailed("no on-screen window to receive the event".into())
        })?;
    Ok(event_target(&chosen))
}

#[cfg(not(test))]
fn event_target(window: &crate::target::WindowInfo) -> EventTarget {
    EventTarget {
        window_id: window.id,
        pid: window.pid,
        origin_x: window.bounds.x,
        origin_y: window.bounds.y,
    }
}

#[cfg(not(test))]
fn contains(bounds: &crate::target::WindowBounds, x: i32, y: i32) -> bool {
    let right = i64::from(bounds.x) + i64::from(bounds.width);
    let bottom = i64::from(bounds.y) + i64::from(bounds.height);
    i64::from(x) >= i64::from(bounds.x)
        && i64::from(y) >= i64::from(bounds.y)
        && i64::from(x) < right
        && i64::from(y) < bottom
}

#[cfg(not(test))]
fn private_source(
) -> Result<objc2_core_foundation::CFRetained<objc2_core_graphics::CGEventSource>, ComputerError> {
    use objc2_core_graphics::{CGEventSource, CGEventSourceStateID};
    let source = CGEventSource::new(CGEventSourceStateID::Private)
        .ok_or_else(|| ComputerError::ExecutionFailed("CGEventSourceCreate failed".into()))?;
    CGEventSource::set_user_data(Some(&source), SYNTHETIC_EVENT_USER_DATA);
    Ok(source)
}

#[cfg(not(test))]
fn dispatch_kind(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    kind: &InputKind,
) -> Result<(), ComputerError> {
    match kind {
        InputKind::Click {
            x,
            y,
            button,
            count,
        } => post_click(source, target, *x, *y, *button, *count),
        InputKind::Move { x, y } => post_move(source, target, *x, *y),
        InputKind::Scroll { x, y, dx, dy } => post_scroll(source, target, *x, *y, *dx, *dy),
        InputKind::Type { text } => post_text(source, target, text),
        InputKind::Key { strokes } => {
            for stroke in strokes {
                post_key(source, target, &stroke.modifiers, &stroke.key)?;
            }
            Ok(())
        }
        InputKind::Drag { points, button } => post_drag(source, target, points, *button),
        InputKind::MouseDown { x, y, button } => {
            post_half(source, target, *x, *y, *button, PointerPhase::Down)?;
            crate::backend::held::note_button(held_button(target, *x, *y, *button));
            Ok(())
        }
        InputKind::MouseUp { x, y, button } => {
            post_half(source, target, *x, *y, *button, PointerPhase::Up)?;
            crate::backend::held::clear_button(*button);
            Ok(())
        }
        InputKind::HoldKey { modifiers, key, ms } => post_hold(source, target, modifiers, key, *ms),
    }
}

#[cfg(not(test))]
fn post_click(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    x: i32,
    y: i32,
    button: MouseButton,
    count: u8,
) -> Result<(), ComputerError> {
    use objc2_core_graphics::{CGEvent, CGEventField};
    let point = local_point(target, x, y);
    for (half, click_state) in crate::tier::click_state_sequence(count) {
        let phase = match half {
            crate::tier::ClickHalf::Down => PointerPhase::Down,
            crate::tier::ClickHalf::Up => PointerPhase::Up,
        };
        let (kind, mouse) = pointer_event(button, phase);
        let event =
            CGEvent::new_mouse_event(Some(source), kind, point, mouse).ok_or_else(|| {
                ComputerError::ExecutionFailed("CGEventCreateMouseEvent failed".into())
            })?;
        stamp(&event, target);
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::MouseEventClickState,
            i64::from(click_state),
        );
        set_button_number(&event, button);
        CGEvent::post_to_pid(target.pid, Some(&event));
    }
    Ok(())
}

#[cfg(not(test))]
fn post_half(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    x: i32,
    y: i32,
    button: MouseButton,
    phase: PointerPhase,
) -> Result<(), ComputerError> {
    use objc2_core_graphics::CGEvent;
    let (kind, mouse) = pointer_event(button, phase);
    let event = CGEvent::new_mouse_event(Some(source), kind, local_point(target, x, y), mouse)
        .ok_or_else(|| ComputerError::ExecutionFailed("CGEventCreateMouseEvent failed".into()))?;
    stamp(&event, target);
    set_button_number(&event, button);
    CGEvent::post_to_pid(target.pid, Some(&event));
    Ok(())
}

#[cfg(not(test))]
fn post_drag(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    points: &[(i32, i32)],
    button: MouseButton,
) -> Result<(), ComputerError> {
    if points.len() < 2 {
        return Err(ComputerError::InvalidArguments(
            "drag needs at least 2 points".into(),
        ));
    }
    let first = points[0];
    post_half(source, target, first.0, first.1, button, PointerPhase::Down)?;
    for point in &points[1..] {
        std::thread::sleep(std::time::Duration::from_millis(8));
        post_half(source, target, point.0, point.1, button, PointerPhase::Drag)?;
    }
    let last = points[points.len() - 1];
    post_half(source, target, last.0, last.1, button, PointerPhase::Up)
}

#[cfg(not(test))]
fn held_button(
    target: &EventTarget,
    x: i32,
    y: i32,
    button: MouseButton,
) -> crate::backend::held::HeldButton {
    crate::backend::held::HeldButton {
        button,
        x,
        y,
        where_: crate::backend::held::HeldWhere::Event {
            pid: target.pid,
            window_id: target.window_id,
            origin_x: target.origin_x,
            origin_y: target.origin_y,
        },
    }
}

#[cfg(not(test))]
fn set_button_number(event: &objc2_core_graphics::CGEvent, button: MouseButton) {
    use objc2_core_graphics::{CGEvent, CGEventField};
    if let Some(number) = button_number(button) {
        CGEvent::set_integer_value_field(Some(event), CGEventField::MouseEventButtonNumber, number);
    }
}

#[cfg(not(test))]
fn post_move(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    x: i32,
    y: i32,
) -> Result<(), ComputerError> {
    use objc2_core_graphics::CGEvent;
    let (kind, mouse) = pointer_event(MouseButton::Left, PointerPhase::Move);
    let event = CGEvent::new_mouse_event(Some(source), kind, local_point(target, x, y), mouse)
        .ok_or_else(|| ComputerError::ExecutionFailed("CGEventCreateMouseEvent failed".into()))?;
    stamp(&event, target);
    CGEvent::post_to_pid(target.pid, Some(&event));
    Ok(())
}

#[cfg(not(test))]
fn post_scroll(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    x: Option<i32>,
    y: Option<i32>,
    dx: i32,
    dy: i32,
) -> Result<(), ComputerError> {
    use objc2_core_graphics::{CGEvent, CGScrollEventUnit};
    if dx == 0 && dy == 0 {
        return Ok(());
    }
    let event =
        CGEvent::new_scroll_wheel_event2(Some(source), CGScrollEventUnit::Line, 2, -dy, dx, 0)
            .ok_or_else(|| {
                ComputerError::ExecutionFailed("CGEventCreateScrollWheelEvent failed".into())
            })?;
    if let (Some(x), Some(y)) = (x, y) {
        CGEvent::set_location(Some(&event), local_point(target, x, y));
    }
    stamp(&event, target);
    CGEvent::post_to_pid(target.pid, Some(&event));
    Ok(())
}

#[cfg(not(test))]
fn post_text(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    text: &str,
) -> Result<(), ComputerError> {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "text must not be empty".into(),
        ));
    }
    for chunk in units.chunks(20) {
        post_unicode(source, target, chunk, true)?;
        post_unicode(source, target, chunk, false)?;
    }
    Ok(())
}

#[cfg(not(test))]
fn post_unicode(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    units: &[u16],
    key_down: bool,
) -> Result<(), ComputerError> {
    use objc2_core_graphics::CGEvent;
    let event = CGEvent::new_keyboard_event(Some(source), 0, key_down).ok_or_else(|| {
        ComputerError::ExecutionFailed("CGEventCreateKeyboardEvent failed".into())
    })?;
    stamp(&event, target);
    // SAFETY: `units` is a live UTF-16 buffer. UniChar is a u16. The length fits the API.
    unsafe {
        CGEvent::keyboard_set_unicode_string(Some(&event), units.len() as _, units.as_ptr().cast());
    }
    CGEvent::post_to_pid(target.pid, Some(&event));
    Ok(())
}

#[cfg(not(test))]
fn post_key(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    modifiers: &[Modifier],
    key: &str,
) -> Result<(), ComputerError> {
    crate::keys::macos_function_gap(key)?;
    let flags = event_flags(modifiers);
    for modifier in modifiers {
        post_keycode(
            source,
            target,
            modifier_keycode(*modifier),
            true,
            flags,
            None,
        )?;
    }
    let code = virtual_keycode(key).unwrap_or(0);
    let unicode = (key.chars().count() == 1 && virtual_keycode(key).is_none()).then_some(key);
    post_keycode(source, target, code, true, flags, unicode)?;
    post_keycode(source, target, code, false, flags, unicode)?;
    for modifier in modifiers.iter().rev() {
        post_keycode(
            source,
            target,
            modifier_keycode(*modifier),
            false,
            flags,
            None,
        )?;
    }
    Ok(())
}

#[cfg(not(test))]
fn post_hold(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    modifiers: &[Modifier],
    key: &str,
    ms: u64,
) -> Result<(), ComputerError> {
    if ms > crate::tier::MAX_HOLD_MS {
        return Err(ComputerError::InvalidArguments(format!(
            "hold_key ms must be at most {}",
            crate::tier::MAX_HOLD_MS
        )));
    }
    crate::keys::macos_function_gap(key)?;
    let flags = event_flags(modifiers);
    for modifier in modifiers {
        post_keycode(
            source,
            target,
            modifier_keycode(*modifier),
            true,
            flags,
            None,
        )?;
    }
    let code = virtual_keycode(key).unwrap_or(0);
    let unicode = (key.chars().count() == 1 && virtual_keycode(key).is_none()).then_some(key);
    post_keycode(source, target, code, true, flags, unicode)?;
    crate::backend::held::note_key(crate::backend::held::HeldKey {
        modifiers: modifiers.to_vec(),
        key: key.to_string(),
        where_: crate::backend::held::HeldWhere::Event {
            pid: target.pid,
            window_id: target.window_id,
            origin_x: target.origin_x,
            origin_y: target.origin_y,
        },
    });
    if ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    let released = (|| {
        post_keycode(source, target, code, false, flags, unicode)?;
        for modifier in modifiers.iter().rev() {
            post_keycode(
                source,
                target,
                modifier_keycode(*modifier),
                false,
                flags,
                None,
            )?;
        }
        Ok(())
    })();
    if released.is_ok() {
        crate::backend::held::clear_keys();
    }
    released
}

/// Button up for a button the event tier left down. Called from the process registry.
#[cfg(not(test))]
pub(crate) fn release_posted_button(
    pid: i32,
    window_id: u32,
    origin_x: i32,
    origin_y: i32,
    x: i32,
    y: i32,
    button: MouseButton,
) -> Result<(), ComputerError> {
    let target = EventTarget {
        window_id,
        pid,
        origin_x,
        origin_y,
    };
    let source = private_source()?;
    post_half(&source, &target, x, y, button, PointerPhase::Up)
}

/// Key up for a hold the event tier did not finish.
#[cfg(not(test))]
pub(crate) fn release_posted_key(
    pid: i32,
    window_id: u32,
    origin_x: i32,
    origin_y: i32,
    modifiers: &[Modifier],
    key: &str,
) -> Result<(), ComputerError> {
    let target = EventTarget {
        window_id,
        pid,
        origin_x,
        origin_y,
    };
    let source = private_source()?;
    let flags = event_flags(modifiers);
    let code = virtual_keycode(key).unwrap_or(0);
    let unicode = (key.chars().count() == 1 && virtual_keycode(key).is_none()).then_some(key);
    post_keycode(&source, &target, code, false, flags, unicode)?;
    for modifier in modifiers.iter().rev() {
        post_keycode(
            &source,
            &target,
            modifier_keycode(*modifier),
            false,
            flags,
            None,
        )?;
    }
    Ok(())
}

#[cfg(not(test))]
fn post_keycode(
    source: &objc2_core_graphics::CGEventSource,
    target: &EventTarget,
    code: u16,
    key_down: bool,
    flags: objc2_core_graphics::CGEventFlags,
    unicode: Option<&str>,
) -> Result<(), ComputerError> {
    use objc2_core_graphics::CGEvent;
    let event = CGEvent::new_keyboard_event(Some(source), code, key_down).ok_or_else(|| {
        ComputerError::ExecutionFailed("CGEventCreateKeyboardEvent failed".into())
    })?;
    stamp(&event, target);
    CGEvent::set_flags(Some(&event), flags);
    if let Some(text) = unicode {
        let units: Vec<u16> = text.encode_utf16().take(20).collect();
        // SAFETY: `units` lives for this call. UniChar is a u16.
        unsafe {
            CGEvent::keyboard_set_unicode_string(
                Some(&event),
                units.len() as _,
                units.as_ptr().cast(),
            );
        }
    }
    CGEvent::post_to_pid(target.pid, Some(&event));
    Ok(())
}

#[cfg(not(test))]
fn event_flags(modifiers: &[Modifier]) -> objc2_core_graphics::CGEventFlags {
    use objc2_core_graphics::CGEventFlags;
    let mut flags = CGEventFlags::empty();
    for modifier in modifiers {
        flags |= match modifier {
            Modifier::Shift => CGEventFlags::MaskShift,
            Modifier::Control => CGEventFlags::MaskControl,
            Modifier::Alt => CGEventFlags::MaskAlternate,
            Modifier::Command | Modifier::Meta => CGEventFlags::MaskCommand,
        };
    }
    flags
}

#[cfg(not(test))]
fn modifier_keycode(modifier: Modifier) -> u16 {
    match modifier {
        Modifier::Shift => 0x38,
        Modifier::Control => 0x3b,
        Modifier::Alt => 0x3a,
        Modifier::Command | Modifier::Meta => 0x37,
    }
}

#[cfg(not(test))]
fn local_point(target: &EventTarget, x: i32, y: i32) -> objc2_core_foundation::CGPoint {
    let (local_x, local_y) =
        crate::tier::window_local_point(x, y, target.origin_x, target.origin_y);
    objc2_core_foundation::CGPoint::new(local_x as _, local_y as _)
}

#[cfg(not(test))]
fn stamp(event: &objc2_core_graphics::CGEvent, target: &EventTarget) {
    use objc2_core_graphics::{CGEvent, CGEventField};
    for (field, value) in crate::tier::event_integer_fields(target.window_id, target.pid) {
        CGEvent::set_integer_value_field(Some(event), CGEventField(field), value);
    }
}

#[cfg(not(test))]
fn cursor_xy() -> Option<(f64, f64)> {
    use objc2_core_graphics::CGEvent;
    let event = CGEvent::new(None)?;
    let point = CGEvent::location(Some(&event));
    Some((point.x, point.y))
}

#[cfg(test)]
mod tests {
    use super::type_focus_warning;

    #[test]
    fn type_focus_warning_emits_for_a_type_job() {
        // `type --app TextEdit --tier event` with no click first posted
        // keystrokes that no element was listening to. The warning is the
        // visible signal to the user (and to automation) that the action
        // ran but the target app had no AXFocusedUIElement.
        let warning = type_focus_warning(true).expect("type job must warn");
        assert!(
            warning.contains("no focused element"),
            "warning text must name the cause: {warning:?}"
        );
        assert!(
            warning.contains("--element"),
            "warning must point at the mitigation: {warning:?}"
        );
    }

    #[test]
    fn type_focus_warning_skips_non_type_jobs() {
        // Coordinate clicks and keystrokes against an element have their
        // own focus model. Only Type is at risk of being silently dropped
        // when no element has focus.
        assert!(type_focus_warning(false).is_none());
    }
}
