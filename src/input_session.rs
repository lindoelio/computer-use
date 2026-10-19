//! Keeps mouse buttons and held keys down across calls in one process.
//!
//! `serve` holds one [`InputSession`] for the whole stdin loop, so a
//! `mouse_down` in one request stays down until a later `mouse_up` or until
//! serve exits. A one-shot command does not open a session: `dispatch`
//! releases anything still held when it returns. A batch opens a nested
//! session so its steps share the hold. The release runs when the last
//! session on this thread drops.

use std::cell::Cell;

use crate::backend::DesktopBackend;

std::thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// True while at least one [`InputSession`] is alive on this thread.
pub fn session_open() -> bool {
    DEPTH.with(|depth| depth.get() > 0)
}

/// Guard that releases held buttons and keys when the last one drops.
pub struct InputSession<'a> {
    backend: &'a dyn DesktopBackend,
}

impl<'a> InputSession<'a> {
    /// Increment the session depth.
    ///
    /// The last drop calls [`DesktopBackend::release_held`].
    pub fn open(backend: &'a dyn DesktopBackend) -> Self {
        DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self { backend }
    }
}

impl Drop for InputSession<'_> {
    fn drop(&mut self) {
        let release = DEPTH.with(|depth| {
            let next = depth.get().saturating_sub(1);
            depth.set(next);
            next == 0
        });
        if release {
            let _ = self.backend.release_held();
        }
    }
}
