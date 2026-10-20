//! Windows native window enumeration.
//!
//! `EnumWindows` + `GetWindowTextW` + `GetWindowThreadProcessId` + `QueryFullProcessImageNameW`
//! drive `list_apps` and `list_windows`. The legacy PowerShell foreground script
//! (see `P11/P12` design) is gone.
//!
//! Z-order comes from a `GetAncestor(GA_ROOT)` filter plus `IsWindowVisible`. The
//! heap is z-ordered by insertion time: `EnumWindows` walks top-level windows
//! bottom-to-top in their owner/HWND order. We assign the array order as the
//! z-order, index 0 is the front window — matching the macOS contract.

// Win32 FFI requires `unsafe` blocks; the crate's `#![deny(unsafe_code)]` is
// relaxed here because the entire module is target-gated to `cfg(target_os =
// "windows")` and the calls have no safe equivalent.
#![allow(unsafe_code)]

use std::path::PathBuf;

use crate::error::ComputerError;
use crate::target::{assign_z_order, filter_windows_for_pid};
use crate::target::{
    ActivationPolicy, AppInfo, ListedWindow, WindowBounds, WindowInfo, WindowList,
};

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible, GA_ROOT,
};

/// Maximum title length we read into. Windows titles are UTF-16; 512 wide chars
/// is far above the practical limit and small enough to keep on the stack.
const TITLE_BUF_LEN: usize = 512;

/// Build the [`AppInfo`] rows from the live [`WindowSnapshot`]s.
///
/// One row per unique pid. The first title seen wins; later titles for the
/// same process are ignored so a notepad window with three documents still
/// produces one app row.
pub(crate) fn build_apps(snapshots: &[WindowSnapshot]) -> Vec<AppInfo> {
    use std::collections::BTreeMap;
    let mut by_pid: BTreeMap<u32, &WindowSnapshot> = BTreeMap::new();
    for snap in snapshots {
        by_pid.entry(snap.pid).or_insert(snap);
    }
    let mut apps: Vec<AppInfo> = by_pid
        .into_values()
        .map(|snap| AppInfo {
            name: snap.app_name.clone(),
            // Windows processes do not carry a stable bundle id. Leave it empty
            // so callers fall back to name/path/pid.
            bundle_id: None,
            pid: snap.pid as i32,
            path: snap
                .exe_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            // `EnumWindows` walks visible + invisible top-level windows.
            // Treat every returned app as live: a hidden owner window would
            // still enumerate here, but the frontmost contract uses
            // `GetForegroundWindow` for the active row in P12b.
            active: true,
            // `IsWindowVisible` is already applied during enumeration.
            hidden: false,
            // Windows has no NSApplicationActivationPolicy analogue; every
            // app defaults to a regular foreground app.
            activation_policy: ActivationPolicy::Regular,
        })
        .collect();
    // Stable order: pid ascending. Matches what `tasklist` prints.
    apps.sort_by_key(|app| app.pid);
    apps
}

/// Build the [`WindowList`] from the live [`WindowSnapshot`]s.
///
/// The list is z-ordered by the `EnumWindows` array order (index 0 is the
/// front window — Windows enumerates top-level windows in z-order from back
/// to front). Each row carries the bounds, on-screen flag, and the platform
/// HWND, which becomes the `WindowInfo.id`.
pub(crate) fn build_windows(snapshots: &[WindowSnapshot]) -> WindowList {
    let listed: Vec<ListedWindow> = snapshots
        .iter()
        .map(|snap| ListedWindow {
            id: snap.hwnd,
            pid: snap.pid as i32,
            app: snap.app_name.clone(),
            title: snap.title.clone(),
            bounds: snap.bounds,
            // Windows does not expose CGWindowLevel; we only ever see normal
            // windows here, so layer 0 keeps the parity row honest.
            layer: 0,
            on_screen: snap.visible,
        })
        .collect();
    let windows: Vec<WindowInfo> = assign_z_order(listed);
    let note = if snapshots.is_empty() {
        Some("no top-level windows visible".to_string())
    } else {
        None
    };
    WindowList { windows, note }
}

