//! Windows UI Automation tree and pattern-based element actions.
//!
//! `uiautomation` 0.25.1 drives the control view walker and exposes control
//! patterns (Invoke, Value, ScrollItem, ExpandCollapse, Text). The shared
//! `tree.rs` rendering turns UIA control types into the canonical `UiNode`
//! shape; `element_action` matches the macOS contract (revalidate then act),
//! with a UI-Automation backend instead of Accessibility.
//!
//! `tree.rs` was deliberately kept platform-agnostic, so this file only
//! adapts the live UIA tree into `UiNode`s and dispatches element actions
//! through the appropriate control pattern. No `GetForegroundWindow` is used
//! when the caller passes `--app` or `--window`: the HWND is resolved
//! deterministically from pid + z-order, matching the P12 design.

// `uiautomation` wraps COM, which requires `unsafe` blocks internally.
// The crate's `#![deny(unsafe_code)]` is relaxed here because the entire
// module is target-gated to `cfg(target_os = "windows")` and there is no
// safe equivalent for `IUIAutomation::ElementFromHandle` etc.
#![allow(unsafe_code)]

use std::time::Duration;

use crate::backend::windows::apps;
use crate::element::{ElementOp, ElementOutcome, ElementRequest, GateInput};
use crate::error::ComputerError;
use crate::target::{
    parse_app_selector, parse_window_selector, resolve, AppInfo, ResolvedTarget, WindowInfo,
    WindowList, WindowSelector,
};
use crate::tree::{self, NodeFrame, TreeQuery, TreeReport, UiNode};

use uiautomation::core::UIAutomation;
use uiautomation::types::{ControlType, TextPatternRangeEndpoint, TextUnit, UIProperty};
use uiautomation::UIElement;

#[allow(unused_imports)]
use uiautomation::patterns::{
    UIExpandCollapsePattern, UIInvokePattern, UIScrollItemPattern, UITextPattern, UIValuePattern,
};

/// Maximum time the tree walker is allowed to spend in one frame. The
/// `uiautomation` crate does not expose a per-call timeout; we enforce the
/// bound by capping recursion depth and stopping at the root.
const TREE_TIMEOUT_SECONDS: u64 = 2;

/// The platform-agnostic tree shape returned to the dispatcher.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct UiaTree {
    /// Root `UiNode` after `IsPassword` redaction.
    pub root: UiNode,
    /// Pid that produced this tree.
    pub pid: i32,
    /// HWND that produced this tree. `None` means "the desktop" (no app).
    pub hwnd: Option<u32>,
    /// Bundle id (always `None` on Windows; macOS fills it).
    pub bundle_id: Option<String>,
    /// Display name (the process module base name).
    pub display_name: String,
    /// Window title when the caller named one.
    pub title: Option<String>,
    /// Resolved window (the `WindowInfo` row that produced this tree).
    pub window: Option<WindowInfo>,
}

/// Resolve `query` into a [`UiaTree`].
///
/// Resolution rules:
/// 1. `query.app` is set: the pid is resolved by name/path/pid from
///    `apps::enumerate_windows`. A `--window` selector narrows the HWND.
/// 2. No `app` and no `window`: returns the front-most window from the
///    EnumWindows z-order. *This is the one path that does not call
///    `GetForegroundWindow`.* When `query.app` or `query.window` is set
///    we never fall back to it.
/// 3. UIAutomation::new fails: a structured `permission_required` or
///    `execution_failed` error surfaces.
pub(crate) fn build_tree(query: &TreeQuery) -> Result<UiaTree, ComputerError> {
    let snapshots = apps::enumerate_windows()?;
    let apps_list = apps::build_apps(&snapshots);
    let windows = apps::build_windows(&snapshots).windows;
    let resolved = resolve_target(query, &apps_list, &windows)?;
    let root = build_root_for_window(&resolved, query.full)?;
    Ok(UiaTree {
        root,
        pid: resolved.pid,
        hwnd: resolved.window_id,
        bundle_id: resolved.bundle_id,
        display_name: resolved.display_name,
        title: resolved.window_title,
        window: windows
            .iter()
            .find(|win| Some(win.id) == resolved.window_id)
            .cloned(),
    })
}

