//! In-memory backend for unit tests. It never touches the real desktop.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::backend::{
    AppleEventReport, DesktopBackend, DockClickReport, DockListReport, MenuClickReport,
    MenuListReport, MenuNode, WindowTarget,
};
use crate::element::{self, ElementError, ElementOutcome, ElementRequest, HitInfo};
use crate::error::ComputerError;
use crate::menu_path::MenuPath;
use crate::policy::{self, quit_is_protected};
use crate::protocol::AppleEventLanguage;
use crate::target::{
    filter_windows_for_pid, parse_app_selector, parse_window_selector, resolve_app, resolve_window,
    ActivationPolicy, AppInfo, AppSelector, DoctorReport, LaunchResult, QuitResult, ResponsibleApp,
    WindowBounds, WindowInfo, WindowList,
};
use crate::tree::{self, NodeFrame, SigNode, TreeQuery, TreeReport, UiNode};
use crate::types::{DisplayInfo, Frame, FrontmostInfo, Modifier, MouseButton, RawCapture, Region};
use crate::window_groups::SpacesReport;

/// Fault injected by [`StubBackend::capture_window`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StubCaptureFault {
    /// Capture succeeds.
    #[default]
    None,
    /// Screen Recording is off.
    Permission,
    /// The window id is absent.
    MissingWindow,
}

#[derive(Debug)]
pub struct StubBackend {
    pub clicks: AtomicU32,
    pub moves: AtomicU32,
    /// `tree` calls that wrote a baseline.
    pub tree_stores: AtomicU32,
    /// `observe_tree` calls, which must not write one.
    pub tree_observations: AtomicU32,
    pub scrolls: Mutex<Vec<(i32, i32)>>,
    pub typed: Mutex<Vec<String>>,
    pub keys: Mutex<Vec<(Vec<Modifier>, String)>>,
    pub fail_accessibility: bool,
    pub fail_click: bool,
    /// Regular activation-policy apps.
    pub apps: Vec<AppInfo>,
    /// Accessory and prohibited apps. Returned only when `list_apps(true)`.
    pub accessory_apps: Vec<AppInfo>,
    /// Windows in front-to-back order.
    pub windows: Vec<WindowInfo>,
    pub launched: Mutex<Vec<String>>,
    pub activated: Mutex<Vec<(String, Option<String>)>>,
    pub quit: Mutex<Vec<String>>,
    pub accessibility_apps: Mutex<Vec<Option<String>>>,
    pub doctor_report: DoctorReport,
    /// Value shown on the stub text node. Tests change it to force a diff.
    pub stub_value: Mutex<String>,
    /// Per-observation values for `wait`/`verify`. The last value repeats.
    pub tree_values: Mutex<std::collections::VecDeque<String>>,
    /// Per-observation element views for `wait`/`verify`. The last value repeats.
    pub element_views: Mutex<std::collections::VecDeque<Option<crate::element::ElementView>>>,
    /// OCR blocks the stub returns. `None` keeps the unsupported error.
    pub ocr_blocks: Mutex<Option<Vec<crate::vision::OcrBlock>>>,
    /// Window frame/state calls the stub recorded, one line each.
    pub window_calls: Mutex<Vec<String>>,
    /// Status the stub reports from `window_status`.
    pub window_status_fixture: Mutex<crate::target::WindowStatus>,
    /// Frontmost app name the stub reports. `None` means Finder.
    pub frontmost_app: Option<String>,
    /// Focused element the stub reports for the secure-field guard.
    pub focused: Mutex<Option<crate::element::ElementPeek>>,
    /// When true, `peek_focused` returns an error and the caller warns.
    pub fail_focused: bool,
    /// In-memory tree baselines keyed by session, pid, and window id.
    pub baselines: Mutex<HashMap<(String, i32, u32), SigNode>>,
    /// `type --element` uses `AXValue` when this is true.
    pub value_settable: bool,
    /// `AXNumberOfCharacters` for `select_text`. `None` skips the length check.
    pub char_count: Option<i64>,
    /// `AXScrollToVisible` fails after it is recorded.
    pub fail_scroll: bool,
    /// Coordinate hit-test returns an error. The click still runs.
    pub fail_hit: bool,
    /// Click attempts, including ones [`Self::fail_click`] rejects.
    pub click_attempts: AtomicU32,
    /// Times [`DesktopBackend::hit_test`] ran.
    pub hit_calls: AtomicU32,
    /// Role and title returned by [`DesktopBackend::hit_test`].
    pub hit: Mutex<Option<HitInfo>>,
    /// Actions the stub performed, as `(name, element index)`.
    pub performs: Mutex<Vec<(String, u32)>>,
    /// Cached element trees. Element actions do not write [`Self::baselines`].
    pub element_fixtures: Mutex<HashMap<(String, i32, u32), ElementFixture>>,
    /// Fresh tree used for revalidation. Empty means the cached tree is still current.
    pub live_override: Mutex<Option<LiveOverride>>,
    /// Desktop screenshot size in pixels. `None` keeps the 2×2 fixture.
    pub capture_pixels: Option<(u32, u32)>,
    /// Window-capture fault. Desktop capture ignores it.
    pub capture_fault: StubCaptureFault,
    /// When true, screenshot reports `display_unavailable` before it resolves a window.
    pub block_display: bool,
    /// Page URL returned by [`DesktopBackend::browser_url`], keyed by pid.
    pub browser_urls: Mutex<HashMap<i32, String>>,
    /// Tier names that [`DesktopBackend::perform_input`] rejects before acting.
    pub fail_tiers: Mutex<Vec<String>>,
    /// Tiers [`DesktopBackend::perform_input`] was asked to run, in order.
    pub tiers_run: Mutex<Vec<String>>,
    /// Event and synthetic tiers report cursor motion. With `--require-background` they fail first.
    pub event_moves_cursor: bool,
    /// Scroll-bar value the stub reports before a scroll.
    pub scroll_value: Mutex<f64>,
    /// False models macOS dropping a wheel event posted to a non-frontmost pid:
    /// the event tier then reports `via: ax_scrollbar` with the fallback warning.
    /// True models a delivered wheel event: `via: event`.
    pub scroll_event_delivered: bool,
    /// The AX scroll-bar tier finds no scroll area, so nothing can be verified.
    pub scroll_area_missing: bool,
    /// `AX` refuses the scroll-bar value write.
    pub scroll_ax_refused: bool,
    /// Secure-field rectangles returned by `secure_ocr_frames`, in image
    /// pixels. `None` keeps the empty list every non-macOS backend returns.
    pub secure_frames: Mutex<Option<Vec<crate::vision::ocr::ImageRect>>>,
    /// Screenshot map handed to the last `secure_ocr_frames` call. Platform
    /// frames are screen points, so redaction is only correct when the
    /// dispatcher passes the map of the capture the OCR ran on.
    pub secure_frames_map: Mutex<Option<crate::image_util::ScreenshotMap>>,
    /// Screenshot map handed to the last `marks_for_tree` call. Element frames
    /// are screen points, so boxes are only right when the dispatcher passes the
    /// map of the capture the marks are stamped on.
    pub marks_map: Mutex<Option<crate::image_util::ScreenshotMap>>,
    /// Buttons still down. One-shot dispatch releases these.
    pub held_buttons: Mutex<Vec<MouseButton>>,
    /// Buttons released by [`DesktopBackend::release_held`], not by `mouse_up`.
    pub releases: Mutex<Vec<MouseButton>>,
    /// Hold durations recorded without sleeping.
    pub holds_ms: Mutex<Vec<u64>>,
    /// In-memory clipboard text. Never the system pasteboard.
    pub clipboard_text: Mutex<Option<String>>,
    /// In-memory clipboard RGBA. Used when text is absent.
    pub clipboard_image: Mutex<Option<Vec<u8>>>,
    /// Displays returned by `list_displays`. The default is one 800x600 screen
    /// at the origin; the `--display` tests set a second monitor beside it.
    pub displays: Vec<DisplayInfo>,
    /// Menu bar entries the stub returns from `menu_list`.
    pub menu_sections: Mutex<Vec<MenuNode>>,
    /// Menu click requests, recorded by `menu_click`.
    pub menu_clicks: Mutex<Vec<MenuPath>>,
    /// Apple event script calls. The body is **not** stored verbatim; only
    /// the script length and the language survive the test boundary.
    pub apple_event_calls: Mutex<Vec<(String, AppleEventLanguage, usize)>>,
    /// Stubbed `osascript` output for `apple_event`.
    pub apple_event_stdout: Mutex<String>,
    /// Stubbed `osascript` stderr for `apple_event`.
    pub apple_event_stderr: Mutex<String>,
    /// When true, `apple_event` returns `execution_failed` with `apple_event_stderr`.
    pub apple_event_fail: bool,
    /// Dock items the stub returns from `dock_list`.
    pub dock_items: Mutex<Vec<String>>,
    /// Dock click requests, recorded by `dock_click`.
    pub dock_clicks: Mutex<Vec<String>>,
    /// Stubbed `spaces` report.
    pub spaces_report: Mutex<SpacesReport>,
    /// When true, the stubbed menu click returns an `element_not_found`.
    pub fail_menu_click: bool,
    /// When true, the stubbed menu click finds the item but reports it disabled.
    pub menu_disabled: bool,
}

