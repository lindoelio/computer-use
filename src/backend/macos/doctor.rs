//! Accessibility and Screen Recording probes.
//!
//! A normal `doctor` call does not prompt. `--request` asks for both permissions.
//! The Accessibility prompt is asynchronous and does not change the returned bit.

#![allow(unsafe_code)]

use std::ffi::c_void;

use objc2_application_services::{kAXTrustedCheckOptionPrompt, AXIsProcessTrustedWithOptions};
use objc2_core_foundation::{
    kCFBooleanTrue, kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFDictionary,
};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

use crate::error::ComputerError;
use crate::target::{DoctorReport, ResponsibleApp};

use super::apps;

pub(crate) fn doctor(request: bool) -> Result<DoctorReport, ComputerError> {
    let accessibility = ax_trusted(request);
    let screen_capture = if request {
        // The wrapper shows the system dialog and then reports the current grant.
        CGRequestScreenCaptureAccess();
        CGPreflightScreenCaptureAccess()
    } else {
        CGPreflightScreenCaptureAccess()
    };
    let responsible = apps::responsible_app();
    let subject = permission_subject(responsible.as_ref());
    let mut fixes = Vec::new();
    if !accessibility {
        fixes.push(format!(
            "System Settings → Privacy & Security → Accessibility → enable {subject}"
        ));
    }
    if !screen_capture {
        fixes.push(format!(
            "System Settings → Privacy & Security → Screen Recording → enable {subject}"
        ));
    }
    let mut notes = Vec::new();
    if request {
        notes.push(
            "the Accessibility prompt is asynchronous and does not change this result".to_string(),
        );
    }
    if responsible.is_none() && (!accessibility || !screen_capture) {
        notes.push(
            "could not find a parent .app bundle; enable the terminal that launched computer-use"
                .to_string(),
        );
    }
    Ok(DoctorReport {
        accessibility: Some(accessibility),
        screen_capture: Some(screen_capture),
        input_monitor: None,
        responsible_app: responsible,
        fixes,
        note: if notes.is_empty() {
            None
        } else {
            Some(notes.join("; "))
        },
    })
}

fn permission_subject(app: Option<&ResponsibleApp>) -> String {
    match app {
        Some(app) => app.name.clone(),
        None => "computer-use".into(),
    }
}

fn ax_trusted(prompt: bool) -> bool {
    if !prompt {
        // SAFETY: a null options dictionary reads trust and does not prompt.
        return unsafe { AXIsProcessTrustedWithOptions(None) };
    }
    // SAFETY: kAXTrustedCheckOptionPrompt is a process-lifetime CFString.
    let key = std::ptr::from_ref(unsafe { kAXTrustedCheckOptionPrompt }).cast::<c_void>();
    // SAFETY: kCFBooleanTrue is a process-lifetime CFBoolean. The static is an Option.
    let boolean = unsafe { kCFBooleanTrue }.expect("kCFBooleanTrue");
    let value = std::ptr::from_ref(boolean).cast::<c_void>();
    let mut keys = [key];
    let mut values = [value];
    // SAFETY: the callbacks are the standard CFType key and value callbacks.
    // The dictionary retains the one CFString key and the CFBoolean value.
    let dict = unsafe {
        CFDictionary::new(
            None,
            keys.as_mut_ptr(),
            values.as_mut_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
    };
    // SAFETY: the dictionary is CFString → CFBoolean, which is the documented options type.
    unsafe { AXIsProcessTrustedWithOptions(dict.as_deref()) }
}
