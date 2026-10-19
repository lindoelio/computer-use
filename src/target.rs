//! Application and window selectors.
//!
//! Resolution runs against a table the backend filled in. Unit tests pass a
//! fake table and never call AppKit or CoreGraphics.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ComputerError;

/// How the caller named an application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppSelector {
    /// Process id.
    Pid(i32),
    /// Bundle identifier such as `com.apple.TextEdit`.
    BundleId(String),
    /// Localized name. May contain quotes and spaces.
    Name(String),
    /// Filesystem path of a `.app` bundle or an executable.
    Path(PathBuf),
}

/// How the caller named a window.
///
/// A bare number is a z-order index, with 0 in front. A window id uses the
/// `id:` prefix so it is not confused with that index. Any other string is a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowSelector {
    /// Index in the listed z-order. 0 is front.
    Index(u32),
    /// `kCGWindowNumber`.
    Id(u32),
    /// Window title.
    Title(String),
}

/// How macOS classifies a process. Helpers are accessory or prohibited.
///
/// `list-apps` omits this field. Missing values deserialize as [`ActivationPolicy::Regular`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActivationPolicy {
    /// Foreground app (`NSApplicationActivationPolicyRegular`).
    #[default]
    Regular,
    /// Menu-bar or agent app.
    Accessory,
    /// Process that does not normally activate. Helper processes use this.
    Prohibited,
    /// The platform did not report a policy.
    Unknown,
}

/// One running application, as returned by `list_apps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// Localized name.
    pub name: String,
    /// Bundle identifier, when the process has one.
    pub bundle_id: Option<String>,
    /// Process id.
    pub pid: i32,
    /// Canonical executable path, when it could be read.
    pub path: Option<String>,
    /// Whether the app is frontmost.
    pub active: bool,
    /// Whether the app is hidden.
    pub hidden: bool,
    /// Activation policy. Omitted from `list-apps` JSON.
    #[serde(default, skip_serializing)]
    pub activation_policy: ActivationPolicy,
}

/// Rectangle in logical screen points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowBounds {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width. Zero when the platform omitted the frame.
    pub width: u32,
    /// Height. Zero when the platform omitted the frame.
    pub height: u32,
}

/// One window, as returned by `list_windows`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Platform window id.
    pub id: u32,
    /// Owning process id.
    pub pid: i32,
    /// Owner name.
    pub app: String,
    /// Window title. Empty when Screen Recording hides it.
    pub title: String,
    /// Z-order index in this result. 0 is front.
    pub z: u32,
    /// Screen bounds in logical points.
    pub bounds: WindowBounds,
    /// Window layer. Normal windows are 0.
    pub layer: i32,
    /// Whether the window was listed as on screen.
    pub on_screen: bool,
    /// Whether an accessibility window was joined to this row by bounds.
    pub ax_matched: bool,
}

/// A window row before z-order indexes are assigned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedWindow {
    /// Platform window id.
    pub id: u32,
    /// Owning process id.
    pub pid: i32,
    /// Owner name.
    pub app: String,
    /// Window title.
    pub title: String,
    /// Screen bounds.
    pub bounds: WindowBounds,
    /// Window layer.
    pub layer: i32,
    /// On-screen flag from the window server.
    pub on_screen: bool,
}

/// Windows plus an optional note about how they were collected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WindowList {
    /// Windows in z-order, index 0 in front.
    pub windows: Vec<WindowInfo>,
    /// Why the list is incomplete, when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Result of resolving an app and an optional window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// Process id.
    pub pid: i32,
    /// Bundle identifier.
    pub bundle_id: Option<String>,
    /// Localized name.
    pub display_name: String,
    /// Canonical executable path.
    pub exe_path: Option<PathBuf>,
    /// Window id, when a window was requested or chosen.
    pub window_id: Option<u32>,
    /// Window title.
    pub window_title: Option<String>,
    /// Window bounds.
    pub bounds: Option<WindowBounds>,
}

