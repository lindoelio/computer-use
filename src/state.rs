//! Best-effort desktop snapshot. Subsystem failures become null fields and a note.

use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::{json, Value};

use crate::backend::DesktopBackend;
use crate::error::ComputerError;
use crate::tree::{TreeQuery, TreeReport};
use crate::types::{DisplayInfo, FrontmostInfo};

/// Collect screen, cursor, frontmost window, and accessibility information.
///
/// This function does not panic. A backend panic is caught and reported in `note`.
pub fn collect_state(backend: &dyn DesktopBackend) -> Value {
    collect_state_for(backend, &TreeQuery::default())
}

/// Same snapshot as [`collect_state`], with the tree query from the request.
pub fn collect_state_for(backend: &dyn DesktopBackend, query: &TreeQuery) -> Value {
    match catch_unwind(AssertUnwindSafe(|| collect_state_inner(backend, query))) {
        Ok(value) => value,
        Err(_) => recovered_state(),
    }
}

fn recovered_state() -> Value {
    json!({
        "platform": platform_name(),
        "displays": [],
        "screen": null,
        "cursor": null,
        "frontmost_app": null,
        "frontmost_window": null,
        "tree": null,
        "revision": null,
        "window_id": null,
        "pid": null,
        "source": null,
        "truncated": null,
        "accessibility_legacy": null,
        "note": "get-state panicked internally and was recovered"
    })
}

fn collect_state_inner(backend: &dyn DesktopBackend, query: &TreeQuery) -> Value {
    let mut notes = Vec::new();

    let displays = match guard("displays", || backend.list_displays()) {
        Ok(displays) => displays,
        Err(note) => {
            notes.push(note);
            Vec::new()
        }
    };
    let screen = screen_value(primary_display(&displays));

    let cursor = match guard("cursor", || backend.cursor_position()) {
        Ok((x, y)) => json!({"x": x, "y": y}),
        Err(note) => {
            notes.push(note);
            Value::Null
        }
    };

    let front = match catch_unwind(AssertUnwindSafe(|| backend.frontmost())) {
        Ok(info) => info,
        Err(_) => FrontmostInfo {
            note: Some("frontmost lookup panicked and was recovered".into()),
            ..FrontmostInfo::default()
        },
    };
    if let Some(note) = front.note {
        notes.push(note);
    }

    let tree = match catch_unwind(AssertUnwindSafe(|| backend.tree(query))) {
        Ok(Ok(report)) => tree_value(&report, &mut notes),
        Ok(Err(error)) => {
            notes.push(error.to_string());
            empty_tree()
        }
        Err(_) => {
            notes.push("accessibility inspection panicked and was recovered".into());
            empty_tree()
        }
    };

    let mut body = json!({
        "platform": platform_name(),
        "displays": displays,
        "screen": screen,
        "cursor": cursor,
        "frontmost_app": front.app,
        "frontmost_window": front.window,
        "note": if notes.is_empty() { Value::Null } else { Value::String(notes.join("; ")) }
    });
    if let (Some(body), Value::Object(fields)) = (body.as_object_mut(), tree) {
        for (key, value) in fields {
            body.insert(key, value);
        }
    }
    body
}

fn empty_tree() -> Value {
    json!({
        "tree": null,
        "revision": null,
        "window_id": null,
        "pid": null,
        "source": null,
        "truncated": null,
        "accessibility_legacy": null,
    })
}

fn tree_value(report: &TreeReport, notes: &mut Vec<String>) -> Value {
    if let Some(note) = report.note.as_deref() {
        notes.push(note.to_string());
    }
    let legacy = match report.legacy.as_deref() {
        Some(text) => serde_json::from_str::<Value>(text.trim()).unwrap_or(Value::Null),
        None => Value::Null,
    };
    json!({
        "tree": report.tree,
        "revision": report.revision,
        "window_id": report.window_id,
        "pid": report.pid,
        "source": report.source,
        "truncated": report.truncated,
        "accessibility_legacy": legacy,
    })
}

fn primary_display(displays: &[DisplayInfo]) -> Option<&DisplayInfo> {
    if let Some(display) = displays.iter().find(|display| display.is_primary) {
        return Some(display);
    }
    displays.first()
}

fn screen_value(display: Option<&DisplayInfo>) -> Value {
    match display {
        Some(display) => json!({
            "id": display.id,
            "name": display.name,
            "x": display.x,
            "y": display.y,
            "width": display.width,
            "height": display.height,
            "scale": display.scale,
        }),
        None => Value::Null,
    }
}

fn guard<T>(label: &str, function: impl FnOnce() -> Result<T, ComputerError>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(function)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(format!("{label}: {error}")),
        Err(_) => Err(format!("{label} panicked and was recovered")),
    }
}

fn platform_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_backend::StubBackend;

    #[test]
    fn state_shape_from_a_stub_backend() {
        let backend = StubBackend::default();
        let value = collect_state(&backend);
        assert_eq!(value["displays"][0]["name"], "Stub");
        assert_eq!(value["screen"]["width"], 800);
        assert_eq!(value["screen"]["scale"], 2.0);
        assert_eq!(value["cursor"]["x"], 4);
        assert_eq!(value["cursor"]["y"], 5);
        assert_eq!(value["frontmost_app"], "Finder");
        assert_eq!(value["frontmost_window"], "Desktop");
        assert_eq!(value["source"], "stub");
        assert!(value["tree"].as_str().unwrap().contains("AXWindow"));
        assert!(value["accessibility_legacy"].is_null());
        assert!(value["note"].is_null());
        assert!(value["platform"].is_string());
    }

    #[test]
    fn accessibility_failure_is_null_with_a_note() {
        let backend = StubBackend {
            fail_accessibility: true,
            ..StubBackend::default()
        };
        let value = collect_state(&backend);
        assert!(value["tree"].is_null());
        assert!(value["accessibility_legacy"].is_null());
        assert!(value["note"].as_str().unwrap().contains("accessibility"));
        assert_eq!(value["frontmost_app"], "Finder");
    }
}
