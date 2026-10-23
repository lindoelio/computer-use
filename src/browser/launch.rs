//! Managed Chromium: start a dedicated browser with an isolated profile,
//! record its debug port, and quit it later.
//!
//! Attach mode stays the default. This module exists because Chrome 136+
//! refuses `--remote-debugging-port` on the default data directory, so a
//! managed profile is the reliable path for automation and for tests.

#![cfg(feature = "cdp")]
#![allow(unsafe_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cache;
use crate::error::ComputerError;

/// One managed browser record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedSession {
    /// Session id.
    pub session: String,
    /// Browser pid.
    pub pid: u32,
    /// Debug port.
    pub port: u16,
    /// `http://127.0.0.1:<port>`.
    pub cdp_url: String,
    /// `ws://127.0.0.1:<port><path>` from `DevToolsActivePort`.
    pub ws_url: Option<String>,
    /// Profile directory.
    pub profile: String,
    /// Browser executable.
    pub browser: String,
    /// Creation time in unix milliseconds.
    pub created_unix_ms: u64,
}

fn sessions_dir() -> Result<PathBuf, ComputerError> {
    let dir = cache::cache_dir().join("browser-sessions");
    fs::create_dir_all(&dir).map_err(ComputerError::Io)?;
    Ok(dir)
}

fn session_file(session: &str) -> Result<PathBuf, ComputerError> {
    let safe: String = session
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    Ok(sessions_dir()?.join(format!("{safe}.json")))
}

fn write_record(record: &ManagedSession) -> Result<(), ComputerError> {
    let path = session_file(&record.session)?;
    let text = serde_json::to_string_pretty(record).map_err(|error| {
        ComputerError::ExecutionFailed(format!("browser session encode failed: {error}"))
    })?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(ComputerError::Io)?;
    file.write_all(text.as_bytes()).map_err(ComputerError::Io)?;
    Ok(())
}

/// Read one record by session id.
pub fn read_record(session: &str) -> Result<Option<ManagedSession>, ComputerError> {
    let path = session_file(session)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(ComputerError::Io)?;
    serde_json::from_str(&text).map(Some).map_err(|error| {
        ComputerError::ExecutionFailed(format!("browser session is not readable: {error}"))
    })
}

/// List every managed session.
pub fn list_records() -> Result<Vec<ManagedSession>, ComputerError> {
    let dir = sessions_dir()?;
    let mut records = Vec::new();
    for entry in fs::read_dir(dir).map_err(ComputerError::Io)? {
        let entry = entry.map_err(ComputerError::Io)?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = fs::read_to_string(entry.path()) {
            if let Ok(record) = serde_json::from_str::<ManagedSession>(&text) {
                records.push(record);
            }
        }
    }
    records.sort_by_key(|record| record.created_unix_ms);
    Ok(records)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.is_file()
            && fs::metadata(path)
                .map(|meta| meta.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn candidate_names() -> Vec<&'static str> {
    if cfg!(target_os = "macos") {
        vec![
            "Google Chrome",
            "Google Chrome Canary",
            "Chromium",
            "Microsoft Edge",
            "Brave Browser",
        ]
    } else if cfg!(target_os = "windows") {
        vec!["chrome.exe", "msedge.exe", "brave.exe"]
    } else {
        vec![
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
            "brave-browser",
        ]
    }
}

fn macos_app_path(name: &str) -> PathBuf {
    PathBuf::from(format!("/Applications/{name}.app/Contents/MacOS/{name}"))
}

/// Resolve the Chromium executable. `explicit` may be a path or a name.
pub fn find_browser(explicit: Option<&str>) -> Result<PathBuf, ComputerError> {
    if let Some(value) = explicit {
        let path = PathBuf::from(value);
        if is_executable(&path) {
            return Ok(path);
        }
        if let Some(found) = search_path(value) {
            return Ok(found);
        }
        if cfg!(target_os = "macos") {
            let app = macos_app_path(value);
            if is_executable(&app) {
                return Ok(app);
            }
        }
        return Err(ComputerError::InvalidArguments(format!(
            "browser executable not found: {value}"
        )));
    }
    for name in candidate_names() {
        if cfg!(target_os = "macos") {
            let app = macos_app_path(name);
            if is_executable(&app) {
                return Ok(app);
            }
        }
        if let Some(found) = search_path(name) {
            return Ok(found);
        }
    }
    Err(ComputerError::CdpUnavailable(
        "no Chromium browser found; install Chrome/Chromium or pass --browser <path>".into(),
    ))
}

fn search_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if is_executable(&exe) {
                return Some(exe);
            }
        }
    }
    None
}