/// An application that was launched or activated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaunchResult {
    /// Localized name.
    pub name: String,
    /// Bundle identifier.
    pub bundle_id: Option<String>,
    /// Process id after launch.
    pub pid: i32,
    /// Canonical executable path.
    pub path: Option<String>,
    /// Window id when `activate` resolved one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<u32>,
    /// Window title when `activate` resolved one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
}

/// `quit` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuitResult {
    /// Localized name.
    pub name: String,
    /// Bundle identifier.
    pub bundle_id: Option<String>,
    /// Process id the terminate request was sent to.
    pub pid: i32,
    /// Whether the platform accepted the request. The process may still be exiting.
    pub accepted: bool,
}

/// The GUI app that owns this process for TCC prompts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResponsibleApp {
    /// Localized name.
    pub name: String,
    /// Bundle identifier.
    pub bundle_id: Option<String>,
    /// Process id of that app, not of this binary.
    pub pid: i32,
    /// Bundle path.
    pub path: Option<String>,
}

/// `doctor` report. Null probes mean this OS does not check them yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    /// Accessibility trust. Null until the platform probe exists.
    pub accessibility: Option<bool>,
    /// Screen capture trust. Null until the platform probe exists.
    pub screen_capture: Option<bool>,
    /// Input monitor / event tap. Null until P6.
    pub input_monitor: Option<bool>,
    /// Parent GUI app that must be toggled in Settings, when it could be found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responsible_app: Option<ResponsibleApp>,
    /// Settings paths the user can follow.
    pub fixes: Vec<String>,
    /// Extra context. Omitted when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl DoctorReport {
    /// Report used on platforms whose probes land in a later phase.
    pub fn pending(phase: &str) -> Self {
        Self {
            accessibility: None,
            screen_capture: None,
            input_monitor: None,
            responsible_app: None,
            fixes: Vec::new(),
            note: Some(format!(
                "accessibility and screen-capture probes land in {phase}"
            )),
        }
    }
}

/// Parse `name`, `bundle id`, `pid`, or a path.
pub fn parse_app_selector(text: &str) -> Result<AppSelector, ComputerError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "app selector is empty".into(),
        ));
    }
    if looks_like_path(text) {
        return Ok(AppSelector::Path(PathBuf::from(expand_tilde(text))));
    }
    if text.chars().all(|ch| ch.is_ascii_digit()) {
        let pid = text
            .parse::<i32>()
            .map_err(|_| ComputerError::InvalidArguments(format!("pid is out of range: {text}")))?;
        return Ok(AppSelector::Pid(pid));
    }
    if is_bundle_id(text) {
        return Ok(AppSelector::BundleId(text.to_string()));
    }
    Ok(AppSelector::Name(text.to_string()))
}

/// Parse a window title, z-order index, `index:N`, or `id:N`.
pub fn parse_window_selector(text: &str) -> Result<WindowSelector, ComputerError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "window selector is empty".into(),
        ));
    }
    if let Some(rest) = text
        .strip_prefix("id:")
        .or_else(|| text.strip_prefix("ID:"))
    {
        return Ok(WindowSelector::Id(parse_u32(rest, "window id")?));
    }
    if let Some(rest) = text
        .strip_prefix("index:")
        .or_else(|| text.strip_prefix("INDEX:"))
    {
        return Ok(WindowSelector::Index(parse_u32(rest, "window index")?));
    }
    if text.chars().all(|ch| ch.is_ascii_digit()) {
        return Ok(WindowSelector::Index(parse_u32(text, "window index")?));
    }
    Ok(WindowSelector::Title(text.to_string()))
}

/// True when `frame` is within 2 points of `bounds` on each edge and size.
pub fn bounds_match_frame(bounds: WindowBounds, frame: crate::tree::NodeFrame) -> bool {
    (f64::from(bounds.x) - frame.x).abs() <= 2.0
        && (f64::from(bounds.y) - frame.y).abs() <= 2.0
        && (f64::from(bounds.width) - frame.width).abs() <= 2.0
        && (f64::from(bounds.height) - frame.height).abs() <= 2.0
}