#[derive(Clone, Debug)]
pub struct ElementFixture {
    ui: UiNode,
    revision_override: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct LiveOverride {
    ui: UiNode,
    pid: Option<i32>,
    window_id: Option<u32>,
}

impl Default for StubBackend {
    fn default() -> Self {
        Self {
            clicks: AtomicU32::new(0),
            moves: AtomicU32::new(0),
            tree_stores: AtomicU32::new(0),
            tree_observations: AtomicU32::new(0),
            scrolls: Mutex::new(Vec::new()),
            typed: Mutex::new(Vec::new()),
            keys: Mutex::new(Vec::new()),
            fail_accessibility: false,
            fail_click: false,
            apps: fixture_apps(),
            accessory_apps: vec![accessory_app()],
            windows: fixture_windows(),
            launched: Mutex::new(Vec::new()),
            activated: Mutex::new(Vec::new()),
            quit: Mutex::new(Vec::new()),
            accessibility_apps: Mutex::new(Vec::new()),
            doctor_report: fixture_doctor(),
            stub_value: Mutex::new("a".into()),
            tree_values: Mutex::new(std::collections::VecDeque::new()),
            element_views: Mutex::new(std::collections::VecDeque::new()),
            ocr_blocks: Mutex::new(None),
            window_calls: Mutex::new(Vec::new()),
            window_status_fixture: Mutex::new(crate::target::WindowStatus::default()),
            frontmost_app: Some("Finder".into()),
            focused: Mutex::new(None),
            fail_focused: false,
            baselines: Mutex::new(HashMap::new()),
            value_settable: true,
            char_count: None,
            fail_scroll: false,
            fail_hit: false,
            click_attempts: AtomicU32::new(0),
            hit_calls: AtomicU32::new(0),
            hit: Mutex::new(None),
            performs: Mutex::new(Vec::new()),
            element_fixtures: Mutex::new(HashMap::new()),
            live_override: Mutex::new(None),
            capture_pixels: None,
            capture_fault: StubCaptureFault::None,
            block_display: false,
            browser_urls: Mutex::new(HashMap::new()),
            fail_tiers: Mutex::new(Vec::new()),
            tiers_run: Mutex::new(Vec::new()),
            event_moves_cursor: false,
            scroll_value: Mutex::new(0.0),
            scroll_event_delivered: true,
            scroll_area_missing: false,
            scroll_ax_refused: false,
            secure_frames: Mutex::new(None),
            secure_frames_map: Mutex::new(None),
            marks_map: Mutex::new(None),
            held_buttons: Mutex::new(Vec::new()),
            releases: Mutex::new(Vec::new()),
            holds_ms: Mutex::new(Vec::new()),
            clipboard_text: Mutex::new(None),
            clipboard_image: Mutex::new(None),
            displays: fixture_displays(),
            menu_sections: Mutex::new(Vec::new()),
            menu_clicks: Mutex::new(Vec::new()),
            apple_event_calls: Mutex::new(Vec::new()),
            apple_event_stdout: Mutex::new(String::new()),
            apple_event_stderr: Mutex::new(String::new()),
            apple_event_fail: false,
            dock_items: Mutex::new(Vec::new()),
            dock_clicks: Mutex::new(Vec::new()),
            spaces_report: Mutex::new(SpacesReport {
                screens: Vec::new(),
                note: String::new(),
            }),
            fail_menu_click: false,
            menu_disabled: false,
        }
    }
}

impl DesktopBackend for StubBackend {
    fn display_block(&self) -> Option<ComputerError> {
        if self.block_display {
            Some(ComputerError::DisplayUnavailable(
                "the screen is locked".into(),
            ))
        } else {
            None
        }
    }

    fn screenshot(&self, _region: Option<Region>) -> Result<Frame, ComputerError> {
        if let Some((width, height)) = self.capture_pixels {
            let pixels = usize::try_from(width)
                .ok()
                .and_then(|width| usize::try_from(height).ok().map(|height| width * height))
                .ok_or_else(|| ComputerError::ExecutionFailed("stub frame is too large".into()))?;
            return Frame::new(width, height, 2.0, vec![0; pixels * 4]);
        }
        Frame::new(
            2,
            2,
            2.0,
            vec![10, 20, 30, 255, 1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255],
        )
    }

    fn capture_screen(&self, region: Option<Region>) -> Result<RawCapture, ComputerError> {
        let frame = self.screenshot(region)?;
        let (origin_x, origin_y, logical_width, logical_height) = if let Some(region) = region {
            (region.x, region.y, region.width, region.height)
        } else {
            let scale = frame.scale();
            (
                0,
                0,
                ((f64::from(frame.width()) / scale).round() as u32).max(1),
                ((f64::from(frame.height()) / scale).round() as u32).max(1),
            )
        };
        Ok(RawCapture {
            frame,
            origin_x,
            origin_y,
            logical_width,
            logical_height,
            capture_source: "stub".into(),
            occluded: false,
            warnings: Vec::new(),
        })
    }

    fn capture_window(&self, target: &WindowTarget) -> Result<RawCapture, ComputerError> {
        match self.capture_fault {
            StubCaptureFault::Permission => Err(ComputerError::PermissionRequired(
                "Screen Recording is off. System Settings → Privacy & Security → Screen Recording"
                    .into(),
            )),
            StubCaptureFault::MissingWindow => Err(ComputerError::WindowNotFound(format!(
                "cg window {} is not in SCShareableContent",
                target.window_id
            ))),
            StubCaptureFault::None => {
                if target.bounds.width == 0 || target.bounds.height == 0 {
                    return Err(ComputerError::InvalidArguments(
                        "window bounds are empty".into(),
                    ));
                }
                let width = target.bounds.width.saturating_mul(2).max(1);
                let height = target.bounds.height.saturating_mul(2).max(1);
                let pixels = usize::try_from(width)
                    .ok()
                    .and_then(|width| usize::try_from(height).ok().map(|height| width * height))
                    .ok_or_else(|| {
                        ComputerError::ExecutionFailed("stub window frame is too large".into())
                    })?;
                Ok(RawCapture {
                    frame: Frame::new(width, height, 2.0, vec![0; pixels * 4])?,
                    origin_x: target.bounds.x,
                    origin_y: target.bounds.y,
                    logical_width: target.bounds.width,
                    logical_height: target.bounds.height,
                    capture_source: "stub".into(),
                    occluded: false,
                    warnings: Vec::new(),
                })
            }
        }
    }

