//! Platform desktop backends.
//!
//! Screenshot capture uses `xcap`. Pointer and keyboard input use `enigo`.
//! Accessibility and the frontmost window are platform-specific and best-effort.

mod capture;
pub(crate) mod clipboard;
pub(crate) mod command;
pub(crate) mod held;
mod input;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use crate::element::{ElementError, ElementOutcome, ElementRequest, HitInfo};
use crate::error::ComputerError;
use crate::protocol::AppleEventLanguage;
use crate::target::{AppInfo, DoctorReport, LaunchResult, QuitResult, WindowBounds, WindowList};
use crate::tree::{TreeQuery, TreeReport};
use crate::types::{DisplayInfo, Frame, FrontmostInfo, Modifier, MouseButton, RawCapture, Region};
use crate::window_groups::SpacesReport;
use serde::Serialize;

/// One window selected for capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowTarget {
    /// Platform window id. On macOS this is the CG window id.
    pub window_id: u32,
    /// Owning process id.
    pub pid: i32,
    /// Screen bounds in logical points.
    pub bounds: WindowBounds,
}

/// Error for an operation this operating system does not implement.
pub(crate) fn unsupported(operation: &str) -> ComputerError {
    ComputerError::Unsupported(format!(
        "{operation} is not available on {}",
        std::env::consts::OS
    ))
}

pub(crate) use command::run_with_stdin;
pub use input::validate_key;

/// One entry under a menu bar item or submenu. The macOS backend fills `title`
/// from `AXTitle`, `enabled` from `AXEnabled`, and recurses into `AXMenu`
/// children up to `depth` levels.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MenuNode {
    /// Item title from `AXTitle`. Empty string when the menu hides it.
    pub title: String,
    /// `AXEnabled`. Items are still listed when `false`; the macOS backend
    /// refuses to press them.
    pub enabled: bool,
    /// Submenu children. Empty for leaf items.
    pub children: Vec<MenuNode>,
}

/// Full menu report for one running app.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MenuListReport {
    /// App name as resolved by the platform.
    pub app: String,
    /// Sections under the menu bar. Each one is a top-level menu (File, Edit,
    /// ...) with the items inside it.
    pub sections: Vec<MenuNode>,
}

/// What `menu_click` did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MenuClickReport {
    /// `true` when the click was pressed. `false` is reserved for a deliberate
    /// no-op (the path resolved but the trailing segment was a separator).
    pub pressed: bool,
}

/// Outcome of an Apple event script.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppleEventReport {
    /// Number of bytes in the original script. The body is never logged.
    pub script_bytes: usize,
    /// Lowercase hex hash of the script body. Audit lines include this so a
    /// script can be identified without leaking its content.
    pub script_hash: String,
    /// Standard output, trimmed to one line. Empty when the script did not
    /// write anything.
    pub stdout: String,
    /// Standard error, trimmed. Empty when `osascript` was silent.
    pub stderr: String,
    /// Exit status of `osascript`.
    pub exit_status: i32,
}

/// One Dock item.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DockItem {
    /// Item title as the Dock exposes it.
    pub title: String,
}

/// Full Dock listing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DockListReport {
    /// Items in Dock order.
    pub items: Vec<DockItem>,
}

/// What `dock_click` did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DockClickReport {
    /// `true` when the click was pressed.
    pub pressed: bool,
}

/// Desktop automation surface used by [`crate::dispatch`].
///
/// Also exported as [`Backend`].
pub trait DesktopBackend: Send + Sync {
    /// Capture the desktop, or a logical region of it, as RGBA pixels.
    fn screenshot(&self, region: Option<Region>) -> Result<Frame, ComputerError>;

    /// Desktop or region capture plus the map inputs (origin and logical size).
    fn capture_screen(&self, region: Option<Region>) -> Result<RawCapture, ComputerError> {
        capture::capture_screen(region)
    }