/// Assign z-order from the CG array order. Index 0 is the front window.
pub fn assign_z_order(entries: Vec<ListedWindow>) -> Vec<WindowInfo> {
    entries
        .into_iter()
        .enumerate()
        .map(|(z, entry)| WindowInfo {
            id: entry.id,
            pid: entry.pid,
            app: entry.app,
            title: entry.title,
            z: z as u32,
            bounds: entry.bounds,
            layer: entry.layer,
            on_screen: entry.on_screen,
            ax_matched: false,
        })
        .collect()
}

/// Keep one process and renumber z-order so 0 is that app's front window.
pub fn filter_windows_for_pid(windows: &[WindowInfo], pid: i32) -> Vec<WindowInfo> {
    windows
        .iter()
        .filter(|window| window.pid == pid)
        .enumerate()
        .map(|(z, window)| {
            let mut copy = window.clone();
            copy.z = z as u32;
            copy
        })
        .collect()
}

/// Resolve `selector` against `apps`.
pub fn resolve_app<'a>(
    apps: &'a [AppInfo],
    selector: &AppSelector,
) -> Result<&'a AppInfo, ComputerError> {
    if let AppSelector::Name(name) = selector {
        let exact: Vec<&AppInfo> = apps
            .iter()
            .filter(|app| app.name.eq_ignore_ascii_case(name))
            .collect();
        if !exact.is_empty() {
            return pick_one(&exact, name);
        }
    }
    let matches: Vec<&AppInfo> = apps
        .iter()
        .filter(|app| app_matches(app, selector))
        .collect();
    if matches.is_empty() {
        return Err(ComputerError::AppNotFound(selector_label(selector)));
    }
    pick_one(&matches, &selector_label(selector))
}

/// Resolve an optional window inside an already filtered list.
pub fn resolve_window<'a>(
    windows: &'a [WindowInfo],
    selector: &WindowSelector,
) -> Result<&'a WindowInfo, ComputerError> {
    let found = match selector {
        WindowSelector::Index(index) => windows.iter().find(|window| window.z == *index),
        WindowSelector::Id(id) => windows.iter().find(|window| window.id == *id),
        WindowSelector::Title(title) => {
            let exact = windows
                .iter()
                .find(|window| window.title.eq_ignore_ascii_case(title));
            if exact.is_some() {
                exact
            } else {
                let needle = title.to_ascii_lowercase();
                windows
                    .iter()
                    .find(|window| window.title.to_ascii_lowercase().contains(&needle))
            }
        }
    };
    found.ok_or_else(|| ComputerError::WindowNotFound(window_label(selector)))
}

/// Resolve an app and, when requested, one of its windows.
pub fn resolve(
    apps: &[AppInfo],
    windows: &[WindowInfo],
    app: &AppSelector,
    window: Option<&WindowSelector>,
) -> Result<ResolvedTarget, ComputerError> {
    let app = resolve_app(apps, app)?;
    let owned = filter_windows_for_pid(windows, app.pid);
    let chosen = match window {
        Some(selector) => Some(resolve_window(&owned, selector)?.clone()),
        None => None,
    };
    Ok(ResolvedTarget {
        pid: app.pid,
        bundle_id: app.bundle_id.clone(),
        display_name: app.name.clone(),
        exe_path: app.path.as_ref().map(PathBuf::from),
        window_id: chosen.as_ref().map(|item| item.id),
        window_title: chosen.as_ref().map(|item| item.title.clone()),
        bounds: chosen.as_ref().map(|item| item.bounds),
    })
}

