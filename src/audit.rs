//! Append-only JSONL audit log.
//!
//! The file is `${XDG_STATE_HOME:-~/.local/state}/computer-use/audit.jsonl`,
//! or `%LOCALAPPDATA%\computer-use\audit.jsonl` on Windows when `XDG_STATE_HOME`
//! is unset. Typed text, element values, clipboard bytes, and blocked URLs are
//! not fields on the line, so they are never written. A write failure is
//! returned to the caller and does not by itself fail the desktop action.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::error::ComputerError;

static AUDIT_LOCK: Mutex<()> = Mutex::new(());

std::thread_local! {
    static STATE_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Restores the previous test state directory when dropped.
pub struct StateDirGuard {
    previous: Option<PathBuf>,
}

impl Drop for StateDirGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        STATE_DIR.with(|slot| *slot.borrow_mut() = previous);
    }
}

/// Point audit and pause files at `dir` until the guard drops.
///
/// Unit tests use this so they never create `~/.local/state`.
pub fn push_state_dir(dir: impl Into<PathBuf>) -> StateDirGuard {
    let previous = STATE_DIR.with(|slot| slot.borrow_mut().replace(dir.into()));
    StateDirGuard { previous }
}

/// Directory that holds `audit.jsonl` and the `paused` marker.
///
/// `None` in tests that did not set a state directory or `XDG_STATE_HOME`.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(overridden) = STATE_DIR.with(|slot| slot.borrow().clone()) {
        return Some(overridden);
    }
    ambient_state_dir()
}

/// Unit tests ignore `XDG_STATE_HOME` so they never append to the real state directory.
#[cfg(test)]
fn ambient_state_dir() -> Option<PathBuf> {
    None
}

#[cfg(not(test))]
fn ambient_state_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("computer-use"));
        }
    }
    production_state_dir()
}

#[cfg(not(test))]
fn production_state_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            if !local.is_empty() {
                return Some(PathBuf::from(local).join("computer-use"));
            }
        }
    }
    dirs::home_dir().map(|home| home.join(".local").join("state").join("computer-use"))
}

/// One audit record. Fields that would carry typed text are intentionally absent.
#[derive(Debug, Clone)]
pub struct AuditRecord<'a> {
    /// Session id, or the process id when the caller did not set one.
    pub session: &'a str,
    /// Action name such as `press` or `quit`. Never the typed text.
    pub action: &'a str,
    /// Bundle id when the target resolved.
    pub bundle_id: Option<&'a str>,
    /// Process id when the target resolved.
    pub pid: Option<i32>,
    /// Window id when one was targeted.
    pub window_id: Option<u32>,
    /// Element index when the action named one.
    pub element: Option<u32>,
    /// `ok` or the error code. This is the action outcome, not a constant.
    pub code: &'a str,
    /// `allow`, `deny`, or `confirm`.
    pub decision: &'a str,
    /// `builtin`, `admin`, `user`, `default`, or another policy source.
    pub source: &'a str,
    /// Input tier when the action reported one.
    pub tier: Option<&'a str>,
}

#[derive(Serialize)]
struct AuditLine<'a> {
    ts: String,
    session: &'a str,
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bundle_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    element: Option<u32>,
    code: &'a str,
    decision: &'a str,
    source: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tier: Option<&'a str>,
}

/// Append one JSON line. Does nothing when no state directory is available in tests.
pub fn record(entry: &AuditRecord<'_>) -> Result<(), ComputerError> {
    let Some(dir) = state_dir() else {
        return Ok(());
    };
    let line = AuditLine {
        ts: utc_timestamp(unix_secs()),
        session: entry.session,
        action: entry.action,
        bundle_id: entry.bundle_id,
        pid: entry.pid,
        window_id: entry.window_id,
        element: entry.element,
        code: entry.code,
        decision: entry.decision,
        source: entry.source,
        tier: entry.tier,
    };
    let text = serde_json::to_string(&line)
        .map_err(|error| ComputerError::ExecutionFailed(format!("audit encode failed: {error}")))?;
    let _guard = AUDIT_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    append_line(&dir.join("audit.jsonl"), &text)
}

fn append_line(path: &Path, line: &str) -> Result<(), ComputerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ComputerError::Io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        // O_NOFOLLOW: do not append through a symlink planted on the log path.
        #[cfg(target_os = "linux")]
        options.custom_flags(0x20000);
        #[cfg(target_os = "macos")]
        options.custom_flags(0x0100);
    }
    let mut file = options.open(path).map_err(ComputerError::Io)?;
    file.write_all(line.as_bytes()).map_err(ComputerError::Io)?;
    file.write_all(b"\n").map_err(ComputerError::Io)?;
    Ok(())
}

fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Format a Unix timestamp as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_timestamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;
    let second = tod % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`, shifted so day 0 is 1970-01-01.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (if month <= 2 { y + 1 } else { y }) as i32;
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_epoch_formats_as_1970() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(86_400), "1970-01-02T00:00:00Z");
    }

    #[test]
    fn writer_redacts_a_value_because_it_has_no_value_field() {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let _guard = push_state_dir(&dir);
        record(&AuditRecord {
            session: "9",
            action: "set_value",
            bundle_id: Some("com.apple.Notes"),
            pid: Some(412),
            window_id: Some(88),
            element: Some(12),
            code: "policy_denied",
            decision: "deny",
            source: "builtin",
            tier: Some("ax"),
        })
        .unwrap();
        let text = fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert!(text.contains("\"action\":\"set_value\""));
        assert!(text.contains("\"code\":\"policy_denied\""));
        assert!(text.contains("\"decision\":\"deny\""));
        assert!(text.contains("\"source\":\"builtin\""));
        assert!(!text.contains("\"value\""));
        assert!(!text.contains("super-secret-token"));
        let _ = fs::remove_dir_all(&dir);
    }
}