/// Snapshot from a single `EnumWindows` callback.
#[derive(Debug, Clone)]
pub(crate) struct WindowSnapshot {
    /// Stable HWND cast to `u32`. Zero is reserved for `HWND_MESSAGE`.
    pub hwnd: u32,
    /// Owning process id.
    pub pid: u32,
    /// Process module base name (e.g. `notepad`). Comes from `QueryFullProcessImageNameW`.
    pub app_name: String,
    /// Window title, possibly empty when the title bar is hidden.
    pub title: String,
    /// Screen-space frame in virtual-screen pixels.
    pub bounds: WindowBounds,
    /// Whether `IsWindowVisible` returned true.
    pub visible: bool,
    /// Canonical path from `QueryFullProcessImageNameW`, when accessible.
    pub exe_path: Option<PathBuf>,
}

/// Enumerate every visible top-level window.
///
/// Each callback row resolves the process id, opens the process with
/// `PROCESS_QUERY_LIMITED_INFORMATION`, queries the module path, and reads the
/// title in a single pass. Anything that fails for one window is skipped — a
/// partial list still gets returned so the caller can keep going.
pub(crate) fn enumerate_windows() -> Result<Vec<WindowSnapshot>, ComputerError> {
    let mut snapshots: Vec<WindowSnapshot> = Vec::new();
    // SAFETY: `EnumWindows` invokes the callback once per top-level HWND on
    // the calling thread. The callback returns `BOOL`; `1` (true) keeps the
    // walk going, `0` stops it. The `LPARAM` carries the snapshot vec.
    let lparam = LPARAM(&mut snapshots as *mut _ as isize);
    unsafe {
        EnumWindows(Some(enum_callback), lparam).map_err(|error| {
            ComputerError::ExecutionFailed(format!("EnumWindows failed: {error}"))
        })?;
    }
    Ok(snapshots)
}

unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // Skip child windows and owner-less message-only windows.
    let root = GetAncestor(hwnd, GA_ROOT);
    if root != hwnd {
        return BOOL(1);
    }
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }

    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 {
        return BOOL(1);
    }

    let mut title_buf = [0u16; TITLE_BUF_LEN];
    let len = GetWindowTextW(hwnd, &mut title_buf);
    let title = if len > 0 {
        String::from_utf16_lossy(&title_buf[..len as usize])
    } else {
        String::new()
    };

    let mut rect = RECT::default();
    let rect_ok = GetWindowRect(hwnd, &mut rect).is_ok();

    let (exe_path, app_name) = match process_path(pid) {
        Ok(path) => {
            let name = module_base_name(&path).unwrap_or_else(|| String::from("unknown"));
            (Some(path), name)
        }
        Err(_) => (None, String::from("unknown")),
    };

    let bounds = if rect_ok {
        let width = rect.right.saturating_sub(rect.left).max(0) as u32;
        let height = rect.bottom.saturating_sub(rect.top).max(0) as u32;
        WindowBounds {
            x: rect.left,
            y: rect.top,
            width,
            height,
        }
    } else {
        WindowBounds {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        }
    };

    let snapshots = &mut *(lparam.0 as *mut Vec<WindowSnapshot>);
    snapshots.push(WindowSnapshot {
        hwnd: hwnd.0 as u32,
        pid,
        app_name,
        title,
        bounds,
        visible: true,
        exe_path,
    });

    BOOL(1)
}

/// Open a pid for read-only query and return its full image path.
fn process_path(pid: u32) -> windows::core::Result<PathBuf> {
    // SAFETY: `OpenProcess` returns a real handle or an error. The handle is
    // closed with `CloseHandle` once the path is read.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }?;
    let result = read_image_name(&handle);
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
    result
}

/// Read the full image name from an open process handle.
///
/// The `windows` crate exposes `PWSTR` for the output; we use a fixed buffer
/// of 1024 wide chars and let `QueryFullProcessImageNameW` truncate.
fn read_image_name(handle: &windows::Win32::Foundation::HANDLE) -> windows::core::Result<PathBuf> {
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY: `buffer` is a writable wide-char slice large enough for any
    // realistic path on Windows. `QueryFullProcessImageNameW` writes the
    // path in place and updates `size` with the count (excluding the NUL).
    unsafe {
        QueryFullProcessImageNameW(
            *handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )?;
    }
    if size == 0 {
        return Err(windows::core::Error::from(windows::core::HRESULT(0)));
    }
    let path = String::from_utf16_lossy(&buffer[..size as usize]);
    Ok(PathBuf::from(path))
}

/// Keep the windows and apps of one process, in z-order.
#[allow(dead_code)]
pub(crate) fn keep_pid(windows: &[WindowInfo], pid: i32) -> Vec<WindowInfo> {
    filter_windows_for_pid(windows, pid)
}

