//! P10: Chrome DevTools Protocol (CDP) attach-only client.
//!
//! The CLI thread stays sync. Every browser call spins up a fresh
//! `tokio::runtime::Builder::new_current_thread()` runtime, runs the async CDP
//! request, and drops the runtime before returning. The backend never spawns
//! Chrome — the user already started the browser with
//! `--remote-debugging-port=N`, and we just attach.
//!
//! Built only when the `cdp` feature is on. The default build exposes the
//! `unsupported` error code and nothing else.
//!
//! ## Port discovery
//!
//! When the caller passes `--cdp` without `--cdp-url`, we look up the Chrome
//! pid that owns the `--app` (or the frontmost one if no app is named) and
//! read its argv:
//!
//! * macOS: `sysctl KERN_PROCARGS2` for the pid.
//! * Linux: `/proc/<pid>/cmdline` (NUL-separated UTF-8).
//! * Windows: best-effort; not implemented in this phase (see ROADMAP.md).
//!
//! Only the first matching `--remote-debugging-port=N` wins. The handler
//! `ws://127.0.0.1:<N>` is what we hand to `chromiumoxide::Browser::connect`.
//!
//! ## Viewport -> screen mapping (P5 reuse)
//!
//! `browser_click` resolves a CSS selector, asks the element to click, and
//! chromiumoxide clicks in CSS pixel space. The screenshot layer maps those
//! coordinates into physical screen coordinates for the existing P3 click
//! path. HiDPI and CSS pixels-above-fold cases still rely on the configured
//! `coord` ratio like the rest of the codebase; we do not introduce a new
//! viewport transform here.

pub mod cdp;

pub use cdp::{discover_debug_port, parse_debug_port, ActionOptions, AttachOptions, TabSummary};
