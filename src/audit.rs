//! Append-only JSONL audit log.
//!
//! The file is `${XDG_STATE_HOME:-~/.local/state}/computer-use/audit.jsonl`,
//! or `%LOCALAPPDATA%\computer-use\audit.jsonl` on Windows when `XDG_STATE_HOME`
//! is unset. Typed text, element values, clipboard bytes, and blocked URLs are
//! not fields on the line, so they are never written. A write failure is
//! returned to the caller and does not by itself fail the desktop action.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::error::ComputerError;

static AUDIT_LOCK: Mutex<()> = Mutex::new(());

/// Default rotation threshold: 10 MiB.
pub const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Default number of rotated backups to keep.
pub const DEFAULT_KEEP: u32 = 1;

/// Runtime audit configuration. Policy sets it per action; the environment can
/// only raise the rotation threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuditConfig {
    /// Write audit lines at all.
    pub enabled: bool,
    /// Rotate when the current file reaches this many bytes.
    pub max_bytes: u64,
    /// Backups to keep (1..=5).
    pub keep: u32,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bytes: DEFAULT_MAX_BYTES,
            keep: DEFAULT_KEEP,
        }
    }
}

std::thread_local! {
    static AUDIT_CONFIG: std::cell::RefCell<Option<AuditConfig>> =
        const { std::cell::RefCell::new(None) };
}

/// Restores the previous audit configuration when dropped.
pub struct ConfigGuard {
    previous: Option<AuditConfig>,
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        AUDIT_CONFIG.with(|slot| *slot.borrow_mut() = previous);
    }
}

/// Override the audit configuration until the guard drops. Tests use this.
pub fn push_config(config: AuditConfig) -> ConfigGuard {
    let previous = AUDIT_CONFIG.with(|slot| slot.borrow_mut().replace(config));
    ConfigGuard { previous }
}

/// Set the configuration for the current process. Called once per action from
/// the loaded policy.
pub fn set_config(config: AuditConfig) {
    AUDIT_CONFIG.with(|slot| *slot.borrow_mut() = Some(config));
}

fn config() -> AuditConfig {
    #[allow(unused_mut)]
    let mut config = AUDIT_CONFIG.with(|slot| *slot.borrow()).unwrap_or_default();
    #[cfg(not(test))]
    apply_env_override(
        &mut config,
        std::env::var("COMPUTER_USE_AUDIT_MAX_BYTES")
            .ok()
            .as_deref(),
    );
    config
}

/// `COMPUTER_USE_AUDIT_MAX_BYTES` can only raise the rotation threshold, and
/// never below the 1 KiB floor the policy enforces.
fn apply_env_override(config: &mut AuditConfig, raw: Option<&str>) {
    let Some(raw) = raw else {
        return;
    };
    let Ok(bytes) = raw.trim().parse::<u64>() else {
        return;
    };
    if bytes >= 1024 && bytes > config.max_bytes {
        config.max_bytes = bytes;
    }
}

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
    /// Character count for `clipboard_set`. The text itself is never stored.
    pub chars: Option<u64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    chars: Option<u64>,
}

/// Append one JSON line. Does nothing when no state directory is available in tests.
pub fn record(entry: &AuditRecord<'_>) -> Result<(), ComputerError> {
    let Some(dir) = state_dir() else {
        return Ok(());
    };
    let config = config();
    if !config.enabled {
        return Ok(());
    }
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
        chars: entry.chars,
    };
    let text = serde_json::to_string(&line)
        .map_err(|error| ComputerError::ExecutionFailed(format!("audit encode failed: {error}")))?;
    let _guard = AUDIT_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let path = dir.join("audit.jsonl");
    rotate_best_effort(&dir, &path, &config);
    append_line(&path, &text)
}

fn rotated_path(path: &Path, index: u32) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("audit.jsonl"));
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

fn rotate_best_effort(dir: &Path, path: &Path, config: &AuditConfig) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if config.max_bytes == 0 || metadata.len() < config.max_bytes {
        return;
    }
    let lock_path = dir.join("audit.lock");
    let Ok(_lock) = crate::foreground::try_lock(&lock_path) else {
        return;
    };
    let keep = config.keep.clamp(1, 5);
    let oldest = rotated_path(path, keep);
    let _ = fs::remove_file(&oldest);
    for index in (1..keep).rev() {
        let from = rotated_path(path, index);
        let to = rotated_path(path, index + 1);
        let _ = fs::rename(&from, &to);
    }
    let _ = fs::rename(path, rotated_path(path, 1));
}

/// Current audit file path, when a state directory is available.
pub fn audit_path() -> Option<PathBuf> {
    state_dir().map(|dir| dir.join("audit.jsonl"))
}