    /// Capture one window. macOS uses ScreenCaptureKit. Other platforms crop
    /// the desktop image when the bounds are known.
    fn capture_window(&self, target: &WindowTarget) -> Result<RawCapture, ComputerError> {
        #[cfg(target_os = "macos")]
        {
            macos::capture_window(target)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = target.pid;
            let _ = target.window_id;
            capture::crop_window(target.bounds)
        }
    }

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

    /// Ask an application to quit. `force` uses a forced terminate where the OS has one.
    fn quit(&self, _app: &str, _force: bool) -> Result<QuitResult, ComputerError> {
        Err(unsupported("quit"))
    }

    /// Walk `app`'s menu bar. The macOS backend uses
    /// `AXMenuBar -> AXMenuBarItem -> AXMenu`. Other platforms return
    /// `unsupported`. `depth` caps recursion; out-of-depth submenus are
    /// reported as a single placeholder child.
    fn menu_list(&self, _app: &str, _depth: u32) -> Result<MenuListReport, ComputerError> {
        Err(unsupported("menu_list"))
    }

    /// Press one menu item by `">"`-separated titles. A miss is
    /// `element_not_found`. A disabled item is `execution_failed`.
    fn menu_click(
        &self,
        _app: &str,
        _path: &crate::menu_path::MenuPath,
    ) -> Result<MenuClickReport, ComputerError> {
        Err(unsupported("menu_click"))
    }

    /// Run an Apple event via `osascript`. macOS only. `script` is the body
    /// passed with `-e`; it never goes through a shell. The script body is
    /// never logged verbatim; only its byte count and SHA-256 are recorded.
    fn apple_event(
        &self,
        _app: &str,
        _script: &str,
        _language: AppleEventLanguage,
    ) -> Result<AppleEventReport, ComputerError> {
        Err(unsupported("apple_event"))
    }

    /// List Dock items. macOS only.
    fn dock_list(&self) -> Result<DockListReport, ComputerError> {
        Err(unsupported("dock_list"))
    }

    /// Click a Dock item by title. macOS only. A miss is `element_not_found`.
    fn dock_click(&self, _title: &str) -> Result<DockClickReport, ComputerError> {
        Err(unsupported("dock_click"))
    }

    /// Group on-screen windows by the screen that contains each window's
    /// center. Mission Control moves are not attempted; the skylight feature,
    /// when active, may opt into best-effort space moves.
    fn spaces(&self) -> Result<SpacesReport, ComputerError> {
        Err(unsupported("spaces"))
    }

    /// Permission probe. `options.prompt` may show a system prompt on macOS.
    ///
    /// The default does not prompt and does not open Settings.
    fn doctor(
        &self,
        _options: crate::doctor::DoctorOptions,
    ) -> Result<DoctorReport, ComputerError> {
        Err(unsupported("doctor"))
    }

    /// Numbered accessibility tree for the key window.
    fn tree(&self, _query: &TreeQuery) -> Result<TreeReport, ComputerError> {
        Err(unsupported("tree"))
    }

    /// Restore accessibility flags recorded for `session`.
    ///
    /// The returned pids are the processes whose flags were written back.
    /// An empty list means there was nothing to restore.
    fn end_session(&self, _session: Option<&str>) -> Result<Vec<i32>, ComputerError> {
        Ok(Vec::new())
    }

    /// Press, focus, set a value, select text, perform an action, scroll, or type
    /// into one cached element. The element is revalidated before the action.
    fn element_action(&self, request: &ElementRequest) -> Result<ElementOutcome, ElementError> {
        #[cfg(target_os = "macos")]
        {
            macos::element_action(request)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(ElementError::new(unsupported(request.op.action_name())))
        }
    }

    /// Locked, asleep, or no display. `None` when capture may proceed.
    ///
    /// The default is `None` so the stub does not read the live session.
    /// The macOS backend overrides it.
    fn display_block(&self) -> Option<ComputerError> {
        None
    }

