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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    /// Latest tree used to revalidate element indices.
    ///
    /// `tree --diff` keeps comparing against [`Self::root`]. An element action
    /// updates this field and leaves the diff baseline in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<SigNode>,
    /// [`revision_of`](crate::tree::revision_of) of [`Self::observed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_revision: Option<u64>,
}

const OWNER_FILE: &str = "owner.json";
/// Sessions older than this, whose owner process is not alive, are stale.
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Directory that holds every session. This does not create it.
pub fn sessions_root() -> PathBuf {
    cache_root().join("sessions")
}

fn session_path(session: Option<&str>) -> PathBuf {
    let name =
        sanitize_token(session.unwrap_or("")).unwrap_or_else(|| std::process::id().to_string());
    sessions_root().join(name)
}

/// Directory for one session. Creates it, mode `0700`, and writes `owner.json` once.
pub fn session_dir(session: Option<&str>) -> Result<PathBuf, ComputerError> {
    let dir = session_path(session);
    fs::create_dir_all(&dir).map_err(ComputerError::Io)?;
    #[cfg(unix)]
    set_mode(&dir, 0o700);
    ensure_owner(&dir)?;
    Ok(dir)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionOwner {
    pid: u32,
    created_unix_ms: u64,
}

fn ensure_owner(dir: &Path) -> Result<(), ComputerError> {
    let path = dir.join(OWNER_FILE);
    if path.exists() {
        return Ok(());
    }
    let owner = SessionOwner {
        pid: std::process::id(),
        created_unix_ms: unix_ms(SystemTime::now()),
    };
    write_secret_json(&path, &owner)
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn time_from_unix_ms(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
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

const SCREENSHOT_MAP_FILE: &str = "screenshot-map.json";

/// Write the last screenshot map. This file is not a tree snapshot.
pub fn store_screenshot_map(
    session: Option<&str>,
    map: &crate::image_util::ScreenshotMap,
) -> Result<(), ComputerError> {
    let path = session_dir(session)?.join(SCREENSHOT_MAP_FILE);
    write_secret_json(&path, map)
}

/// Read the last screenshot map for this session, if one was stored.
pub fn load_screenshot_map(
    session: Option<&str>,
) -> Result<Option<crate::image_util::ScreenshotMap>, ComputerError> {
    let path = session_path(session).join(SCREENSHOT_MAP_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(ComputerError::Io)?;
    serde_json::from_str(&text).map(Some).map_err(|error| {
        ComputerError::ExecutionFailed(format!(
            "screenshot map {} is not readable: {error}",
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
        if !is_snapshot_file(&path) {
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
        if !is_snapshot_file(&path) {
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

fn is_snapshot_file(path: &Path) -> bool {
    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name != SCREENSHOT_MAP_FILE && name != OWNER_FILE
}

/// Options for [`prune_sessions`].
#[derive(Clone, Debug)]
pub struct PruneOptions {
    /// Report matches without deleting them.
    pub dry_run: bool,
    /// Age after which a session with a dead or unknown owner is stale.
    pub ttl: Duration,
    /// Clock used for the age check.
    pub now: SystemTime,
}

impl Default for PruneOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            ttl: DEFAULT_SESSION_TTL,
            now: SystemTime::now(),
        }
    }
}

/// Directories [`prune_sessions`] removed or left in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PruneReport {
    /// Session directory names that were removed, or would be in a dry run.
    pub removed: Vec<String>,
    /// Session directory names that stayed.
    pub kept: Vec<String>,
}

/// Remove empty session directories and stale ones under `root`.
///
/// A directory with no snapshot or screenshot map is empty and is removed even
/// when its owner pid is alive. A directory that still has content is kept
/// while `alive` says the owner pid is running. Content whose owner is dead
/// or unknown is removed once it is at least `ttl` old. `alive` is injected
/// so tests do not depend on the process table.
pub fn prune_sessions(
    root: &Path,
    options: &PruneOptions,
    alive: &dyn Fn(u32) -> bool,
) -> Result<PruneReport, ComputerError> {
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    if !root.exists() {
        return Ok(PruneReport { removed, kept });
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(root).map_err(ComputerError::Io)? {
        let entry = entry.map_err(ComputerError::Io)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        names.push((name, path));
    }
    names.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, path) in names {
        if should_prune(&path, &name, options, alive)? {
            if !options.dry_run {
                fs::remove_dir_all(&path).map_err(ComputerError::Io)?;
            }
            removed.push(name);
        } else {
            kept.push(name);
        }
    }
    Ok(PruneReport { removed, kept })
}

/// Drop empty session directories under the real cache. Content is left alone.
pub fn prune_empty_sessions() -> Result<Vec<String>, ComputerError> {
    let root = sessions_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut removed = Vec::new();
    for entry in fs::read_dir(&root).map_err(ComputerError::Io)? {
        let entry = entry.map_err(ComputerError::Io)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match session_has_content(&path) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(_) => continue,
        }
        fs::remove_dir_all(&path).map_err(ComputerError::Io)?;
        if let Ok(name) = entry.file_name().into_string() {
            removed.push(name);
        }
    }
    removed.sort();
    Ok(removed)
}

/// True when `pid` is still running. Signal 0 does not deliver a signal.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();
        match output {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout);
                text.split(',')
                    .any(|field| field.trim().trim_matches('"') == pid.to_string())
            }
            Err(_) => false,
        }
    }
}

fn should_prune(
    path: &Path,
    name: &str,
    options: &PruneOptions,
    alive: &dyn Fn(u32) -> bool,
) -> Result<bool, ComputerError> {
    if !session_has_content(path)? {
        return Ok(true);
    }
    if let Some(pid) = owner_pid(path, name) {
        if alive(pid) {
            return Ok(false);
        }
    }
    Ok(session_age(path, options.now) >= options.ttl)
}

fn session_has_content(dir: &Path) -> Result<bool, ComputerError> {
    let entries = fs::read_dir(dir).map_err(ComputerError::Io)?;
    for entry in entries {
        let entry = entry.map_err(ComputerError::Io)?;
        if entry.file_name() == std::ffi::OsStr::new(OWNER_FILE) {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

fn owner_pid(dir: &Path, name: &str) -> Option<u32> {
    if let Some(owner) = read_owner(dir) {
        return Some(owner.pid);
    }
    name.parse::<u32>().ok()
}

fn read_owner(dir: &Path) -> Option<SessionOwner> {
    let text = fs::read_to_string(dir.join(OWNER_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn session_age(dir: &Path, now: SystemTime) -> Duration {
    let created = read_owner(dir)
        .map(|owner| time_from_unix_ms(owner.created_unix_ms))
        .or_else(|| dir.metadata().ok().and_then(|meta| meta.modified().ok()));
    match created {
        Some(created) => now.duration_since(created).unwrap_or(Duration::ZERO),
        None => Duration::MAX,
    }
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
    write_secret_json(path, snapshot)
}

fn write_secret_json(path: &Path, value: &impl Serialize) -> Result<(), ComputerError> {
    let text = serde_json::to_string(value).map_err(|error| {
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
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("computer-use");
        }
    }
    #[cfg(test)]
    {
        panic!("test used the session cache without XDG_CACHE_HOME");
    }
    #[cfg(not(test))]
    {
        cache_root_unchecked()
    }
}

#[cfg(not(test))]
fn cache_root_unchecked() -> PathBuf {
    #[cfg(windows)]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("computer-use")
            .join("cache")
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
pub(crate) static CACHE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
#[allow(unsafe_code)]
pub(crate) struct CacheEnv {
    dir: PathBuf,
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
#[allow(unsafe_code)]
impl CacheEnv {
    /// Point `XDG_CACHE_HOME` at a temp directory until this value drops.
    pub(crate) fn acquire() -> Self {
        let lock = CACHE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "computer-use-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp cache dir");
        let previous = std::env::var_os("XDG_CACHE_HOME");
        // SAFETY: CACHE_ENV_LOCK is held until drop, which restores the previous value.
        unsafe { std::env::set_var("XDG_CACHE_HOME", &dir) };
        Self {
            dir,
            previous,
            _lock: lock,
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
impl Drop for CacheEnv {
    fn drop(&mut self) {
        // SAFETY: the same lock taken in acquire is still held.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
                None => std::env::remove_var("XDG_CACHE_HOME"),
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use crate::tree::SigNode;

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
                secure: false,
                children: Vec::new(),
            },
            truncated: false,
            pid,
            window_id: window,
            bundle_id: Some("com.apple.Notes".into()),
            electron: None,
            observed: None,
            observed_revision: None,
        }
    }

    #[test]
    fn round_trip_under_xdg_cache_home() {
        let _guard = CACHE_ENV_LOCK.lock().unwrap();
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
        // SAFETY: CACHE_ENV_LOCK is held, and this test restores the variable before release.
        // Other tests do not read XDG_CACHE_HOME.
        unsafe { std::env::set_var("XDG_CACHE_HOME", &dir) };
        let session = Some("unit");
        let snapshot = sample(12, 34, "Notes");
        store(session, &snapshot).unwrap();
        let loaded = load(session, Some("com.apple.Notes"), 12, 34)
            .unwrap()
            .expect("stored snapshot");
        assert_eq!(loaded, snapshot);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = snapshot_path(session, Some("com.apple.Notes"), 12, 34).unwrap();
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
        let before = load(session, Some("com.apple.Notes"), 12, 34)
            .unwrap()
            .unwrap();
        let map = crate::image_util::prepare_shot(
            &image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255])),
            crate::image_util::ShotFit {
                scale: 2.0,
                origin_x: 0,
                origin_y: 0,
                logical_width: 1,
                logical_height: 1,
                max_width: 1440,
                canvas: None,
            },
        )
        .unwrap()
        .map;
        store_screenshot_map(session, &map).unwrap();
        let after = load(session, Some("com.apple.Notes"), 12, 34)
            .unwrap()
            .unwrap();
        assert_eq!(before, after);
        let loaded = load_screenshot_map(session).unwrap().expect("map");
        assert_eq!(loaded.width, map.width);
        assert!(pending_restores(session).unwrap().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let map_path = session_dir(session).unwrap().join("screenshot-map.json");
            let mode = fs::metadata(&map_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        }
        // SAFETY: same lock as set_var. The variable is removed before the guard drops.
        unsafe { std::env::remove_var("XDG_CACHE_HOME") };
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_removes_empty_and_stale_dirs_and_keeps_a_live_owner() {
        let root = std::env::temp_dir().join(format!(
            "computer-use-prune-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let now = SystemTime::now();
        write_session(&root, "empty", 111, now, false);
        write_session(
            &root,
            "live",
            222,
            now - Duration::from_secs(10 * 24 * 60 * 60),
            true,
        );
        write_session(&root, "dead-fresh", 333, now, true);
        write_session(
            &root,
            "dead-old",
            444,
            now - Duration::from_secs(48 * 60 * 60),
            true,
        );
        let alive = |pid: u32| pid == 222;
        let options = PruneOptions {
            dry_run: true,
            ttl: DEFAULT_SESSION_TTL,
            now,
        };
        let dry = prune_sessions(&root, &options, &alive).unwrap();
        assert_eq!(
            dry.removed,
            vec!["dead-old".to_string(), "empty".to_string()]
        );
        assert!(root.join("empty").exists(), "dry run must not delete");
        assert!(root.join("live").exists());
        assert!(root.join("dead-fresh").exists());

        let removed = prune_sessions(
            &root,
            &PruneOptions {
                dry_run: false,
                ttl: DEFAULT_SESSION_TTL,
                now,
            },
            &alive,
        )
        .unwrap();
        assert_eq!(
            removed.removed,
            vec!["dead-old".to_string(), "empty".to_string()]
        );
        assert_eq!(
            removed.kept,
            vec!["dead-fresh".to_string(), "live".to_string()]
        );
        assert!(!root.join("empty").exists());
        assert!(!root.join("dead-old").exists());
        assert!(root.join("live").join("snapshot.json").exists());
        assert!(root.join("dead-fresh").join("snapshot.json").exists());
        let _ = fs::remove_dir_all(&root);
    }

    fn write_session(root: &Path, name: &str, pid: u32, created: SystemTime, content: bool) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        let owner = serde_json::json!({
            "pid": pid,
            "created_unix_ms": created
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        });
        fs::write(dir.join("owner.json"), owner.to_string()).unwrap();
        if content {
            fs::write(dir.join("snapshot.json"), "{\"revision\":1}").unwrap();
        }
    }

    #[test]
    fn current_process_is_alive_and_a_huge_pid_is_not() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(u32::MAX));
    }
}
