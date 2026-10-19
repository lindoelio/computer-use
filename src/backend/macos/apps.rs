//! Running applications, windows, launch, activate, and quit.
//!
//! AppKit calls stay on the CLI thread. Unit tests do not construct this module's
//! Apple types; they drive [`crate::target`] with a fake table.

#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSRunningApplication,
    NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_core_foundation::{
    kCFRunLoopDefaultMode, CFArray, CFDictionary, CFNumber, CFRunLoop, CFString, CGRect,
};
use objc2_core_graphics::{
    kCGWindowBounds, kCGWindowLayer, kCGWindowName, kCGWindowNumber, kCGWindowOwnerName,
    kCGWindowOwnerPID, CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo,
    CGWindowListOption,
};
use objc2_foundation::{NSBundle, NSError, NSString, NSURL};

use crate::error::ComputerError;
use crate::policy::{self, quit_is_protected};
use crate::target::{
    assign_z_order, canonical_exe_path, filter_windows_for_pid, parse_app_selector,
    parse_window_selector, resolve_app, resolve_window, ActivationPolicy, AppInfo, AppSelector,
    LaunchResult, ListedWindow, QuitResult, ResponsibleApp, WindowBounds, WindowList,
};

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// `kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements`.
const ON_SCREEN_WINDOWS: u32 = 1 | (1 << 4);
const PROC_PIDT_SHORTBSDINFO: c_int = 13;

#[repr(C)]
struct ProcBsdShortInfo {
    pbsi_pid: u32,
    pbsi_ppid: u32,
    pbsi_pgid: u32,
    pbsi_status: u32,
    pbsi_comm: [c_char; 16],
    pbsi_flags: u32,
    pbsi_uid: u32,
    pbsi_gid: u32,
    pbsi_ruid: u32,
    pbsi_rgid: u32,
    pbsi_svuid: u32,
    pbsi_svgid: u32,
    pbsi_rfu: u32,
}

unsafe extern "C" {
    fn proc_pidinfo(
        pid: c_int,
        flavor: c_int,
        arg: u64,
        buffer: *mut c_void,
        buffersize: c_int,
    ) -> c_int;
    /// TCC's responsible pid, or `-1`. Present in libSystem.
    /// A host reparented to launchd is not this pid's parent.
    fn responsibility_get_pid_responsible_for_pid(pid: c_int) -> c_int;
}

pub(crate) fn list_apps(all: bool) -> Result<Vec<AppInfo>, ComputerError> {
    let workspace = NSWorkspace::sharedWorkspace();
    let running = workspace.runningApplications();
    let mut apps = Vec::new();
    for app in running.iter() {
        if app.isTerminated() {
            continue;
        }
        if !all && app.activationPolicy() != NSApplicationActivationPolicy::Regular {
            continue;
        }
        apps.push(app_info(&app));
    }
    Ok(apps)
}

/// On-screen windows in CG z-order, without the accessibility join.
pub(crate) fn on_screen_windows() -> Result<Vec<crate::target::WindowInfo>, ComputerError> {
    Ok(assign_z_order(copy_window_list()?))
}

pub(crate) fn list_windows() -> Result<WindowList, ComputerError> {
    let mut windows = on_screen_windows()?;
    let note = super::ax::annotate_matches(&mut windows);
    Ok(WindowList { windows, note })
}

/// Parent pid from `PROC_PIDT_SHORTBSDINFO.pbsi_ppid`.
pub(crate) fn parent_process_id(pid: i32) -> Option<i32> {
    parent_pid(pid)
}

pub(crate) fn launch(selector: &str) -> Result<LaunchResult, ComputerError> {
    let parsed = parse_app_selector(selector)?;
    policy::deny_or_allow(policy::selector_denial(&parsed))?;
    let target = resolve_launch_target(&parsed)?;
    policy::deny_or_allow(policy::identity_denial(
        target.bundle_id.as_deref(),
        target.exe_path.as_deref(),
        None,
        None,
    ))?;
    let pid = open_bundle(&target.bundle_path)?;
    Ok(LaunchResult {
        name: target.name,
        bundle_id: target.bundle_id,
        pid,
        path: target
            .exe_path
            .map(|path| path.to_string_lossy().into_owned()),
        window_id: None,
        window_title: None,
    })
}