/// Canonicalize when the path exists. Otherwise keep the original path.
pub fn canonical_exe_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn pick_one<'a>(matches: &[&'a AppInfo], label: &str) -> Result<&'a AppInfo, ComputerError> {
    let regular: Vec<&AppInfo> = matches
        .iter()
        .copied()
        .filter(|app| app.activation_policy == ActivationPolicy::Regular)
        .collect();
    let pool: &[&AppInfo] = if regular.is_empty() {
        matches
    } else {
        &regular
    };
    if pool.len() == 1 {
        return Ok(pool[0]);
    }
    let active: Vec<&AppInfo> = pool.iter().copied().filter(|app| app.active).collect();
    if active.len() == 1 {
        return Ok(active[0]);
    }
    let pids: Vec<String> = pool.iter().map(|app| app.pid.to_string()).collect();
    Err(ComputerError::InvalidArguments(format!(
        "app selector {label:?} matched {} processes ({})",
        pool.len(),
        pids.join(", ")
    )))
}

fn app_matches(app: &AppInfo, selector: &AppSelector) -> bool {
    match selector {
        AppSelector::Pid(pid) => app.pid == *pid,
        AppSelector::BundleId(bundle_id) => app
            .bundle_id
            .as_deref()
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(bundle_id)),
        AppSelector::Name(name) => {
            app.name.eq_ignore_ascii_case(name) || path_names_app(app.path.as_deref(), name)
        }
        AppSelector::Path(path) => paths_match(app.path.as_deref(), path),
    }
}

fn path_names_app(path: Option<&str>, name: &str) -> bool {
    let Some(path) = path else {
        return false;
    };
    let wanted_app = format!("{name}.app");
    Path::new(path).components().any(|component| {
        let text = component.as_os_str().to_string_lossy();
        text.eq_ignore_ascii_case(&wanted_app)
            || text.eq_ignore_ascii_case(name)
                && Path::new(path)
                    .file_name()
                    .and_then(|file| file.to_str())
                    .is_some_and(|file| file.eq_ignore_ascii_case(name))
    })
}

fn paths_match(stored: Option<&str>, wanted: &Path) -> bool {
    let Some(stored) = stored else {
        return false;
    };
    let stored_path = Path::new(stored);
    stored_path == wanted || canonical_exe_path(stored_path) == canonical_exe_path(wanted)
}

fn looks_like_path(text: &str) -> bool {
    text.contains('/')
        || text.contains('\\')
        || text.to_ascii_lowercase().ends_with(".app")
        || text.to_ascii_lowercase().ends_with(".exe")
}

fn is_bundle_id(text: &str) -> bool {
    let mut labels = text.split('.');
    let Some(first) = labels.next() else {
        return false;
    };
    if !is_bundle_label(first) {
        return false;
    }
    let mut count = 1;
    for label in labels {
        if !is_bundle_label(label) {
            return false;
        }
        count += 1;
    }
    count >= 2
}

fn is_bundle_label(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn expand_tilde(text: &str) -> String {
    let Some(rest) = text.strip_prefix("~/") else {
        return text.to_string();
    };
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return text.to_string();
    };
    Path::new(&home).join(rest).to_string_lossy().into_owned()
}

fn parse_u32(text: &str, name: &str) -> Result<u32, ComputerError> {
    text.parse::<u32>()
        .map_err(|_| ComputerError::InvalidArguments(format!("{name} is not an integer: {text}")))
}

fn selector_label(selector: &AppSelector) -> String {
    match selector {
        AppSelector::Pid(pid) => pid.to_string(),
        AppSelector::BundleId(bundle_id) => bundle_id.clone(),
        AppSelector::Name(name) => name.clone(),
        AppSelector::Path(path) => path.display().to_string(),
    }
}

