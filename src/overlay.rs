//! Agent-cursor overlay protocol.
//!
//! `computer-use overlay serve` listens on `<cache>/overlay.sock`. The main
//! CLI sends one JSON line `{x,y,session}` after a successful positional
//! action. A missing helper adds the warning `overlay helper not running`
//! and does not fail the action. Tests pass a temp socket path and do not
//! open a window.

#![allow(unsafe_code)]

#[cfg(unix)]
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ComputerError;

std::thread_local! {
    static SOCKET_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Restores the previous overlay socket override.
pub struct SocketPathGuard {
    previous: Option<PathBuf>,
}

impl Drop for SocketPathGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        SOCKET_PATH.with(|slot| *slot.borrow_mut() = previous);
    }
}

/// Send overlay points to `path` until the guard drops.
pub fn push_socket_path(path: impl Into<PathBuf>) -> SocketPathGuard {
    let previous = SOCKET_PATH.with(|slot| slot.borrow_mut().replace(path.into()));
    SocketPathGuard { previous }
}

/// One cursor position for the helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayPoint {
    /// Screen x in logical points.
    pub x: i32,
    /// Screen y in logical points.
    pub y: i32,
    /// Session id. The helper picks a ring color from this string.
    pub session: String,
}

/// Newline-terminated JSON for one point.
pub fn encode_point(x: i32, y: i32, session: &str) -> String {
    let point = OverlayPoint {
        x,
        y,
        session: session.to_string(),
    };
    let mut line = serde_json::to_string(&point).unwrap_or_else(|_| "{}".to_string());
    line.push('\n');
    line
}

/// Parse one socket line. Binary input and non-objects are rejected.
pub fn parse_overlay_line(bytes: &[u8]) -> Result<OverlayPoint, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "overlay message is not utf-8 json")?;
    let text = text.trim();
    if text.is_empty() {
        return Err("overlay message is empty");
    }
    if !text.starts_with('{') {
        return Err("overlay message is not a json object");
    }
    let point: OverlayPoint =
        serde_json::from_str(text).map_err(|_| "overlay message is missing x, y, or session")?;
    Ok(point)
}

/// Hue in `0..360` for a session id. Stable for the same string.
pub fn session_hue(session: &str) -> u32 {
    let mut hash: u32 = 2_166_136_261;
    for byte in session.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash % 360
}

/// Tell the helper about a point. `Some(warning)` when it is not listening.
///
/// Tests that did not call [`push_socket_path`] do nothing, so they never
/// touch the real cache socket.
pub fn note_point(x: i32, y: i32, session: &str) -> Option<&'static str> {
    let path = SOCKET_PATH
        .with(|slot| slot.borrow().clone())
        .or_else(production_socket)?;
    if deliver(&path, x, y, session).is_err() {
        Some("overlay helper not running")
    } else {
        None
    }
}

#[cfg(all(not(test), target_os = "macos"))]
fn production_socket() -> Option<PathBuf> {
    Some(crate::cache::cache_dir().join("overlay.sock"))
}

#[cfg(not(all(not(test), target_os = "macos")))]
fn production_socket() -> Option<PathBuf> {
    None
}

/// Run the overlay helper. Off macOS this is `unsupported`.
pub fn serve() -> Result<(), ComputerError> {
    crate::backend::serve_overlay()
}

fn deliver(path: &Path, x: i32, y: i32, session: &str) -> Result<(), ()> {
    #[cfg(unix)]
    {
        unix_send(path, &encode_point(x, y, session))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, x, y, session);
        Err(())
    }
}

#[cfg(unix)]
fn unix_send(path: &Path, line: &str) -> Result<(), ()> {
    let mut stream = connect_timeout(path, Duration::from_millis(200))?;
    stream
        .set_write_timeout(Some(Duration::from_millis(200)))
        .map_err(|_| ())?;
    stream.write_all(line.as_bytes()).map_err(|_| ())?;
    Ok(())
}

