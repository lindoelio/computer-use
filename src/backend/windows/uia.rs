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

use crate::backend::windows::activate::{self, LockRoute};
use crate::backend::windows::apps;
use crate::element::{ElementError, ElementOp, ElementOutcome, ElementRequest, GateInput};
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
///
/// The whole tree build runs on the UIA worker so the live COM walks are
/// bounded by `TREE_TIMEOUT_SECONDS`.
fn build_root_for_window(target: &ResolvedTarget, full: bool) -> Result<UiNode, ComputerError> {
    let hwnd = target
        .window_id
        .ok_or_else(|| ComputerError::WindowNotFound("resolved target has no window id".into()))?;
    let hwnd_isize = hwnd as isize;
    run_uia_with_timeout(
        TREE_TIMEOUT_SECONDS,
        move || -> Result<UiNode, ComputerError> {
            let automation = uia_with_timeout()?;
            let element = automation
                .element_from_handle(uiautomation::types::Handle::from(hwnd_isize))
                .map_err(|error| ComputerError::WindowNotFound(format!("hwnd {hwnd}: {error}")))?;
            walk(&automation, &element, 0, full)
        },
    )
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

/// Create a `UIAutomation`, mapping the COM failure modes onto our error
/// enum. The call itself is fast (it instantiates a CLSCTX_ALL
/// `IUIAutomation` after `CoInitializeEx(MTA)`), so no timeout wrapper is
/// needed here — the wrapper is applied by [`run_uia_with_timeout`] at
/// the call sites that actually walk live COM (`build_root_for_window`
/// and the dispatch path), where the timeout matters.
///
/// E_ACCESSDENIED comes back when the process lacks UIA access
/// (integrity level higher than the target).
pub(crate) fn uia_with_timeout() -> Result<UIAutomation, ComputerError> {
    UIAutomation::new().map_err(|error| match error.code() as u32 {
        0x80070005 | 0x80004005 => {
            ComputerError::PermissionRequired(format!("UI Automation: {error}"))
        }
        _ => ComputerError::ExecutionFailed(format!("UIAutomation::new failed: {error}")),
    })
}

/// Run a closure that does UIA work on a fresh worker thread, and wait up
/// to `seconds` for it. The worker is COM-initialised by the closure
/// itself (via [`uia_with_timeout`] or `UIAutomation::new`), so each
/// closure that touches UIA *must* build its own `UIAutomation` instance
/// inside the closure rather than reuse one across calls. Pass closures by
/// value; the closure runs exactly once.
///
/// On timeout the worker is detached and the error
/// `UI Automation call timed out after Ns` is returned. The caller never
/// blocks indefinitely on a stuck UIA target.
///
/// `UIElement` and `UIAutomation` are not `Send`; closures must not
/// return them. Restructure the call site so the closure drives the
/// whole COM operation and returns a primitive / owned value
/// (typically [`UiNode`] or [`ElementOutcome`]).
pub(crate) fn run_uia_with_timeout<T, F>(seconds: u64, closure: F) -> Result<T, ComputerError>
where
    F: FnOnce() -> Result<T, ComputerError> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<Result<T, ComputerError>>();
    let builder = std::thread::Builder::new();
    builder
        .name("cu-uia".into())
        .spawn(move || {
            let result = closure();
            let _ = tx.send(result);
        })
        .map_err(|error| {
            ComputerError::ExecutionFailed(format!("could not spawn UIA worker: {error}"))
        })?;
    match rx.recv_timeout(Duration::from_secs(seconds)) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // We deliberately do not join the thread: it will complete when
            // UIA does, but the CLI must not block on a stuck target
            // window. The detached worker has no observable side effects
            // on the caller.
            Err(ComputerError::ExecutionFailed(format!(
                "UI Automation call timed out after {seconds}s"
            )))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(
            ComputerError::ExecutionFailed("UI Automation worker disconnected".into()),
        ),
    }
}