fn window_label(selector: &WindowSelector) -> String {
    match selector {
        WindowSelector::Index(index) => index.to_string(),
        WindowSelector::Id(id) => format!("id:{id}"),
        WindowSelector::Title(title) => title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apps() -> Vec<AppInfo> {
        vec![
            AppInfo {
                name: "Notes".into(),
                bundle_id: Some("com.apple.Notes".into()),
                pid: 10,
                path: Some("/System/Applications/Notes.app/Contents/MacOS/Notes".into()),
                active: true,
                hidden: false,
                activation_policy: ActivationPolicy::Regular,
            },
            AppInfo {
                name: "Notes".into(),
                bundle_id: Some("com.example.Notes".into()),
                pid: 11,
                path: Some("/Applications/Notes.app/Contents/MacOS/Notes".into()),
                active: false,
                hidden: false,
                activation_policy: ActivationPolicy::Regular,
            },
            AppInfo {
                name: r#"Foo "Bar""#.into(),
                bundle_id: Some("dev.example.foo-bar".into()),
                pid: 412,
                path: Some(r#"/Applications/Foo "Bar".app/Contents/MacOS/Foo"#.into()),
                active: false,
                hidden: false,
                activation_policy: ActivationPolicy::Regular,
            },
            AppInfo {
                name: "Terminal".into(),
                bundle_id: Some("com.apple.Terminal".into()),
                pid: 20,
                path: Some("/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal".into()),
                active: false,
                hidden: false,
                activation_policy: ActivationPolicy::Regular,
            },
        ]
    }

    fn windows() -> Vec<WindowInfo> {
        assign_z_order(vec![
            ListedWindow {
                id: 100,
                pid: 10,
                app: "Notes".into(),
                title: "Shopping".into(),
                bounds: WindowBounds {
                    x: 0,
                    y: 0,
                    width: 800,
                    height: 600,
                },
                layer: 0,
                on_screen: true,
            },
            ListedWindow {
                id: 200,
                pid: 20,
                app: "Terminal".into(),
                title: "zsh".into(),
                bounds: WindowBounds {
                    x: 10,
                    y: 10,
                    width: 400,
                    height: 200,
                },
                layer: 0,
                on_screen: true,
            },
            ListedWindow {
                id: 101,
                pid: 10,
                app: "Notes".into(),
                title: "Hello".into(),
                bounds: WindowBounds {
                    x: 20,
                    y: 30,
                    width: 100,
                    height: 50,
                },
                layer: 3,
                on_screen: true,
            },
        ])
    }

    #[test]
    fn parser_accepts_pid_bundle_path_and_a_name_with_a_quote() {
        assert_eq!(parse_app_selector("412").unwrap(), AppSelector::Pid(412));
        assert_eq!(
            parse_app_selector("com.apple.TextEdit").unwrap(),
            AppSelector::BundleId("com.apple.TextEdit".into())
        );
        assert_eq!(
            parse_app_selector(r#"Foo "Bar""#).unwrap(),
            AppSelector::Name(r#"Foo "Bar""#.into())
        );
        assert!(matches!(
            parse_app_selector("/Applications/Notes.app").unwrap(),
            AppSelector::Path(_)
        ));
        assert!(parse_app_selector("   ").is_err());
        assert!(parse_app_selector("com..apple").is_err() || !is_bundle_id("com..apple"));
        assert!(!is_bundle_id(r#"Foo "Bar""#));
    }

    #[test]
    fn resolve_matches_quote_name_bundle_and_pid_against_a_fake_table() {
        let apps = apps();
        let quoted = parse_app_selector(r#"Foo "Bar""#).unwrap();
        let resolved = resolve_app(&apps, &quoted).unwrap();
        assert_eq!(resolved.pid, 412);
        assert_eq!(resolved.name, r#"Foo "Bar""#);

        let bundle = parse_app_selector("com.apple.Terminal").unwrap();
        assert_eq!(resolve_app(&apps, &bundle).unwrap().pid, 20);

        let pid = parse_app_selector("10").unwrap();
        assert_eq!(
            resolve_app(&apps, &pid).unwrap().bundle_id.as_deref(),
            Some("com.apple.Notes")
        );

        assert!(resolve_app(&apps, &AppSelector::Pid(999)).is_err());
    }

    #[test]
    fn ambiguous_name_prefers_the_active_app() {
        let apps = apps();
        let notes = AppSelector::Name("Notes".into());
        assert_eq!(resolve_app(&apps, &notes).unwrap().pid, 10);

        let mut both_inactive = apps.clone();
        both_inactive[0].active = false;
        let error = resolve_app(&both_inactive, &notes).unwrap_err();
        assert_eq!(error.code(), "invalid_arguments");
        assert!(error.to_string().contains("10"));
    }

    #[test]
    fn ambiguous_name_prefers_a_regular_app_over_a_helper() {
        let mut table = apps();
        table.push(AppInfo {
            name: "Google Chrome".into(),
            bundle_id: Some("com.google.Chrome".into()),
            pid: 1,
            path: None,
            active: false,
            hidden: false,
            activation_policy: ActivationPolicy::Regular,
        });
        table.push(AppInfo {
            name: "Google Chrome".into(),
            bundle_id: Some("com.google.Chrome.helper".into()),
            pid: 3,
            path: None,
            active: true,
            hidden: false,
            activation_policy: ActivationPolicy::Prohibited,
        });
        let resolved = resolve_app(&table, &AppSelector::Name("Google Chrome".into())).unwrap();
        assert_eq!(resolved.pid, 1);
    }

    #[test]
    fn bounds_match_allows_two_points() {
        let bounds = WindowBounds {
            x: 10,
            y: 20,
            width: 100,
            height: 80,
        };
        assert!(bounds_match_frame(
            bounds,
            crate::tree::NodeFrame {
                x: 12.0,
                y: 18.0,
                width: 101.0,
                height: 80.0,
            }
        ));
        assert!(!bounds_match_frame(
            bounds,
            crate::tree::NodeFrame {
                x: 14.0,
                y: 20.0,
                width: 100.0,
                height: 80.0,
            }
        ));
    }

    #[test]
    fn z_order_is_the_array_index_and_filters_renumber() {
        let listed = windows();
        assert_eq!(listed[0].z, 0);
        assert_eq!(listed[0].id, 100);
        assert_eq!(listed[1].z, 1);
        assert_eq!(listed[2].z, 2);
        assert!(!listed[0].ax_matched);

        let notes = filter_windows_for_pid(&listed, 10);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].z, 0);
        assert_eq!(notes[0].id, 100);
        assert_eq!(notes[1].z, 1);
        assert_eq!(notes[1].id, 101);

        let target = resolve(
            &apps(),
            &listed,
            &AppSelector::Name("Notes".into()),
            Some(&WindowSelector::Index(1)),
        )
        .unwrap();
        assert_eq!(target.pid, 10);
        assert_eq!(target.window_id, Some(101));
        assert_eq!(target.window_title.as_deref(), Some("Hello"));
    }

    #[test]
    fn window_selector_distinguishes_index_id_and_title() {
        assert_eq!(
            parse_window_selector("0").unwrap(),
            WindowSelector::Index(0)
        );
        assert_eq!(
            parse_window_selector("id:200").unwrap(),
            WindowSelector::Id(200)
        );
        assert_eq!(
            parse_window_selector("index:2").unwrap(),
            WindowSelector::Index(2)
        );
        let listed = windows();
        assert_eq!(
            resolve_window(&listed, &WindowSelector::Id(200))
                .unwrap()
                .title,
            "zsh"
        );
        assert_eq!(
            resolve_window(&listed, &WindowSelector::Title("hello".into()))
                .unwrap()
                .id,
            101
        );
        assert!(resolve_window(&listed, &WindowSelector::Title("missing".into())).is_err());
        assert!(parse_window_selector("id:nope").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_exe_path_resolves_a_symlink() {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-target-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("Notes");
        std::fs::write(&real, b"x").unwrap();
        let link = dir.join("alias");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let canonical = canonical_exe_path(&link);
        assert_eq!(
            canonical.file_name().and_then(|name| name.to_str()),
            Some("Notes")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_doctor_names_the_phase() {
        let report = DoctorReport::pending("P13");
        assert!(report.accessibility.is_none());
        assert!(report.screen_capture.is_none());
        assert!(report.input_monitor.is_none());
        assert!(report.note.unwrap().contains("P13"));
    }
}
