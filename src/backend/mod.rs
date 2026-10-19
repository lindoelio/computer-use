//! Platform desktop backends.
//!
//! Screenshot capture uses `xcap`. Pointer and keyboard input use `enigo`.
//! Accessibility and the frontmost window are platform-specific and best-effort.

mod capture;
mod command;
mod input;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use crate::error::ComputerError;
use crate::target::{AppInfo, DoctorReport, LaunchResult, QuitResult, WindowList};
use crate::types::{DisplayInfo, Frame, FrontmostInfo, Modifier, MouseButton, Region};

/// Error for an operation this operating system does not implement.
pub(crate) fn unsupported(operation: &str) -> ComputerError {
    ComputerError::Unsupported(format!(
        "{operation} is not available on {}",
        std::env::consts::OS
    ))
}

pub use input::validate_key;

/// Desktop automation surface used by [`crate::dispatch`].
///
/// Also exported as [`Backend`].
pub trait DesktopBackend: Send + Sync {
    /// Capture the desktop, or a logical region of it, as RGBA pixels.
    fn screenshot(&self, region: Option<Region>) -> Result<Frame, ComputerError>;

    /// Move the pointer without clicking.
    fn move_mouse(&self, x: i32, y: i32) -> Result<(), ComputerError>;

    /// Move the pointer to `(x, y)` and click once.
    fn simulate_click(&self, x: i32, y: i32, button: MouseButton) -> Result<(), ComputerError>;

    /// Type Unicode text into the focused control.
    fn simulate_type(&self, text: &str) -> Result<(), ComputerError>;

    /// Press modifiers, tap `key`, then release the modifiers.
    ///
    /// `key` is a canonical name from [`crate::keys::canonical_key`].
    fn simulate_key(&self, modifiers: &[Modifier], key: &str) -> Result<(), ComputerError>;

    /// Scroll by wheel steps. Positive `dy` scrolls down. Positive `dx` scrolls right.
    fn simulate_scroll(&self, dx: i32, dy: i32) -> Result<(), ComputerError>;

    /// Current pointer position in logical pixels.
    fn cursor_position(&self) -> Result<(i32, i32), ComputerError>;

    /// Connected displays. Does not capture pixels.
    fn list_displays(&self) -> Result<Vec<DisplayInfo>, ComputerError>;

    /// Frontmost application and window. Never panics; missing data is `None`.
    fn frontmost(&self) -> FrontmostInfo;

    /// Accessibility tree as a JSON string, when the platform can produce one.
    fn inspect_accessibility(&self, app_name: Option<&str>) -> Result<String, ComputerError>;

    /// Running applications. `all` includes accessory and prohibited apps.
    fn list_apps(&self, _all: bool) -> Result<Vec<AppInfo>, ComputerError> {
        Err(unsupported("list_apps"))
    }

    /// On-screen windows in z-order. Index 0 is front.
    fn list_windows(&self) -> Result<WindowList, ComputerError> {
        Err(unsupported("list_windows"))
    }

    /// Launch an application by name, bundle id, or path.
    fn launch(&self, _app: &str) -> Result<LaunchResult, ComputerError> {
        Err(unsupported("launch"))
    }

    /// Bring an application forward. `window` selects a window inside it.
    fn activate(&self, _app: &str, _window: Option<&str>) -> Result<LaunchResult, ComputerError> {
        Err(unsupported("activate"))
    }

    /// Ask an application to quit. This does not force-terminate.
    fn quit(&self, _app: &str) -> Result<QuitResult, ComputerError> {
        Err(unsupported("quit"))
    }

    /// Permission probe. `request` may show a system prompt on macOS.
    fn doctor(&self, _request: bool) -> Result<DoctorReport, ComputerError> {
        Err(unsupported("doctor"))
    }
}

/// Alias for [`DesktopBackend`].
pub use DesktopBackend as Backend;