/// Newest `lines` audit entries, reading the rotated backup first when present.
pub fn tail(lines: usize) -> Result<Vec<String>, ComputerError> {
    let Some(dir) = state_dir() else {
        return Ok(Vec::new());
    };
    let want = lines.clamp(1, 500);
    let base = dir.join("audit.jsonl");
    // Read newest first and prepend, so the budget always lands on the most
    // recent lines instead of letting an older backup consume it.
    let mut paths = vec![base.clone()];
    for index in 1..=5 {
        paths.push(rotated_path(&base, index));
    }
    let mut out: Vec<String> = Vec::new();
    for path in paths {
        if out.len() >= want {
            break;
        }
        let Ok(mut older) = tail_lines(&path, want - out.len()) else {
            continue;
        };
        older.append(&mut out);
        out = older;
    }
    if out.len() > want {
        out.drain(0..out.len() - want);
    }
    Ok(out)
}

/// The last `want` non-empty lines of `path`, read backwards in 64 KiB chunks.
///
/// The whole file is never loaded: only as many chunks as the requested lines
/// need, capped by the file size.
fn tail_lines(path: &Path, want: usize) -> std::io::Result<Vec<String>> {
    const CHUNK: u64 = 64 * 1024;
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut end = len;
    let mut newlines = 0usize;
    // One extra newline so the line before the last `want` is complete.
    while end > 0 && newlines <= want {
        let start = end.saturating_sub(CHUNK);
        let mut buffer = vec![0u8; (end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer)?;
        newlines += buffer.iter().filter(|byte| **byte == b'\n').count();
        chunks.push(buffer);
        end = start;
    }
    let mut data = Vec::new();
    for chunk in chunks.into_iter().rev() {
        data.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&data);
    let mut lines: Vec<String> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect();
    // The first read started at a chunk boundary that may be mid-line.
    if end > 0 && lines.len() > 1 {
        lines.remove(0);
    }
    if lines.len() > want {
        lines.drain(0..lines.len() - want);
    }
    Ok(lines)
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
    fn rotation_keeps_a_backup_and_tail_returns_the_newest_lines() {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-audit-rotation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        let _state = push_state_dir(&dir);
        let _config = push_config(AuditConfig {
            enabled: true,
            max_bytes: 256,
            keep: 1,
        });
        for index in 0..20 {
            record(&AuditRecord {
                session: "test",
                action: "wait",
                bundle_id: None,
                pid: None,
                window_id: None,
                element: None,
                code: "ok",
                decision: "allow",
                source: "default",
                tier: None,
                chars: Some(index),
            })
            .unwrap();
        }
        assert!(dir.join("audit.jsonl").is_file());
        assert!(dir.join("audit.jsonl.1").is_file());
        let lines = tail(100).unwrap();
        assert!(!lines.is_empty(), "tail must return the retained lines");
        assert!(
            lines.last().unwrap().contains("\"chars\":19"),
            "tail must end with the newest line: {lines:?}"
        );
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line).expect("audit line is JSON");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("audit.jsonl"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_audit_writes_nothing() {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-audit-disabled-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        let _state = push_state_dir(&dir);
        let _config = push_config(AuditConfig {
            enabled: false,
            ..AuditConfig::default()
        });
        record(&AuditRecord {
            session: "test",
            action: "wait",
            bundle_id: None,
            pid: None,
            window_id: None,
            element: None,
            code: "ok",
            decision: "allow",
            source: "default",
            tier: None,
            chars: None,
        })
        .unwrap();
        assert!(!dir.join("audit.jsonl").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn audit_limit_can_only_be_raised() {
        let mut config = AuditConfig {
            enabled: true,
            max_bytes: 1024 * 1024,
            keep: 1,
        };
        apply_env_override(&mut config, None);
        assert_eq!(config.max_bytes, 1024 * 1024);
        apply_env_override(&mut config, Some("512"));
        assert_eq!(
            config.max_bytes,
            1024 * 1024,
            "below the 1 KiB floor changes nothing"
        );
        apply_env_override(&mut config, Some("2048"));
        assert_eq!(config.max_bytes, 1024 * 1024, "a lower value is ignored");
        apply_env_override(&mut config, Some("20971520"));
        assert_eq!(config.max_bytes, 20 * 1024 * 1024);
        apply_env_override(&mut config, Some("  nope "));
        assert_eq!(config.max_bytes, 20 * 1024 * 1024);
    }

    #[test]
    fn tail_reads_across_a_chunk_boundary() {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-audit-tail-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let mut text = String::new();
        for index in 0..5_000 {
            text.push_str(&format!(
                "{{\"n\":{index},\"pad\":\"{}\"}}\n",
                "x".repeat(40)
            ));
        }
        fs::write(&path, &text).unwrap();
        assert!(
            fs::metadata(&path).unwrap().len() > 64 * 1024,
            "the fixture must span more than one read chunk"
        );

        let lines = tail_lines(&path, 50).unwrap();
        assert_eq!(lines.len(), 50, "exactly the requested lines");
        // The head of the first chunk is a partial line and must be dropped.
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line).expect("a complete JSON line");
        }
        assert!(lines.last().unwrap().contains("\"n\":4999"));
        assert!(lines[0].contains("\"n\":4950"), "{}", lines[0]);
        let _ = fs::remove_dir_all(&dir);
    }

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
            chars: None,
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
