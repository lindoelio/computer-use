//! Private SkyLight front-process call.
//!
//! Compiled only with `--features skylight`, and only when the caller asked
//! for `--tier synthetic`. A missing symbol is `unsupported`. This file never
//! aborts, and unit tests do not compile it.

#![allow(unsafe_code)]

use std::ffi::{c_void, CStr};

use crate::error::ComputerError;
use crate::tier::SKYLIGHT_FRONT_SYMBOLS;

/// Same layout as Apple's `ProcessSerialNumber`. The objc2 binding keeps this type private.
/// The fields are written by `GetProcessForPID` and only passed back to SkyLight.
#[repr(C)]
#[allow(dead_code)]
struct ProcessSerialNumber {
    high_long_of_psn: u32,
    low_long_of_psn: u32,
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn GetProcessForPID(pid: libc::pid_t, psn: *mut ProcessSerialNumber) -> i32;
}

const SKYLIGHT: &CStr = c"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";

type SlpsFn = unsafe extern "C" fn(*const ProcessSerialNumber, u32, u32) -> i32;
type SlsFn = unsafe extern "C" fn(i32, *const ProcessSerialNumber, u32) -> i32;
type CpsFn = unsafe extern "C" fn(*const ProcessSerialNumber) -> i32;
type ConnFn = unsafe extern "C" fn() -> i32;

/// Make `pid` look frontmost. Does not call `NSRunningApplication`.
pub(crate) fn claim(pid: i32, window_id: u32) -> Result<(), ComputerError> {
    // SAFETY: the path is a NUL-terminated literal. RTLD_LAZY only resolves
    // symbols we call below. The handle is closed before this function returns.
    let handle = unsafe { libc::dlopen(SKYLIGHT.as_ptr(), libc::RTLD_LAZY) };
    if handle.is_null() {
        return Err(ComputerError::Unsupported(
            "SkyLight.framework did not load".into(),
        ));
    }
    let outcome = claim_loaded(handle, pid, window_id);
    // SAFETY: `handle` came from dlopen in this function and is not used again.
    unsafe { libc::dlclose(handle) };
    outcome
}

fn claim_loaded(handle: *mut c_void, pid: i32, window_id: u32) -> Result<(), ComputerError> {
    let slps = symbol(handle, c"SLPSSetFrontProcessWithOptions");
    let sls = symbol(handle, c"SLSSetFrontProcessWithOptions");
    let cps = symbol(handle, c"CPSSetFrontProcess");
    let connection = symbol(handle, c"SLSMainConnectionID");
    if slps.is_null() && sls.is_null() && cps.is_null() {
        return Err(ComputerError::Unsupported(format!(
            "SkyLight symbol {} was not found",
            SKYLIGHT_FRONT_SYMBOLS.join(", ")
        )));
    }
    if slps.is_null() && cps.is_null() && !sls.is_null() && connection.is_null() {
        return Err(ComputerError::Unsupported(
            "SkyLight symbol SLSMainConnectionID was not found".into(),
        ));
    }
    // SAFETY: GetProcessForPID writes two u32 fields into this zeroed struct.
    // The pointer is valid for the call. A non-zero status is an error.
    let mut psn: ProcessSerialNumber = unsafe { std::mem::zeroed() };
    let lookup = unsafe { GetProcessForPID(pid, &mut psn) };
    if lookup != 0 {
        return Err(ComputerError::ExecutionFailed(format!(
            "GetProcessForPID failed: {lookup}"
        )));
    }
    let status = if !slps.is_null() {
        // SAFETY: dlsym returned this symbol from the handle we still own.
        // `psn` is live. The options argument is 0.
        let function: SlpsFn = unsafe { std::mem::transmute(slps) };
        unsafe { function(&psn, window_id, 0) }
    } else if !sls.is_null() && !connection.is_null() {
        // SAFETY: both symbols were resolved on `handle`. The connection id is
        // only passed into the front-process call.
        let connection: ConnFn = unsafe { std::mem::transmute(connection) };
        let function: SlsFn = unsafe { std::mem::transmute(sls) };
        let connection_id = unsafe { connection() };
        unsafe { function(connection_id, &psn, 0) }
    } else if !cps.is_null() {
        // SAFETY: dlsym returned CPSSetFrontProcess from the handle we still own.
        let function: CpsFn = unsafe { std::mem::transmute(cps) };
        unsafe { function(&psn) }
    } else {
        return Err(ComputerError::Unsupported(format!(
            "SkyLight symbol {} was not found",
            SKYLIGHT_FRONT_SYMBOLS.join(", ")
        )));
    };
    if status != 0 {
        return Err(ComputerError::ExecutionFailed(format!(
            "synthetic focus failed: {status}"
        )));
    }
    Ok(())
}

fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
    // SAFETY: `handle` is an open dlopen handle and `name` is a C string.
    unsafe { libc::dlsym(handle, name.as_ptr()) }
}