pub(crate) fn activate(app: &str, window: Option<&str>) -> Result<LaunchResult, ComputerError> {
    let (info, chosen) = resolve_running(app, window)?;
    let running = running_app(info.pid)?;
    // SAFETY: NSApplicationActivateIgnoringOtherApps is the option the P1 contract names.
    // It is deprecated on macOS 14 and still the documented way to activate over the front app.
    let accepted = {
        #[allow(deprecated)]
        running.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps)
    };
    if !accepted {
        return Err(ComputerError::ExecutionFailed(format!(
            "activate was not accepted for {} (pid {})",
            info.name, info.pid
        )));
    }
    Ok(LaunchResult {
        name: info.name,
        bundle_id: info.bundle_id,
        pid: info.pid,
        path: info.path,
        window_id: chosen.as_ref().map(|item| item.id),
        window_title: chosen.map(|item| item.title),
    })
}

pub(crate) fn quit(app: &str) -> Result<QuitResult, ComputerError> {
    let (info, _) = resolve_running(app, None)?;
    if quit_is_protected(
        info.pid,
        std::process::id() as i32,
        info.bundle_id.as_deref(),
        &info.name,
        info.path.as_deref(),
    ) {
        return Err(ComputerError::PolicyDenied(
            "refusing to quit loginwindow, WindowServer, or this process".into(),
        ));
    }
    let running = running_app(info.pid)?;
    let accepted = running.terminate();
    if !accepted {
        return Err(ComputerError::ExecutionFailed(format!(
            "quit was not accepted for {} (pid {})",
            info.name, info.pid
        )));
    }
    Ok(QuitResult {
        name: info.name,
        bundle_id: info.bundle_id,
        pid: info.pid,
        accepted,
    })
}

/// Walk parent processes to the GUI `.app` that owns this process.
///
/// Agent hosts are often reparented to launchd, so the parent chain stops
/// before the bundle. `responsibility_get_pid_responsible_for_pid` returns
/// the pid TCC checks in that case.
pub(crate) fn responsible_app() -> Option<ResponsibleApp> {
    if let Some(app) = ancestor_bundle_app(std::process::id() as i32) {
        return Some(app);
    }
    responsible_pid().and_then(bundle_app)
}

fn ancestor_bundle_app(mut pid: i32) -> Option<ResponsibleApp> {
    for _ in 0..16 {
        if pid <= 1 {
            break;
        }
        if let Some(app) = bundle_app(pid) {
            return Some(app);
        }
        pid = parent_pid(pid)?;
    }
    None
}

fn responsible_pid() -> Option<i32> {
    // SAFETY: the function returns one pid or -1 and writes no caller memory.
    // `responsibility_get_responsible_for_pid` is a different, four-argument
    // symbol. Calling that one with two arguments leaves x3 uninitialized.
    let pid = unsafe { responsibility_get_pid_responsible_for_pid(std::process::id() as c_int) };
    if pid <= 1 {
        None
    } else {
        Some(pid)
    }
}

struct LaunchTarget {
    name: String,
    bundle_id: Option<String>,
    exe_path: Option<PathBuf>,
    bundle_path: PathBuf,
}

fn resolve_running(
    app: &str,
    window: Option<&str>,
) -> Result<(AppInfo, Option<crate::target::WindowInfo>), ComputerError> {
    let selector = parse_app_selector(app)?;
    policy::deny_or_allow(policy::selector_denial(&selector))?;
    let apps = list_apps(true)?;
    let info = resolve_app(&apps, &selector)?.clone();
    // A window list needs Screen Recording. Skip it when the caller did not name a window.
    let chosen = match window {
        Some(window) => {
            let selector = parse_window_selector(window)?;
            let listed = list_windows()?;
            let owned = filter_windows_for_pid(&listed.windows, info.pid);
            Some(resolve_window(&owned, &selector)?.clone())
        }
        None => None,
    };
    policy::deny_or_allow(policy::identity_denial(
        info.bundle_id.as_deref(),
        info.path.as_deref().map(Path::new),
        chosen.as_ref().map(|item| item.title.as_str()),
        None,
    ))?;
    Ok((info, chosen))
}

fn resolve_launch_target(selector: &AppSelector) -> Result<LaunchTarget, ComputerError> {
    match selector {
        AppSelector::Pid(pid) => launch_target_from_pid(*pid),
        AppSelector::BundleId(bundle_id) => launch_target_from_bundle_id(bundle_id),
        AppSelector::Name(name) => launch_target_from_name(name),
        AppSelector::Path(path) => launch_target_from_path(path),
    }
}