    /// Page URL for a browser process. `Ok(None)` when the platform has no URL.
    ///
    /// A [`ComputerError::PermissionRequired`] fails closed when a block list
    /// caused the call. Other platforms return `Ok(None)`.
    fn browser_url(&self, pid: i32) -> Result<Option<String>, ComputerError> {
        #[cfg(target_os = "macos")]
        {
            macos::browser_url(pid)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = pid;
            Ok(None)
        }
    }

    /// Title and secure bit of a cached element. `Ok(None)` when it is not cached.
    ///
    /// The default on macOS reads the session cache. The stub reads its fixture.
    fn peek_element(
        &self,
        app: Option<&str>,
        window: Option<&str>,
        session: Option<&str>,
        index: u32,
    ) -> Result<Option<crate::element::ElementPeek>, ComputerError> {
        #[cfg(target_os = "macos")]
        {
            macos::peek_element(app, window, session, index)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (app, window, session, index);
            Ok(None)
        }
    }

    /// Install the listen-only user-input tap when this process should pause.
    ///
    /// `None` when the tap is installed or was not requested. The warning string
    /// is `user-input pause unavailable`. Tests never create a real event tap.
    fn user_input_pause_warning(&self) -> Option<&'static str> {
        #[cfg(test)]
        {
            None
        }
        #[cfg(all(not(test), target_os = "macos"))]
        {
            macos::input::ensure_installed()
        }
        #[cfg(all(not(test), not(target_os = "macos")))]
        {
            Some("user-input pause unavailable")
        }
    }

    /// Hit-test a screen point. macOS reports the element under the point.
    /// Other platforms return `Ok(None)` and the coordinate click still runs.
    fn hit_test(&self, x: i32, y: i32) -> Result<Option<HitInfo>, ComputerError> {
        #[cfg(target_os = "macos")]
        {
            macos::hit_test(x, y)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (x, y);
            Ok(None)
        }
    }

    /// Whether background event posting exists. The stub overrides this.
    fn input_support(&self) -> crate::tier::Capabilities {
        #[cfg(target_os = "macos")]
        {
            crate::tier::Capabilities { event: true }
        }
        #[cfg(not(target_os = "macos"))]
        {
            crate::tier::Capabilities { event: false }
        }
    }

    /// Walk the latest tree and return rectangles in image pixels plus the
    /// label to draw (`n` for tree index). The screenshot command uses
    /// these for the `--marks` overlay.
    ///
    /// Empty results are not an error; the dispatcher decides whether to
    /// fall back to OCR or to leave the image unmarked. Non-macOS platforms
    /// always return `Ok(Vec::new())`; OCR for those platforms is `Unsupported`.
    fn marks_for_tree(
        &self,
        _query: &TreeQuery,
        _map: &crate::image_util::ScreenshotMap,
    ) -> Result<Vec<crate::vision::MarkBox>, ComputerError> {
        Ok(Vec::new())
    }

    /// Run OCR on one captured image and return the redacted, screen-remapped
    /// blocks. `image` is the prepared shot's pixels.
    fn recognize_text(
        &self,
        _image: &image::RgbaImage,
        _map: &crate::image_util::ScreenshotMap,
    ) -> Result<crate::vision::OcrResult, ComputerError> {
        Err(unsupported("ocr"))
    }

    /// Image-pixel rectangles of every secure element in the latest tree.
    /// The OCR dispatcher drops recognized text whose block falls inside
    /// any of these rectangles. The default returns an empty list, which
    /// matches every non-macOS platform and the mock backend.
    fn secure_ocr_frames(
        &self,
        _query: &TreeQuery,
    ) -> Result<Vec<crate::vision::ocr::ImageRect>, ComputerError> {
        Ok(Vec::new())
    }

    /// Read the clipboard. The default uses the system pasteboard outside tests.
    ///
    /// The stub overrides this and never touches the real pasteboard.
    fn clipboard_get(&self) -> Result<clipboard::ClipboardContents, ComputerError> {
        clipboard::clipboard_get()
    }

    /// Write clipboard text. Empty text is refused before this is called.
    fn clipboard_set_text(&self, text: &str) -> Result<(), ComputerError> {
        clipboard::clipboard_set_text(text)
    }

    /// Write an RGBA image to the clipboard.
    fn clipboard_set_image(
        &self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), ComputerError> {
        clipboard::clipboard_set_image(width, height, rgba)
    }

    /// Release mouse buttons and keys this process still holds.
    ///
    /// The real registry is process-global. The stub overrides this method.
    fn release_held(&self) -> Result<(), ComputerError> {
        held::release_all()
    }

    /// Run one input tier. The default posts nothing.
    ///
    /// `event` and `synthetic` are `unsupported` off macOS. `activate` uses the
    /// existing foreground enigo path and does not restore focus.
    fn perform_input(
        &self,
        job: &crate::tier::InputJob,
    ) -> Result<crate::tier::InputReport, ComputerError> {
        #[cfg(target_os = "macos")]
        {
            macos::perform_input(job)
        }
        #[cfg(not(target_os = "macos"))]
        {
            // `Self` is unsized when this default runs through `dyn DesktopBackend`.
            off_platform_input(self, job)
        }
    }
}