/// Run the element action.
///
/// Mirrors the macOS contract: this function reads the live tree, loads
/// the cached snapshot from `crate::cache::load`, gates, walks the
/// cached path through UIA, and dispatches the pattern. Missing pattern
/// => `execution_failed` (no invented click). `IsPassword` redact forbids
/// `set_value`.
///
/// The UIA path-walk + dispatch is run inside a [`run_uia_with_timeout`]
/// worker so a stuck target window cannot block the CLI: on timeout we
/// return `execution_failed("UI Automation call timed out after Ns")` and
/// the worker detaches. The resolved `UIAutomation` / `UIElement` never
/// leave the worker thread — the closure returns the final
/// `ElementOutcome`.
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
    let revision = tree::revision_of(&live_rendered.root);
    let op = request.op.clone();
    let request_clone = request.clone();
    let gate_clone = gate.clone();

    let dispatched = run_uia_with_timeout(
        TREE_TIMEOUT_SECONDS,
        move || -> Result<Result<ElementOutcome, DispatchFailure>, ComputerError> {
            let element = resolve_uia_element_inner(hwnd, &gate.path)?;
            Ok(dispatch_inner(&element, &op, revision, &request_clone))
        },
    );

    let failure = match dispatched {
        Err(timeout) => return Err(ElementError::new(timeout)),
        Ok(Ok(outcome)) => return Ok(outcome),
        Ok(Err(DispatchFailure::Fatal(error))) => return Err(error),
        Ok(Err(DispatchFailure::Retryable(failure))) => failure,
    };

    // The pattern is missing or its call failed. Retry as tier `activate`,
    // which is the documented foreground fallback (see `activate.rs`).
    let (route, lock) = acquire_lock(request.allow_foreground).map_err(ElementError::new)?;
    let plan = activate::plan(
        &request.op,
        &failure,
        request.require_background,
        route,
        gate_clone.center,
    );
    let focus_restored = match plan {
        activate::FallbackPlan::Refuse(error) => return Err(ElementError::new(error)),
        activate::FallbackPlan::Activate => {
            // `lock` stays held for the whole foreground round-trip.
            let restored =
                activate::run(hwnd, &request.op, &gate_clone).map_err(ElementError::new)?;
            drop(lock);
            restored
        }
    };
    let extra: Vec<String> = gate_clone.outside_warning.into_iter().collect();
    Ok(activate::outcome(request, revision, focus_restored, &extra))
}

/// Take `foreground.lock` for the activate fallback.
///
/// `--allow-foreground` skips it. A platform that cannot name a lock path is
/// reported as [`LockRoute::Allowed`], which is how `macos::input` treats the
/// same situation. A lock held by another session is *not* an error here: the
/// refusal is [`activate::plan`]'s job, so the reason is tested in one place.
fn acquire_lock(
    allow_foreground: bool,
) -> Result<(LockRoute, Option<crate::foreground::ForegroundLock>), ComputerError> {
    if allow_foreground {
        return Ok((LockRoute::Allowed, None));
    }
    let Some(path) = crate::foreground::lock_path() else {
        return Ok((LockRoute::Allowed, None));
    };
    match crate::foreground::try_lock(&path) {
        Ok(lock) => Ok((LockRoute::Acquired, Some(lock))),
        Err(crate::foreground::LockError::Held { pid, .. }) => Ok((LockRoute::Busy(pid), None)),
        Err(other) => Err(other.into_computer()),
    }
}

/// Why the pattern path inside the UIA worker did not produce an outcome.
enum DispatchFailure {
    /// No fallback is allowed; surface this error.
    Fatal(ElementError),
    /// A pattern was missing or its call failed; the activate fallback may
    /// reproduce the action.
    Retryable(activate::PatternFailure),
}

/// Dispatch one [`ElementOp`] through the appropriate UI Automation pattern.
/// Worker-internal — must not be called from outside the
/// `run_uia_with_timeout` closure that also resolves the element,
/// because [`UIElement`] is not `Send` and would otherwise have to cross
/// a thread boundary.
fn dispatch_inner(
    element: &UIElement,
    op: &ElementOp,
    revision: u64,
    request: &ElementRequest,
) -> Result<ElementOutcome, DispatchFailure> {
    use crate::backend::windows::activate::PatternFailure;
    use crate::element::ElementError;
    // A missing pattern and a failed call are both recoverable: some Win32
    // apps ignore `Invoke` until their window is foreground, so both hand the
    // caller a `Retryable` failure for the activate fallback to consider.
    let pattern_missing = |pattern: &'static str, error: uiautomation::errors::Error| {
        DispatchFailure::Retryable(PatternFailure::Missing {
            pattern,
            detail: error.to_string(),
        })
    };
    let pattern_call = |pattern: &'static str, error: uiautomation::errors::Error| {
        DispatchFailure::Retryable(PatternFailure::Call {
            pattern,
            detail: error.to_string(),
        })
    };
    let fatal = |message: String| -> DispatchFailure {
        DispatchFailure::Fatal(ElementError::new(ComputerError::ExecutionFailed(message)))
    };
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
                return Err(fatal(
                    "uia:set_value: refusing to write a password element".into(),
                ));
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
                return Err(fatal(
                    "uia:type: refusing to type into a password element".into(),
                ));
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
            return Err(fatal(format!(
                "uia:perform_action: \"{name}\" is not supported on Windows; use one of press/set_value/select_text/scroll_to"
            )));
        }
    };
    Ok(crate::element::outcome(
        request,
        revision,
        Vec::new(),
        Some(via.to_string()),
    ))
}

