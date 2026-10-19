//! Group on-screen windows by the screen that contains each window's center.
//!
//! The structure is intentionally platform-agnostic so the JSON shape is the
//! same on every backend. The macOS backend fills it from `NSScreen` and the
//! CG window list. Linux and Windows backends fill it from `xrandr` and the
//! Win32 enumeration. Anything genuinely unavailable returns `unsupported`.

use serde::Serialize;

use crate::target::WindowInfo;

/// One screen with its on-screen windows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpaceScreen {
    /// Screen index in display order. `0` is the primary screen on most
    /// desktops but the field is not a guarantee of primacy.
    pub index: u32,
    /// Logical name when the platform provides one (NSScreen localized name).
    /// Empty when unknown.
    pub name: String,
    /// Left edge in the virtual desktop.
    pub x: i32,
    /// Top edge in the virtual desktop.
    pub y: i32,
    /// Logical width in screen points.
    pub width: u32,
    /// Logical height in screen points.
    pub height: u32,
    /// Whether this is the primary display.
    pub is_primary: bool,
    /// Windows whose center lies inside this screen, ordered by `id` ascending.
    pub windows: Vec<WindowInfo>,
}

/// Full spaces report. One entry per screen. Linux and Windows backends fill
/// `note` when Mission Control or virtual desktops are not part of the
/// platform.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpacesReport {
    /// One entry per connected display.
    pub screens: Vec<SpaceScreen>,
    /// Free-form note explaining what is and is not supported on this platform.
    /// Empty when the report is whole.
    pub note: String,
}

/// Group `windows` by which screen in `displays` contains each window's center.
/// Windows whose center is outside every screen fall into a synthetic group
/// attached to the first screen; this is rare (a multi-screen arrangement
/// where a window is mid-drag) but the algorithm must not lose the window.
pub fn group_windows(
    displays: &[crate::types::DisplayInfo],
    windows: &[WindowInfo],
) -> Vec<SpaceScreen> {
    let mut out: Vec<SpaceScreen> = displays
        .iter()
        .enumerate()
        .map(|(index, display)| SpaceScreen {
            index: index as u32,
            name: display.name.clone(),
            x: display.x,
            y: display.y,
            width: display.width,
            height: display.height,
            is_primary: display.is_primary,
            windows: Vec::new(),
        })
        .collect();
    if out.is_empty() {
        return out;
    }
    let mut orphan: Vec<WindowInfo> = Vec::new();
    for window in windows {
        let center_x = window.bounds.x + (window.bounds.width as i32) / 2;
        let center_y = window.bounds.y + (window.bounds.height as i32) / 2;
        let owner = out.iter_mut().find(|screen| {
            point_inside(
                center_x,
                center_y,
                screen.x,
                screen.y,
                screen.width,
                screen.height,
            )
        });
        match owner {
            Some(screen) => screen.windows.push(window.clone()),
            None => orphan.push(window.clone()),
        }
    }
    if !orphan.is_empty() {
        let first = &mut out[0];
        first.windows.append(&mut orphan);
    }
    for screen in out.iter_mut() {
        screen.windows.sort_by_key(|window| window.id);
    }
    out
}

fn point_inside(x: i32, y: i32, sx: i32, sy: i32, sw: u32, sh: u32) -> bool {
    x >= sx && y >= sy && x < sx + (sw as i32) && y < sy + (sh as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::WindowBounds;
    use crate::types::DisplayInfo;

    fn window(id: u32, x: i32, y: i32, w: u32, h: u32) -> WindowInfo {
        WindowInfo {
            id,
            pid: 1,
            app: "Test".to_string(),
            title: format!("win {id}"),
            bounds: WindowBounds {
                x,
                y,
                width: w,
                height: h,
            },
            z: 0,
            layer: 0,
            on_screen: true,
            ax_matched: false,
        }
    }

    fn display(index: u32, x: i32, y: i32, w: u32, h: u32) -> DisplayInfo {
        DisplayInfo {
            id: index,
            name: format!("Display {index}"),
            x,
            y,
            width: w,
            height: h,
            scale: 1.0,
            is_primary: index == 0,
        }
    }

    #[test]
    fn group_assigns_windows_by_center() {
        let screens = vec![
            display(0, 0, 0, 1000, 1000),
            display(1, 1000, 0, 1000, 1000),
        ];
        let windows = vec![
            window(1, 100, 100, 200, 200),
            window(2, 1100, 100, 200, 200),
        ];
        let groups = group_windows(&screens, &windows);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].windows.len(), 1);
        assert_eq!(groups[0].windows[0].id, 1);
        assert_eq!(groups[1].windows.len(), 1);
        assert_eq!(groups[1].windows[0].id, 2);
    }

    #[test]
    fn orphan_window_falls_into_the_first_screen() {
        let screens = vec![display(0, 0, 0, 1000, 1000)];
        let windows = vec![window(7, 5000, 5000, 200, 200)];
        let groups = group_windows(&screens, &windows);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].windows.len(), 1);
        assert_eq!(groups[0].windows[0].id, 7);
    }

    #[test]
    fn empty_display_list_yields_empty_group() {
        let groups = group_windows(&[], &[]);
        assert!(groups.is_empty());
    }
}
