//! Linux frontmost-window lookup.
//!
//! Input uses enigo (X11 / libxdo). The accessibility tree is not queried:
//! AT-SPI is not wired in this crate, so callers receive a note instead of a
//! panic. `xdotool`, when installed, supplies the frontmost window title.

use std::time::Duration;

use crate::error::ComputerError;
use crate::types::FrontmostInfo;

use super::command::{run_timeout, stderr_snippet};

/// Linux desktop backend. Methods are implemented in the parent module.
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

pub(crate) fn inspect_accessibility(_app_name: Option<&str>) -> Result<String, ComputerError> {
    Err(ComputerError::Unsupported(
        "accessibility tree is not available on Linux in this build. frontmost window title is reported when xdotool can see it".into(),
    ))
}