/// Foreground enigo path used when event posting is not available.
///
/// `B: ?Sized` so the default method can pass `self` from a trait object.
#[cfg(not(target_os = "macos"))]
fn off_platform_input<B: DesktopBackend + ?Sized>(
    backend: &B,
    job: &crate::tier::InputJob,
) -> Result<crate::tier::InputReport, ComputerError> {
    use crate::tier::{InputKind, InputReport, TierName};
    match job.tier {
        TierName::Event => {
            return Err(unsupported("tier event"));
        }
        TierName::Synthetic => {
            return Err(unsupported("tier synthetic"));
        }
        TierName::Ax => {
            return Err(unsupported("tier ax for coordinate and keyboard input"));
        }
        TierName::Activate => {}
    }
    if job.require_background {
        return Err(ComputerError::ExecutionFailed(
            "--require-background refuses tier activate because it uses the foreground cursor"
                .into(),
        ));
    }
    match &job.kind {
        InputKind::Click {
            x,
            y,
            button,
            count,
        } => {
            for index in 0..*count {
                if index > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                backend.simulate_click(*x, *y, *button)?;
            }
        }
        InputKind::Move { x, y } => backend.move_mouse(*x, *y)?,
        InputKind::Scroll { x, y, dx, dy } => {
            if let (Some(x), Some(y)) = (x, y) {
                backend.move_mouse(*x, *y)?;
            }
            backend.simulate_scroll(*dx, *dy)?;
        }
        InputKind::Type { text } => backend.simulate_type(text)?,
        InputKind::Key { strokes } => {
            for stroke in strokes {
                backend.simulate_key(&stroke.modifiers, &stroke.key)?;
            }
        }
        InputKind::Drag { points, button } => input::drag(points, *button)?,
        InputKind::MouseDown { x, y, button } => input::button_down(*x, *y, *button)?,
        InputKind::MouseUp { x, y, button } => input::button_up(*x, *y, *button)?,
        InputKind::HoldKey { modifiers, key, ms } => input::hold_key(modifiers, key, *ms)?,
    }
    Ok(InputReport {
        tier: "activate",
        cursor_moved: true,
        focus_restored: None,
        warnings: Vec::new(),
    })
}

/// Overlay helper. macOS opens the click-through window. Other platforms return `unsupported`.
pub(crate) fn serve_overlay() -> Result<(), ComputerError> {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        macos::overlay::serve()
    }
    #[cfg(all(target_os = "macos", test))]
    {
        Err(ComputerError::Unsupported(
            "overlay serve does not open a window in tests".into(),
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported("overlay serve"))
    }
}