/// Create the backend for the current operating system.
pub fn create_desktop_backend() -> Box<dyn DesktopBackend> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacOsDesktopBackend)
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxDesktopBackend)
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WindowsDesktopBackend)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        compile_error!("computer-use supports macOS, Linux, and Windows");
    }
}

macro_rules! impl_platform_backend {
    ($ty:ty, $inspect:path, $front:path, $doctor:path, $list_apps:path, $list_windows:path, $launch:path, $activate:path, $quit:path) => {
        impl DesktopBackend for $ty {
            fn screenshot(&self, region: Option<Region>) -> Result<Frame, ComputerError> {
                capture::capture(region)
            }

            fn move_mouse(&self, x: i32, y: i32) -> Result<(), ComputerError> {
                input::move_mouse(x, y)
            }

            fn simulate_click(
                &self,
                x: i32,
                y: i32,
                button: MouseButton,
            ) -> Result<(), ComputerError> {
                input::click(x, y, button)
            }

            fn simulate_type(&self, text: &str) -> Result<(), ComputerError> {
                input::type_text(text)
            }

            fn simulate_key(&self, modifiers: &[Modifier], key: &str) -> Result<(), ComputerError> {
                input::press_key(modifiers, key)
            }

            fn simulate_scroll(&self, dx: i32, dy: i32) -> Result<(), ComputerError> {
                input::scroll(dx, dy)
            }

            fn cursor_position(&self) -> Result<(i32, i32), ComputerError> {
                input::cursor_position()
            }

            fn list_displays(&self) -> Result<Vec<DisplayInfo>, ComputerError> {
                capture::list_displays()
            }

            fn frontmost(&self) -> FrontmostInfo {
                $front()
            }

            fn inspect_accessibility(
                &self,
                app_name: Option<&str>,
            ) -> Result<String, ComputerError> {
                $inspect(app_name)
            }

            fn list_apps(&self, all: bool) -> Result<Vec<AppInfo>, ComputerError> {
                $list_apps(all)
            }

            fn list_windows(&self) -> Result<WindowList, ComputerError> {
                $list_windows()
            }

            fn launch(&self, app: &str) -> Result<LaunchResult, ComputerError> {
                $launch(app)
            }

            fn activate(
                &self,
                app: &str,
                window: Option<&str>,
            ) -> Result<LaunchResult, ComputerError> {
                $activate(app, window)
            }

            fn quit(&self, app: &str) -> Result<QuitResult, ComputerError> {
                $quit(app)
            }

            fn doctor(&self, request: bool) -> Result<DoctorReport, ComputerError> {
                $doctor(request)
            }
        }
    };
}

#[cfg(target_os = "macos")]
impl_platform_backend!(
    macos::MacOsDesktopBackend,
    macos::inspect_accessibility,
    macos::frontmost,
    macos::doctor,
    macos::list_apps,
    macos::list_windows,
    macos::launch,
    macos::activate,
    macos::quit
);
#[cfg(target_os = "linux")]
impl_platform_backend!(
    linux::LinuxDesktopBackend,
    linux::inspect_accessibility,
    linux::frontmost,
    linux::doctor,
    linux::list_apps,
    linux::list_windows,
    linux::launch,
    linux::activate,
    linux::quit
);
#[cfg(target_os = "windows")]
impl_platform_backend!(
    windows::WindowsDesktopBackend,
    windows::inspect_accessibility,
    windows::frontmost,
    windows::doctor,
    windows::list_apps,
    windows::list_windows,
    windows::launch,
    windows::activate,
    windows::quit
);

#[cfg(target_os = "linux")]
pub use linux::LinuxDesktopBackend;
#[cfg(target_os = "macos")]
pub use macos::MacOsDesktopBackend;
#[cfg(target_os = "windows")]
pub use windows::WindowsDesktopBackend;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_desktop_backend_is_a_trait_object() {
        let backend = create_desktop_backend();
        let _backend: &dyn DesktopBackend = backend.as_ref();
    }
}
