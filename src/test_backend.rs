//! In-memory backend for unit tests. It never touches the real desktop.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::backend::DesktopBackend;
use crate::error::ComputerError;
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

    fn inspect_accessibility(&self, _app_name: Option<&str>) -> Result<String, ComputerError> {
        if self.fail_accessibility {
            Err(ComputerError::Unsupported(
                "accessibility is unavailable".into(),
            ))
        } else {
            Ok(r#"{"role":"window"}"#.to_string())
        }
    }
}