/// Resolve one cached element through UIA. Worker-internal — must not be
/// called from outside the `run_uia_with_timeout` closure; the returned
/// [`UIElement`] cannot cross thread boundaries.
pub(crate) fn resolve_uia_element_inner(
    hwnd: u32,
    path: &[usize],
) -> Result<UIElement, ComputerError> {
    let hwnd_isize = hwnd as isize;
    let automation = uia_with_timeout()?;
    let root = automation
        .element_from_handle(uiautomation::types::Handle::from(hwnd_isize))
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

/// Put keyboard focus on one cached element through UIA.
///
/// Used by the activate fallback for `type`: `set_focus` is a background-safe
/// UIA call, so the only foreground step is the `SetForegroundWindow` the
/// fallback already performed. Runs inside the worker because [`UIElement`]
/// cannot cross a thread boundary. Only `type` calls it, and the test build
/// never reaches the input path.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn focus_element(hwnd: u32, path: &[usize]) -> Result<(), ComputerError> {
    let path = path.to_vec();
    run_uia_with_timeout(
        TREE_TIMEOUT_SECONDS,
        move || -> Result<(), ComputerError> {
            let element = resolve_uia_element_inner(hwnd, &path)?;
            element.set_focus().map_err(|error| {
                ComputerError::ExecutionFailed(format!("uia:focus: set_focus failed: {error}"))
            })
        },
    )
}

/// Probe whether UI Automation is reachable. Used by `doctor`. Lives
/// inside the worker so the probe itself is bounded by the UIA timeout.
pub(crate) fn doctor_accessibility() -> Result<bool, ComputerError> {
    run_uia_with_timeout(TREE_TIMEOUT_SECONDS, || -> Result<bool, ComputerError> {
        let _: UIAutomation = uia_with_timeout()?;
        Ok(true)
    })
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
        // Index 0 is the root (Window); subsequent indexes walk preorder.
        // 1: Edit "Doc content"; 2: Edit "Password"; 3: Button "Save";
        // 4: CheckBox "Wrap text".
        assert!(text.contains("[0] Window"), "missing [0] Window:\n{text}");
        assert!(text.contains("[1] Edit"), "missing [1] Edit:\n{text}");
        assert!(text.contains("[2] Edit"), "missing [2] Edit:\n{text}");
        assert!(text.contains("[3] Button"), "missing [3] Button:\n{text}");
        assert!(
            text.contains("[4] CheckBox"),
            "missing [4] CheckBox:\n{text}"
        );
        // Tree rendering keeps the visible labels for non-secure nodes.
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
        // The placeholder is `••••`.
        assert!(rendered.text.contains("••••"));
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
        // No --window selector means `window_id` stays None. The
        // dispatcher picks the front-most window of the resolved pid
        // later. The pid is what we test here.
        assert_eq!(resolved.pid, 42);
        assert_eq!(resolved.display_name, "notepad");
    }

    #[test]
    fn resolve_target_rejects_unmatched_pid() {
        let apps_list = fake_apps();
        let windows = vec![fake_window(1, 10, "Untitled", "notepad")];
        let q = pid_only_query(999);
        // Per ROADMAP, an unmatched `--app pid:NNN` returns
        // `app_not_found`. This is the only contract here: the dispatcher
        // turns a missing app into a structured `AppNotFound` error before
        // any element work runs, so callers can rely on it.
        let err = resolve_target(&q, &apps_list, &windows).unwrap_err();
        assert!(
            matches!(err, ComputerError::AppNotFound(_)),
            "unexpected error: {err:?}"
        );
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

    #[test]
    fn run_uia_with_timeout_returns_error_when_closure_overruns() {
        // Sleeping longer than the bound must yield a structured
        // `ExecutionFailed` with the standard message. The closure is the
        // generic worker — it has no UIA dependency, so this test runs
        // purely against the wrapper.
        let result: Result<(), ComputerError> = run_uia_with_timeout(1, || {
            std::thread::sleep(Duration::from_millis(1500));
            Ok(())
        });
        let err = result.unwrap_err();
        match err {
            ComputerError::ExecutionFailed(message) => {
                assert!(
                    message.contains("UI Automation call timed out after 1s"),
                    "expected timeout message, got: {message}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn run_uia_with_timeout_returns_closure_result_when_fast_enough() {
        // A closure that finishes within the bound returns its value
        // unchanged.
        let result: Result<u32, ComputerError> = run_uia_with_timeout(2, || Ok(7));
        assert_eq!(result.unwrap(), 7);
    }

    #[test]
    fn run_uia_with_timeout_propagates_closure_errors() {
        // Closure errors are returned verbatim, not wrapped into a
        // timeout.
        let result: Result<(), ComputerError> =
            run_uia_with_timeout(2, || Err(ComputerError::ExecutionFailed("nope".into())));
        match result.unwrap_err() {
            ComputerError::ExecutionFailed(message) => assert_eq!(message, "nope"),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
