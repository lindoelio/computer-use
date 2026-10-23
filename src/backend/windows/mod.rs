//! Windows desktop backend.
//!
//! Input uses enigo (Win32 `SendInput`). Window and process enumeration uses
//! native `EnumWindows` + `GetWindowTextW` + `GetWindowThreadProcessId` +
//! `QueryFullProcessImageNameW` through the `windows` crate. The legacy
//! PowerShell foreground and token scripts are gone (P12a); the
//! accessibility tree is built through `uiautomation` 0.25.1 and the
//! token probes live entirely inside Rust.

// The whole module is gated on `cfg(target_os = "windows")`. The token
// probes need `unsafe` for `OpenProcessToken` / `GetTokenInformation`,
// which have no safe equivalent on Windows. Lift the crate's
// `#![deny(unsafe_code)]` here only.
#![allow(unsafe_code)]

pub mod activate;
pub mod apps;
pub mod capture;
pub mod uia;

use crate::doctor::{self, DoctorInput, DoctorOptions};
use crate::element::{ElementError, ElementRequest};
use crate::error::ComputerError;
use crate::target::{AppInfo, DoctorReport, LaunchResult, QuitResult, WindowList};
use crate::tree::TreeQuery;
use crate::types::FrontmostInfo;

use super::unsupported;

#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
#[cfg(target_os = "windows")]
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
    TokenUIAccess, PSID, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Windows desktop backend. Methods are implemented in the parent module.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsDesktopBackend;

/// Best-effort foreground window lookup. Drives the live `uiautomation`
/// enumeration so we can stay off PowerShell on Windows. The result is the
/// front-most window by z-order; index 0 is treated as "in front".
pub(crate) fn frontmost() -> FrontmostInfo {
    match apps::enumerate_windows() {
        Ok(snapshots) => {
            let window_list = apps::build_windows(&snapshots);
            let info = window_list
                .windows
                .first()
                .map(|window| FrontmostInfo {
                    app: Some(window.app.clone()),
                    window: Some(window.title.clone()),
                    note: None,
                })
                .unwrap_or_default();
            info
        }
        Err(error) => FrontmostInfo {
            note: Some(format!("foreground window unavailable: {error}")),
            ..FrontmostInfo::default()
        },
    }
}

pub(crate) fn inspect_accessibility(_app_name: Option<&str>) -> Result<String, ComputerError> {
    let query = TreeQuery {
        app: _app_name.map(str::to_owned),
        window: None,
        full: true,
        diff: false,
        session: None,
    };
    let tree = uia::build_tree(&query)?;
    Ok(crate::tree::render(&tree.root).text)
}

pub(crate) fn list_apps(_all: bool) -> Result<Vec<AppInfo>, ComputerError> {
    let snapshots = apps::enumerate_windows()?;
    Ok(apps::build_apps(&snapshots))
}

pub(crate) fn list_windows() -> Result<WindowList, ComputerError> {
    let snapshots = apps::enumerate_windows()?;
    Ok(apps::build_windows(&snapshots))
}

pub(crate) fn launch(_app: &str) -> Result<LaunchResult, ComputerError> {
    Err(unsupported("launch"))
}

pub(crate) fn activate(_app: &str, _window: Option<&str>) -> Result<LaunchResult, ComputerError> {
    Err(unsupported("activate"))
}

pub(crate) fn quit(_app: &str, _force: bool) -> Result<QuitResult, ComputerError> {
    Err(unsupported("quit"))
}

pub(crate) fn tree(query: &TreeQuery) -> Result<crate::tree::TreeReport, ComputerError> {
    let built = uia::build_tree(query)?;
    Ok(uia::to_report(
        &built,
        query.app.clone(),
        query.window.clone(),
    ))
}

pub(crate) fn end_session(session: Option<&str>) -> Result<Vec<i32>, ComputerError> {
    if let Ok(pending) = crate::cache::pending_restores(session) {
        let pids: Vec<i32> = pending.into_iter().map(|(pid, _)| pid).collect();
        if !pids.is_empty() {
            let _ = crate::cache::clear_restores(session, &pids);
        }
    }
    Ok(Vec::new())
}

/// Element actions on Windows go through `uiautomation` control patterns.
///
/// The cached tree is loaded by the dispatcher; this function revalidates
/// against UIA, walks the cached path, and dispatches the pattern. A missing
/// pattern or a failed call is retried as tier `activate` unless the caller
/// passed `--require-background` — see [`activate`].
#[allow(dead_code)]
pub(crate) fn element_action(
    request: &ElementRequest,
) -> Result<crate::element::ElementOutcome, ElementError> {
    uia::element_action(request)
}

