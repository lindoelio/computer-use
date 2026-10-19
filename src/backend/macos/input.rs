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
        "home" => 0x73,
        "pageup" => 0x74,
        "delete" => 0x75,
        "end" => 0x77,
        "pagedown" => 0x79,
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
    }
}

#[cfg(not(test))]
fn perform_input_live(job: &InputJob) -> Result<InputReport, ComputerError> {
    match job.tier {
        TierName::Ax => Err(ComputerError::Unsupported(
            "tier ax does not post coordinate or keyboard events; pass --element".into(),
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
    dispatch_kind(&source, &target, &job.kind)?;
    let after = cursor_xy();
    let mut warnings = Vec::new();
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
    Ok(InputReport {
        tier: job.tier.as_str(),
        cursor_moved: moved,
        focus_restored: None,
        warnings,
    })
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
        InputKind::Key { modifiers, key } => super::super::input::press_key(modifiers, key)?,
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
        InputKind::Key { modifiers, key } => post_key(source, target, modifiers, key),
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
    let clicks = count.max(1);
    for click_state in 1..=clicks {
        for phase in [PointerPhase::Down, PointerPhase::Up] {
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
            CGEvent::post_to_pid(target.pid, Some(&event));
        }
    }
    Ok(())
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