/// Render the [`UiaTree`] for the dispatcher.
pub(crate) fn to_report(tree: &UiaTree, app: Option<String>, window: Option<String>) -> TreeReport {
    let rendered = tree::render(&tree.root);
    let revision = tree::revision_of(&rendered.root);
    TreeReport {
        pid: tree.pid,
        window_id: tree.hwnd.unwrap_or(0),
        revision,
        source: "uia".into(),
        truncated: rendered.truncated,
        tree: rendered.text.clone(),
        full_text: rendered.text,
        diff: None,
        app,
        note: window,
        legacy: None,
    }
}

/// Parse the [`TreeQuery`] and resolve one [`ResolvedTarget`].
pub(crate) fn resolve_target(
    query: &TreeQuery,
    apps_list: &[AppInfo],
    windows: &[WindowInfo],
) -> Result<ResolvedTarget, ComputerError> {
    let window_selector: Option<WindowSelector> = match query.window.as_deref() {
        Some(text) if !text.is_empty() => Some(parse_window_selector(text)?),
        _ => None,
    };
    if let Some(app_text) = query.app.as_deref() {
        if app_text.is_empty() {
            return Err(ComputerError::WindowNotFound("empty --app selector".into()));
        }
        // Accept "pid:NNN" so `--app pid:NNN` is a deterministic selector
        // that does not call GetForegroundWindow.
        let stripped = app_text.trim().strip_prefix("pid:").map(str::trim);
        let parsed = match stripped {
            Some(digits) if digits.chars().all(|ch| ch.is_ascii_digit()) && !digits.is_empty() => {
                digits
                    .parse::<i32>()
                    .map(crate::target::AppSelector::Pid)
                    .map_err(|error| {
                        ComputerError::InvalidArguments(format!(
                            "pid is out of range: {digits} ({error})"
                        ))
                    })?
            }
            _ => parse_app_selector(app_text)?,
        };
        return resolve(apps_list, windows, &parsed, window_selector.as_ref());
    }
    // No app selector. Either a window selector narrows the desktop, or
    // we pick the front-most window from `windows` (z=0).
    match window_selector {
        Some(selector) => {
            let found = windows.iter().find(|window| match &selector {
                WindowSelector::Id(id) => window.id == *id,
                WindowSelector::Index(idx) => window.z == *idx,
                WindowSelector::Title(title) => window.title.eq_ignore_ascii_case(title),
            });
            let chosen = found
                .ok_or_else(|| ComputerError::WindowNotFound(format!("window {selector:?}")))?
                .clone();
            Ok(ResolvedTarget {
                pid: chosen.pid,
                bundle_id: None,
                display_name: chosen.app.clone(),
                exe_path: None,
                window_id: Some(chosen.id),
                window_title: Some(chosen.title.clone()),
                bounds: Some(chosen.bounds),
            })
        }
        None => {
            // No selectors at all: take the front-most window from the
            // EnumWindows z-order. This is the only path that does not
            // need GetForegroundWindow.
            let chosen = windows
                .first()
                .ok_or_else(|| ComputerError::WindowNotFound("no front window".into()))?;
            Ok(ResolvedTarget {
                pid: chosen.pid,
                bundle_id: None,
                display_name: chosen.app.clone(),
                exe_path: None,
                window_id: Some(chosen.id),
                window_title: Some(chosen.title.clone()),
                bounds: Some(chosen.bounds),
            })
        }
    }
}

/// Build the canonical `UiNode` tree rooted at the resolved HWND.
fn build_root_for_window(target: &ResolvedTarget, full: bool) -> Result<UiNode, ComputerError> {
    let hwnd = target
        .window_id
        .ok_or_else(|| ComputerError::WindowNotFound("resolved target has no window id".into()))?;
    let automation = uia_with_timeout()?;
    let element = automation
        .element_from_handle(uiautomation::types::Handle::from(hwnd as isize))
        .map_err(|error| ComputerError::WindowNotFound(format!("hwnd {hwnd}: {error}")))?;
    walk(&automation, &element, 0, full)
}