    fn move_mouse(&self, _x: i32, _y: i32) -> Result<(), ComputerError> {
        self.moves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn simulate_click(&self, _x: i32, _y: i32, _button: MouseButton) -> Result<(), ComputerError> {
        self.click_attempts.fetch_add(1, Ordering::SeqCst);
        if self.fail_click {
            return Err(ComputerError::ExecutionFailed("click failed".into()));
        }
        self.clicks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn simulate_type(&self, text: &str) -> Result<(), ComputerError> {
        self.typed.lock().expect("typed").push(text.to_string());
        Ok(())
    }

    fn simulate_key(&self, modifiers: &[Modifier], key: &str) -> Result<(), ComputerError> {
        self.keys
            .lock()
            .expect("keys")
            .push((modifiers.to_vec(), key.to_string()));
        Ok(())
    }

    fn simulate_scroll(&self, dx: i32, dy: i32) -> Result<(), ComputerError> {
        self.scrolls.lock().expect("scrolls").push((dx, dy));
        Ok(())
    }

    fn cursor_position(&self) -> Result<(i32, i32), ComputerError> {
        Ok((4, 5))
    }

    fn list_displays(&self) -> Result<Vec<DisplayInfo>, ComputerError> {
        Ok(self.displays.clone())
    }

    fn frontmost(&self) -> FrontmostInfo {
        FrontmostInfo {
            app: Some(
                self.frontmost_app
                    .clone()
                    .unwrap_or_else(|| "Finder".into()),
            ),
            window: Some("Desktop".into()),
            note: None,
        }
    }

    fn peek_focused(
        &self,
        _app: Option<&str>,
        _window: Option<&str>,
    ) -> Result<Option<crate::element::ElementPeek>, ComputerError> {
        if self.fail_focused {
            return Err(ComputerError::ExecutionFailed("focus lookup failed".into()));
        }
        Ok(self.focused.lock().expect("focused").clone())
    }

    fn view_element(
        &self,
        _query: &TreeQuery,
        _index: u32,
    ) -> Result<Option<crate::element::ElementView>, ComputerError> {
        let mut queue = self.element_views.lock().expect("element views");
        if queue.len() > 1 {
            Ok(queue.pop_front().unwrap_or(None))
        } else {
            Ok(queue.front().cloned().flatten())
        }
    }

    fn inspect_accessibility(&self, app_name: Option<&str>) -> Result<String, ComputerError> {
        self.accessibility_apps
            .lock()
            .expect("accessibility apps")
            .push(app_name.map(str::to_string));
        if self.fail_accessibility {
            Err(ComputerError::Unsupported(
                "accessibility is unavailable".into(),
            ))
        } else {
            Ok(r#"{"role":"window"}"#.to_string())
        }
    }

    fn list_apps(&self, all: bool) -> Result<Vec<AppInfo>, ComputerError> {
        let mut apps = self.apps.clone();
        if all {
            apps.extend(self.accessory_apps.iter().cloned());
        }
        Ok(apps)
    }

    fn list_windows(&self) -> Result<WindowList, ComputerError> {
        Ok(WindowList {
            windows: self.windows.clone(),
            note: None,
        })
    }

    fn set_window_frame(
        &self,
        app: &str,
        window: Option<&str>,
        x: Option<f64>,
        y: Option<f64>,
        width: Option<f64>,
        height: Option<f64>,
    ) -> Result<(), ComputerError> {
        self.window_calls
            .lock()
            .expect("window calls")
            .push(format!(
                "frame app={app} window={} x={} y={} width={} height={}",
                window.unwrap_or("-"),
                opt_f64(x),
                opt_f64(y),
                opt_f64(width),
                opt_f64(height),
            ));
        Ok(())
    }

    fn set_window_state(
        &self,
        app: &str,
        window: Option<&str>,
        minimized: Option<bool>,
        fullscreen: Option<bool>,
    ) -> Result<(), ComputerError> {
        self.window_calls
            .lock()
            .expect("window calls")
            .push(format!(
                "state app={app} window={} minimized={minimized:?} fullscreen={fullscreen:?}",
                window.unwrap_or("-"),
            ));
        Ok(())
    }

    fn window_status(
        &self,
        _app: &str,
        _window: Option<&str>,
    ) -> Result<crate::target::WindowStatus, ComputerError> {
        Ok(self
            .window_status_fixture
            .lock()
            .expect("window status")
            .clone())
    }

    fn launch(&self, app: &str) -> Result<LaunchResult, ComputerError> {
        let selector = parse_app_selector(app)?;
        policy::deny_or_allow(policy::selector_denial(&selector))?;
        let apps = self.all_apps();
        if let Ok(info) = resolve_app(&apps, &selector) {
            policy::deny_or_allow(policy::identity_denial(
                info.bundle_id.as_deref(),
                info.path.as_deref().map(Path::new),
                None,
                None,
            ))?;
            self.launched
                .lock()
                .expect("launched")
                .push(app.to_string());
            return Ok(LaunchResult {
                name: info.name.clone(),
                bundle_id: info.bundle_id.clone(),
                pid: info.pid,
                path: info.path.clone(),
                window_id: None,
                window_title: None,
            });
        }
        match &selector {
            AppSelector::Name(name) => {
                self.launched
                    .lock()
                    .expect("launched")
                    .push(app.to_string());
                Ok(LaunchResult {
                    name: name.clone(),
                    bundle_id: None,
                    pid: 9000,
                    path: None,
                    window_id: None,
                    window_title: None,
                })
            }
            _ => Err(ComputerError::AppNotFound(app.to_string())),
        }
    }

    fn activate(&self, app: &str, window: Option<&str>) -> Result<LaunchResult, ComputerError> {
        let (info, chosen) = self.resolve_running(app, window)?;
        self.activated
            .lock()
            .expect("activated")
            .push((app.to_string(), window.map(str::to_string)));
        Ok(LaunchResult {
            name: info.name,
            bundle_id: info.bundle_id,
            pid: info.pid,
            path: info.path,
            window_id: chosen.as_ref().map(|item| item.id),
            window_title: chosen.map(|item| item.title),
        })
    }

    fn quit(&self, app: &str, _force: bool) -> Result<QuitResult, ComputerError> {
        let (info, _) = self.resolve_running(app, None)?;
        if quit_is_protected(
            info.pid,
            std::process::id() as i32,
            info.bundle_id.as_deref(),
            &info.name,
            info.path.as_deref(),
        ) {
            return Err(ComputerError::PolicyDenied(
                "refusing to quit loginwindow, WindowServer, or this process".into(),
            ));
        }
        self.quit.lock().expect("quit").push(app.to_string());
        Ok(QuitResult {
            name: info.name,
            bundle_id: info.bundle_id,
            pid: info.pid,
            accepted: true,
        })
    }

    fn tree(&self, query: &TreeQuery) -> Result<TreeReport, ComputerError> {
        self.tree_stores.fetch_add(1, Ordering::SeqCst);
        self.build_stub_tree(query, true)
    }

    fn observe_tree(&self, query: &TreeQuery) -> Result<TreeReport, ComputerError> {
        self.tree_observations.fetch_add(1, Ordering::SeqCst);
        self.build_stub_tree(query, false)
    }

    fn end_session(&self, _session: Option<&str>) -> Result<Vec<i32>, ComputerError> {
        self.baselines.lock().expect("baselines").clear();
        self.element_fixtures.lock().expect("fixtures").clear();
        self.live_override.lock().expect("live").take();
        Ok(Vec::new())
    }

    fn element_action(&self, request: &ElementRequest) -> Result<ElementOutcome, ElementError> {
        let query = TreeQuery {
            app: request.app.clone(),
            window: request.window.clone(),
            full: false,
            diff: false,
            session: request.session.clone(),
        };
        if let Some(app) = query.app.as_deref() {
            let selector = parse_app_selector(app)?;
            policy::deny_or_allow(policy::selector_denial(&selector))?;
        }
        let (info, window_id, title, _) = self.resolve_tree_target(&query)?;
        policy::deny_or_allow(policy::identity_denial(
            info.bundle_id.as_deref(),
            info.path.as_deref().map(Path::new),
            Some(title.as_str()),
            None,
        ))?;
        let key = (
            query.session.clone().unwrap_or_default(),
            info.pid,
            window_id,
        );
        let fixture = self
            .element_fixtures
            .lock()
            .expect("fixtures")
            .get(&key)
            .cloned();
        let Some(fixture) = fixture else {
            let label = request.app.as_deref().unwrap_or(info.name.as_str());
            return Err(ComputerError::ElementNotFound(format!(
                "element {} is not in the session cache; {}",
                request.index,
                element::refresh_hint(label)
            ))
            .into());
        };
        let live_override = self.live_override.lock().expect("live").clone();
        let live_ui = live_override
            .as_ref()
            .map(|item| item.ui.clone())
            .unwrap_or_else(|| fixture.ui.clone());
        let live_pid = live_override
            .as_ref()
            .and_then(|item| item.pid)
            .unwrap_or(info.pid);
        let live_window = live_override
            .as_ref()
            .and_then(|item| item.window_id)
            .unwrap_or(window_id);
        let rendered = tree::render(&fixture.ui);
        let cached_revision = fixture
            .revision_override
            .unwrap_or_else(|| tree::revision_of(&rendered.root));
        let label = request.app.as_deref().unwrap_or(info.name.as_str());
        let gate = element::gate(&element::GateInput {
            op: &request.op,
            index: request.index,
            require_visible: request.require_visible,
            app_label: label,
            cached: &rendered.root,
            cached_revision,
            cached_pid: info.pid,
            cached_window: window_id,
            resolved_pid: info.pid,
            resolved_window: window_id,
            live: &live_ui,
            live_pid,
            live_window,
        })?;
        let settable = if gate.needs_settable {
            self.value_settable
        } else {
            false
        };
        let char_count = if gate.needs_chars {
            self.char_count
        } else {
            None
        };
        let planned = element::steps(
            &request.op,
            &gate,
            settable,
            char_count,
            request.tier.as_deref(),
        )?;
        let mut warnings = Vec::new();
        if let Some(warning) = gate.outside_warning.clone() {
            warnings.push(warning);
        }
        if gate.scroll_first {
            self.record(request.index, "AXScrollToVisible");
            if self.fail_scroll {
                if request.require_visible {
                    return Err(
                        ComputerError::ExecutionFailed("AXScrollToVisible failed".into()).into(),
                    );
                }
                warnings.push("AXScrollToVisible failed".into());
            }
        }
        let mut via = None;
        for step in &planned {
            if let element::Step::TypeEnigo { text } = step {
                self.record(request.index, "AXFocused");
                self.typed.lock().expect("typed").push(text.clone());
                self.record(request.index, "event");
                via = Some("event".into());
                continue;
            }
            if let element::Step::CenterClick {
                x,
                y,
                button,
                count,
            } = step
            {
                let report = crate::attempt::run(
                    self,
                    &crate::attempt::AttemptFlags {
                        app: request.app.clone(),
                        window: request.window.clone(),
                        tier: request.tier.clone(),
                        allow_foreground: request.allow_foreground,
                        require_background: request.require_background,
                        no_overlay: request.no_overlay,
                        session: request.session.clone(),
                    },
                    crate::tier::InputKind::Click {
                        x: *x,
                        y: *y,
                        button: *button,
                        count: *count,
                    },
                )?;
                via = Some(report.tier.to_string());
                continue;
            }
            if matches!(step, element::Step::TypeSetValue { .. }) {
                via = Some("set_value".into());
            }
            if matches!(step, element::Step::ScrollTo) && self.fail_scroll {
                self.record(request.index, step.record_name());
                return Err(
                    ComputerError::ExecutionFailed("AXScrollToVisible failed".into()).into(),
                );
            }
            for _ in 0..step.repeats() {
                self.record(request.index, step.record_name());
            }
        }
        let rendered_live = tree::render(&live_ui);
        let revision = tree::revision_of(&rendered_live.root);
        self.element_fixtures.lock().expect("fixtures").insert(
            key,
            ElementFixture {
                ui: live_ui,
                revision_override: None,
            },
        );
        Ok(element::outcome(request, revision, warnings, via))
    }

    fn browser_url(&self, pid: i32) -> Result<Option<String>, ComputerError> {
        Ok(self
            .browser_urls
            .lock()
            .expect("browser urls")
            .get(&pid)
            .cloned())
    }

    fn menu_list(&self, app: &str, _depth: u32) -> Result<MenuListReport, ComputerError> {
        let selector = parse_app_selector(app)?;
        policy::deny_or_allow(policy::selector_denial(&selector))?;
        let sections = self.menu_sections.lock().expect("menu sections").clone();
        Ok(MenuListReport {
            app: app.to_string(),
            sections,
        })
    }

    fn menu_click(&self, app: &str, path: &MenuPath) -> Result<MenuClickReport, ComputerError> {
        let selector = parse_app_selector(app)?;
        policy::deny_or_allow(policy::selector_denial(&selector))?;
        self.menu_clicks
            .lock()
            .expect("menu clicks")
            .push(path.clone());
        if self.fail_menu_click {
            return Err(ComputerError::ElementNotFound(format!(
                "menu segment {:?} not found",
                path.segments.last()
            )));
        }
        if self.menu_disabled {
            return Err(ComputerError::ExecutionFailed(format!(
                "menu segment {:?} is disabled",
                path.segments.last()
            )));
        }
        Ok(MenuClickReport { pressed: true })
    }

    fn apple_event(
        &self,
        app: &str,
        script: &str,
        language: AppleEventLanguage,
    ) -> Result<AppleEventReport, ComputerError> {
        let selector = parse_app_selector(app)?;
        policy::deny_or_allow(policy::selector_denial(&selector))?;
        // The script body is never logged verbatim. Record only the language
        // and length so the audit line can identify a script run.
        self.apple_event_calls
            .lock()
            .expect("apple event calls")
            .push((app.to_string(), language, script.len()));
        let stdout = self
            .apple_event_stdout
            .lock()
            .expect("apple event stdout")
            .clone();
        let stderr = self
            .apple_event_stderr
            .lock()
            .expect("apple event stderr")
            .clone();
        if self.apple_event_fail {
            return Err(ComputerError::ExecutionFailed(format!(
                "osascript stub failed: {stderr}"
            )));
        }
        let hash = fnv1a64_hex(script.as_bytes());
        Ok(AppleEventReport {
            script_bytes: script.len(),
            script_hash: hash,
            stdout,
            stderr,
            exit_status: 0,
        })
    }

    fn dock_list(&self) -> Result<DockListReport, ComputerError> {
        let items = self
            .dock_items
            .lock()
            .expect("dock items")
            .iter()
            .map(|title| crate::backend::DockItem {
                title: title.clone(),
            })
            .collect();
        Ok(DockListReport { items })
    }

    fn dock_click(&self, title: &str) -> Result<DockClickReport, ComputerError> {
        let items = self.dock_items.lock().expect("dock items").clone();
        let needle = title.to_lowercase();
        let match_position = items.iter().position(|item| item.to_lowercase() == needle);
        let Some(_) = match_position else {
            return Err(ComputerError::ElementNotFound(format!(
                "dock item {title:?} not found"
            )));
        };
        self.dock_clicks
            .lock()
            .expect("dock clicks")
            .push(title.to_string());
        Ok(DockClickReport { pressed: true })
    }

    fn spaces(&self) -> Result<SpacesReport, ComputerError> {
        Ok(self.spaces_report.lock().expect("spaces report").clone())
    }

    fn peek_element(
        &self,
        app: Option<&str>,
        window: Option<&str>,
        session: Option<&str>,
        index: u32,
    ) -> Result<Option<element::ElementPeek>, ComputerError> {
        let query = TreeQuery {
            app: app.map(str::to_string),
            window: window.map(str::to_string),
            full: false,
            diff: false,
            session: session.map(str::to_string),
        };
        let Ok((info, window_id, _, _)) = self.resolve_tree_target(&query) else {
            return Ok(None);
        };
        let key = (
            query.session.clone().unwrap_or_default(),
            info.pid,
            window_id,
        );
        let fixtures = self.element_fixtures.lock().expect("fixtures");
        let Some(fixture) = fixtures.get(&key) else {
            return Ok(None);
        };
        let Some(node) = tree::find_ui(&fixture.ui, index) else {
            return Ok(None);
        };
        let title = node
            .identifier
            .clone()
            .or_else(|| node.title.clone())
            .or_else(|| node.description.clone());
        Ok(Some(element::ElementPeek {
            title,
            secure: node.secure,
        }))
    }

    fn hit_test(&self, _x: i32, _y: i32) -> Result<Option<HitInfo>, ComputerError> {
        self.hit_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_hit {
            return Err(ComputerError::ExecutionFailed("hit test failed".into()));
        }
        Ok(self.hit.lock().expect("hit").clone())
    }

    fn doctor(&self, options: crate::doctor::DoctorOptions) -> Result<DoctorReport, ComputerError> {
        let mut report = self.doctor_report.clone();
        if options.prompt {
            crate::doctor::push_note(&mut report, "stub ignores --prompt");
        }
        if options.open_settings {
            crate::doctor::push_note(&mut report, "stub ignores --open-settings");
        }
        Ok(report)
    }

    fn input_support(&self) -> crate::tier::Capabilities {
        crate::tier::Capabilities { event: true }
    }

    fn perform_input(
        &self,
        job: &crate::tier::InputJob,
    ) -> Result<crate::tier::InputReport, ComputerError> {
        use crate::tier::{InputKind, InputReport, TierName};
        self.tiers_run
            .lock()
            .expect("tiers")
            .push(job.tier.as_str().to_string());
        if self
            .fail_tiers
            .lock()
            .expect("fail tiers")
            .iter()
            .any(|name| name == job.tier.as_str())
        {
            return Err(ComputerError::ExecutionFailed(format!(
                "stub tier {} failed",
                job.tier.as_str()
            )));
        }
        if job.tier == TierName::Synthetic && !crate::tier::skylight_compiled() {
            return Err(ComputerError::Unsupported(
                "built without skylight feature".into(),
            ));
        }
        if job.tier == TierName::Ax && !crate::tier::is_scroll(&job.kind) {
            return Err(ComputerError::Unsupported(
                "tier ax does not post coordinate or keyboard events".into(),
            ));
        }
        if job.tier == TierName::Activate && job.require_background {
            return Err(ComputerError::ExecutionFailed(
                "--require-background refuses tier activate because it uses the foreground cursor"
                    .into(),
            ));
        }
        let background = matches!(job.tier, TierName::Event | TierName::Synthetic);
        if background && self.event_moves_cursor && job.require_background {
            return Err(ComputerError::ExecutionFailed(
                "cursor moved and --require-background is set".into(),
            ));
        }
        let mut scroll = None;
        match &job.kind {
            InputKind::Click {
                x,
                y,
                button,
                count,
            } => {
                for _ in 0..*count {
                    self.simulate_click(*x, *y, *button)?;
                }
            }
            InputKind::Move { x, y } => self.move_mouse(*x, *y)?,
            InputKind::Scroll { x, y, dx, dy } => {
                if job.tier == TierName::Activate {
                    if let (Some(x), Some(y)) = (x, y) {
                        self.move_mouse(*x, *y)?;
                    }
                }
                self.simulate_scroll(*dx, *dy)?;
                scroll = Some(self.simulated_scroll(job.tier, *dx, *dy)?);
            }
            InputKind::Type { text } => self.simulate_type(text)?,
            InputKind::Key { strokes } => {
                for stroke in strokes {
                    self.simulate_key(&stroke.modifiers, &stroke.key)?;
                }
            }
            InputKind::Drag { points, .. } => {
                if points.len() < 2 {
                    return Err(ComputerError::InvalidArguments(
                        "drag needs at least 2 points".into(),
                    ));
                }
                let moves = u32::try_from(points.len() - 1).unwrap_or(u32::MAX);
                self.moves.fetch_add(moves, Ordering::SeqCst);
            }
            InputKind::MouseDown { button, .. } => {
                let mut held = self.held_buttons.lock().expect("held");
                held.retain(|held_button| held_button != button);
                held.push(*button);
            }
            InputKind::MouseUp { button, .. } => {
                self.held_buttons
                    .lock()
                    .expect("held")
                    .retain(|held_button| held_button != button);
            }
            InputKind::HoldKey { modifiers, key, ms } => {
                if *ms > crate::tier::MAX_HOLD_MS {
                    return Err(ComputerError::InvalidArguments(format!(
                        "hold_key ms must be at most {}",
                        crate::tier::MAX_HOLD_MS
                    )));
                }
                self.holds_ms.lock().expect("holds").push(*ms);
                self.simulate_key(modifiers, key)?;
            }
        }
        Ok(InputReport {
            tier: job.tier.as_str(),
            cursor_moved: job.tier == TierName::Activate || (background && self.event_moves_cursor),
            focus_restored: (job.tier == TierName::Activate).then_some(true),
            warnings: scroll
                .as_ref()
                .map(|outcome| outcome.warnings.clone())
                .unwrap_or_default(),
            scroll,
        })
    }

    fn clipboard_get(&self) -> Result<crate::backend::clipboard::ClipboardContents, ComputerError> {
        if let Some(text) = self.clipboard_text.lock().expect("clipboard").clone() {
            if !text.is_empty() {
                return Ok(crate::backend::clipboard::ClipboardContents::Text(text));
            }
        }
        if let Some(rgba) = self
            .clipboard_image
            .lock()
            .expect("clipboard image")
            .clone()
        {
            return Ok(crate::backend::clipboard::ClipboardContents::Image {
                width: 1,
                height: 1,
                rgba,
            });
        }
        Ok(crate::backend::clipboard::ClipboardContents::Empty)
    }

    fn clipboard_set_text(&self, text: &str) -> Result<(), ComputerError> {
        *self.clipboard_text.lock().expect("clipboard") = Some(text.to_string());
        Ok(())
    }

    fn clipboard_set_image(
        &self,
        _width: u32,
        _height: u32,
        rgba: &[u8],
    ) -> Result<(), ComputerError> {
        *self.clipboard_image.lock().expect("clipboard image") = Some(rgba.to_vec());
        *self.clipboard_text.lock().expect("clipboard") = None;
        Ok(())
    }

    fn release_held(&self) -> Result<(), ComputerError> {
        let held = std::mem::take(&mut *self.held_buttons.lock().expect("held"));
        self.releases.lock().expect("releases").extend(held);
        Ok(())
    }

    // P11: OCR + marks hooks on the stub. The mock backend never reads
    // pixels, so OCR is `unsupported` and marks return an empty list. Real
    // platform backends override these to call macOS Vision and re-walk
    // the UI tree.
    fn recognize_text(
        &self,
        _image: &image::RgbaImage,
        _map: &crate::image_util::ScreenshotMap,
    ) -> Result<crate::vision::OcrResult, ComputerError> {
        if let Some(blocks) = self.ocr_blocks.lock().expect("ocr blocks").clone() {
            return Ok(crate::vision::OcrResult {
                blocks,
                note: Some("stub".into()),
            });
        }
        Err(ComputerError::Unsupported(
            "ocr is not available in the mock backend".into(),
        ))
    }

    fn secure_ocr_frames(
        &self,
        _query: &TreeQuery,
        map: &crate::image_util::ScreenshotMap,
    ) -> Result<Vec<crate::vision::ocr::ImageRect>, ComputerError> {
        *self.secure_frames_map.lock().expect("secure map") = Some(map.clone());
        Ok(self
            .secure_frames
            .lock()
            .expect("secure frames")
            .clone()
            .unwrap_or_default())
    }

    fn marks_for_tree(
        &self,
        query: &TreeQuery,
        map: &crate::image_util::ScreenshotMap,
    ) -> Result<Vec<crate::vision::MarkBox>, ComputerError> {
        *self.marks_map.lock().expect("marks map") = Some(map.clone());
        let root = self.marks_fixture(query)?;
        // Same walker the platform backends use, so the mock proves the shared
        // frame-to-pixel path and not a parallel test-only implementation.
        Ok(crate::vision::marks_for_nodes(&root, map))
    }
}

impl StubBackend {
    fn build_stub_tree(&self, query: &TreeQuery, store: bool) -> Result<TreeReport, ComputerError> {
        self.accessibility_apps
            .lock()
            .expect("accessibility apps")
            .push(query.app.clone());
        if self.fail_accessibility {
            return Err(ComputerError::Unsupported(
                "accessibility is unavailable".into(),
            ));
        }
        let (info, window_id, title, _) = self.resolve_tree_target(query)?;
        let pid = info.pid;
        let value = {
            let mut queue = self.tree_values.lock().expect("tree values");
            if queue.len() > 1 {
                queue.pop_front().unwrap_or_default()
            } else {
                queue
                    .front()
                    .cloned()
                    .unwrap_or_else(|| self.stub_value.lock().expect("stub value").clone())
            }
        };
        let mut window = UiNode::bare("AXWindow");
        window.title = Some(title);
        window.frame = Some(NodeFrame {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        });
        let mut text = UiNode::bare("AXTextArea");
        text.value = Some(value);
        window.children = vec![text];
        let rendered = tree::render(&window);
        let key = (query.session.clone().unwrap_or_default(), pid, window_id);
        self.element_fixtures.lock().expect("fixtures").insert(
            key.clone(),
            ElementFixture {
                ui: window.clone(),
                revision_override: None,
            },
        );
        let mut baselines = self.baselines.lock().expect("baselines");
        let previous = baselines.get(&key).cloned();
        let diff = previous
            .as_ref()
            .map(|previous| tree::diff_trees(previous, &rendered.root));
        let printed = tree::choose_text(previous.as_ref(), &rendered, query.full);
        if store {
            baselines.insert(key, rendered.root.clone());
        }
        let app = info.bundle_id.clone().or_else(|| Some(info.name.clone()));
        Ok(TreeReport {
            pid,
            window_id,
            revision: tree::revision_of(&rendered.root),
            source: "stub".into(),
            truncated: rendered.truncated,
            tree: printed,
            full_text: rendered.text,
            diff,
            app,
            note: None,
            legacy: None,
        })
    }

    /// What the stub reports for one verified scroll.
    ///
    /// This models the macOS contract: the wheel event is posted, the scroll
    /// bar is read back, and a bar that did not move falls back to
    /// `AXValue`. [`Self::scroll_event_delivered`] selects which side of that
    /// the stub takes, so the response fields are testable on every OS.
    ///
    /// The stub does not actually measure `AXSize`, so the step is the legacy
    /// `Step::approx()` (five percent, no warnings). That keeps the existing
    /// behaviour-based tests stable while the real macOS backend carries the
    /// measured step.
    fn simulated_scroll(
        &self,
        tier: crate::tier::TierName,
        dx: i32,
        dy: i32,
    ) -> Result<crate::scroll::Outcome, ComputerError> {
        use crate::scroll::{self, AxCause, AxFailure, Axis, Outcome, Step};
        if self.scroll_area_missing {
            return Err(ComputerError::ExecutionFailed(
                "no AXScrollArea under the point and none in the window, so the scroll cannot be verified"
                    .into(),
            ));
        }
        let axes: Vec<Axis> = Axis::both()
            .into_iter()
            .filter(|axis| axis.delta(dx, dy) != 0)
            .collect();
        // No axis is addressed, so there is no bar and nothing to verify.
        let Some(axis) = axes.first().copied() else {
            return Ok(Outcome::none());
        };
        let step = Step::approx();
        let steps = axis.delta(dx, dy);
        let before = *self.scroll_value.lock().expect("scroll value");
        // Tier activate is a real foreground wheel, and tier ax drives the bar
        // directly, so neither one posts a wheel event it then has to verify.
        let cause = if tier == crate::tier::TierName::Ax {
            AxCause::ExplicitAxTier
        } else {
            AxCause::WheelEventNotDelivered
        };
        if tier == crate::tier::TierName::Activate
            || (tier != crate::tier::TierName::Ax && self.scroll_event_delivered)
        {
            let after = scroll::next_value(before, steps, &step);
            *self.scroll_value.lock().expect("scroll value") = after;
            return Ok(Outcome::event(before, after, step.value));
        }
        if scroll::at_limit(before, steps) {
            return Ok(Outcome::at_limit(axis, steps, before, Some(step.value)));
        }
        if self.scroll_ax_refused {
            return Err(ComputerError::ExecutionFailed(
                AxFailure::Refused.reason(axis),
            ));
        }
        let next = scroll::next_value(before, steps, &step);
        *self.scroll_value.lock().expect("scroll value") = next;
        scroll::ax_result(axis, before, next, Some(next), cause, &step)
            .map_err(|failure| ComputerError::ExecutionFailed(failure.reason(axis)))
    }

    /// Two monitors side by side: the default 800x600 screen at the origin and
    /// a 640x480 one whose origin is `(800, 100)`. This is the display list the
    /// `--display` filter tests run against.
    pub fn with_two_displays() -> Self {
        Self {
            displays: vec![
                fixture_displays().remove(0),
                DisplayInfo {
                    id: 2,
                    name: "Stub Right".into(),
                    x: 800,
                    y: 100,
                    width: 640,
                    height: 480,
                    scale: 1.0,
                    is_primary: false,
                },
            ],
            ..Self::default()
        }
    }

    fn all_apps(&self) -> Vec<AppInfo> {
        let mut apps = self.apps.clone();
        apps.extend(self.accessory_apps.iter().cloned());
        apps
    }

    fn resolve_running(
        &self,
        app: &str,
        window: Option<&str>,
    ) -> Result<(AppInfo, Option<WindowInfo>), ComputerError> {
        let selector = parse_app_selector(app)?;
        policy::deny_or_allow(policy::selector_denial(&selector))?;
        let apps = self.all_apps();
        let info = resolve_app(&apps, &selector)?.clone();
        let chosen = match window {
            Some(window) => {
                let selector = parse_window_selector(window)?;
                let owned = filter_windows_for_pid(&self.windows, info.pid);
                Some(resolve_window(&owned, &selector)?.clone())
            }
            None => None,
        };
        policy::deny_or_allow(policy::identity_denial(
            info.bundle_id.as_deref(),
            info.path.as_deref().map(Path::new),
            chosen.as_ref().map(|item| item.title.as_str()),
            None,
        ))?;
        Ok((info, chosen))
    }

    /// Cache a Notes window at pid 10, window 100, for element-action tests.
    pub fn seed_element(&self, ui: UiNode) {
        self.element_fixtures.lock().expect("fixtures").insert(
            (String::new(), 10, 100),
            ElementFixture {
                ui,
                revision_override: None,
            },
        );
    }

    /// Replace the fresh tree. The cached signature stays in place until an action succeeds.
    pub fn set_live(&self, ui: UiNode) {
        self.live_override
            .lock()
            .expect("live")
            .replace(LiveOverride {
                ui,
                pid: None,
                window_id: None,
            });
    }

    /// Force the cached revision header so the next action is `stale_element`.
    pub fn set_cached_revision(&self, revision: u64) {
        let mut fixtures = self.element_fixtures.lock().expect("fixtures");
        let fixture = fixtures
            .get_mut(&(String::new(), 10, 100))
            .expect("seed an element before overriding its revision");
        fixture.revision_override = Some(revision);
    }

    fn record(&self, index: u32, name: &str) {
        self.performs
            .lock()
            .expect("performs")
            .push((name.to_string(), index));
    }

    fn resolve_tree_target(
        &self,
        query: &TreeQuery,
    ) -> Result<(AppInfo, u32, String, Option<WindowBounds>), ComputerError> {
        let apps = self.all_apps();
        let info = if let Some(app) = query.app.as_deref() {
            let selector = parse_app_selector(app)?;
            resolve_app(&apps, &selector)?.clone()
        } else {
            apps.into_iter()
                .find(|app| app.active && app.activation_policy == ActivationPolicy::Regular)
                .ok_or_else(|| ComputerError::AppNotFound("frontmost app".into()))?
        };
        let owned = filter_windows_for_pid(&self.windows, info.pid);
        let chosen = match query.window.as_deref() {
            Some(window) => {
                let selector = parse_window_selector(window)?;
                Some(resolve_window(&owned, &selector)?.clone())
            }
            None => owned.first().cloned(),
        };
        let title = chosen
            .as_ref()
            .map(|window| window.title.clone())
            .unwrap_or_else(|| info.name.clone());
        let window_id = chosen.as_ref().map(|window| window.id).unwrap_or(0);
        Ok((
            info,
            window_id,
            title,
            chosen.as_ref().map(|window| window.bounds),
        ))
    }

    /// Framed tree handed to `marks_for_tree`.
    ///
    /// It mirrors a real backend: the window frame is the target window's
    /// screen-point bounds and the child sits inside it, so a capture of that
    /// window yields in-canvas boxes while a capture of something else yields
    /// none. `set_live` replaces it with a caller-supplied tree.
    fn marks_fixture(&self, query: &TreeQuery) -> Result<UiNode, ComputerError> {
        if let Some(live) = self.live_override.lock().expect("live").as_ref() {
            return Ok(live.ui.clone());
        }
        let (_, _, title, bounds) = self.resolve_tree_target(query)?;
        let bounds = bounds.unwrap_or(WindowBounds {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        });
        let mut window = UiNode::bare("AXWindow");
        window.title = Some(title);
        window.frame = Some(NodeFrame {
            x: f64::from(bounds.x),
            y: f64::from(bounds.y),
            width: f64::from(bounds.width),
            height: f64::from(bounds.height),
        });
        let mut text = UiNode::bare("AXTextArea");
        text.frame = Some(NodeFrame {
            x: f64::from(bounds.x) + 10.0,
            y: f64::from(bounds.y) + 10.0,
            width: f64::from(bounds.width) - 20.0,
            height: 40.0,
        });
        window.children = vec![text];
        Ok(window)
    }
}

/// The single screen the stub reports by default. P14 tests add a second one
/// through [`StubBackend::with_displays`].
fn fixture_displays() -> Vec<DisplayInfo> {
    vec![DisplayInfo {
        id: 1,
        name: "Stub".into(),
        x: 0,
        y: 0,
        width: 800,
        height: 600,
        scale: 2.0,
        is_primary: true,
    }]
}

fn fixture_apps() -> Vec<AppInfo> {
    vec![
        AppInfo {
            name: "Notes".into(),
            bundle_id: Some("com.apple.Notes".into()),
            pid: 10,
            path: Some("/Applications/Notes.app/Contents/MacOS/Notes".into()),
            active: true,
            hidden: false,
            activation_policy: ActivationPolicy::Regular,
        },
        AppInfo {
            name: "Terminal".into(),
            bundle_id: Some("com.apple.Terminal".into()),
            pid: 20,
            path: Some("/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal".into()),
            active: false,
            hidden: false,
            activation_policy: ActivationPolicy::Regular,
        },
        AppInfo {
            name: "Foo \"Bar\"".into(),
            bundle_id: Some("example.foo".into()),
            pid: 412,
            path: Some("/Applications/Foo.app/Contents/MacOS/Foo".into()),
            active: false,
            hidden: false,
            activation_policy: ActivationPolicy::Regular,
        },
    ]
}

fn accessory_app() -> AppInfo {
    AppInfo {
        name: "Widget".into(),
        bundle_id: Some("example.widget".into()),
        pid: 77,
        path: None,
        active: false,
        hidden: true,
        activation_policy: ActivationPolicy::Accessory,
    }
}

fn opt_f64(value: Option<f64>) -> String {
    match value {
        Some(value) if value.fract() == 0.0 => format!("{}", value as i64),
        Some(value) => format!("{value}"),
        None => "-".into(),
    }
}

fn fixture_windows() -> Vec<WindowInfo> {
    vec![
        {
            let mut front = window(100, 10, "Notes", "Shopping", 0);
            front.ax_matched = true;
            front
        },
        window(200, 20, "Terminal", "bash", 1),
        window(101, 10, "Notes", "Welcome", 2),
    ]
}

fn window(id: u32, pid: i32, app: &str, title: &str, z: u32) -> WindowInfo {
    WindowInfo {
        id,
        pid,
        app: app.into(),
        title: title.into(),
        z,
        bounds: WindowBounds {
            x: 10,
            y: 20,
            width: 400,
            height: 300,
        },
        layer: 0,
        on_screen: true,
        ax_matched: false,
    }
}

fn fixture_doctor() -> DoctorReport {
    crate::doctor::assemble(crate::doctor::DoctorInput {
        accessibility: Some(true),
        screen_recording: Some(false),
        post_events: Some(true),
        input_monitoring: Some(true),
        input_monitor: None,
        path: Some("/usr/local/bin/computer-use".into()),
        bundle_id: None,
        bundled: false,
        signature: Some("adhoc".into()),
        responsible: Some(ResponsibleApp {
            name: "Terminal".into(),
            bundle_id: Some("com.apple.Terminal".into()),
            pid: 20,
            path: Some("/Applications/Utilities/Terminal.app".into()),
        }),
        required: crate::doctor::MACOS_REQUIRED,
        ..crate::doctor::DoctorInput::default()
    })
}

fn fnv1a64_hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{ElementOp, ElementRequest};

    fn frame(x: f64, y: f64, width: f64, height: f64) -> NodeFrame {
        NodeFrame {
            x,
            y,
            width,
            height,
        }
    }

    fn button(title: &str, origin_y: f64, actions: &[&str]) -> UiNode {
        let mut node = UiNode::bare("AXButton");
        node.title = Some(title.into());
        node.frame = Some(frame(12.0, origin_y, 80.0, 24.0));
        node.actions = actions.iter().map(|action| (*action).to_string()).collect();
        node
    }

    fn window(child: UiNode) -> UiNode {
        let mut root = UiNode::bare("AXWindow");
        root.title = Some("Notes".into());
        root.frame = Some(frame(0.0, 0.0, 800.0, 600.0));
        root.children = vec![child];
        root
    }

    fn request(op: ElementOp, index: u32) -> ElementRequest {
        ElementRequest {
            op,
            index,
            require_visible: false,
            app: Some("Notes".into()),
            window: None,
            session: None,
            tier: None,
            allow_foreground: false,
            require_background: false,
            no_overlay: true,
        }
    }

    fn performs(backend: &StubBackend) -> Vec<(String, u32)> {
        backend.performs.lock().expect("performs").clone()
    }

    #[test]
    fn stub_records_the_action_name_and_index() {
        let backend = StubBackend::default();
        backend.seed_element(window(button("Save", 10.0, &["AXPress"])));
        let outcome = backend
            .element_action(&request(
                ElementOp::Press {
                    count: 2,
                    button: crate::types::MouseButton::Left,
                },
                1,
            ))
            .unwrap();
        assert_eq!(outcome.action, "press");
        assert_eq!(outcome.tier, "ax");
        assert_eq!(outcome.element, 1);
        assert_eq!(outcome.count, Some(2));
        assert_eq!(
            performs(&backend),
            vec![("AXPress".into(), 1), ("AXPress".into(), 1)]
        );
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 0);
    }

    fn set_role(node: &mut UiNode) {
        node.children[0].role = "AXTextArea".into();
    }

    fn set_title(node: &mut UiNode) {
        node.children[0].title = Some("Delete".into());
    }

    fn set_frame(node: &mut UiNode) {
        node.children[0].frame = Some(frame(40.0, 10.0, 80.0, 24.0));
    }

    #[test]
    fn changed_role_identifier_or_frame_performs_nothing() {
        let cached = window(button("Save", 10.0, &["AXPress"]));
        for mutate in [set_role, set_title, set_frame] {
            let backend = StubBackend::default();
            backend.seed_element(cached.clone());
            let mut live = cached.clone();
            mutate(&mut live);
            backend.set_live(live);
            let error = backend
                .element_action(&request(
                    ElementOp::Press {
                        count: 1,
                        button: crate::types::MouseButton::Left,
                    },
                    1,
                ))
                .unwrap_err();
            assert_eq!(error.error.code(), "stale_element");
            assert!(error.tree.is_some());
            assert!(performs(&backend).is_empty());
        }
    }

    #[test]
    fn mismatched_cached_revision_performs_nothing() {
        let backend = StubBackend::default();
        backend.seed_element(window(button("Save", 10.0, &["AXPress"])));
        backend.set_cached_revision(u64::MAX);
        let error = backend
            .element_action(&request(
                ElementOp::Press {
                    count: 1,
                    button: crate::types::MouseButton::Left,
                },
                1,
            ))
            .unwrap_err();
        assert_eq!(error.error.code(), "stale_element");
        assert!(performs(&backend).is_empty());
    }

    #[test]
    fn same_role_within_frame_tolerance_is_allowed() {
        let backend = StubBackend::default();
        let cached = window(button("Save", 10.0, &["AXPress"]));
        backend.seed_element(cached.clone());
        let mut live = cached;
        live.children[0].frame = Some(frame(14.0, 10.0, 80.0, 24.0));
        backend.set_live(live);
        assert!(backend
            .element_action(&request(ElementOp::Focus, 1))
            .is_ok());
        assert_eq!(performs(&backend), vec![("AXFocused".into(), 1)]);
    }

    #[test]
    fn secure_node_refuses_set_value_and_select_text() {
        let mut field = UiNode::bare("AXSecureTextField");
        field.title = Some("Password".into());
        field.secure = true;
        let backend = StubBackend::default();
        backend.seed_element(window(field));
        for op in [
            ElementOp::SetValue {
                value: "secret".into(),
            },
            ElementOp::SelectText { start: 0, end: 1 },
            ElementOp::Type {
                text: "secret".into(),
            },
        ] {
            let error = backend.element_action(&request(op, 1)).unwrap_err();
            assert_eq!(error.error.code(), "policy_denied");
        }
        assert!(performs(&backend).is_empty());
        assert!(backend.typed.lock().unwrap().is_empty());
    }

    #[test]
    fn missing_action_lists_names_and_records_nothing() {
        let backend = StubBackend::default();
        backend.seed_element(window(button("Save", 10.0, &["AXShowMenu"])));
        let error = backend
            .element_action(&request(
                ElementOp::Perform {
                    name: "AXPress".into(),
                },
                1,
            ))
            .unwrap_err();
        assert_eq!(error.error.code(), "execution_failed");
        assert!(error.error.to_string().contains("AXShowMenu"));
        assert!(performs(&backend).is_empty());
    }

    #[test]
    fn scroll_to_is_attempted_when_the_frame_is_outside_the_window() {
        let backend = StubBackend::default();
        backend.seed_element(window(button(
            "Save",
            700.0,
            &["AXPress", "AXScrollToVisible"],
        )));
        backend
            .element_action(&request(
                ElementOp::Press {
                    count: 1,
                    button: crate::types::MouseButton::Left,
                },
                1,
            ))
            .unwrap();
        assert_eq!(
            performs(&backend),
            vec![("AXScrollToVisible".into(), 1), ("AXPress".into(), 1)]
        );

        let backend = StubBackend::default();
        backend.seed_element(window(button(
            "Save",
            700.0,
            &["AXPress", "AXScrollToVisible"],
        )));
        backend
            .element_action(&request(ElementOp::ScrollTo, 1))
            .unwrap();
        assert_eq!(performs(&backend), vec![("AXScrollToVisible".into(), 1)]);
    }

    #[test]
    fn denied_app_performs_nothing() {
        let backend = StubBackend::default();
        backend.seed_element(window(button("Save", 10.0, &["AXPress"])));
        let error = backend
            .element_action(&ElementRequest {
                op: ElementOp::Press {
                    count: 1,
                    button: crate::types::MouseButton::Left,
                },
                index: 1,
                require_visible: false,
                app: Some("Terminal".into()),
                window: None,
                session: None,
                tier: None,
                allow_foreground: false,
                require_background: false,
                no_overlay: true,
            })
            .unwrap_err();
        assert_eq!(error.error.code(), "policy_denied");
        assert!(performs(&backend).is_empty());
    }

    #[test]
    fn missing_cache_is_element_not_found() {
        let backend = StubBackend::default();
        let error = backend
            .element_action(&request(ElementOp::Focus, 0))
            .unwrap_err();
        assert_eq!(error.error.code(), "element_not_found");
        assert!(error.error.to_string().contains("refresh indices"));
        assert!(performs(&backend).is_empty());
    }

    #[test]
    fn type_prefers_set_value_and_falls_back_to_enigo() {
        let backend = StubBackend::default();
        backend.seed_element(window(button("Save", 10.0, &["AXPress"])));
        let outcome = backend
            .element_action(&request(ElementOp::Type { text: "hi".into() }, 1))
            .unwrap();
        assert_eq!(outcome.tier, "ax");
        assert_eq!(outcome.via.as_deref(), Some("set_value"));
        assert_eq!(performs(&backend), vec![("AXValue".into(), 1)]);

        let backend = StubBackend {
            value_settable: false,
            ..StubBackend::default()
        };
        backend.seed_element(window(button("Save", 10.0, &["AXPress"])));
        let outcome = backend
            .element_action(&request(ElementOp::Type { text: "hi".into() }, 1))
            .unwrap();
        assert_eq!(outcome.tier, "event");
        assert!(!outcome.cursor_moved);
        assert!(outcome.focus_restored.is_none());
        assert_eq!(outcome.via.as_deref(), Some("event"));
        assert_eq!(
            performs(&backend),
            vec![("AXFocused".into(), 1), ("event".into(), 1)]
        );
        assert_eq!(backend.typed.lock().unwrap().as_slice(), ["hi"]);
    }
}
