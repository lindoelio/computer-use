//! Accessibility, Screen Recording, Post Event, and Input Monitoring probes.
//!
//! A normal `doctor` call does not prompt and does not open System Settings.
//! `--prompt` (alias `--request`) calls the request APIs. Those dialogs are
//! asynchronous and do not change the returned bits until the user decides.
//! `--open-settings` runs `open` on the first missing pane.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::time::Duration;

use objc2_application_services::{
    kAXTrustedCheckOptionPrompt, AXIsProcessTrusted, AXIsProcessTrustedWithOptions,
};
use objc2_core_foundation::{
    kCFBooleanTrue, kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFDictionary,
};
use objc2_core_graphics::{
    CGPreflightListenEventAccess, CGPreflightPostEventAccess, CGPreflightScreenCaptureAccess,
    CGRequestListenEventAccess, CGRequestPostEventAccess, CGRequestScreenCaptureAccess,
};
use objc2_foundation::NSBundle;

use crate::backend::command::{run_timeout, stderr_snippet};
use crate::doctor::{self, DoctorInput, DoctorOptions, PermissionProbe};
use crate::error::ComputerError;
use crate::target::DoctorReport;

use super::apps;

const CODESIGN_TIMEOUT: Duration = Duration::from_secs(4);
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

struct LiveProbe;

impl PermissionProbe for LiveProbe {
    fn accessibility(&mut self, prompt: bool) -> Option<bool> {
        Some(if prompt {
            ax_trusted_with_prompt()
        } else {
            // SAFETY: AXIsProcessTrusted reads the trust bit and does not prompt.
            unsafe { AXIsProcessTrusted() }
        })
    }

    fn screen_recording(&mut self, prompt: bool) -> Option<bool> {
        if prompt {
            CGRequestScreenCaptureAccess();
        }
        Some(CGPreflightScreenCaptureAccess())
    }

    fn post_events(&mut self, prompt: bool) -> Option<bool> {
        if prompt {
            CGRequestPostEventAccess();
        }
        Some(CGPreflightPostEventAccess())
    }

    fn input_monitoring(&mut self, prompt: bool) -> Option<bool> {
        if prompt {
            CGRequestListenEventAccess();
        }
        Some(CGPreflightListenEventAccess())
    }
}

pub(crate) fn doctor(options: DoctorOptions) -> Result<DoctorReport, ComputerError> {
    let sample = doctor::sample_permissions(&mut LiveProbe, options.prompt);
    let path = doctor::current_executable();
    let (bundle_id, bundle_path) = main_bundle();
    let bundled = doctor::is_helper_bundle(
        path.as_deref(),
        bundle_id.as_deref(),
        bundle_path.as_deref(),
    );
    let (signature, team_id, sign_note) = signature_of(path.as_deref());
    let responsible = apps::responsible_app();
    let mut notes = Vec::new();
    if options.prompt {
        notes.push(
            "prompts are asynchronous and do not change this result until the user decides"
                .to_string(),
        );
    }
    if let Some(note) = sign_note {
        notes.push(note);
    }
    let input = DoctorInput {
        accessibility: sample.accessibility,
        screen_recording: sample.screen_recording,
        post_events: sample.post_events,
        input_monitoring: sample.input_monitoring,
        input_monitor: super::input::status(),
        path,
        bundle_id,
        bundled,
        signature,
        team_id,
        responsible,
        notes,
        required: doctor::MACOS_REQUIRED,
        recommend_bundle: !bundled,
        open_settings: options.open_settings,
        ..DoctorInput::default()
    };
    let mut report = doctor::assemble(input);
    if let Some(url) = doctor::settings_url_to_open(&report, options.open_settings) {
        let url = url.to_string();
        match run_timeout("open", &[&url], OPEN_TIMEOUT) {
            Ok(output) if output.status.success() => {}
            Ok(output) => doctor::push_note(
                &mut report,
                &format!(
                    "could not open System Settings: {}",
                    stderr_snippet(&output.stderr)
                ),
            ),
            Err(error) => doctor::push_note(
                &mut report,
                &format!("could not open System Settings: {error}"),
            ),
        }
    }
    Ok(report)
}

fn main_bundle() -> (Option<String>, Option<String>) {
    let bundle = NSBundle::mainBundle();
    let bundle_id = bundle
        .bundleIdentifier()
        .map(|value| value.to_string())
        .filter(|value| !value.is_empty());
    let bundle_path = bundle.bundlePath().to_string();
    let bundle_path = if bundle_path.is_empty() {
        None
    } else {
        Some(bundle_path)
    };
    (bundle_id, bundle_path)
}

fn signature_of(path: Option<&str>) -> (Option<String>, Option<String>, Option<String>) {
    let Some(path) = path else {
        return (
            Some("unknown".to_string()),
            None,
            Some("could not read the executable path for codesign".to_string()),
        );
    };
    match run_timeout("codesign", &["-dv", path], CODESIGN_TIMEOUT) {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let (status, team) = doctor::parse_codesign(&format!("{stderr}\n{stdout}"));
            (Some(status.to_string()), team, None)
        }
        Err(error) => (
            Some("unknown".to_string()),
            None,
            Some(format!("codesign: {error}")),
        ),
    }
}

fn ax_trusted_with_prompt() -> bool {
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