fn launch_target_from_pid(pid: i32) -> Result<LaunchTarget, ComputerError> {
    let running = running_app(pid)?;
    let info = app_info(&running);
    let bundle = running
        .bundleURL()
        .and_then(|url| url_path(&url))
        .ok_or_else(|| {
            ComputerError::AppNotFound(format!("pid {pid} has no application bundle"))
        })?;
    Ok(LaunchTarget {
        name: info.name,
        bundle_id: info.bundle_id,
        exe_path: info.path.map(PathBuf::from),
        bundle_path: bundle,
    })
}

fn launch_target_from_bundle_id(bundle_id: &str) -> Result<LaunchTarget, ComputerError> {
    let workspace = NSWorkspace::sharedWorkspace();
    let identifier = NSString::from_str(bundle_id);
    if let Some(url) = workspace.URLForApplicationWithBundleIdentifier(&identifier) {
        return target_from_bundle_url(&url);
    }
    let apps = list_apps(true)?;
    let info = resolve_app(&apps, &AppSelector::BundleId(bundle_id.to_string()))?;
    launch_target_from_pid(info.pid)
}

fn launch_target_from_name(name: &str) -> Result<LaunchTarget, ComputerError> {
    if let Some(path) = find_app_by_name(name) {
        return target_from_bundle_path(&path);
    }
    let apps = list_apps(true)?;
    match resolve_app(&apps, &AppSelector::Name(name.to_string())) {
        Ok(info) => launch_target_from_pid(info.pid),
        Err(_) => Err(ComputerError::AppNotFound(name.to_string())),
    }
}

fn launch_target_from_path(path: &Path) -> Result<LaunchTarget, ComputerError> {
    let bundle = app_bundle_path(path)?;
    if bundle.exists() {
        return target_from_bundle_path(&bundle);
    }
    if let Some(stem) = bundle.file_stem().and_then(|stem| stem.to_str()) {
        if let Some(found) = find_app_by_name(stem) {
            return target_from_bundle_path(&found);
        }
    }
    Err(ComputerError::AppNotFound(format!(
        "application not found at {}",
        path.display()
    )))
}

fn target_from_bundle_path(path: &Path) -> Result<LaunchTarget, ComputerError> {
    let text = path.to_string_lossy();
    let url = NSURL::fileURLWithPath(&NSString::from_str(&text));
    target_from_bundle_url(&url)
}

fn target_from_bundle_url(url: &NSURL) -> Result<LaunchTarget, ComputerError> {
    let bundle_path = url_path(url).ok_or_else(|| {
        ComputerError::AppNotFound("application URL has no filesystem path".into())
    })?;
    let name = bundle_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("application")
        .to_string();
    let bundle = NSBundle::bundleWithURL(url);
    let bundle_id = bundle
        .as_ref()
        .and_then(|bundle| bundle.bundleIdentifier())
        .map(|identifier| identifier.to_string());
    let exe_path = bundle
        .as_ref()
        .and_then(|bundle| bundle.executableURL())
        .and_then(|url| url_path(&url))
        .map(|path| canonical_exe_path(&path));
    Ok(LaunchTarget {
        name,
        bundle_id,
        exe_path,
        bundle_path,
    })
}

fn open_bundle(bundle_path: &Path) -> Result<i32, ComputerError> {
    let workspace = NSWorkspace::sharedWorkspace();
    let url = NSURL::fileURLWithPath(&NSString::from_str(&bundle_path.to_string_lossy()));
    let configuration = NSWorkspaceOpenConfiguration::new();
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(move |app: *mut NSRunningApplication, error: *mut NSError| {
        let result = if !error.is_null() {
            // SAFETY: AppKit passes a live NSError for the duration of this callback, or null.
            let message = unsafe { &*error }.localizedDescription().to_string();
            Err(message)
        } else if app.is_null() {
            Err("launch returned no application".into())
        } else {
            // SAFETY: AppKit passes a live NSRunningApplication for the duration of this callback.
            Ok(unsafe { &*app }.processIdentifier())
        };
        let _ = sender.send(result);
    });
    workspace.openApplicationAtURL_configuration_completionHandler(
        &url,
        &configuration,
        Some(&block),
    );
    wait_for_launch(receiver)
}

