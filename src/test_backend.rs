//! In-memory backend for unit tests. It never touches the real desktop.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::backend::DesktopBackend;
use crate::error::ComputerError;
use crate::policy::{self, quit_is_protected};
use crate::target::{
    filter_windows_for_pid, parse_app_selector, parse_window_selector, resolve_app, resolve_window,
    AppInfo, AppSelector, DoctorReport, LaunchResult, QuitResult, ResponsibleApp, WindowBounds,
    WindowInfo, WindowList,
};
use crate::types::{DisplayInfo, Frame, FrontmostInfo, Modifier, MouseButton, Region};

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
        }
    }
}

impl DesktopBackend for StubBackend {
    fn screenshot(&self, _region: Option<Region>) -> Result<Frame, ComputerError> {
        Frame::new(
            2,
            2,
            2.0,
            vec![10, 20, 30, 255, 1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255],
        )
    }

    fn move_mouse(&self, _x: i32, _y: i32) -> Result<(), ComputerError> {
        self.moves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn simulate_click(&self, _x: i32, _y: i32, _button: MouseButton) -> Result<(), ComputerError> {
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
            note: Some("ax_matched is false; joining CG windows to AX windows lands in P2".into()),
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

    fn quit(&self, app: &str) -> Result<QuitResult, ComputerError> {
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
        },
        AppInfo {
            name: "Terminal".into(),
            bundle_id: Some("com.apple.Terminal".into()),
            pid: 20,
            path: Some("/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal".into()),
            active: false,
            hidden: false,
        },
        AppInfo {
            name: "Foo \"Bar\"".into(),
            bundle_id: Some("example.foo".into()),
            pid: 412,
            path: Some("/Applications/Foo.app/Contents/MacOS/Foo".into()),
            active: false,
            hidden: false,
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
    }
}

fn fixture_windows() -> Vec<WindowInfo> {
    vec![
        window(100, 10, "Notes", "Shopping", 0),
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