/// Restore Electron flags and stop observers. No-op off macOS.
pub fn shutdown_sessions() {
    #[cfg(target_os = "macos")]
    macos::shutdown_sessions();
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
    ($ty:ty, $inspect:path, $front:path, $doctor:path, $list_apps:path, $list_windows:path, $launch:path, $activate:path, $quit:path, $tree:path, $end_session:path) => {
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

            fn display_block(&self) -> Option<ComputerError> {
                #[cfg(target_os = "macos")]
                {
                    macos::session_display_block()
                }
                #[cfg(not(target_os = "macos"))]
                {
                    None
                }
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

            fn quit(&self, app: &str, force: bool) -> Result<QuitResult, ComputerError> {
                $quit(app, force)
            }

            fn doctor(
                &self,
                options: crate::doctor::DoctorOptions,
            ) -> Result<DoctorReport, ComputerError> {
                $doctor(options)
            }

            fn tree(&self, query: &TreeQuery) -> Result<TreeReport, ComputerError> {
                $tree(query)
            }

            fn end_session(&self, session: Option<&str>) -> Result<Vec<i32>, ComputerError> {
                $end_session(session)
            }

            // P9: menu bar, Apple events, Dock, and Spaces are macOS-only.
            // Linux and Windows keep the `unsupported` default from the trait.
            #[cfg(target_os = "macos")]
            fn menu_list(&self, app: &str, depth: u32) -> Result<MenuListReport, ComputerError> {
                macos::menu::list(app, depth)
            }

            #[cfg(target_os = "macos")]
            fn menu_click(
                &self,
                app: &str,
                path: &crate::menu_path::MenuPath,
            ) -> Result<MenuClickReport, ComputerError> {
                macos::menu::click(app, path)
            }

            #[cfg(target_os = "macos")]
            fn apple_event(
                &self,
                app: &str,
                script: &str,
                language: AppleEventLanguage,
            ) -> Result<AppleEventReport, ComputerError> {
                let _ = app;
                let outcome = macos::apple::run(script, language)?;
                Ok(AppleEventReport {
                    script_bytes: outcome.script_bytes,
                    script_hash: outcome.script_hash,
                    stdout: outcome.stdout,
                    stderr: outcome.stderr,
                    exit_status: outcome.exit_status,
                })
            }

            #[cfg(target_os = "macos")]
            fn dock_list(&self) -> Result<DockListReport, ComputerError> {
                macos::dock::list()
            }

            #[cfg(target_os = "macos")]
            fn dock_click(&self, title: &str) -> Result<DockClickReport, ComputerError> {
                macos::dock::click(title)
            }

            #[cfg(target_os = "macos")]
            fn spaces(&self) -> Result<crate::window_groups::SpacesReport, ComputerError> {
                Ok(macos::spaces::report())
            }

            // P11: marks overlay reuses the latest tree; OCR runs Apple
            // Vision. macOS overrides the trait defaults; Linux and Windows
            // keep the empty/unsupported ones.
            #[cfg(target_os = "macos")]
            fn marks_for_tree(
                &self,
                query: &TreeQuery,
                map: &crate::image_util::ScreenshotMap,
            ) -> Result<Vec<crate::vision::MarkBox>, ComputerError> {
                macos::marks_for_tree(query, map)
            }

            #[cfg(target_os = "macos")]
            fn recognize_text(
                &self,
                image: &image::RgbaImage,
                map: &crate::image_util::ScreenshotMap,
            ) -> Result<crate::vision::OcrResult, ComputerError> {
                macos::recognize_text(image, map)
            }

            #[cfg(target_os = "macos")]
            fn secure_ocr_frames(
                &self,
                query: &TreeQuery,
            ) -> Result<Vec<crate::vision::ocr::ImageRect>, ComputerError> {
                macos::secure_ocr_frames(query)
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
    macos::quit,
    macos::tree,
    macos::end_session
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
    linux::quit,
    linux::tree,
    linux::end_session
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
    windows::quit,
    windows::tree,
    windows::end_session
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