fn wait_for_launch(receiver: mpsc::Receiver<Result<i32, String>>) -> Result<i32, ComputerError> {
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    loop {
        if let Ok(result) = receiver.try_recv() {
            return result.map_err(ComputerError::ExecutionFailed);
        }
        if Instant::now() >= deadline {
            return Err(ComputerError::ExecutionFailed(
                "launch timed out after 30s".into(),
            ));
        }
        // SAFETY: kCFRunLoopDefaultMode is a process-lifetime mode string.
        // Pumping the main run loop lets a main-queue completion handler run.
        let mode = unsafe { kCFRunLoopDefaultMode };
        let _ = CFRunLoop::run_in_mode(mode, 0.05, false);
    }
}

fn copy_window_list() -> Result<Vec<ListedWindow>, ComputerError> {
    let options = CGWindowListOption(ON_SCREEN_WINDOWS);
    let Some(array) = CGWindowListCopyWindowInfo(options, 0) else {
        if !objc2_core_graphics::CGPreflightScreenCaptureAccess() {
            return Err(ComputerError::PermissionRequired(
                "Screen Recording is off. System Settings → Privacy & Security → Screen Recording → enable the app named by computer-use doctor".into(),
            ));
        }
        return Err(ComputerError::ExecutionFailed(
            "CGWindowListCopyWindowInfo returned no window list".into(),
        ));
    };
    Ok(windows_from_array(&array))
}

fn windows_from_array(array: &CFArray) -> Vec<ListedWindow> {
    let count = array.count();
    let mut windows = Vec::with_capacity(count.max(0) as usize);
    for index in 0..count {
        // SAFETY: index is in range. CGWindowListCopyWindowInfo stores CFDictionaries.
        // The array retain keeps each dictionary alive for this call.
        let value = unsafe { array.value_at_index(index) };
        if value.is_null() {
            continue;
        }
        let dict = unsafe { &*(value.cast::<CFDictionary>()) };
        if let Some(window) = window_from_dict(dict) {
            windows.push(window);
        }
    }
    windows
}

fn window_from_dict(dict: &CFDictionary) -> Option<ListedWindow> {
    let id = cf_i64(dict, unsafe { kCGWindowNumber })?.try_into().ok()?;
    if id == 0 {
        return None;
    }
    let pid = cf_i64(dict, unsafe { kCGWindowOwnerPID })?;
    let pid = i32::try_from(pid).ok()?;
    let app = cf_string(dict, unsafe { kCGWindowOwnerName });
    let title = cf_string(dict, unsafe { kCGWindowName });
    let layer = cf_i64(dict, unsafe { kCGWindowLayer })
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(0);
    let bounds = cf_bounds(dict, unsafe { kCGWindowBounds });
    Some(ListedWindow {
        id,
        pid,
        app,
        title,
        bounds,
        layer,
        on_screen: true,
    })
}

fn cf_string(dict: &CFDictionary, key: &CFString) -> String {
    // SAFETY: key is a process-lifetime CFString. The returned value is borrowed from dict.
    let value = unsafe { dict.value(std::ptr::from_ref(key).cast()) };
    if value.is_null() {
        return String::new();
    }
    // SAFETY: window-name values are CFStrings. dict keeps the value alive.
    unsafe { &*value.cast::<CFString>() }.to_string()
}

fn cf_i64(dict: &CFDictionary, key: &CFString) -> Option<i64> {
    // SAFETY: key is a process-lifetime CFString. The value is borrowed from dict.
    let value = unsafe { dict.value(std::ptr::from_ref(key).cast()) };
    if value.is_null() {
        return None;
    }
    let number = unsafe { &*value.cast::<CFNumber>() };
    number.as_i64()
}

fn cf_bounds(dict: &CFDictionary, key: &CFString) -> WindowBounds {
    // SAFETY: key is a process-lifetime CFString. A present bounds value is a CFDictionary.
    let value = unsafe { dict.value(std::ptr::from_ref(key).cast()) };
    if value.is_null() {
        return empty_bounds();
    }
    let bounds_dict = unsafe { &*value.cast::<CFDictionary>() };
    let mut rect = CGRect::default();
    // SAFETY: bounds_dict is a CGRect dictionary for this call. rect is a live out-parameter.
    let ok = unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds_dict), &mut rect) };
    if !ok {
        return empty_bounds();
    }
    WindowBounds {
        x: round_i32(rect.origin.x),
        y: round_i32(rect.origin.y),
        width: round_u32(rect.size.width),
        height: round_u32(rect.size.height),
    }
}

