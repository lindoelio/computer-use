//! Exclusive lock for tier `activate`.
//!
//! The file is `<cache>/foreground.lock`. A second session that asks for
//! `activate` gets `execution_failed` naming the holder pid. Tests pass an
//! explicit path and never touch `~/.cache`.

#![allow(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::ComputerError;

std::thread_local! {
    static LOCK_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Restores the previous lock-path override.
#[cfg_attr(not(test), allow(dead_code))]
pub struct LockPathGuard {
    previous: Option<PathBuf>,
}

impl Drop for LockPathGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        LOCK_PATH.with(|slot| *slot.borrow_mut() = previous);
    }
}

/// Use `path` as `foreground.lock` until the guard drops.
#[cfg_attr(not(test), allow(dead_code))]
pub fn push_lock_path(path: impl Into<PathBuf>) -> LockPathGuard {
    let previous = LOCK_PATH.with(|slot| slot.borrow_mut().replace(path.into()));
    LockPathGuard { previous }
}

/// Lock file for this call. `None` in tests that did not set a path.
pub fn lock_path() -> Option<PathBuf> {
    if let Some(path) = LOCK_PATH.with(|slot| slot.borrow().clone()) {
        return Some(path);
    }
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        Some(crate::cache::cache_dir().join("foreground.lock"))
    }
}

/// Held lock. Drop releases it.
#[derive(Debug)]
pub struct ForegroundLock {
    file: File,
}

impl Drop for ForegroundLock {
    fn drop(&mut self) {
        let _ = unlock_file(&self.file);
    }
}

/// Why [`try_lock`] did not acquire the file.
#[derive(Debug)]
pub enum LockError {
    /// Another session holds the lock.
    Held {
        /// Pid written by the holder. Zero when the file was empty.
        pid: u32,
    },
    /// The file could not be opened or locked.
    Failed(ComputerError),
}

impl LockError {
    /// `execution_failed` naming the holder when one was recorded.
    pub fn into_computer(self) -> ComputerError {
        match self {
            Self::Held { pid } if pid > 0 => {
                ComputerError::ExecutionFailed(format!("foreground.lock is held by pid {pid}"))
            }
            Self::Held { .. } => {
                ComputerError::ExecutionFailed("foreground.lock is held by another session".into())
            }
            Self::Failed(error) => error,
        }
    }
}

/// Take an exclusive non-blocking lock and record this process id.
pub fn try_lock(path: &Path) -> Result<ForegroundLock, LockError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| LockError::Failed(ComputerError::Io(error)))?;
        #[cfg(unix)]
        set_mode(parent, 0o700);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| LockError::Failed(ComputerError::Io(error)))?;
    match try_lock_file(&file) {
        Ok(true) => {}
        Ok(false) => {
            return Err(LockError::Held {
                pid: read_holder(path),
            })
        }
        Err(error) => return Err(LockError::Failed(ComputerError::Io(error))),
    }
    if let Err(error) = write_holder(&mut file) {
        let _ = unlock_file(&file);
        return Err(LockError::Failed(error));
    }
    Ok(ForegroundLock { file })
}

/// `Some(pid)` when another session holds the lock. Acquiring and dropping means it was free.
pub fn holder_if_taken(path: &Path) -> Result<Option<u32>, LockError> {
    match try_lock(path) {
        Ok(guard) => {
            drop(guard);
            Ok(None)
        }
        Err(LockError::Held { pid }) => Ok(Some(pid)),
        Err(error) => Err(error),
    }
}

fn write_holder(file: &mut File) -> Result<(), ComputerError> {
    file.seek(SeekFrom::Start(0)).map_err(ComputerError::Io)?;
    file.set_len(0).map_err(ComputerError::Io)?;
    writeln!(file, "{}", std::process::id()).map_err(ComputerError::Io)?;
    file.flush().map_err(ComputerError::Io)?;
    Ok(())
}

fn read_holder(path: &Path) -> u32 {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(unix)]
fn try_lock_file(file: &File) -> io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `file` is an open descriptor we own. LOCK_NB returns instead of waiting.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock
        || error.raw_os_error() == Some(libc::EWOULDBLOCK)
        || error.raw_os_error() == Some(libc::EAGAIN)
    {
        return Ok(false);
    }
    Err(error)
}

#[cfg(unix)]
fn unlock_file(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `file` is the same descriptor [`try_lock_file`] locked.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn try_lock_file(file: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    let mut overlapped = Overlapped::zero();
    // SAFETY: `file` is open and `overlapped` is a zeroed synchronous OVERLAPPED.
    // LockFileEx with FAIL_IMMEDIATELY returns when the lock is busy.
    let ok = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if ok != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    // ERROR_LOCK_VIOLATION
    if error.raw_os_error() == Some(33) {
        return Ok(false);
    }
    Err(error)
}

#[cfg(windows)]
fn unlock_file(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    let mut overlapped = Overlapped::zero();
    // SAFETY: `file` is the handle [`try_lock_file`] locked. The range matches that call.
    let ok = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
    if ok != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
const LOCKFILE_EXCLUSIVE_LOCK: u32 = 2;
#[cfg(windows)]
const LOCKFILE_FAIL_IMMEDIATELY: u32 = 1;

#[cfg(windows)]
#[repr(C)]
#[allow(dead_code)] // C reads these fields; Rust only writes them.
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: *mut std::ffi::c_void,
}

#[cfg(windows)]
impl Overlapped {
    fn zero() -> Self {
        Self {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: std::ptr::null_mut(),
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LockFileEx(
        file: *mut std::ffi::c_void,
        flags: u32,
        reserved: u32,
        bytes_low: u32,
        bytes_high: u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn UnlockFileEx(
        file: *mut std::ffi::c_void,
        reserved: u32,
        bytes_low: u32,
        bytes_high: u32,
        overlapped: *mut Overlapped,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_names_the_holder_and_releases() {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("foreground.lock");
        let held = try_lock(&path).unwrap();
        let error = try_lock(&path).unwrap_err();
        let message = error.into_computer().to_string();
        assert!(
            message.contains(&std::process::id().to_string()),
            "{message}"
        );
        assert!(message.contains("foreground.lock"));
        drop(held);
        let again = try_lock(&path).unwrap();
        drop(again);
        let _ = fs::remove_dir_all(&dir);
    }
}