/// Extract the file stem (no directory, no extension) from a process path.
///
/// `C:\Windows\System32\notepad.exe` → `notepad`. UWP package paths use a
/// `\` separator and `Windows.Resources` hosts keep an `.exe` suffix we want
/// to drop for the display name. When the path has no separator we treat the
/// whole name as the stem.
fn module_base_name(path: &std::path::Path) -> Option<String> {
    let file = path.file_name().or_else(|| path.file_stem())?;
    let text = file.to_string_lossy();
    let trimmed = text.strip_suffix(".exe").unwrap_or(&text);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Look up one [`WindowInfo`] by the HWND the user passed to `--window id=`.
#[allow(dead_code)]
pub(crate) fn window_by_id(windows: &[WindowInfo], id: u32) -> Option<&WindowInfo> {
    windows.iter().find(|window| window.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn builds_apps_from_unique_pids() {
        let snaps = vec![
            WindowSnapshot {
                hwnd: 1,
                pid: 10,
                app_name: "notepad".into(),
                title: "Untitled".into(),
                bounds: WindowBounds {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                },
                visible: true,
                exe_path: Some(PathBuf::from(r"C:\Windows\notepad.exe")),
            },
            WindowSnapshot {
                hwnd: 2,
                pid: 10,
                app_name: "notepad".into(),
                title: "second".into(),
                bounds: WindowBounds {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                },
                visible: true,
                exe_path: Some(PathBuf::from(r"C:\Windows\notepad.exe")),
            },
            WindowSnapshot {
                hwnd: 3,
                pid: 11,
                app_name: "explorer".into(),
                title: "Files".into(),
                bounds: WindowBounds {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 200,
                },
                visible: true,
                exe_path: Some(PathBuf::from(r"C:\Windows\explorer.exe")),
            },
        ];
        let apps = build_apps(&snaps);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].pid, 10);
        assert_eq!(apps[0].name, "notepad");
        assert_eq!(apps[0].path.as_deref(), Some(r"C:\Windows\notepad.exe"));
        assert_eq!(apps[1].pid, 11);
    }

    #[test]
    fn builds_windows_in_z_order() {
        let snaps = vec![
            WindowSnapshot {
                hwnd: 5,
                pid: 20,
                app_name: "notepad".into(),
                title: "two".into(),
                bounds: WindowBounds {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                visible: true,
                exe_path: None,
            },
            WindowSnapshot {
                hwnd: 6,
                pid: 20,
                app_name: "notepad".into(),
                title: "one".into(),
                bounds: WindowBounds {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                visible: true,
                exe_path: None,
            },
        ];
        let list = build_windows(&snaps);
        // Array order: first is z=0 (front).
        assert_eq!(list.windows.len(), 2);
        assert_eq!(list.windows[0].id, 5);
        assert_eq!(list.windows[0].z, 0);
        assert_eq!(list.windows[1].id, 6);
        assert_eq!(list.windows[1].z, 1);
        assert_eq!(list.windows[0].pid, 20);
    }

    #[test]
    fn empty_snapshots_yield_a_note() {
        let list = build_windows(&[]);
        assert!(list.windows.is_empty());
        assert!(list.note.is_some());
    }

    #[test]
    fn module_base_name_strips_extension_and_directory() {
        assert_eq!(
            module_base_name(&PathBuf::from(r"C:\Windows\System32\notepad.exe")),
            Some("notepad".to_string())
        );
        assert_eq!(
            module_base_name(&PathBuf::from("notepad")),
            Some("notepad".to_string())
        );
        assert_eq!(module_base_name(&PathBuf::from(".exe")), None);
    }

    #[test]
    fn keep_pid_renumbers_z_order() {
        let wins = vec![
            WindowInfo {
                id: 1,
                pid: 7,
                app: "x".into(),
                title: "a".into(),
                z: 0,
                bounds: WindowBounds {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                layer: 0,
                on_screen: true,
                ax_matched: true,
            },
            WindowInfo {
                id: 2,
                pid: 8,
                app: "y".into(),
                title: "b".into(),
                z: 1,
                bounds: WindowBounds {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                layer: 0,
                on_screen: true,
                ax_matched: true,
            },
        ];
        let filtered = keep_pid(&wins, 8);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].z, 0);
        assert_eq!(filtered[0].id, 2);
    }
}
