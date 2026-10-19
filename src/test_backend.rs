//! In-memory backend for unit tests. It never touches the real desktop.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::backend::{DesktopBackend, WindowTarget};
use crate::element::{self, ElementError, ElementOutcome, ElementRequest, HitInfo};
use crate::error::ComputerError;
use crate::policy::{self, quit_is_protected};
use crate::target::{
    filter_windows_for_pid, parse_app_selector, parse_window_selector, resolve_app, resolve_window,
    ActivationPolicy, AppInfo, AppSelector, DoctorReport, LaunchResult, QuitResult, ResponsibleApp,
    WindowBounds, WindowInfo, WindowList,
};
use crate::tree::{self, NodeFrame, SigNode, TreeQuery, TreeReport, UiNode};
use crate::types::{DisplayInfo, Frame, FrontmostInfo, Modifier, MouseButton, RawCapture, Region};

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
        Ok(vec![DisplayInfo {
            id: 1,
            name: "Stub".into(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            scale: 2.0,
            is_primary: true,
        }])
    }

    fn frontmost(&self) -> FrontmostInfo {
        FrontmostInfo {
            app: Some("Finder".into()),
            window: Some("Desktop".into()),
            note: None,
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
        self.accessibility_apps
            .lock()
            .expect("accessibility apps")
            .push(query.app.clone());
        if self.fail_accessibility {
            return Err(ComputerError::Unsupported(
                "accessibility is unavailable".into(),
            ));
        }
        let (info, window_id, title) = self.resolve_tree_target(query)?;
        let pid = info.pid;
        let value = self.stub_value.lock().expect("stub value").clone();
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
        baselines.insert(key, rendered.root.clone());
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
            session: request.session.clone(),
        };
        if let Some(app) = query.app.as_deref() {
            let selector = parse_app_selector(app)?;
            policy::deny_or_allow(policy::selector_denial(&selector))?;
        }
        let (info, window_id, title) = self.resolve_tree_target(&query)?;
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
        let planned = element::steps(&request.op, &gate, settable, char_count)?;
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
            session: session.map(str::to_string),
        };
        let Ok((info, window_id, _)) = self.resolve_tree_target(&query) else {
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

    fn doctor(&self, request: bool) -> Result<DoctorReport, ComputerError> {
        let mut report = self.doctor_report.clone();
        if request {
            let extra = "stub ignores --request";
            report.note = Some(match report.note.take() {
                Some(note) => format!("{note}; {extra}"),
                None => extra.into(),
            });
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
        if job.tier == TierName::Ax {
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
            }
            InputKind::Type { text } => self.simulate_type(text)?,
            InputKind::Key { modifiers, key } => self.simulate_key(modifiers, key)?,
        }
        Ok(InputReport {
            tier: job.tier.as_str(),
            cursor_moved: job.tier == TierName::Activate || (background && self.event_moves_cursor),
            focus_restored: (job.tier == TierName::Activate).then_some(true),
            warnings: Vec::new(),
        })
    }
}

impl StubBackend {
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
    ) -> Result<(AppInfo, u32, String), ComputerError> {
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
        Ok((info, window_id, title))
    }
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
    DoctorReport {
        accessibility: Some(true),
        screen_capture: Some(false),
        input_monitor: None,
        responsible_app: Some(ResponsibleApp {
            name: "Terminal".into(),
            bundle_id: Some("com.apple.Terminal".into()),
            pid: 20,
            path: Some("/Applications/Utilities/Terminal.app".into()),
        }),
        fixes: vec![
            "System Settings → Privacy & Security → Screen Recording → enable Terminal".into(),
        ],
        note: None,
    }
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
            .element_action(&request(ElementOp::Press { count: 2 }, 1))
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
                .element_action(&request(ElementOp::Press { count: 1 }, 1))
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
            .element_action(&request(ElementOp::Press { count: 1 }, 1))
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
            .element_action(&request(ElementOp::Press { count: 1 }, 1))
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
                op: ElementOp::Press { count: 1 },
                index: 1,
                require_visible: false,
                app: Some("Terminal".into()),
                window: None,
                session: None,
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