/// Launch a managed Chromium. Returns the session record as JSON.
pub fn launch(
    browser: Option<&str>,
    headless: bool,
    profile: Option<&str>,
    port: Option<u16>,
    url: Option<&str>,
) -> Result<Value, ComputerError> {
    let executable = find_browser(browser)?;
    let session_id = format!("browser-{}", now_ms());
    let profile_dir = match profile {
        Some(value) => PathBuf::from(value),
        None => cache::cache_dir()
            .join("browser-profiles")
            .join(&session_id),
    };
    fs::create_dir_all(&profile_dir).map_err(ComputerError::Io)?;

    let port_value = port.unwrap_or(0);
    let mut command = Command::new(&executable);
    command
        .arg(format!("--remote-debugging-port={port_value}"))
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--remote-allow-origins=*")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-networking")
        .arg("--disable-component-update")
        .arg("--disable-sync")
        .arg("--disable-extensions")
        .arg("--disable-default-apps")
        .arg("--disable-features=Translate,OptimizationHints")
        .arg("--metrics-recording-only")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if headless {
        command.arg("--headless=new");
    }
    command.arg(url.unwrap_or("about:blank"));
    let child = command.spawn().map_err(|error| {
        ComputerError::ExecutionFailed(format!("could not start {}: {error}", executable.display()))
    })?;
    let pid = child.id();

    let port_file = profile_dir.join("DevToolsActivePort");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut contents = None;
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(&port_file) {
            if !text.trim().is_empty() {
                contents = Some(text);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let contents = contents.ok_or_else(|| {
        ComputerError::CdpUnavailable(format!(
            "browser started (pid {pid}) but {} never appeared",
            port_file.display()
        ))
    })?;
    let mut lines = contents.lines();
    let port = lines
        .next()
        .and_then(|line| line.trim().parse::<u16>().ok())
        .ok_or_else(|| ComputerError::CdpUnavailable("DevToolsActivePort has no port".into()))?;
    let ws_path = lines.next().map(str::trim).filter(|line| !line.is_empty());
    let ws_url = ws_path.map(|path| format!("ws://127.0.0.1:{port}{path}"));
    let record = ManagedSession {
        session: session_id.clone(),
        pid,
        port,
        cdp_url: format!("http://127.0.0.1:{port}"),
        ws_url,
        profile: profile_dir.display().to_string(),
        browser: executable.display().to_string(),
        created_unix_ms: now_ms(),
    };
    write_record(&record)?;
    Ok(json!({
        "session": record.session,
        "pid": record.pid,
        "port": record.port,
        "cdp_url": record.cdp_url,
        "ws_url": record.ws_url,
        "profile": record.profile,
        "browser": record.browser,
    }))
}

fn kill_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, libc::SIGTERM) == 0 }
    }
    #[cfg(windows)]
    {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Stop a managed browser. `session` defaults to the newest record.
pub fn quit(session: Option<&str>, purge: bool) -> Result<Value, ComputerError> {
    let record = match session {
        Some(id) => read_record(id)?,
        None => list_records()?.pop(),
    };
    let Some(record) = record else {
        return Ok(json!({"killed": false, "reason": "no managed browser session"}));
    };
    let alive = cache::pid_alive(record.pid);
    let killed = if alive { kill_pid(record.pid) } else { true };
    // Give the browser a moment to exit before removing the profile.
    for _ in 0..20 {
        if !cache::pid_alive(record.pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = fs::remove_file(session_file(&record.session)?);
    let mut profile_removed = false;
    if purge {
        profile_removed = fs::remove_dir_all(&record.profile).is_ok();
    }
    Ok(json!({
        "killed": killed,
        "pid": record.pid,
        "session": record.session,
        "profile_removed": profile_removed,
    }))
}

/// List managed sessions as JSON.
pub fn sessions() -> Result<Value, ComputerError> {
    let records = list_records()?;
    let items: Vec<Value> = records
        .into_iter()
        .map(|record| {
            json!({
                "session": record.session,
                "pid": record.pid,
                "port": record.port,
                "cdp_url": record.cdp_url,
                "ws_url": record.ws_url,
                "profile": record.profile,
                "browser": record.browser,
                "alive": cache::pid_alive(record.pid),
                "created_unix_ms": record.created_unix_ms,
            })
        })
        .collect();
    Ok(json!({"sessions": items}))
}