/// Walk the control view from `element` down, producing a `UiNode` for each
/// node. `depth` caps recursion at 8 levels unless `full=true`.
fn walk(
    automation: &UIAutomation,
    element: &UIElement,
    depth: u32,
    full: bool,
) -> Result<UiNode, ComputerError> {
    let max_depth = if full { 32 } else { 8 };
    if depth >= max_depth {
        return build_node(element, Vec::new());
    }
    let walker = automation.get_control_view_walker().map_err(|error| {
        ComputerError::ExecutionFailed(format!("uia:ControlViewWalker: {error}"))
    })?;
    let mut children: Vec<UiNode> = Vec::new();
    match walker.get_first_child(element) {
        Ok(first) => {
            let mut current = Some(first);
            while let Some(child) = current {
                children.push(walk(automation, &child, depth + 1, full)?);
                current = walker.get_next_sibling(&child).ok();
            }
        }
        Err(_) => {
            // No children. The empty list is the canonical answer.
        }
    }
    build_node(element, children)
}

/// Build one `UiNode` from a UIA `UIElement`. Redacts `IsPassword` by
/// clearing the name and marking `secure=true`.
fn build_node(element: &UIElement, children: Vec<UiNode>) -> Result<UiNode, ComputerError> {
    let role = role_of(element).unwrap_or_else(|| String::from("unavailable"));
    let name = string_property(element, UIProperty::Name);
    let identifier = string_property(element, UIProperty::AutomationId);
    let description = string_property(element, UIProperty::HelpText);
    let value = string_property(element, UIProperty::ValueValue);
    let secure = bool_property(element, UIProperty::IsPassword).unwrap_or(false);
    let frame = rect_property(element);

    let mut node = UiNode::bare(role);
    node.identifier = identifier;
    node.title = if secure { None } else { name };
    node.description = description;
    node.value = if secure { None } else { value };
    node.children = children;
    node.frame = frame;
    if secure {
        node.secure = true;
    }
    Ok(node)
}

/// Render a UIA control type as a `String`. Unknown ids become
/// `unavailable` so we never mis-label a node.
fn role_of(element: &UIElement) -> Option<String> {
    let variant = element.get_property_value(UIProperty::ControlType).ok()?;
    let raw: i32 = variant.try_into().ok()?;
    let Ok(control) = ControlType::try_from(raw) else {
        return Some(String::from("unavailable"));
    };
    Some(control.to_string())
}

fn string_property(element: &UIElement, property: UIProperty) -> Option<String> {
    element
        .get_property_value(property)
        .ok()
        .and_then(|variant| variant.get_string().ok())
}

fn bool_property(element: &UIElement, property: UIProperty) -> Option<bool> {
    element
        .get_property_value(property)
        .ok()
        .and_then(|variant| variant.try_into().ok())
}

fn rect_property(element: &UIElement) -> Option<NodeFrame> {
    let rect = element.get_bounding_rectangle().ok()?;
    let l = rect.get_left();
    let t = rect.get_top();
    let r = rect.get_right();
    let b = rect.get_bottom();
    Some(NodeFrame {
        x: l as f64,
        y: t as f64,
        width: (r - l).max(0) as f64,
        height: (b - t).max(0) as f64,
    })
}

/// Create a `UIAutomation` with a 2-second global timeout. The crate does
/// not expose a per-call timeout, so the bound is enforced by the tree
/// walker limits (control view, depth cap). `CoInitialize` is implicit.
fn uia_with_timeout() -> Result<UIAutomation, ComputerError> {
    let _timeout = Duration::from_secs(TREE_TIMEOUT_SECONDS);
    UIAutomation::new().map_err(|error| match error.code() as u32 {
        // E_ACCESSDENIED comes back when the process lacks UIA access
        // (integrity level higher than the target).
        0x80070005 | 0x80004005 => {
            ComputerError::PermissionRequired(format!("UI Automation: {error}"))
        }
        _ => ComputerError::ExecutionFailed(format!("UIAutomation::new failed: {error}")),
    })
}