fn empty_bounds() -> WindowBounds {
    WindowBounds {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    }
}

fn round_i32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32
}

fn round_u32(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    value.round().min(u32::MAX as f64) as u32
}

fn app_info(app: &NSRunningApplication) -> AppInfo {
    let pid = app.processIdentifier();
    let bundle_id = app.bundleIdentifier().map(|value| value.to_string());
    let name = app
        .localizedName()
        .map(|value| value.to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| bundle_id.clone())
        .unwrap_or_else(|| format!("pid:{pid}"));
    let path = app
        .executableURL()
        .and_then(|url| url_path(&url))
        .map(|path| canonical_exe_path(&path).to_string_lossy().into_owned());
    AppInfo {
        name,
        bundle_id,
        pid,
        path,
        active: app.isActive(),
        hidden: app.isHidden(),
        activation_policy: activation_policy(app.activationPolicy()),
    }
}

fn activation_policy(policy: NSApplicationActivationPolicy) -> ActivationPolicy {
    if policy == NSApplicationActivationPolicy::Regular {
        ActivationPolicy::Regular
    } else if policy == NSApplicationActivationPolicy::Accessory {
        ActivationPolicy::Accessory
    } else if policy == NSApplicationActivationPolicy::Prohibited {
        ActivationPolicy::Prohibited
    } else {
        ActivationPolicy::Unknown
    }
}

fn bundle_app(pid: i32) -> Option<ResponsibleApp> {
    let app = running_app(pid).ok()?;
    let bundle_id = app.bundleIdentifier().map(|value| value.to_string())?;
    let path = app.bundleURL().and_then(|url| url_path(&url))?;
    if !path.to_string_lossy().contains(".app") {
        return None;
    }
    let name = app
        .localizedName()
        .map(|value| value.to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| bundle_id.clone());
    Some(ResponsibleApp {
        name,
        bundle_id: Some(bundle_id),
        pid,
        path: Some(path.to_string_lossy().into_owned()),
    })
}

fn running_app(pid: i32) -> Result<Retained<NSRunningApplication>, ComputerError> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid).ok_or_else(|| {
        ComputerError::AppNotFound(format!("pid {pid} is not a running application"))
    })
}

fn url_path(url: &NSURL) -> Option<PathBuf> {
    url.path().map(|path| PathBuf::from(path.to_string()))
}

fn parent_pid(pid: i32) -> Option<i32> {
    let mut info = std::mem::MaybeUninit::<ProcBsdShortInfo>::uninit();
    // SAFETY: the buffer matches proc_bsdshortinfo and the size argument is that struct.
    // On success proc_pidinfo fills it. On failure it returns <= 0 and we discard the buffer.
    let written = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDT_SHORTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<ProcBsdShortInfo>() as c_int,
        )
    };
    if written < std::mem::size_of::<ProcBsdShortInfo>() as c_int {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let parent = info.pbsi_ppid as i32;
    if parent <= 0 || parent == pid {
        None
    } else {
        Some(parent)
    }
}

fn find_app_by_name(name: &str) -> Option<PathBuf> {
    let wanted = name.strip_suffix(".app").unwrap_or(name);
    for root in application_roots() {
        if let Some(found) = search_apps(&root, wanted, 0) {
            return Some(found);
        }
    }
    None
}

fn application_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots
}

fn search_apps(dir: &Path, wanted: &str, depth: usize) -> Option<PathBuf> {
    if depth > 2 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if let Some(stem) = name.strip_suffix(".app") {
            if stem.eq_ignore_ascii_case(wanted) {
                return Some(path);
            }
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if let Some(found) = search_apps(&path, wanted, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

fn app_bundle_path(path: &Path) -> Result<PathBuf, ComputerError> {
    let mut cursor = path.to_path_buf();
    loop {
        if cursor
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        {
            return Ok(cursor);
        }
        if !cursor.pop() {
            break;
        }
    }
    Err(ComputerError::AppNotFound(format!(
        "path is not an application bundle: {}",
        path.display()
    )))
}