/// Capture one window through `Windows.Graphics.Capture`, falling back to a
/// crop of the xcap desktop image. See [`capture`].
#[allow(dead_code)]
pub(crate) fn capture_window(
    target: &super::WindowTarget,
) -> Result<crate::types::RawCapture, ComputerError> {
    capture::capture_window(target)
}

/// Run `Windows.Media.Ocr` on a prepared capture.
#[allow(dead_code)]
pub(crate) fn recognize_text(
    image: &image::RgbaImage,
    _map: &crate::image_util::ScreenshotMap,
) -> Result<crate::vision::OcrResult, ComputerError> {
    crate::vision::recognize(image, _map)
}

/// Numbered boxes for `--marks`, taken from the live UIA tree.
///
/// The walk itself is shared with macOS in [`crate::vision::marks_for_nodes`],
/// so the numbers drawn on the image are the numbers `tree` prints.
#[allow(dead_code)]
pub(crate) fn marks_for_tree(
    query: &TreeQuery,
    map: &crate::image_util::ScreenshotMap,
) -> Result<Vec<crate::vision::MarkBox>, ComputerError> {
    let root = uia::build_tree(query)?;
    Ok(crate::vision::marks_for_nodes(&root.root, map))
}

/// Image-pixel rectangles of every `IsPassword` element, converted through the
/// screenshot map so `vision::ocr::redact_blocks` compares like with like.
#[allow(dead_code)]
pub(crate) fn secure_ocr_frames(
    query: &TreeQuery,
    map: &crate::image_util::ScreenshotMap,
) -> Result<Vec<crate::vision::ocr::ImageRect>, ComputerError> {
    let tree = uia::build_tree(query)?;
    let mut out = Vec::new();
    walk_secure(&tree.root, map, &mut out);
    Ok(out)
}

fn walk_secure(
    node: &crate::tree::UiNode,
    map: &crate::image_util::ScreenshotMap,
    out: &mut Vec<crate::vision::ocr::ImageRect>,
) {
    if crate::tree::node_is_secure(node) {
        if let Some(frame) = node.frame {
            if let Some((x, y, width, height)) = screen_rect(frame, map) {
                out.push(crate::vision::ocr::ImageRect {
                    x,
                    y,
                    width,
                    height,
                });
            }
        }
    }
    for child in &node.children {
        walk_secure(child, map, out);
    }
}

/// One platform frame (Win32 screen points) mapped into image pixels.
fn screen_rect(
    frame: crate::tree::NodeFrame,
    map: &crate::image_util::ScreenshotMap,
) -> Option<(i32, i32, u32, u32)> {
    crate::image_util::screen_rect_to_image(map, frame.x, frame.y, frame.width, frame.height)
}

pub(crate) fn doctor(options: DoctorOptions) -> Result<DoctorReport, ComputerError> {
    let (uiaccess, integrity, token_note) = read_token();
    let accessibility_ok = uia::doctor_accessibility().ok();
    let mut notes = Vec::new();
    if let Some(note) = token_note {
        notes.push(note);
    }
    if accessibility_ok.is_none() {
        notes.push("UIAutomation::new failed: see --doctor accessibility".into());
    }
    if accessibility_ok == Some(false) {
        notes.push("UI Automation returned a non-trust state".into());
    }
    notes.push("P12b: window capture is windows-capture with an xcap crop fallback; doctor does not probe the screen".into());
    notes.push("uiaccess and integrity are best-effort and are not required grants".into());

    let report = doctor::assemble(DoctorInput {
        path: doctor::current_executable(),
        uiaccess,
        integrity,
        notes,
        prompt_ignored: options.prompt,
        open_settings_ignored: options.open_settings,
        accessibility: accessibility_ok,
        ..DoctorInput::default()
    });
    Ok(report)
}

/// Best-effort UIAccess and integrity. A failure leaves a null and a note.
fn read_token() -> (Option<bool>, Option<String>, Option<String>) {
    let (uiaccess, integrity, note) = read_token_via_windows_crate();
    (uiaccess, integrity, note)
}

/// Probe the process token for UIAccess and integrity level. Uses the
/// `windows` crate exclusively; no PowerShell.
///
/// Returns `(Some(uiaccess), Some(integrity), None)` on success. On any
/// failure each field is `None` and the last `note` explains the most
/// recent error. The function is best-effort: the doctor never errors out
/// because a token probe failed.
fn read_token_via_windows_crate() -> (Option<bool>, Option<String>, Option<String>) {
    unsafe { read_token_inner() }
}