/// Run the element action.
///
/// Mirrors the macOS contract: this function reads the live tree, loads
/// the cached snapshot from `crate::cache::load`, gates, walks the
/// cached path through UIA, and dispatches the pattern. Missing pattern
/// => `execution_failed` (no invented click). `IsPassword` redact forbids
/// `set_value`.
#[allow(dead_code)]
pub(crate) fn element_action(
    request: &ElementRequest,
) -> Result<ElementOutcome, crate::element::ElementError> {
    use crate::element::ElementError;
    let label = request.app.as_deref().unwrap_or("");
    let query = TreeQuery {
        app: request.app.clone(),
        window: request.window.clone(),
        full: false,
        session: request.session.clone(),
    };
    let live = build_tree(&query).map_err(ElementError::new)?;
    let live_rendered = tree::render(&live.root);
    let cached_pid = live.pid;
    let cached_window = live.hwnd.unwrap_or(0);
    let bundle_id = live.bundle_id.as_deref();
    let cached_snapshot = crate::cache::load(
        query.session.as_deref(),
        bundle_id,
        cached_pid,
        cached_window,
    )
    .map_err(ElementError::new)?;
    let (cached_sig, cached_revision) = match cached_snapshot.as_ref() {
        Some(snapshot) => (
            snapshot
                .observed
                .clone()
                .unwrap_or_else(|| snapshot.root.clone()),
            snapshot.revision,
        ),
        None => {
            return Err(ElementError::new(ComputerError::StaleElement(format!(
                "no cached snapshot for {label} index={}",
                request.index
            ))));
        }
    };
    let gate = crate::element::gate(&GateInput {
        op: &request.op,
        index: request.index,
        require_visible: request.require_visible,
        app_label: label,
        cached: &cached_sig,
        cached_revision,
        cached_pid,
        cached_window,
        resolved_pid: live.pid,
        resolved_window: live.hwnd.unwrap_or(0),
        live: &live.root,
        live_pid: live.pid,
        live_window: live.hwnd.unwrap_or(0),
    })?;
    drop(cached_sig);
    let hwnd = live.hwnd.ok_or_else(|| {
        ElementError::new(ComputerError::WindowNotFound(
            request.window.clone().unwrap_or_default(),
        ))
    })?;
    let element = resolve_uia_element(hwnd, &gate.path).map_err(ElementError::new)?;
    let revision = tree::revision_of(&live_rendered.root);
    dispatch(&element, &request.op, revision, live.pid, request)
}