#[cfg(unix)]
fn connect_timeout(path: &Path, timeout: Duration) -> Result<std::os::unix::net::UnixStream, ()> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: the socket is created here and either returned via `from_raw_fd` or closed.
    // The address points at a stack `sockaddr_un` that outlives `connect` and `poll`.
    // The length includes the `sun_len` field on macOS and the trailing NUL, matching `std`.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(());
        }
        let flags = libc::fcntl(fd, libc::F_GETFL, 0);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            libc::close(fd);
            return Err(());
        }
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        if bytes.contains(&0) || bytes.len() >= addr.sun_path.len() {
            libc::close(fd);
            return Err(());
        }
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            addr.sun_path.as_mut_ptr().cast::<u8>(),
            bytes.len(),
        );
        let mut len = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len();
        if bytes.first().is_some_and(|byte| *byte != 0) {
            len += 1;
        }
        let rc = libc::connect(fd, (&raw const addr).cast(), len as libc::socklen_t);
        if rc < 0 {
            let err = io::Error::last_os_error();
            let in_progress = err.raw_os_error() == Some(libc::EINPROGRESS)
                || err.raw_os_error() == Some(libc::EAGAIN)
                || err.kind() == io::ErrorKind::WouldBlock;
            if !in_progress {
                libc::close(fd);
                return Err(());
            }
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
            if libc::poll(&mut pollfd, 1, ms) <= 0 {
                libc::close(fd);
                return Err(());
            }
            let mut so_error: libc::c_int = 0;
            let mut so_len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            let got = libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&raw mut so_error).cast(),
                &mut so_len,
            );
            if got < 0 || so_error != 0 {
                libc::close(fd);
                return Err(());
            }
        }
        if flags >= 0 {
            let _ = libc::fcntl(fd, libc::F_SETFL, flags);
        }
        Ok(std::os::unix::net::UnixStream::from_raw_fd(fd))
    }
}

/// Remove a stale socket, bind, and set mode `0600`.
#[cfg(unix)]
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub(crate) fn bind_socket(path: &Path) -> io::Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    if let Some(parent) = path.parent() {
        fs_create(parent)?;
        fs_set_mode(parent, 0o700);
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

#[cfg(unix)]
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
fn fs_create(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)
}

#[cfg(unix)]
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
fn fs_set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_a_json_line_and_rejects_a_binary_blob() {
        let line = encode_point(10, 20, "session-a");
        let point = parse_overlay_line(line.as_bytes()).unwrap();
        assert_eq!(
            point,
            OverlayPoint {
                x: 10,
                y: 20,
                session: "session-a".into(),
            }
        );
        assert!(parse_overlay_line(&[0xff, 0xfe, 0x00]).is_err());
        assert!(parse_overlay_line(b"not-json\n").is_err());
        assert_eq!(parse_overlay_line(b"   "), Err("overlay message is empty"));
        assert_eq!(
            parse_overlay_line(b"[]"),
            Err("overlay message is not a json object")
        );
        assert!(session_hue("notes") < 360);
        assert_eq!(session_hue("notes"), session_hue("notes"));
    }

    #[test]
    fn missing_helper_warns_and_a_live_socket_receives_the_line() {
        // `sockaddr_un` is about 100 bytes. A long temp directory does not fit.
        #[cfg(unix)]
        let dir = std::path::PathBuf::from(format!("/tmp/cuo{}", std::process::id()));
        #[cfg(not(unix))]
        let dir = std::env::temp_dir().join(format!("cuo{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing.sock");
        let _guard = push_socket_path(&missing);
        assert_eq!(note_point(3, 4, "s"), Some("overlay helper not running"));
        drop(_guard);

        #[cfg(unix)]
        {
            use std::io::BufRead;
            let path = dir.join("overlay.sock");
            let listener = bind_socket(&path).unwrap();
            let _guard = push_socket_path(&path);
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                std::io::BufReader::new(stream)
                    .read_line(&mut line)
                    .unwrap();
                line
            });
            assert_eq!(note_point(8, 9, "agent"), None);
            let line = server.join().unwrap();
            let point = parse_overlay_line(line.as_bytes()).unwrap();
            assert_eq!(point.x, 8);
            assert_eq!(point.y, 9);
            assert_eq!(point.session, "agent");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
