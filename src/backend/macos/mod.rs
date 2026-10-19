//! macOS accessibility and frontmost-window lookup.
//!
//! Screenshots and input are shared. The accessibility tree is read in-process
//! from `AXUIElement`. `osascript` remains the fallback when that path fails
//! for a reason other than permission or a messaging timeout. App inventory
//! and `doctor` use AppKit and CoreGraphics.

mod apps;
mod ax;
mod capture;
mod doctor;

pub(crate) use apps::{activate, launch, list_apps, list_windows, quit};
pub(crate) use capture::session_display_block;
pub(crate) use doctor::doctor;

use crate::tree::{TreeQuery, TreeReport};

/// Key-window accessibility tree.
pub(crate) fn tree(query: &TreeQuery) -> Result<TreeReport, ComputerError> {
    ax::tree(query)
}

/// Revalidate one cached element and perform the action through Accessibility.
pub(crate) fn element_action(
    request: &crate::element::ElementRequest,
) -> Result<crate::element::ElementOutcome, crate::element::ElementError> {
    ax::element_action(request)
}

/// Role and title of the element at a screen point.
pub(crate) fn hit_test(x: i32, y: i32) -> Result<Option<crate::element::HitInfo>, ComputerError> {
    ax::hit_test(x, y)
}

/// Restore Electron flags for `session` and stop its observers.
pub(crate) fn end_session(session: Option<&str>) -> Result<Vec<i32>, ComputerError> {
    ax::end_session(session)
}

/// Process-exit restore. See [`ax::shutdown_sessions`].
pub(crate) fn shutdown_sessions() {
    ax::shutdown_sessions();
}

/// Capture one window with ScreenCaptureKit, then an xcap crop if that fails.
pub(crate) fn capture_window(
    target: &super::WindowTarget,
) -> Result<crate::types::RawCapture, ComputerError> {
    capture::capture_window(target)
}

use std::time::Duration;

use crate::error::ComputerError;
use crate::types::FrontmostInfo;

use super::command::{run_timeout, stderr_snippet};

/// macOS desktop backend. Methods are implemented in the parent module.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacOsDesktopBackend;

const FRONTMOST_TIMEOUT: Duration = Duration::from_secs(4);
const ACCESSIBILITY_TIMEOUT: Duration = Duration::from_secs(8);

pub(crate) fn frontmost() -> FrontmostInfo {
    const SCRIPT: &str = r#"tell application "System Events"
  set p to first process whose frontmost is true
  set appName to name of p
  set winName to ""
  try
    set winName to name of front window of p
  end try
  return appName & linefeed & winName
end tell"#;
    match run_timeout("osascript", &["-e", SCRIPT], FRONTMOST_TIMEOUT) {
        Ok(output) if output.status.success() => split_frontmost(&output.stdout),
        Ok(output) => FrontmostInfo {
            note: Some(permission_or_error(&output.stderr, "frontmost app")),
            ..FrontmostInfo::default()
        },
        Err(error) => FrontmostInfo {
            note: Some(format!("frontmost app unavailable: {error}")),
            ..FrontmostInfo::default()
        },
    }
}

pub(crate) fn inspect_accessibility(app_name: Option<&str>) -> Result<String, ComputerError> {
    let script = accessibility_script(app_name);
    let output = run_timeout(
        "osascript",
        &["-l", "JavaScript", "-e", &script],
        ACCESSIBILITY_TIMEOUT,
    )
    .map_err(|error| ComputerError::ExecutionFailed(format!("accessibility failed: {error}")))?;
    if !output.status.success() {
        return Err(ComputerError::ExecutionFailed(permission_or_error(
            &output.stderr,
            "accessibility",
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.len() > 1024 * 1024 {
        return Err(ComputerError::ExecutionFailed(
            "accessibility tree exceeded 1 MiB and was dropped".into(),
        ));
    }
    if text.is_empty() {
        return Err(ComputerError::ExecutionFailed(
            "accessibility returned an empty result".into(),
        ));
    }
    Ok(text)
}

fn split_frontmost(stdout: &[u8]) -> FrontmostInfo {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines();
    let app = lines.next().map(str::trim).filter(|line| !line.is_empty());
    let window = lines.next().map(str::trim).filter(|line| !line.is_empty());
    FrontmostInfo {
        app: app.map(str::to_string),
        window: window.map(str::to_string),
        note: None,
    }
}

fn permission_or_error(stderr: &[u8], what: &str) -> String {
    let snippet = stderr_snippet(stderr);
    let lower = snippet.to_ascii_lowercase();
    if lower.contains("not allowed")
        || lower.contains("not authorized")
        || lower.contains("assistive")
        || lower.contains("1002")
        || lower.contains("-1719")
        || lower.contains("-25211")
    {
        format!(
            "{what} needs Accessibility permission. Enable this terminal or computer-use in System Settings > Privacy & Security > Accessibility, then retry"
        )
    } else {
        format!("{what} failed: {snippet}")
    }
}

fn accessibility_script(app_name: Option<&str>) -> String {
    let target = match app_name {
        Some(name) => {
            let quoted = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".to_string());
            format!("var app = Application('System Events').processes[{quoted}];")
        }
        None => "var app = Application('System Events').processes.whose({frontmost: true})[0];"
            .to_string(),
    };
    format!(
        r#"(function () {{
  function safe(fn) {{
    try {{ return fn(); }} catch (e) {{ return null; }}
  }}
  function describeElement(el, depth) {{
    if (depth > 6) return {{role: "truncated"}};
    try {{
      var desc = {{
        role: safe(function () {{ return el.role(); }}),
        description: safe(function () {{ return el.description(); }}),
        title: safe(function () {{ return el.title(); }}),
        value: safe(function () {{ return el.value(); }}),
        position: safe(function () {{ return el.position(); }}),
        size: safe(function () {{ return el.size(); }}),
        children: []
      }};
      try {{
        var kids = el.uiElements();
        var limit = Math.min(kids.length, 30);
        for (var i = 0; i < limit; i++) {{
          desc.children.push(describeElement(kids[i], depth + 1));
        }}
        if (kids.length > limit) desc.truncated_children = kids.length - limit;
      }} catch (e) {{}}
      return desc;
    }} catch (e) {{
      return {{role: "unknown", error: String(e)}};
    }}
  }}
  {target}
  if (!app) return JSON.stringify({{error: "application not found"}});
  var win = null;
  try {{ win = app.windows[0]; }} catch (e) {{}}
  var appName = safe(function () {{ return app.name(); }});
  if (!win) return JSON.stringify({{app: appName, error: "no windows"}});
  return JSON.stringify({{app: appName, window: describeElement(win, 0)}});
}})()"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_embeds_a_quoted_app_name() {
        let script = accessibility_script(Some(r#"Foo "Bar""#));
        assert!(script.contains(r#"processes["Foo \"Bar\""]"#));
        assert!(script.contains("describeElement"));
        let front = accessibility_script(None);
        assert!(front.contains("frontmost: true"));
    }

    #[test]
    fn frontmost_parser_splits_lines() {
        let info = split_frontmost(b"Finder\nDesktop\n");
        assert_eq!(info.app.as_deref(), Some("Finder"));
        assert_eq!(info.window.as_deref(), Some("Desktop"));
        let empty = split_frontmost(b"\n\n");
        assert!(empty.app.is_none());
        assert!(empty.window.is_none());
    }
}
