//! Cross-platform desktop automation for agents.
//!
//! The CLI and the library share [`dispatch`]: a typed [`Action`] goes in and a
//! [`Response`] comes out. Platform backends live behind [`DesktopBackend`].
//!
//! # Example
//!
//! ```
//! use computer_use::{dispatch, Action};
//!
//! let response = dispatch(Action::Wait {
//!     ms: Some(0),
//!     seconds: None,
//! });
//! assert!(response.ok);
//! assert_eq!(response.to_value()["waited_ms"].as_u64(), Some(0));
//! ```

#![deny(unsafe_code)]

pub mod adapters;
mod attempt;
pub mod audit;
pub mod backend;
pub mod browser;
pub mod cache;
mod doctor;
pub mod element;
pub mod error;
mod foreground;
mod image_util;
mod input_session;
pub mod isolation;
pub mod keys;
pub mod menu_path;
pub mod overlay;
pub mod policy;
pub mod protocol;
pub mod scroll;
pub mod shortcuts;
pub mod state;
pub mod target;
mod tier;
pub mod tree;
pub mod types;
pub mod vision;
pub mod window_groups;

#[cfg(test)]
pub(crate) mod test_backend;

pub use backend::{create_desktop_backend, Backend, DesktopBackend};
pub use error::{ComputerError, ErrorBody};
pub use image_util::{
    encode_image, image_to_screen, screen_to_image, EncodedImage, MapFormulas, MapPoint,
    ScreenshotMap,
};
pub use input_session::InputSession;
pub use keys::{parse_key_combo, parse_key_sequence, KeyCombo};
pub use protocol::{
    dispatch, dispatch_context, dispatch_with, handle_request_line, handle_request_line_with,
    parse_request, Action, CallContext, Request, Response,
};
pub use types::{
    parse_path, parse_region, DisplayInfo, Frame, FrontmostInfo, ImageFormat, Modifier,
    MouseButton, Point, Region, ScrollDirection,
};

/// Restore Electron accessibility flags and stop AX observers.
///
/// The CLI calls this before it exits. A crash skips it, so a flag this process
/// turned on can stay set until the target app quits. `session end` restores
/// from the session cache in a later process.
pub fn shutdown_sessions() {
    backend::shutdown_sessions();
}

#[cfg(target_os = "linux")]
pub use backend::LinuxDesktopBackend;
#[cfg(target_os = "macos")]
pub use backend::MacOsDesktopBackend;
#[cfg(target_os = "windows")]
pub use backend::WindowsDesktopBackend;
