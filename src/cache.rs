//! Per-session tree snapshots.
//!
//! Unix files are created mode `0600`. `XDG_CACHE_HOME` wins on every OS so
//! tests can point at a temp directory. Without it, Windows uses
//! `%LOCALAPPDATA%\computer-use\cache` and other platforms use `~/.cache`.
//! `dirs::cache_dir` is not used: on macOS that is `~/Library/Caches`, and the
//! session path contract is `~/.cache`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ComputerError;
use crate::tree::{ElectronEdit, SigNode};

/// One saved observation. Secure-field values are already redacted in `root`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Structural revision.
    pub revision: u64,
    /// Last full tree text.
    pub tree: String,
    /// Signature tree for the next diff.
    pub root: SigNode,
    /// Whether the full text hit a cap.
    pub truncated: bool,
    /// Target pid.
    pub pid: i32,
    /// Joined window id.
    pub window_id: u32,
    /// Bundle id used in the file name.
    pub bundle_id: Option<String>,
    /// Electron flags to restore. Absent when this process did not change them.
    pub electron: Option<ElectronEdit>,
}

/// Directory for one session.
pub fn session_dir(session: Option<&str>) -> Result<PathBuf, ComputerError> {
    let name =
        sanitize_token(session.unwrap_or("")).unwrap_or_else(|| std::process::id().to_string());
    let dir = cache_root().join("sessions").join(name);
    fs::create_dir_all(&dir).map_err(ComputerError::Io)?;
    #[cfg(unix)]
    set_mode(&dir, 0o700);
    Ok(dir)
}

/// Read the snapshot for one app window, if a previous call stored one.
pub fn load(
    session: Option<&str>,
    bundle_id: Option<&str>,
    pid: i32,
    window_id: u32,
) -> Result<Option<Snapshot>, ComputerError> {
    let path = snapshot_path(session, bundle_id, pid, window_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(ComputerError::Io)?;
    serde_json::from_str(&text).map(Some).map_err(|error| {
        ComputerError::ExecutionFailed(format!(
            "session cache {} is not readable: {error}",
            path.display()
        ))
    })
}

/// Write a snapshot. An older Electron edit is kept when the new one has none.
pub fn store(session: Option<&str>, snapshot: &Snapshot) -> Result<(), ComputerError> {
    let path = snapshot_path(
        session,
        snapshot.bundle_id.as_deref(),
        snapshot.pid,
        snapshot.window_id,
    )?;
    let mut snapshot = snapshot.clone();
    if snapshot.electron.is_none() {
        if let Some(previous) = read_snapshot(&path)? {
            snapshot.electron = previous.electron;
        }
    }
    write_json(&path, &snapshot)
}

/// Electron edits recorded for this session, one per pid.
pub fn pending_restores(session: Option<&str>) -> Result<Vec<(i32, ElectronEdit)>, ComputerError> {
    let dir = session_dir(session)?;
    let mut found = Vec::new();
    let entries = fs::read_dir(&dir).map_err(ComputerError::Io)?;
    for entry in entries {
        let entry = entry.map_err(ComputerError::Io)?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(snapshot) = read_snapshot(&path)? else {
            continue;
        };
        let Some(edit) = snapshot.electron else {
            continue;
        };
        if found.iter().any(|(pid, _)| *pid == snapshot.pid) {
            continue;
        }
        found.push((snapshot.pid, edit));
    }
    Ok(found)
}

/// Drop Electron edits after a successful restore so a second teardown is a no-op.
pub fn clear_restores(session: Option<&str>, pids: &[i32]) -> Result<(), ComputerError> {
    let dir = session_dir(session)?;
    let entries = fs::read_dir(&dir).map_err(ComputerError::Io)?;
    for entry in entries {
        let entry = entry.map_err(ComputerError::Io)?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(mut snapshot) = read_snapshot(&path)? else {
            continue;
        };
        if pids.contains(&snapshot.pid) && snapshot.electron.is_some() {
            snapshot.electron = None;
            write_json(&path, &snapshot)?;
        }
    }
    Ok(())
}

fn snapshot_path(
    session: Option<&str>,
    bundle_id: Option<&str>,
    pid: i32,
    window_id: u32,
) -> Result<PathBuf, ComputerError> {
    let bundle = sanitize_token(bundle_id.unwrap_or("")).unwrap_or_else(|| "app".into());
    Ok(session_dir(session)?.join(format!("{bundle}-{pid}-{window_id}.json")))
}

fn read_snapshot(path: &Path) -> Result<Option<Snapshot>, ComputerError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(ComputerError::Io)?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&text).map(Some).map_err(|error| {
        ComputerError::ExecutionFailed(format!(
            "session cache {} is not readable: {error}",
            path.display()
        ))
    })
}

fn write_json(path: &Path, snapshot: &Snapshot) -> Result<(), ComputerError> {
    let text = serde_json::to_string(snapshot).map_err(|error| {
        ComputerError::ExecutionFailed(format!("session cache encode failed: {error}"))
    })?;
    let mut options = OpenOptions::new();
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

fn cache_root() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("computer-use");
    }
    #[cfg(windows)]
    {
        return dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("computer-use")
            .join("cache");
    }
    #[cfg(not(windows))]
    {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".cache").join("computer-use")
    }
}

fn sanitize_token(value: &str) -> Option<String> {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use crate::tree::SigNode;
    use std::sync::Mutex;

    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn sample(pid: i32, window: u32, value: &str) -> Snapshot {
        Snapshot {
            revision: 7,
            tree: format!("[0] AXWindow \"{value}\"\n"),
            root: SigNode {
                index: 0,
                role: "AXWindow".into(),
                identity: value.into(),
                child_count: 0,
                value: String::new(),
                frame: None,
                children: Vec::new(),
            },
            truncated: false,
            pid,
            window_id: window,
            bundle_id: Some("com.apple.Notes".into()),
            electron: None,
        }
    }

    #[test]
    fn round_trip_under_xdg_cache_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "computer-use-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // SAFETY: ENV_LOCK is held, and this test restores the variable before release.
        // Other tests do not read XDG_CACHE_HOME.
        unsafe { std::env::set_var("XDG_CACHE_HOME", &dir) };
        let session = Some("unit");
        let snapshot = sample(12, 34, "Notes");
        store(session, &snapshot).unwrap();
        let loaded = load(session, Some("com.apple.Notes"), 12, 34)
            .unwrap()
            .expect("stored snapshot");
        assert_eq!(loaded, snapshot);
        let path = snapshot_path(session, Some("com.apple.Notes"), 12, 34).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        }
        let mut with_flags = snapshot.clone();
        with_flags.electron = Some(crate::tree::electron_enable(crate::tree::FlagState {
            manual: Some(false),
            enhanced: Some(false),
        }));
        with_flags.tree = "[0] AXWindow \"next\"\n".into();
        store(session, &with_flags).unwrap();
        let kept = load(session, Some("com.apple.Notes"), 12, 34)
            .unwrap()
            .unwrap();
        assert!(kept.electron.is_some());
        let pending = pending_restores(session).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, 12);
        clear_restores(session, &[12]).unwrap();
        assert!(pending_restores(session).unwrap().is_empty());
        // SAFETY: same lock as set_var. The variable is removed before the guard drops.
        unsafe { std::env::remove_var("XDG_CACHE_HOME") };
        let _ = fs::remove_dir_all(&dir);
    }
}
