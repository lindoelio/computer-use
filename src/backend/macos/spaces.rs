//! Spaces report from `NSScreen` and the existing CG window list.
//!
//! The action only reads state; it never tells Mission Control to move a
//! window between spaces. When the `skylight` feature is active, an opt-in
//! path may call private SkyLight symbols to relocate a window. That path
//! lives behind a feature flag so the safe behavior ships by default.

use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;

use crate::backend::macos::apps;
use crate::target::WindowInfo;
use crate::types::DisplayInfo;
use crate::window_groups::SpacesReport;

/// Group on-screen windows by the screen that contains each window's center.
pub(crate) fn report() -> SpacesReport {
    let windows = match on_screen_windows() {
        Ok(windows) => windows,
        Err(error) => {
            return SpacesReport {
                screens: Vec::new(),
                note: format!("windows unavailable: {error}"),
            }
        }
    };
    let displays = collect_displays();
    let grouped = crate::window_groups::group_windows(&displays, &windows);
    let note = mission_control_note();
    SpacesReport {
        screens: grouped,
        note,
    }
}

fn on_screen_windows() -> Result<Vec<WindowInfo>, crate::error::ComputerError> {
    apps::on_screen_windows()
}

fn collect_displays() -> Vec<DisplayInfo> {
    let mtm = match MainThreadMarker::new() {
        Some(marker) => marker,
        None => return Vec::new(),
    };
    let screens = NSScreen::screens(mtm);
    let count = screens.count();
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let screen = screens.objectAtIndex(index);
        let frame = screen.frame();
        let name = screen.localizedName().to_string();
        out.push(DisplayInfo {
            id: index as u32,
            name,
            x: frame.origin.x as i32,
            y: frame.origin.y as i32,
            width: frame.size.width as u32,
            height: frame.size.height as u32,
            scale: screen.backingScaleFactor(),
            is_primary: index == 0,
        });
    }
    out
}

fn mission_control_note() -> String {
    if cfg!(feature = "skylight") {
        "skylight feature is active; space moves remain best-effort and are not exposed by this action".to_string()
    } else {
        String::new()
    }
}