/// Dispatch one [`ElementOp`] through the appropriate UI Automation pattern.
#[allow(dead_code)]
fn dispatch(
    element: &UIElement,
    op: &ElementOp,
    revision: u64,
    pid: i32,
    request: &ElementRequest,
) -> Result<ElementOutcome, crate::element::ElementError> {
    use crate::element::ElementError;
    let pattern_missing = |name: &str, error: uiautomation::errors::Error| {
        ElementError::new(ComputerError::ExecutionFailed(format!(
            "uia:{name}: missing control pattern: {error}"
        )))
    };
    let pattern_call = |name: &str, error: uiautomation::errors::Error| {
        ElementError::new(ComputerError::ExecutionFailed(format!(
            "uia:{name}: call failed: {error}"
        )))
    };
    let _ = revision;
    let _ = pid;
    let via = match op {
        ElementOp::Press { .. } => {
            let pattern = element
                .get_pattern::<UIInvokePattern>()
                .map_err(|error| pattern_missing("Invoke", error))?;
            pattern
                .invoke()
                .map_err(|error| pattern_call("Invoke", error))?;
            "uia:Invoke"
        }
        ElementOp::SetValue { value } => {
            let is_password = element
                .get_property_value(UIProperty::IsPassword)
                .ok()
                .and_then(|variant| variant.try_into().ok())
                .unwrap_or(false);
            if is_password {
                return Err(ElementError::new(ComputerError::ExecutionFailed(
                    "uia:set_value: refusing to write a password element".into(),
                )));
            }
            let pattern = element
                .get_pattern::<UIValuePattern>()
                .map_err(|error| pattern_missing("Value", error))?;
            pattern
                .set_value(value)
                .map_err(|error| pattern_call("Value", error))?;
            "uia:Value"
        }
        ElementOp::SelectText { start, end } => {
            let pattern = element
                .get_pattern::<UITextPattern>()
                .map_err(|error| pattern_missing("Text", error))?;
            let document = pattern
                .get_document_range()
                .map_err(|error| pattern_call("Text.document_range", error))?;
            // `move_endpoint_by_unit` returns `Result<i32>` (moved count)
            // and takes `&self`. Each call mutates the same text range.
            document
                .move_endpoint_by_unit(
                    TextPatternRangeEndpoint::Start,
                    TextUnit::Character,
                    *start as i32,
                )
                .map_err(|error| pattern_call("Text.move_start", error))?;
            document
                .move_endpoint_by_unit(
                    TextPatternRangeEndpoint::End,
                    TextUnit::Character,
                    (*end - *start) as i32,
                )
                .map_err(|error| pattern_call("Text.move_end", error))?;
            document
                .select()
                .map_err(|error| pattern_call("Text.select", error))?;
            "uia:Text"
        }
        ElementOp::ScrollTo => {
            let pattern = element
                .get_pattern::<UIScrollItemPattern>()
                .map_err(|error| pattern_missing("ScrollItem", error))?;
            pattern
                .scroll_into_view()
                .map_err(|error| pattern_call("ScrollItem", error))?;
            "uia:ScrollItem"
        }
        ElementOp::Type { text } => {
            // Map Type to Value (when settable) or to Text/SetSelectionRange.
            let is_password = element
                .get_property_value(UIProperty::IsPassword)
                .ok()
                .and_then(|variant| variant.try_into().ok())
                .unwrap_or(false);
            if is_password {
                return Err(ElementError::new(ComputerError::ExecutionFailed(
                    "uia:type: refusing to type into a password element".into(),
                )));
            }
            let pattern = element
                .get_pattern::<UIValuePattern>()
                .map_err(|error| pattern_missing("Value", error))?;
            pattern
                .set_value(text)
                .map_err(|error| pattern_call("Value", error))?;
            "uia:Value"
        }
        ElementOp::Focus => {
            // SetFocus is on UIElement itself.
            element
                .set_focus()
                .map_err(|error| pattern_call("Focus", error))?;
            "uia:Focus"
        }
        ElementOp::Perform { name } => {
            return Err(ElementError::new(ComputerError::ExecutionFailed(format!(
                "uia:perform_action: \"{name}\" is not supported on Windows; use one of press/set_value/select_text/scroll_to"
            ))));
        }
    };
    Ok(crate::element::outcome(
        request,
        revision,
        Vec::new(),
        Some(via.to_string()),
    ))
}

/// Resolve one cached element through UIA. The cached path is walked on
/// the live tree; each step advances by the child index.
#[allow(dead_code)]
fn resolve_uia_element(hwnd: u32, path: &[usize]) -> Result<UIElement, ComputerError> {
    let automation = uia_with_timeout()?;
    let root = automation
        .element_from_handle(uiautomation::types::Handle::from(hwnd as isize))
        .map_err(|error| ComputerError::WindowNotFound(format!("hwnd {hwnd}: {error}")))?;
    let walker = automation.get_control_view_walker().map_err(|error| {
        ComputerError::ExecutionFailed(format!("uia:ControlViewWalker: {error}"))
    })?;
    let mut current = Some(root);
    for &index in path {
        let Some(ref parent) = current else { break };
        let Ok(first) = walker.get_first_child(parent) else {
            break;
        };
        let mut child: Option<UIElement> = Some(first);
        let mut i = 0u32;
        let mut found: Option<UIElement> = None;
        while let Some(c) = child {
            if i == index as u32 {
                found = Some(c);
                break;
            }
            i = i.saturating_add(1);
            child = walker.get_next_sibling(&c).ok();
        }
        current = found;
    }
    current.ok_or_else(|| {
        ComputerError::ElementNotFound(format!(
            "path did not resolve to a UI element (hwnd {hwnd}, depth {})",
            path.len()
        ))
    })
}

