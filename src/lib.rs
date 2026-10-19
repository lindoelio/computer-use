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

#![forbid(unsafe_code)]

pub mod backend;
pub mod error;
mod image_util;
pub mod keys;
pub mod protocol;
pub mod state;
pub mod types;

#[cfg(test)]
pub(crate) mod test_backend;

pub use backend::{create_desktop_backend, Backend, DesktopBackend};
pub use error::{ComputerError, ErrorBody};
pub use keys::{parse_key_combo, KeyCombo};
pub use protocol::{
    dispatch, dispatch_with, handle_request_line, parse_request, Action, Request, Response,
};
pub use types::{
    parse_region, DisplayInfo, Frame, FrontmostInfo, ImageFormat, Modifier, MouseButton, Region,
    ScrollDirection,
};

#[cfg(target_os = "linux")]
pub use backend::LinuxDesktopBackend;
#[cfg(target_os = "macos")]
pub use backend::MacOsDesktopBackend;
#[cfg(target_os = "windows")]
pub use backend::WindowsDesktopBackend;
