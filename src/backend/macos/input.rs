//! Listen-only user-input tap for the P6 kill switch.
//!
//! The rest of background input lands in P7. This file owns the tap and the
//! marker constant's comparison. P7 must stamp synthetic events with
//! [`crate::policy::SYNTHETIC_EVENT_USER_DATA`] via `CGEventSource::set_user_data`
//! and `CGEvent::set_integer_value_field` on `CGEventField::EventSourceUserData`
//! (value 42). Field 43 is `kCGEventSourceUserID` and is not the marker.
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