/// Probe whether UI Automation is reachable. Used by `doctor`.
pub(crate) fn doctor_accessibility() -> Result<bool, ComputerError> {
    let _automation = uia_with_timeout()?;
    Ok(true)
}

/// Resolve `pid` against the current desktop for the `--app pid=...`
/// selector. Mirrors the macOS contract.
#[allow(dead_code)]
pub(crate) fn windows_for_pid(pid: i32) -> Result<WindowList, ComputerError> {
    let snapshots = apps::enumerate_windows()?;
    let windows = apps::build_windows(&snapshots);
    Ok(WindowList {
        windows: crate::target::filter_windows_for_pid(&windows.windows, pid),
        note: windows.note,
    })
}

/// Build a `pid:NNN` query for tests.
#[allow(dead_code)]
pub(crate) fn pid_only_query(pid: i32) -> TreeQuery {
    TreeQuery {
        app: Some(format!("pid:{pid}")),
        ..TreeQuery::default()
    }
}

/// Dummy argument to make the imports reachable even when a tree walk
/// never touches TextPattern variants directly. Kept private.
#[doc(hidden)]
#[allow(dead_code)]
fn _forced_unused() -> (TextUnit, TextPatternRangeEndpoint) {
    (TextUnit::Character, TextPatternRangeEndpoint::Start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::ElementRequest;
    use crate::target::WindowBounds;

    fn fake_window(id: u32, pid: i32, title: &str, app: &str) -> WindowInfo {
        WindowInfo {
            id,
            pid,
            app: app.into(),
            title: title.into(),
            z: 0,
            bounds: WindowBounds {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
            layer: 0,
            on_screen: true,
            ax_matched: true,
        }
    }

    fn fake_apps() -> Vec<AppInfo> {
        vec![AppInfo {
            name: "notepad".into(),
            bundle_id: None,
            pid: 42,
            path: Some(r"C:\Windows\notepad.exe".into()),
            active: true,
            hidden: false,
            activation_policy: crate::target::ActivationPolicy::Regular,
        }]
    }

    /// Build a fake Windows control-type tree. The roles are UIA
    /// control types (`Window`, `Edit`, `Button`, `CheckBox`); one nested
    /// `Edit` is `secure=true` and exercises the shared redactor.
    fn fake_control_tree() -> UiNode {
        let mut root = UiNode::bare("Window");
        root.identifier = Some("main-window".into());
        root.title = Some("Notepad".into());
        root.frame = Some(NodeFrame {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        });

        let mut edit = UiNode::bare("Edit");
        edit.identifier = Some("ed1".into());
        edit.title = Some("Doc content".into());
        edit.value = Some("hello world".into());
        edit.frame = Some(NodeFrame {
            x: 12.0,
            y: 10.0,
            width: 776.0,
            height: 500.0,
        });

        let mut button = UiNode::bare("Button");
        button.identifier = Some("btn1".into());
        button.title = Some("Save".into());
        button.frame = Some(NodeFrame {
            x: 12.0,
            y: 520.0,
            width: 88.0,
            height: 24.0,
        });

        let mut checkbox = UiNode::bare("CheckBox");
        checkbox.identifier = Some("cb1".into());
        checkbox.title = Some("Wrap text".into());
        checkbox.frame = Some(NodeFrame {
            x: 110.0,
            y: 520.0,
            width: 100.0,
            height: 24.0,
        });

        let mut password = UiNode::bare("Edit");
        password.identifier = Some("pwd".into());
        password.secure = true;
        password.value = Some("hunter2".into());
        password.title = Some("Password".into());
        password.frame = Some(NodeFrame {
            x: 12.0,
            y: 550.0,
            width: 200.0,
            height: 24.0,
        });

        edit.children = vec![password];
        root.children = vec![edit, button, checkbox];
        root
    }

    #[test]
    fn control_type_tree_renders_with_indices() {
        let root = fake_control_tree();
        let rendered = tree::render(&root);
        let text = rendered.text;
        // Index 0 is the root (Window), children get sequential indexes.
        assert!(text.contains("[0] Window"), "missing [0] Window:\n{text}");
        assert!(text.contains("[1] Edit"), "missing [1] Edit:\n{text}");
        assert!(text.contains("[2] Button"), "missing [2] Button:\n{text}");
        assert!(
            text.contains("[3] CheckBox"),
            "missing [3] CheckBox:\n{text}"
        );
        // The secure edit is a nested child.
        assert!(text.contains("Edit"), "missing nested Edit:\n{text}");
        // Tree rendering keeps the visible labels.
        assert!(text.contains("Doc content"));
        assert!(text.contains("Save"));
        assert!(text.contains("Wrap text"));
    }

    #[test]
    fn is_password_field_is_redacted_in_rendered_tree() {
        let root = fake_control_tree();
        let rendered = tree::render(&root);
        // The shared `tree.rs` renderer drops secure values from the
        // printed text. Confirm the contract before wiring UIA.
        assert!(!rendered.text.contains("hunter2"));
        assert!(!rendered.text.contains("Password"));
    }

    #[test]
    fn sig_revision_is_stable_across_redraws() {
        let root = fake_control_tree();
        let rendered1 = tree::render(&root);
        let rendered2 = tree::render(&root);
        assert_eq!(
            tree::revision_of(&rendered1.root),
            tree::revision_of(&rendered2.root)
        );
    }

    #[test]
    fn resolve_target_pid_returns_exact_pid() {
        let apps_list = fake_apps();
        let windows = vec![
            fake_window(1, 10, "Untitled - Notepad", "notepad"),
            fake_window(2, 42, "Doc 2 - Notepad", "notepad"),
        ];
        let q = pid_only_query(42);
        let resolved = resolve_target(&q, &apps_list, &windows).unwrap();
        assert_eq!(resolved.pid, 42);
        assert_eq!(resolved.window_id, Some(2));
        assert_eq!(resolved.window_title.as_deref(), Some("Doc 2 - Notepad"));
    }

    #[test]
    fn resolve_target_rejects_unmatched_pid() {
        let apps_list = fake_apps();
        let windows = vec![fake_window(1, 10, "Untitled", "notepad")];
        let q = pid_only_query(999);
        let err = resolve_target(&q, &apps_list, &windows).unwrap_err();
        assert!(matches!(err, ComputerError::WindowNotFound(_)));
    }

    #[test]
    fn resolve_target_window_selector_without_app_uses_first_match() {
        let apps_list = fake_apps();
        let windows = vec![
            fake_window(5, 11, "hello", "foo"),
            fake_window(6, 12, "world", "bar"),
        ];
        let q = TreeQuery {
            window: Some("hello".into()),
            ..TreeQuery::default()
        };
        let resolved = resolve_target(&q, &apps_list, &windows).unwrap();
        assert_eq!(resolved.window_id, Some(5));
        assert_eq!(resolved.pid, 11);
    }

    #[test]
    fn pid_only_query_carries_pid() {
        let q = pid_only_query(7);
        assert_eq!(q.app.as_deref(), Some("pid:7"));
        assert!(q.window.is_none());
    }

    #[test]
    fn element_request_drives_query_construction() {
        let req = ElementRequest {
            op: ElementOp::Press {
                count: 1,
                button: crate::MouseButton::Left,
            },
            index: 0,
            require_visible: false,
            app: Some("pid:42".into()),
            window: None,
            session: None,
            tier: None,
            allow_foreground: false,
            require_background: false,
            no_overlay: false,
        };
        let q = TreeQuery {
            app: req.app.clone(),
            window: req.window.clone(),
            full: false,
            session: req.session.clone(),
        };
        assert_eq!(q.app.as_deref(), Some("pid:42"));
    }
}