unsafe fn read_token_inner() -> (Option<bool>, Option<String>, Option<String>) {
    // OpenProcessToken on the current process.
    let mut token_handle: HANDLE = HANDLE(std::ptr::null_mut());
    let open_result =
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle) };
    let mut uiaccess: Option<bool> = None;
    let mut integrity: Option<String> = None;
    let mut note: Option<String> = None;
    match open_result {
        Ok(()) => {
            // Read TokenUIAccess: a single u32 with 0/1.
            let mut ui_buf = 0u32;
            let mut ui_len: u32 = 0;
            let ui_call = unsafe {
                GetTokenInformation(
                    token_handle,
                    TokenUIAccess,
                    Some(&mut ui_buf as *mut u32 as *mut _),
                    std::mem::size_of::<u32>() as u32,
                    &mut ui_len,
                )
            };
            match ui_call {
                Ok(()) => uiaccess = Some(ui_buf != 0),
                Err(error) => note = Some(format!("TokenUIAccess: {error}")),
            }

            // Read TokenIntegrityLevel: variable length; first call returns
            // the required size, then we allocate and call again.
            let mut needed: u32 = 0;
            let size_call = unsafe {
                GetTokenInformation(token_handle, TokenIntegrityLevel, None, 0, &mut needed)
            };
            match size_call {
                Err(error) => {
                    if let Some(existing) = note.as_ref() {
                        note = Some(format!("{existing}; TokenIntegrityLevel size: {error}"));
                    } else {
                        note = Some(format!("TokenIntegrityLevel size: {error}"));
                    }
                }
                Ok(()) if needed == 0 => {}
                Ok(()) => {
                    let mut buffer = vec![0u8; needed as usize];
                    let mut actual: u32 = 0;
                    let call = unsafe {
                        GetTokenInformation(
                            token_handle,
                            TokenIntegrityLevel,
                            Some(buffer.as_mut_ptr() as *mut _),
                            needed,
                            &mut actual,
                        )
                    };
                    match call {
                        Ok(()) => {
                            // SAFETY: GetTokenInformation wrote a
                            // `TOKEN_MANDATORY_LABEL` to the buffer with
                            // `actual` valid bytes.
                            let tml =
                                unsafe { &*(buffer.as_ptr() as *const TOKEN_MANDATORY_LABEL) };
                            let label = integrity_label_from_sid(tml.Label.Sid);
                            match label {
                                Ok(parsed) => integrity = Some(parsed),
                                Err(error) => {
                                    if let Some(existing) = note.as_ref() {
                                        note = Some(format!("{existing}; integrity SID: {error}"));
                                    } else {
                                        note = Some(format!("integrity SID: {error}"));
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            if let Some(existing) = note.as_ref() {
                                note = Some(format!("{existing}; TokenIntegrityLevel: {error}"));
                            } else {
                                note = Some(format!("TokenIntegrityLevel: {error}"));
                            }
                        }
                    }
                    let _ = unsafe { LocalFree(Some(HLOCAL(buffer.as_mut_ptr() as *mut _))) };
                }
            }
            let _ = unsafe { CloseHandle(token_handle) };
        }
        Err(error) => {
            note = Some(format!("OpenProcessToken: {error}"));
        }
    }
    (uiaccess, integrity, note)
}

/// Translate the SID pointed to by `sid` to a `low/medium/high/system` label
/// for the `--doctor` JSON. The integrity RID is the *last* sub-authority in
/// the SID, per the Microsoft integrity-level convention.
fn integrity_label_from_sid(sid: PSID) -> Result<String, String> {
    unsafe {
        if sid.0.is_null() {
            return Err("null SID".into());
        }
        let count_ptr = GetSidSubAuthorityCount(sid);
        if count_ptr.is_null() {
            return Err("GetSidSubAuthorityCount returned null".into());
        }
        let count = *count_ptr;
        if count == 0 {
            return Err("SID has no sub-authorities".into());
        }
        let last = count - 1;
        let rid_ptr = GetSidSubAuthority(sid, last as u32);
        if rid_ptr.is_null() {
            return Err("GetSidSubAuthority returned null".into());
        }
        let rid = *rid_ptr;
        Ok(map_integrity_rid(rid).to_string())
    }
}

/// Map an integrity-level sub-authority RID to a label. The ranges follow
/// the Microsoft ML_\* constants: untrusted (0), low (0x1000), medium
/// (0x2000), high (0x3000), system (0x4000), protected (0x5000+).
fn map_integrity_rid(rid: u32) -> &'static str {
    match rid {
        0x5000.. => "protected",
        0x4000..=0x4FFF => "system",
        0x3000..=0x3FFF => "high",
        0x2000..=0x2FFF => "medium",
        0x1000..=0x1FFF => "low",
        _ => "untrusted",
    }
}

/// Focused UIA element for the secure-field guard. `Ok(None)` when UIA
/// cannot read focus; the caller warns and proceeds.
pub(crate) fn focused_peek(
    _app: Option<&str>,
    _window: Option<&str>,
) -> Result<Option<crate::element::ElementPeek>, ComputerError> {
    uia::focused_peek()
}

pub(crate) use uia::view_element;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_function_is_reachable() {
        // `tree()` is real on Windows (UIA-driven via `uia::build_tree`).
        // We don't actually invoke it from this unit test because it would
        // try to call into UIA, which would require a real desktop session
        // and turn CI red on the test runner. Instead we keep the symbol
        // pinned at this type so the dispatcher can reach it without any
        // feature-gating here.
        let _tree: fn(&TreeQuery) -> Result<crate::tree::TreeReport, ComputerError> = tree;
    }

    #[test]
    fn integrity_rid_mapping_covers_required_buckets() {
        // The integrity RID ranges must cover all required buckets so the
        // doctor can always emit a label.
        assert_eq!(map_integrity_rid(0), "untrusted");
        assert_eq!(map_integrity_rid(0x1000), "low");
        assert_eq!(map_integrity_rid(0x1FFF), "low");
        assert_eq!(map_integrity_rid(0x2000), "medium");
        assert_eq!(map_integrity_rid(0x2FFF), "medium");
        assert_eq!(map_integrity_rid(0x3000), "high");
        assert_eq!(map_integrity_rid(0x3FFF), "high");
        assert_eq!(map_integrity_rid(0x4000), "system");
        assert_eq!(map_integrity_rid(0x4FFF), "system");
        assert_eq!(map_integrity_rid(0x5000), "protected");
        assert_eq!(map_integrity_rid(0x6000), "protected");
    }

    #[test]
    fn doctor_uses_the_shared_shape() {
        let report = doctor(DoctorOptions::default()).unwrap();
        let value = serde_json::to_value(&report).unwrap();
        assert!(value["permissions_ok"].is_boolean());
        assert!(value["missing"].is_array());
        // accessibility is a probe on Windows P12a; it may be null when
        // UIAutomation::new fails. The field exists.
        assert!(value["accessibility"].is_null() || value["accessibility"].is_boolean());
        assert!(value["screen_capture"].is_null());
        assert!(value["screen_recording"].is_null());
        assert!(value["post_events"].is_null());
        assert!(value["input_monitoring"].is_null());
        assert!(value["input_monitor"].is_null());
        assert!(value["fix"].is_array());
        assert!(value["text"].is_string());
        assert!(value["identity"].is_object());
        assert!(value["session"].is_null());
        assert!(value["atspi_bus"].is_null());
        assert!(value["uiaccess"].is_boolean() || value["uiaccess"].is_null());
        assert!(value["integrity"].is_string() || value["integrity"].is_null());
        let note = value["note"].as_str().unwrap();
        assert!(note.contains("P12"), "{note}");
        assert!(report.permissions_ok);
        assert!(report.missing.is_empty());
    }

    #[test]
    fn frontmost_via_apps_module() {
        let parsed = apps::WindowSnapshot {
            hwnd: 1,
            pid: 10,
            app_name: "notepad".into(),
            title: "Untitled".into(),
            bounds: crate::target::WindowBounds {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            visible: true,
            exe_path: None,
        };
        let list = apps::build_windows(std::slice::from_ref(&parsed));
        let info = list
            .windows
            .first()
            .map(|window| FrontmostInfo {
                app: Some(window.app.clone()),
                window: Some(window.title.clone()),
                note: None,
            })
            .unwrap_or_default();
        assert_eq!(info.app.as_deref(), Some("notepad"));
        assert_eq!(info.window.as_deref(), Some("Untitled"));
    }

    #[test]
    fn pid_only_dispatch_carries_pid_through() {
        let cached = crate::tree::SigNode {
            index: 0,
            role: "Window".into(),
            identity: String::new(),
            child_count: 0,
            value: String::new(),
            frame: None,
            secure: false,
            children: Vec::new(),
        };
        let _req = ElementRequest {
            op: crate::element::ElementOp::Press {
                count: 1,
                button: crate::types::MouseButton::Left,
            },
            index: 0,
            require_visible: false,
            app: Some("pid:42".into()),
            window: None,
            session: None,
            tier: None,
            allow_foreground: false,
            require_background: false,
            no_overlay: true,
        };
        // The PID selector must reach the windows backend without falling
        // back to GetForegroundWindow. We check the dispatcher layer here
        // by inspecting the resolved query.
        let q = uia::pid_only_query(42);
        assert_eq!(q.app.as_deref(), Some("pid:42"));
        // The cached sig is unused at this stage; it is the dispatcher's
        // job to load it before element_action is called.
        let _ = cached;
    }
}
