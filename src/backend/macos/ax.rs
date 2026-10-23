//! In-process Accessibility tree for the key window.
//!
//! `osascript` is only the fallback when the application element cannot be
//! read for a reason other than permission or a messaging timeout. A crash
//! skips [`shutdown_sessions`], so an Electron flag this process turned on
//! can stay set until the target app quits. `session end` restores it from
//! the session cache. Library callers that never call `shutdown_sessions`
//! have the same leak.

#![allow(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use objc2_app_kit::NSRunningApplication;
use objc2_application_services::{
    AXError, AXIsProcessTrusted, AXObserver, AXUIElement, AXValue, AXValueType,
};
use objc2_core_foundation::{
    kCFRunLoopDefaultMode, CFArray, CFBoolean, CFIndex, CFNumber, CFRange, CFRetained, CFRunLoop,
    CFString, CFType, CGPoint, CGSize, CFURL,
};

use crate::cache::{self, Snapshot};
use crate::element::{self, ElementError, ElementOutcome, ElementRequest, Gate, HitInfo, Step};
use crate::error::ComputerError;
use crate::policy;
use crate::target::{
    bounds_match_frame, filter_windows_for_pid, parse_app_selector, parse_window_selector,
    resolve_app, resolve_window, ActivationPolicy, AppInfo, WindowBounds, WindowInfo, WindowStatus,
};
use crate::tree::{
    self, ElectronEdit, FlagState, NodeFrame, TreeQuery, TreeReport, UiNode, CHILD_CAP, DEPTH_CAP,
    LINE_CAP,
};

use super::apps;

const ELEMENT_TIMEOUT_SECS: f32 = 2.0;
const TREE_BUDGET: Duration = Duration::from_secs(8);
const LIST_BUDGET: Duration = Duration::from_secs(4);
const WINDOW_LIST_CAP: usize = 128;
const PID_PROBE_CAP: usize = 32;
const VALUE_BATCH: isize = 40;

/// How one `AXError` should be treated. Unit tests cover this mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyClass {
    /// The copy returned a value.
    Ok,
    /// The attribute is absent. This is not a failed node.
    Missing,
    /// Accessibility is not trusted (`kAXErrorAPIDisabled`).
    Permission,
    /// The element did not answer (`kAXErrorCannotComplete` after the messaging timeout).
    Timeout,
    /// Any other framework error.
    Failed,
}

/// Classify `error` without calling Accessibility.
pub(crate) fn classify(error: AXError) -> CopyClass {
    if error == AXError::Success {
        CopyClass::Ok
    } else if error == AXError::NoValue || error == AXError::AttributeUnsupported {
        CopyClass::Missing
    } else if error == AXError::APIDisabled {
        CopyClass::Permission
    } else if error == AXError::CannotComplete {
        CopyClass::Timeout
    } else {
        CopyClass::Failed
    }
}

/// Permission text. `subject` is the app the user must enable in Settings.
pub(crate) fn permission_message_for(subject: &str) -> String {
    format!(
        "Accessibility permission is required. System Settings → Privacy & Security → Accessibility → enable {subject}"
    )
}

#[derive(Debug)]
pub(crate) enum WalkError {
    Permission(String),
    Timeout,
    WindowNotFound(String),
    AppNotFound(String),
    Invalid(String),
    Failed {
        app: Option<AppInfo>,
        window_id: u32,
    },
    Message(String),
}

impl WalkError {
    fn failed(app: Option<AppInfo>, window_id: u32) -> Self {
        Self::Failed { app, window_id }
    }
}

pub(crate) enum AttrError {
    Timeout,
    Permission,
    Failed,
    Missing,
}

struct Guard {
    pid: i32,
    session: Option<String>,
    edit: ElectronEdit,
}

struct ObserverState {
    stop: std::sync::Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    session: Option<String>,
}

fn guards() -> &'static Mutex<Vec<Guard>> {
    static GUARDS: OnceLock<Mutex<Vec<Guard>>> = OnceLock::new();
    GUARDS.get_or_init(|| Mutex::new(Vec::new()))
}

fn dirty_set() -> &'static Mutex<HashSet<i32>> {
    static DIRTY: OnceLock<Mutex<HashSet<i32>>> = OnceLock::new();
    DIRTY.get_or_init(|| Mutex::new(HashSet::new()))
}

fn observers() -> &'static Mutex<HashMap<i32, ObserverState>> {
    static OBSERVERS: OnceLock<Mutex<HashMap<i32, ObserverState>>> = OnceLock::new();
    OBSERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn failed_observers() -> &'static Mutex<HashSet<i32>> {
    static FAILED: OnceLock<Mutex<HashSet<i32>>> = OnceLock::new();
    FAILED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Read the key window and render it. Stores a baseline unless `source` is the fallback.
pub(crate) fn tree(query: &TreeQuery) -> Result<TreeReport, ComputerError> {
    match build_tree(query) {
        Ok(report) => Ok(report),
        Err(WalkError::Failed { app, window_id }) => fallback_osascript(app.as_ref(), window_id),
        Err(error) => Err(error.into_computer()),
    }
}

/// The `tree` walk without the session-cache write.
pub(crate) fn observe_tree(query: &TreeQuery) -> Result<TreeReport, ComputerError> {
    match build_observation(query) {
        Ok(report) => Ok(report),
        Err(WalkError::Failed { app, window_id }) => fallback_osascript(app.as_ref(), window_id),
        Err(error) => Err(error.into_computer()),
    }
}

/// Revalidate `request` against the session cache and perform it.
///
/// The built-in deny list runs before any accessibility call on the target.
/// A role, identifier, or frame mismatch returns `stale_element` and does not act.
pub(crate) fn element_action(request: &ElementRequest) -> Result<ElementOutcome, ElementError> {
    match element_action_inner(request) {
        Ok(outcome) => Ok(outcome),
        Err(ElementInterrupt::Error(error)) => Err(error),
        Err(ElementInterrupt::Walk(error)) => Err(ElementError::new(error.into_computer())),
    }
}

/// Role and title at a screen point. Does not click and does not read values.
pub(crate) fn hit_test(x: i32, y: i32) -> Result<Option<HitInfo>, ComputerError> {
    if !trusted() {
        return Err(ComputerError::PermissionRequired(permission_subject()));
    }
    let system = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the system-wide element is process-global. The wrapper returns +1 or panics on null.
        unsafe { AXUIElement::new_system_wide() }
    })) {
        Ok(element) => element,
        Err(_) => {
            return Err(ComputerError::ExecutionFailed(
                "system-wide accessibility element was not created".into(),
            ))
        }
    };
    ignore_timeout(&system);
    let mut slot: *const AXUIElement = std::ptr::null();
    // SAFETY: slot is a stack out-parameter. On success it holds a +1 AXUIElement, which from_raw owns.
    let error = unsafe {
        system.copy_element_at_position(
            x as core::ffi::c_float,
            y as core::ffi::c_float,
            NonNull::new(&mut slot).expect("slot"),
        )
    };
    match classify(error) {
        CopyClass::Missing => Ok(None),
        CopyClass::Permission => Err(ComputerError::PermissionRequired(permission_subject())),
        CopyClass::Timeout => Err(ComputerError::ExecutionFailed("messaging timeout".into())),
        CopyClass::Failed => Err(ComputerError::ExecutionFailed(format!(
            "hit test failed ({error:?})"
        ))),
        CopyClass::Ok => {
            let Some(ptr) = NonNull::new(slot.cast_mut()) else {
                return Ok(None);
            };
            // SAFETY: Success returned a +1 element. from_raw takes that ownership.
            let element = unsafe { CFRetained::<AXUIElement>::from_raw(ptr) };
            ignore_timeout(&element);
            let role = copy_string(&element, "AXRole")
                .ok()
                .flatten()
                .unwrap_or_else(|| "unknown".into());
            let title = copy_string(&element, "AXTitle").ok().flatten();
            Ok(Some(HitInfo { role, title }))
        }
    }
}

enum ElementInterrupt {
    Error(ElementError),
    Walk(WalkError),
}

impl From<WalkError> for ElementInterrupt {
    fn from(error: WalkError) -> Self {
        Self::Walk(error)
    }
}

impl From<ElementError> for ElementInterrupt {
    fn from(error: ElementError) -> Self {
        Self::Error(error)
    }
}

impl From<ComputerError> for ElementInterrupt {
    fn from(error: ComputerError) -> Self {
        Self::Error(ElementError::new(error))
    }
}

fn element_action_inner(request: &ElementRequest) -> Result<ElementOutcome, ElementInterrupt> {
    let query = tree::TreeQuery {
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
    let app = resolve_target(&query)?;
    let title = policy_window_title(&query, app.pid)?;
    policy::deny_or_allow(policy::identity_denial(
        app.bundle_id.as_deref(),
        app.path.as_deref().map(Path::new),
        title.as_deref(),
        None,
    ))?;
    ensure_trusted()?;
    let live = read_live(&query, true)?;
    let snapshot = cache::load(
        query.session.as_deref(),
        app.bundle_id.as_deref(),
        live.app.pid,
        live.window_id,
    )?;
    let Some(snapshot) = snapshot else {
        let label = request.app.as_deref().unwrap_or(app.name.as_str());
        return Err(ComputerError::ElementNotFound(format!(
            "element {} is not in the session cache; {}",
            request.index,
            element::refresh_hint(label)
        ))
        .into());
    };
    let identity = snapshot.observed.as_ref().unwrap_or(&snapshot.root);
    let cached_revision = snapshot.observed_revision.unwrap_or(snapshot.revision);
    let label = request.app.as_deref().unwrap_or(app.name.as_str());
    let gate = element::gate(&element::GateInput {
        op: &request.op,
        index: request.index,
        require_visible: request.require_visible,
        app_label: label,
        cached: identity,
        cached_revision,
        cached_pid: snapshot.pid,
        cached_window: snapshot.window_id,
        resolved_pid: live.app.pid,
        resolved_window: live.window_id,
        live: &live.root,
        live_pid: live.app.pid,
        live_window: live.window_id,
    })?;
    let element = live.nodes.get(&gate.path).ok_or_else(|| {
        ComputerError::ExecutionFailed(
            "element path did not resolve to an accessibility element".into(),
        )
    })?;
    let settable = if gate.needs_settable {
        value_is_settable(element)?
    } else {
        false
    };
    let char_count = if gate.needs_chars {
        character_count(element)?
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
        match perform_named(element, "AXScrollToVisible") {
            Ok(()) => {}
            Err(error) if request.require_visible => return Err(error.into()),
            Err(error) => warnings.push(format!(
                "AXScrollToVisible failed: {}",
                error.into_computer()
            )),
        }
    }
    let via = run_steps(element, &planned, &gate, live.app.pid, request)?;
    // A failed re-read must not turn a completed action into an error.
    let mut revision = cached_revision;
    match read_live(&query, false) {
        Ok(refreshed) => {
            let rendered = tree::render(&refreshed.root);
            revision = tree::revision_of(&rendered.root);
            let mut updated = snapshot;
            updated.observed = Some(rendered.root);
            updated.observed_revision = Some(revision);
            if let Err(error) = cache::store(query.session.as_deref(), &updated) {
                warnings.push(format!("session cache was not updated: {error}"));
            }
        }
        Err(error) => warnings.push(format!(
            "tree was not re-read after the action: {}",
            error.into_computer()
        )),
    }
    Ok(element::outcome(request, revision, warnings, via))
}

fn run_steps(
    element: &AXUIElement,
    planned: &[Step],
    gate: &Gate,
    pid: i32,
    request: &ElementRequest,
) -> Result<Option<String>, ElementInterrupt> {
    let _ = request.index;
    let mut via = None;
    for step in planned {
        match step {
            Step::Press { count } => {
                element::require_action(&gate.actions, "AXPress")?;
                for _ in 0..*count {
                    perform_named(element, "AXPress")?;
                }
            }
            Step::Focus => set_focused(element)?,
            Step::SetValue { value } | Step::TypeSetValue { text: value } => {
                set_string(element, "AXValue", value)?;
                if matches!(step, Step::TypeSetValue { .. }) {
                    via = Some("set_value".into());
                }
            }
            Step::SelectText { start, end } => set_range(element, *start, *end)?,
            Step::Perform { name } => {
                element::require_action(&gate.actions, name)?;
                perform_named(element, name)?;
            }
            Step::ScrollTo => {
                element::require_action(&gate.actions, "AXScrollToVisible")?;
                perform_named(element, "AXScrollToVisible")?;
            }
            Step::TypeEnigo { text } => {
                set_focused(element)?;
                super::input::post_text_to_pid(pid, text)?;
                via = Some("event".into());
            }
            Step::CenterClick {
                x,
                y,
                button,
                count,
            } => {
                #[cfg(test)]
                {
                    let _ = (x, y, button, count, request);
                    return Err(ComputerError::ExecutionFailed(
                        "event posting is disabled in tests".into(),
                    )
                    .into());
                }
                #[cfg(not(test))]
                {
                    let backend = super::MacOsDesktopBackend;
                    let report = crate::attempt::run(
                        &backend,
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
                }
            }
        }
    }
    Ok(via)
}

fn policy_window_title(query: &tree::TreeQuery, pid: i32) -> Result<Option<String>, WalkError> {
    let windows = match apps::on_screen_windows() {
        Ok(windows) => windows,
        Err(_) => return Ok(None),
    };
    if let Some(selector) = query.window.as_deref() {
        return Ok(Some(window_hint(&windows, pid, selector)?.title));
    }
    Ok(crate::target::filter_windows_for_pid(&windows, pid)
        .first()
        .map(|window| window.title.clone()))
}

fn value_is_settable(element: &AXUIElement) -> Result<bool, WalkError> {
    let attribute = CFString::from_str("AXValue");
    let mut settable: u8 = 0;
    // SAFETY: settable is a stack Boolean out-parameter. element and the attribute name are live.
    let error = unsafe {
        element.is_attribute_settable(&attribute, NonNull::new(&mut settable).expect("settable"))
    };
    match classify(error) {
        CopyClass::Ok => Ok(settable != 0),
        CopyClass::Missing => Ok(false),
        CopyClass::Permission => Err(WalkError::Permission(permission_subject())),
        CopyClass::Timeout => Err(WalkError::Timeout),
        CopyClass::Failed => Err(WalkError::Message(
            "AXUIElementIsAttributeSettable failed for AXValue".into(),
        )),
    }
}

fn character_count(element: &AXUIElement) -> Result<Option<i64>, WalkError> {
    match copy_cf(element, "AXNumberOfCharacters") {
        Ok(Some(value)) => Ok(value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i64)),
        Ok(None) => Ok(None),
        Err(AttrError::Permission) => Err(WalkError::Permission(permission_subject())),
        Err(AttrError::Timeout) => Err(WalkError::Timeout),
        Err(AttrError::Failed | AttrError::Missing) => Ok(None),
    }
}

fn set_string(element: &AXUIElement, name: &str, value: &str) -> Result<(), WalkError> {
    let text = CFRetained::<CFType>::from(CFString::from_str(value));
    set_cf(element, name, &text)
}

fn set_focused(element: &AXUIElement) -> Result<(), WalkError> {
    let boolean = CFBoolean::new(true);
    // SAFETY: CFBoolean is a CFType. The static lives for the process. element is live.
    let value = unsafe { &*(boolean as *const CFBoolean).cast::<CFType>() };
    set_cf(element, "AXFocused", value)
}

fn set_cf(element: &AXUIElement, name: &str, value: &CFType) -> Result<(), WalkError> {
    let attribute = CFString::from_str(name);
    // SAFETY: element is live. attribute is owned for this call. value stays retained until the call returns.
    let error = unsafe { element.set_attribute_value(&attribute, value) };
    classify_set(error)
}

fn set_range(element: &AXUIElement, start: i64, end: i64) -> Result<(), WalkError> {
    let mut range = CFRange {
        location: CFIndex::try_from(start).unwrap_or(CFIndex::MAX),
        length: CFIndex::try_from(end.saturating_sub(start)).unwrap_or(0),
    };
    let Some(value_ptr) = NonNull::new((&mut range as *mut CFRange).cast::<c_void>()) else {
        return Err(WalkError::Message(
            "AXSelectedTextRange could not be encoded".into(),
        ));
    };
    // SAFETY: range is a stack CFRange that outlives AXValueCreate. The wrapper returns +1 or None.
    let ax_value = unsafe { AXValue::new(AXValueType::CFRange, value_ptr) }
        .ok_or_else(|| WalkError::Message("AXValueCreate failed for AXSelectedTextRange".into()))?;
    let typed = CFRetained::<CFType>::from(ax_value);
    set_cf(element, "AXSelectedTextRange", &typed)
}

fn classify_set(error: AXError) -> Result<(), WalkError> {
    match classify(error) {
        CopyClass::Ok => Ok(()),
        CopyClass::Permission => Err(WalkError::Permission(permission_subject())),
        CopyClass::Timeout => Err(WalkError::Timeout),
        CopyClass::Missing | CopyClass::Failed => Err(WalkError::Message(format!(
            "set attribute failed ({error:?})"
        ))),
    }
}

fn perform_named(element: &AXUIElement, name: &str) -> Result<(), WalkError> {
    let action = CFString::from_str(name);
    // SAFETY: element is live. action is a CFString we own for this call. PerformAction does not free it.
    let error = unsafe { element.perform_action(&action) };
    match classify(error) {
        CopyClass::Ok => Ok(()),
        CopyClass::Permission => Err(WalkError::Permission(permission_subject())),
        CopyClass::Timeout => Err(WalkError::Timeout),
        CopyClass::Missing | CopyClass::Failed => Err(WalkError::Message(format!(
            "AXUIElementPerformAction {name} failed ({error:?})"
        ))),
    }
}

/// Restore flags recorded for `session` and stop that session's observers.
pub(crate) fn end_session(session: Option<&str>) -> Result<Vec<i32>, ComputerError> {
    restore_session(session, true)
}

/// Process-exit restore. Best effort: a failure is not surfaced to the CLI.
pub(crate) fn shutdown_sessions() {
    let sessions: Vec<Option<String>> = {
        let guards = lock(guards());
        guards
            .iter()
            .map(|guard| guard.session.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    };
    for session in sessions {
        let _ = restore_session(session.as_deref(), false);
    }
    stop_observers(|_, _| true);
}

/// Set `ax_matched` when an AX window frame is within 2 points of the CG bounds.
///
/// A missing Accessibility grant leaves every flag false and returns a note.
/// This does not fail the window list.
pub(crate) fn annotate_matches(windows: &mut [WindowInfo]) -> Option<String> {
    if !trusted() {
        return Some("ax_matched is false; Accessibility permission is off".into());
    }
    let deadline = Instant::now() + LIST_BUDGET;
    let mut pids = Vec::new();
    for window in windows.iter() {
        if pids.len() >= PID_PROBE_CAP {
            break;
        }
        if !pids.contains(&window.pid) {
            pids.push(window.pid);
        }
    }
    let mut any = false;
    let mut failed = false;
    for pid in pids {
        if Instant::now() >= deadline {
            failed = true;
            break;
        }
        match window_frames(pid, deadline) {
            Ok(frames) => {
                for window in windows.iter_mut().filter(|window| window.pid == pid) {
                    if frames
                        .iter()
                        .any(|frame| bounds_match_frame(window.bounds, *frame))
                    {
                        window.ax_matched = true;
                        any = true;
                    }
                }
            }
            Err(_) => failed = true,
        }
    }
    if !any && failed {
        Some("ax_matched is incomplete; some accessibility windows could not be read".into())
    } else if !any {
        Some("ax_matched is false; no accessibility window matched these bounds".into())
    } else if failed {
        Some("ax_matched is partial; some processes did not answer".into())
    } else {
        None
    }
}

fn build_tree(query: &TreeQuery) -> Result<TreeReport, WalkError> {
    let live = read_live(query, false)?;
    finish_report(query, &live, true)
}

/// Walk the tree without touching the session baseline. The wait engine polls
/// this so a `--text`/`--tree-change` loop neither consumes the caller's diff
/// baseline nor makes `--tree-change` compare against its own previous poll.
fn build_observation(query: &TreeQuery) -> Result<TreeReport, WalkError> {
    let live = read_live(query, false)?;
    finish_report(query, &live, false)
}

/// Walk the tree a second time and return just the root `UiNode` plus the
/// resolved app for set-of-marks rendering. P11 keeps this separate from
/// the public `tree` because that one returns a `TreeReport` for the agent;
/// marks need the framed `UiNode` tree so we can place numbers on the
/// screenshot.
pub(crate) fn root_node(query: &TreeQuery) -> Result<UiNode, ComputerError> {
    let live = read_live(query, false).map_err(|error| error.into_computer())?;
    Ok(live.root)
}

/// Image-pixel rectangles of every secure element in the latest tree. The
/// OCR dispatcher drops blocks whose rect is inside one of these.
pub(crate) fn secure_frames(query: &TreeQuery) -> Result<Vec<NodeFrame>, ComputerError> {
    let root = root_node(query)?;
    Ok(secure_frames_in(&root))
}

fn secure_frames_in(root: &UiNode) -> Vec<NodeFrame> {
    let mut frames = Vec::new();
    walk_secure(root, &mut frames);
    frames
}

fn walk_secure(node: &UiNode, frames: &mut Vec<NodeFrame>) {
    if crate::tree::node_is_secure(node) {
        if let Some(frame) = node.frame {
            frames.push(frame);
        }
    }
    for child in &node.children {
        walk_secure(child, frames);
    }
}

/// Walk the tree once and return one MarkBox per node with a frame.
///
/// The map is used to convert the node's screen-frame into image pixels. The
/// numbering and the walk budget live in [`crate::vision::marks_for_nodes`], so
/// the numbers drawn here are the numbers `tree` prints.
pub(crate) fn marks_for_tree(
    query: &TreeQuery,
    map: &crate::image_util::ScreenshotMap,
) -> Result<Vec<crate::vision::MarkBox>, ComputerError> {
    let root = root_node(query)?;
    Ok(crate::vision::marks_for_nodes(&root, map))
}

fn read_live(query: &TreeQuery, retain_nodes: bool) -> Result<LiveWindow, WalkError> {
    ensure_trusted()?;
    let app = resolve_target(query)?;
    let deadline = Instant::now() + TREE_BUDGET;
    // The observer only marks the pid dirty. P2 still re-fetches every call.
    ensure_observer(app.pid, query.session.as_deref());
    let _ = take_dirty(app.pid);

    let app_element = application(app.pid).map_err(|_| WalkError::failed(Some(app.clone()), 0))?;
    ignore_timeout(&app_element);
    let mut edits = Vec::new();
    if let Some(edit) = enable_electron(app.pid, query.session.as_deref(), &app_element) {
        edits.push((app.pid, edit));
    }

    let cg = cg_windows(query.window.is_some())?;
    let hint = match query.window.as_deref() {
        Some(selector) => Some(window_hint(&cg, app.pid, selector)?),
        None => None,
    };
    let hint_id = hint.as_ref().map(|window| window.id).unwrap_or(0);

    let ax_windows = match copy_windows(&app_element, deadline) {
        Ok(windows) => windows,
        Err(AttrError::Timeout) => return Err(WalkError::Timeout),
        Err(AttrError::Permission) => return Err(WalkError::Permission(permission_subject())),
        Err(AttrError::Failed) | Err(AttrError::Missing) => {
            return Err(WalkError::failed(Some(app), hint_id))
        }
    };
    if ax_windows.is_empty() {
        return Err(WalkError::WindowNotFound(format!(
            "{} has no accessibility window",
            app.name
        )));
    }
    let chosen = select_window(ax_windows, hint.as_ref())?;
    let window_pid = element_pid(&chosen.element).unwrap_or(app.pid);
    if window_pid != app.pid {
        ensure_observer(window_pid, query.session.as_deref());
        if let Ok(element) = application(window_pid) {
            ignore_timeout(&element);
            if let Some(edit) = enable_electron(window_pid, query.session.as_deref(), &element) {
                edits.push((window_pid, edit));
            }
        }
    }

    let mut ctx = WalkCtx {
        deadline,
        window_pid,
        foreign: Vec::new(),
        visited: HashSet::from([app.pid, window_pid]),
        remaining: LINE_CAP,
        retain_nodes,
        nodes: HashMap::new(),
    };
    let mut path = Vec::new();
    let mut root = walk_element(&chosen.element, &mut ctx, 0, &mut path)?;
    graft_hosted(
        &mut root,
        &app,
        query.session.as_deref(),
        &mut ctx,
        &mut edits,
        &mut path,
    )?;
    if root.role == "unavailable" && root.children.is_empty() {
        if Instant::now() >= deadline {
            return Err(WalkError::Timeout);
        }
        return Err(WalkError::failed(Some(app), hint_id));
    }

    let window_id = if hint_id != 0 {
        hint_id
    } else {
        match_cg_window(&cg, window_pid, root.frame).unwrap_or(0)
    };
    Ok(LiveWindow {
        app,
        window_id,
        root,
        edits,
        nodes: ctx.nodes,
    })
}

fn finish_report(
    query: &TreeQuery,
    live: &LiveWindow,
    store: bool,
) -> Result<TreeReport, WalkError> {
    let app = &live.app;
    let window_id = live.window_id;
    let rendered = tree::render(&live.root);
    let revision = tree::revision_of(&rendered.root);
    let previous = cache::load(
        query.session.as_deref(),
        app.bundle_id.as_deref(),
        app.pid,
        window_id,
    )
    .ok()
    .flatten();
    let previous_root = previous.as_ref().map(|snapshot| &snapshot.root);
    let diff = previous_root.map(|previous| tree::diff_trees(previous, &rendered.root));
    let text = tree::choose_text(previous_root, &rendered, query.full);
    let app_label = app.bundle_id.clone().or_else(|| Some(app.name.clone()));
    let mut snapshot = Snapshot {
        revision,
        tree: rendered.text.clone(),
        root: rendered.root.clone(),
        truncated: rendered.truncated,
        pid: app.pid,
        window_id,
        bundle_id: app.bundle_id.clone(),
        electron: live
            .edits
            .iter()
            .find(|(pid, _)| *pid == app.pid)
            .map(|(_, edit)| edit.clone()),
        observed: Some(rendered.root),
        observed_revision: Some(revision),
    };
    let mut note = None;
    if store {
        if let Err(error) = cache::store(query.session.as_deref(), &snapshot) {
            note = Some(format!("session cache was not written: {error}"));
        }
        for (pid, edit) in &live.edits {
            if *pid == app.pid {
                continue;
            }
            snapshot.pid = *pid;
            snapshot.electron = Some(edit.clone());
            let _ = cache::store(query.session.as_deref(), &snapshot);
        }
    }
    Ok(TreeReport {
        pid: app.pid,
        window_id,
        revision,
        source: "ax".into(),
        truncated: rendered.truncated,
        tree: text,
        full_text: rendered.text,
        diff,
        app: app_label,
        note,
        legacy: None,
    })
}

fn fallback_osascript(app: Option<&AppInfo>, window_id: u32) -> Result<TreeReport, ComputerError> {
    let text = super::inspect_accessibility(app.map(|app| app.name.as_str()))?;
    Ok(TreeReport {
        pid: app.map(|app| app.pid).unwrap_or(0),
        window_id,
        revision: 0,
        source: "osascript".into(),
        truncated: false,
        tree: String::new(),
        full_text: String::new(),
        diff: None,
        app: app.and_then(|app| app.bundle_id.clone().or_else(|| Some(app.name.clone()))),
        note: Some("source=osascript".into()),
        legacy: Some(text),
    })
}

fn restore_session(session: Option<&str>, strict: bool) -> Result<Vec<i32>, ComputerError> {
    let mut edits: HashMap<i32, ElectronEdit> = HashMap::new();
    {
        let mut guards = lock(guards());
        let mut kept = Vec::new();
        for guard in guards.drain(..) {
            if session_matches(guard.session.as_deref(), session) {
                edits.insert(guard.pid, guard.edit);
            } else {
                kept.push(guard);
            }
        }
        *guards = kept;
    }
    if let Ok(pending) = cache::pending_restores(session) {
        for (pid, edit) in pending {
            edits.entry(pid).or_insert(edit);
        }
    }
    if edits.is_empty() {
        stop_observers(|_, state| session_matches(state.session.as_deref(), session));
        return Ok(Vec::new());
    }
    let mut restored = Vec::new();
    let mut blocked_permission = false;
    let mut blocked_other = false;
    for (pid, edit) in &edits {
        if !process_alive(*pid) {
            restored.push(*pid);
            continue;
        }
        if !trusted() {
            blocked_permission = true;
            remember_guard(*pid, session, edit.clone());
            continue;
        }
        match restore_pid(*pid, edit) {
            Ok(()) => restored.push(*pid),
            Err(WalkError::Permission(_)) => {
                blocked_permission = true;
                remember_guard(*pid, session, edit.clone());
            }
            Err(_) if !process_alive(*pid) => restored.push(*pid),
            Err(_) => {
                remember_guard(*pid, session, edit.clone());
                if strict {
                    blocked_other = true;
                }
            }
        }
    }
    if !restored.is_empty() {
        let _ = cache::clear_restores(session, &restored);
    }
    stop_observers(|pid, state| {
        restored.contains(&pid) && session_matches(state.session.as_deref(), session)
    });
    if strict && blocked_permission {
        return Err(ComputerError::PermissionRequired(permission_subject()));
    }
    if strict && blocked_other {
        return Err(ComputerError::ExecutionFailed(
            "accessibility flags could not be restored".into(),
        ));
    }
    restored.sort_unstable();
    restored.dedup();
    Ok(restored)
}

fn restore_pid(pid: i32, edit: &ElectronEdit) -> Result<(), WalkError> {
    let element = application(pid).map_err(|_| WalkError::failed(None, 0))?;
    ignore_timeout(&element);
    let flags = tree::electron_restore(edit);
    if let Some(value) = flags.manual {
        set_bool(&element, "AXManualAccessibility", value)?;
    }
    if let Some(value) = flags.enhanced {
        set_bool(&element, "AXEnhancedUserInterface", value)?;
    }
    Ok(())
}

fn remember_guard(pid: i32, session: Option<&str>, edit: ElectronEdit) {
    if edit.restore.manual.is_none() && edit.restore.enhanced.is_none() {
        return;
    }
    let mut guards = lock(guards());
    if let Some(existing) = guards.iter_mut().find(|guard| guard.pid == pid) {
        existing.edit = edit;
        existing.session = session.map(str::to_string);
        return;
    }
    guards.push(Guard {
        pid,
        session: session.map(str::to_string),
        edit,
    });
}

fn session_matches(stored: Option<&str>, requested: Option<&str>) -> bool {
    stored == requested
}

fn process_alive(pid: i32) -> bool {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .is_some_and(|app| !app.isTerminated())
}

struct WalkCtx {
    deadline: Instant,
    window_pid: i32,
    foreign: Vec<i32>,
    visited: HashSet<i32>,
    remaining: usize,
    /// Keep an element for every numbered path. Element actions set this.
    retain_nodes: bool,
    nodes: HashMap<Vec<usize>, CFRetained<AXUIElement>>,
}

struct LiveWindow {
    app: AppInfo,
    window_id: u32,
    root: UiNode,
    edits: Vec<(i32, ElectronEdit)>,
    nodes: HashMap<Vec<usize>, CFRetained<AXUIElement>>,
}

struct AxWin {
    element: CFRetained<AXUIElement>,
    title: String,
    frame: Option<NodeFrame>,
    main: bool,
}

fn walk_element(
    element: &AXUIElement,
    ctx: &mut WalkCtx,
    depth: usize,
    path: &mut Vec<usize>,
) -> Result<UiNode, WalkError> {
    if Instant::now() >= ctx.deadline {
        if depth == 0 {
            return Err(WalkError::Timeout);
        }
        return Ok(unavailable());
    }
    if ctx.remaining == 0 {
        return Ok(unavailable());
    }
    ctx.remaining -= 1;
    ignore_timeout(element);

    let role = match copy_string(element, "AXRole") {
        Ok(Some(role)) if !role.is_empty() => role,
        Ok(_) => "unavailable".into(),
        Err(AttrError::Timeout) if depth == 0 => return Err(WalkError::Timeout),
        Err(AttrError::Permission) => return Err(WalkError::Permission(permission_subject())),
        Err(AttrError::Timeout) | Err(AttrError::Failed) | Err(AttrError::Missing) => {
            "unavailable".into()
        }
    };
    if role == "unavailable" {
        return Ok(unavailable());
    }
    let subrole = copy_string(element, "AXSubrole").ok().flatten();
    let secure = role == "AXSecureTextField"
        || subrole
            .as_deref()
            .is_some_and(|subrole| subrole.contains("Secure"));
    touch_listed(element, ctx.deadline);
    let title = copy_string(element, "AXTitle").ok().flatten();
    let description = copy_string(element, "AXDescription").ok().flatten();
    let identifier = copy_string(element, "AXIdentifier").ok().flatten();
    let value = if secure { None } else { display_value(element) };
    let enabled = copy_bool(element, "AXEnabled")
        .ok()
        .flatten()
        .unwrap_or(true);
    let frame = copy_frame(element);
    let actions = copy_actions(element);
    let (children, child_count) = if depth >= DEPTH_CAP || ctx.remaining == 0 {
        let count = child_total(element).unwrap_or(0);
        (Vec::new(), count)
    } else {
        take_children(element, ctx, depth, path)?
    };
    if ctx.retain_nodes {
        ctx.nodes.insert(path.clone(), retain_ax(element));
    }
    Ok(UiNode {
        role,
        subrole,
        title,
        description,
        identifier,
        value,
        enabled,
        secure,
        actions,
        frame,
        child_count: Some(child_count),
        children,
    })
}

fn take_children(
    element: &AXUIElement,
    ctx: &mut WalkCtx,
    depth: usize,
    path: &mut Vec<usize>,
) -> Result<(Vec<UiNode>, usize), WalkError> {
    let (elements, total) = match copy_child_elements(element, CHILD_CAP, ctx.deadline) {
        Ok(value) => value,
        Err(AttrError::Permission) => return Err(WalkError::Permission(permission_subject())),
        Err(AttrError::Timeout) => {
            let count = child_total(element).unwrap_or(0);
            return Ok((Vec::new(), count));
        }
        Err(_) => return Ok((Vec::new(), 0)),
    };
    let copied = elements.len();
    let mut handled = 0usize;
    let mut children = Vec::new();
    for child in elements {
        if ctx.remaining == 0 || Instant::now() >= ctx.deadline {
            break;
        }
        handled += 1;
        let child_pid = element_pid(&child).unwrap_or(ctx.window_pid);
        if depth == 0 && child_pid != ctx.window_pid {
            if !ctx.foreign.contains(&child_pid) {
                ctx.foreign.push(child_pid);
            }
            continue;
        }
        let child_index = children.len();
        path.push(child_index);
        let walked = walk_element(&child, ctx, depth + 1, path);
        path.pop();
        match walked {
            Ok(node) => children.push(node),
            Err(WalkError::Permission(message)) => return Err(WalkError::Permission(message)),
            Err(_) => children.push(unavailable()),
        }
    }
    // A partial copy or an early line-budget stop keeps the AX total so the
    // renderer can set truncated. A finished list under the cap uses the nodes
    // actually attached; graft_hosted raises that after helper windows land.
    let finished = handled == copied && copied >= total.min(CHILD_CAP);
    let reported = if finished && total <= CHILD_CAP {
        children.len()
    } else {
        total.max(children.len())
    };
    Ok((children, reported))
}

fn graft_hosted(
    root: &mut UiNode,
    app: &AppInfo,
    session: Option<&str>,
    ctx: &mut WalkCtx,
    edits: &mut Vec<(i32, ElectronEdit)>,
    path: &mut Vec<usize>,
) -> Result<(), WalkError> {
    let needs = root.children.is_empty() || !ctx.foreign.is_empty();
    if !needs {
        return Ok(());
    }
    let scanned = if ctx.foreign.is_empty() {
        scanned_helper_pids(app)
    } else {
        Vec::new()
    };
    let helpers = tree::hosted_helper_pids(app.pid, &ctx.foreign, &scanned);
    for pid in helpers {
        if !ctx.visited.insert(pid) {
            continue;
        }
        if Instant::now() >= ctx.deadline {
            root.children.push(unavailable());
            continue;
        }
        ensure_observer(pid, session);
        if let Ok(element) = application(pid) {
            ignore_timeout(&element);
            if let Some(edit) = enable_electron(pid, session, &element) {
                edits.push((pid, edit));
            }
        }
        let child_index = root.children.len();
        path.push(child_index);
        let walked = helper_root(pid, ctx, path);
        path.pop();
        match walked {
            Ok(node) => root.children.push(node),
            Err(WalkError::Permission(message)) => return Err(WalkError::Permission(message)),
            Err(WalkError::Timeout) => return Err(WalkError::Timeout),
            Err(_) => root.children.push(unavailable()),
        }
    }
    let current = root.child_count.unwrap_or(0);
    if current <= CHILD_CAP {
        root.child_count = Some(current.max(root.children.len()));
    }
    Ok(())
}

fn helper_root(pid: i32, ctx: &mut WalkCtx, path: &mut Vec<usize>) -> Result<UiNode, WalkError> {
    let element = application(pid)?;
    ignore_timeout(&element);
    let windows = copy_windows(&element, ctx.deadline).map_err(|error| match error {
        AttrError::Timeout => WalkError::Timeout,
        AttrError::Permission => WalkError::Permission(permission_subject()),
        AttrError::Failed | AttrError::Missing => WalkError::failed(None, 0),
    })?;
    let Some(chosen) = select_window(windows, None).ok() else {
        return Ok(unavailable());
    };
    walk_element(&chosen.element, ctx, 1, path)
}

fn retain_ax(element: &AXUIElement) -> CFRetained<AXUIElement> {
    // SAFETY: element is a live AXUIElement for this call. retain() adds +1, which CFRetained owns.
    unsafe { CFRetained::retain(NonNull::from(element)) }
}

fn scanned_helper_pids(app: &AppInfo) -> Vec<i32> {
    let Ok(running) = apps::list_apps(true) else {
        return Vec::new();
    };
    running
        .into_iter()
        .filter_map(|candidate| {
            let bundle = candidate.bundle_id.as_deref()?;
            if !tree::bundle_matches_parent(&app.name, app.bundle_id.as_deref(), bundle) {
                return None;
            }
            if apps::parent_process_id(candidate.pid) != Some(app.pid) {
                return None;
            }
            Some(candidate.pid)
        })
        .collect()
}

fn select_window(mut windows: Vec<AxWin>, hint: Option<&WindowInfo>) -> Result<AxWin, WalkError> {
    if windows.is_empty() {
        return Err(WalkError::WindowNotFound(
            "accessibility window list was empty".into(),
        ));
    }
    if let Some(hint) = hint {
        if let Some(index) = windows.iter().position(|window| {
            window
                .frame
                .is_some_and(|frame| bounds_match_frame(hint.bounds, frame))
        }) {
            return Ok(windows.swap_remove(index));
        }
        if !hint.title.is_empty() {
            if let Some(index) = windows
                .iter()
                .position(|window| window.title.eq_ignore_ascii_case(&hint.title))
            {
                return Ok(windows.swap_remove(index));
            }
        }
        return Err(WalkError::WindowNotFound(format!(
            "no accessibility window matched id {}",
            hint.id
        )));
    }
    if let Some(index) = windows.iter().position(|window| window.main) {
        return Ok(windows.swap_remove(index));
    }
    Ok(windows.swap_remove(0))
}

fn copy_windows(element: &AXUIElement, deadline: Instant) -> Result<Vec<AxWin>, AttrError> {
    let (elements, _) = copy_elements(element, "AXWindows", WINDOW_LIST_CAP, deadline)?;
    let mut windows = Vec::new();
    for element in elements {
        if Instant::now() >= deadline {
            break;
        }
        ignore_timeout(&element);
        let title = copy_string(&element, "AXTitle")
            .ok()
            .flatten()
            .unwrap_or_default();
        let frame = copy_frame(&element);
        let main = copy_bool(&element, "AXMain")
            .ok()
            .flatten()
            .unwrap_or(false);
        windows.push(AxWin {
            element,
            title,
            frame,
            main,
        });
    }
    Ok(windows)
}

fn window_frames(pid: i32, deadline: Instant) -> Result<Vec<NodeFrame>, AttrError> {
    let element = application(pid).map_err(|_| AttrError::Failed)?;
    ignore_timeout(&element);
    let windows = copy_windows(&element, deadline)?;
    Ok(windows
        .into_iter()
        .filter_map(|window| window.frame)
        .collect())
}

fn enable_electron(pid: i32, session: Option<&str>, element: &AXUIElement) -> Option<ElectronEdit> {
    if lock(guards()).iter().any(|guard| guard.pid == pid) {
        return None;
    }
    let manual = match copy_bool(element, "AXManualAccessibility") {
        Ok(value) => value,
        Err(_) => return None,
    };
    let enhanced = match copy_bool(element, "AXEnhancedUserInterface") {
        Ok(value) => value,
        Err(_) => return None,
    };
    let mut edit = tree::electron_enable(FlagState { manual, enhanced });
    if let Some(true) = edit.set_manual {
        if set_bool(element, "AXManualAccessibility", true).is_err() {
            edit.set_manual = None;
            edit.restore.manual = None;
        }
    }
    if let Some(true) = edit.set_enhanced {
        if set_bool(element, "AXEnhancedUserInterface", true).is_err() {
            edit.set_enhanced = None;
            edit.restore.enhanced = None;
        }
    }
    if edit.restore.manual.is_none() && edit.restore.enhanced.is_none() {
        return None;
    }
    remember_guard(pid, session, edit.clone());
    Some(edit)
}

fn resolve_target(query: &TreeQuery) -> Result<AppInfo, WalkError> {
    let apps = apps::list_apps(true).map_err(|_| WalkError::failed(None, 0))?;
    if let Some(selector) = query.app.as_deref() {
        let parsed = parse_app_selector(selector).map_err(WalkError::from_computer)?;
        let info = resolve_app(&apps, &parsed).map_err(WalkError::from_computer)?;
        return Ok(info.clone());
    }
    apps.iter()
        .find(|app| app.active && app.activation_policy == ActivationPolicy::Regular)
        .or_else(|| apps.iter().find(|app| app.active))
        .cloned()
        .ok_or_else(|| WalkError::AppNotFound("frontmost app".into()))
}

fn cg_windows(required: bool) -> Result<Vec<WindowInfo>, WalkError> {
    match apps::on_screen_windows() {
        Ok(windows) => Ok(windows),
        Err(error) if required => Err(WalkError::from_computer(error)),
        Err(_) => Ok(Vec::new()),
    }
}

fn window_hint(windows: &[WindowInfo], pid: i32, selector: &str) -> Result<WindowInfo, WalkError> {
    let parsed = parse_window_selector(selector).map_err(WalkError::from_computer)?;
    let owned = filter_windows_for_pid(windows, pid);
    resolve_window(&owned, &parsed)
        .cloned()
        .map_err(WalkError::from_computer)
}

fn match_cg_window(windows: &[WindowInfo], pid: i32, frame: Option<NodeFrame>) -> Option<u32> {
    let frame = frame?;
    windows.iter().find_map(|window| {
        (window.pid == pid && bounds_match_frame(window.bounds, frame)).then_some(window.id)
    })
}

fn trusted() -> bool {
    // SAFETY: AXIsProcessTrusted reads the TCC bit for this process and does not prompt.
    unsafe { AXIsProcessTrusted() }
}

/// `AXURL` of the main browser window, or a URL in that window's title.
pub(crate) fn browser_url(pid: i32) -> Result<Option<String>, ComputerError> {
    if !trusted() {
        return Err(ComputerError::PermissionRequired(permission_subject()));
    }
    let app = application(pid).map_err(|error| error.into_computer())?;
    ignore_timeout(&app);
    let deadline = Instant::now() + Duration::from_secs(2);
    let windows = match copy_windows(&app, deadline) {
        Ok(windows) => windows,
        Err(_) => return Ok(None),
    };
    let Some(window) = windows
        .iter()
        .find(|window| window.main)
        .or_else(|| windows.first())
    else {
        return Ok(None);
    };
    if let Ok(Some(url)) = copy_string(&window.element, "AXURL") {
        if policy::url_in_text(&url).is_some() || url.contains("://") {
            return Ok(Some(url));
        }
    }
    Ok(policy::url_in_text(&window.title).map(str::to_string))
}

/// Title and secure bit from the session cache. Does not walk a live tree.
pub(crate) fn peek_element(
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
    let Ok(resolved) = resolve_target(&query) else {
        return Ok(None);
    };
    let Ok(windows) = apps::on_screen_windows() else {
        return Ok(None);
    };
    let owned = filter_windows_for_pid(&windows, resolved.pid);
    let window_id = if let Some(selector) = window {
        let Ok(parsed) = parse_window_selector(selector) else {
            return Ok(None);
        };
        let Ok(chosen) = resolve_window(&owned, &parsed) else {
            return Ok(None);
        };
        chosen.id
    } else if let Some(front) = owned.first() {
        front.id
    } else {
        return Ok(None);
    };
    let Ok(Some(snapshot)) = cache::load(
        session,
        resolved.bundle_id.as_deref(),
        resolved.pid,
        window_id,
    ) else {
        return Ok(None);
    };
    let root = snapshot.observed.as_ref().unwrap_or(&snapshot.root);
    let Some(node) = tree::find_sig(root, index) else {
        return Ok(None);
    };
    let title = (!node.identity.is_empty()).then(|| node.identity.clone());
    Ok(Some(element::ElementPeek {
        title,
        secure: node.secure,
    }))
}

/// Live view of one element, for `wait` and `verify`.
pub(crate) fn view_element(
    query: &TreeQuery,
    index: u32,
) -> Result<Option<element::ElementView>, ComputerError> {
    let root = root_node(query)?;
    let Some(node) = crate::tree::find_ui(&root, index) else {
        return Ok(None);
    };
    let secure = crate::tree::node_is_secure(node);
    Ok(Some(element::ElementView {
        role: node.role.clone(),
        title: node
            .title
            .clone()
            .or_else(|| node.description.clone())
            .or_else(|| node.identifier.clone()),
        value: if secure { None } else { node.value.clone() },
        secure,
        enabled: node.enabled,
        frame: node.frame.map(crate::tree::SavedFrame::from),
        subtree_text: subtree_text(node),
    }))
}

fn subtree_text(node: &crate::tree::UiNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    collect_subtree_text(node, &mut parts);
    parts.join(" ")
}

fn collect_subtree_text(node: &crate::tree::UiNode, parts: &mut Vec<String>) {
    for text in [
        node.value.as_deref(),
        node.title.as_deref(),
        node.description.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !text.is_empty() {
            parts.push(text.to_string());
        }
    }
    for child in &node.children {
        collect_subtree_text(child, parts);
    }
}

/// Move and/or resize one window through `AXPosition` and `AXSize`.
pub(crate) fn set_window_frame(
    app: &str,
    window: Option<&str>,
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
) -> Result<(), ComputerError> {
    let element = resolve_ax_window(app, window)?;
    if x.is_some() || y.is_some() {
        let current = copy_frame(&element).ok_or_else(|| {
            ComputerError::Unsupported("this window exposes no AXPosition".into())
        })?;
        let value = ax_value_point(x.unwrap_or(current.x), y.unwrap_or(current.y))?;
        set_window_value(&element, "AXPosition", &value).map_err(|error| error.into_computer())?;
    }
    if width.is_some() || height.is_some() {
        let current = copy_frame(&element)
            .ok_or_else(|| ComputerError::Unsupported("this window exposes no AXSize".into()))?;
        let value = ax_value_size(
            width.unwrap_or(current.width),
            height.unwrap_or(current.height),
        )?;
        set_window_value(&element, "AXSize", &value).map_err(|error| error.into_computer())?;
    }
    Ok(())
}

/// Set `AXMinimized` and/or `AXFullScreen` on one window.
pub(crate) fn set_window_state(
    app: &str,
    window: Option<&str>,
    minimized: Option<bool>,
    fullscreen: Option<bool>,
) -> Result<(), ComputerError> {
    let element = resolve_ax_window(app, window)?;
    if let Some(value) = minimized {
        set_bool(&element, "AXMinimized", value).map_err(|error| error.into_computer())?;
    }
    if let Some(value) = fullscreen {
        set_bool(&element, "AXFullScreen", value).map_err(|error| error.into_computer())?;
    }
    Ok(())
}

/// Read `AXMinimized`, `AXFullScreen`, `AXMain`, and `AXFocused`.
pub(crate) fn window_status(
    app: &str,
    window: Option<&str>,
) -> Result<WindowStatus, ComputerError> {
    let element = resolve_ax_window(app, window)?;
    Ok(WindowStatus {
        minimized: copy_bool(&element, "AXMinimized").ok().flatten(),
        fullscreen: copy_bool(&element, "AXFullScreen").ok().flatten(),
        main: copy_bool(&element, "AXMain").ok().flatten(),
        focused: copy_bool(&element, "AXFocused").ok().flatten(),
    })
}

/// The `AXWindow` element for `app`/`window`, matched by title and frame and
/// falling back to the main window, then the first window.
fn resolve_ax_window(
    app: &str,
    window: Option<&str>,
) -> Result<CFRetained<AXUIElement>, ComputerError> {
    let (info, target) = apps::resolve_running(app, window)?;
    if !trusted() {
        return Err(ComputerError::PermissionRequired(permission_subject()));
    }
    let application = application(info.pid).map_err(|error| error.into_computer())?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let windows = copy_windows(&application, deadline).map_err(attr_error_to_computer)?;
    let chosen = target
        .as_ref()
        .and_then(|target| {
            windows.iter().position(|candidate| {
                candidate.title.eq_ignore_ascii_case(&target.title)
                    && candidate
                        .frame
                        .map(|frame| bounds_match_frame(target.bounds, frame))
                        .unwrap_or(false)
            })
        })
        .or_else(|| windows.iter().position(|candidate| candidate.main))
        .or_else(|| (!windows.is_empty()).then_some(0));
    let Some(index) = chosen else {
        let label = target
            .as_ref()
            .map(|target| target.title.clone())
            .filter(|title| !title.is_empty())
            .or_else(|| window.map(str::to_string))
            .unwrap_or_else(|| app.to_string());
        return Err(ComputerError::WindowNotFound(label));
    };
    Ok(windows.into_iter().nth(index).expect("index").element)
}

fn ax_value_point(x: f64, y: f64) -> Result<CFRetained<AXValue>, ComputerError> {
    let point = CGPoint { x, y };
    // SAFETY: AXValueCreate copies the point out of the stack frame.
    unsafe {
        AXValue::new(
            AXValueType::CGPoint,
            NonNull::from(&point).cast::<std::ffi::c_void>(),
        )
    }
    .ok_or_else(|| ComputerError::ExecutionFailed("could not build AXPosition".into()))
}

fn ax_value_size(width: f64, height: f64) -> Result<CFRetained<AXValue>, ComputerError> {
    let size = CGSize { width, height };
    // SAFETY: AXValueCreate copies the size out of the stack frame.
    unsafe {
        AXValue::new(
            AXValueType::CGSize,
            NonNull::from(&size).cast::<std::ffi::c_void>(),
        )
    }
    .ok_or_else(|| ComputerError::ExecutionFailed("could not build AXSize".into()))
}

fn set_window_value(element: &AXUIElement, name: &str, value: &AXValue) -> Result<(), WalkError> {
    let attribute = CFString::from_str(name);
    // SAFETY: AXValue is a CFType, element is live, and the attribute name is
    // a CFString we own for this call.
    let value = unsafe { &*(value as *const AXValue).cast::<CFType>() };
    let error = unsafe { element.set_attribute_value(&attribute, value) };
    match classify(error) {
        CopyClass::Ok | CopyClass::Missing => Ok(()),
        CopyClass::Permission => Err(WalkError::Permission(permission_subject())),
        CopyClass::Timeout => Err(WalkError::Timeout),
        CopyClass::Failed => Err(WalkError::failed(None, 0)),
    }
}

fn attr_error_to_computer(error: AttrError) -> ComputerError {
    match error {
        AttrError::Permission => ComputerError::PermissionRequired(permission_subject()),
        AttrError::Timeout => {
            ComputerError::Timeout("accessibility window lookup timed out".into())
        }
        AttrError::Missing | AttrError::Failed => {
            ComputerError::ExecutionFailed("accessibility window lookup failed".into())
        }
    }
}

/// Focused element of the target app, for the secure-field guard.
///
/// Uses `AXFocusedUIElement`, then `AXFocusedWindow` + its focused element.
/// `Ok(None)` means Accessibility is off or the app exposes no focus; the
/// caller turns that into a warning.
pub(crate) fn focused_peek(
    app: Option<&str>,
    _window: Option<&str>,
) -> Result<Option<element::ElementPeek>, ComputerError> {
    if !trusted() {
        return Err(ComputerError::PermissionRequired(permission_subject()));
    }
    let app_text = match app.map(str::trim).filter(|text| !text.is_empty()) {
        Some(app) => app.to_string(),
        None => match super::frontmost().app {
            Some(name) => name,
            None => return Ok(None),
        },
    };
    let resolved = apps::resolve_running(&app_text, None)?;
    let app_element = application(resolved.0.pid).map_err(|error| error.into_computer())?;
    let Some(element) = focused_element(&app_element) else {
        return Ok(None);
    };
    let role = copy_string(&element, "AXRole")
        .ok()
        .flatten()
        .unwrap_or_default();
    let subrole = copy_string(&element, "AXSubRole").ok().flatten();
    let title = copy_string(&element, "AXTitle")
        .ok()
        .flatten()
        .or_else(|| copy_string(&element, "AXDescription").ok().flatten());
    let secure = role == "AXSecureTextField"
        || subrole
            .as_deref()
            .is_some_and(|text| text.to_ascii_lowercase().contains("secure"));
    Ok(Some(element::ElementPeek { title, secure }))
}

fn focused_element(app: &AXUIElement) -> Option<CFRetained<AXUIElement>> {
    if let Ok(Some(value)) = copy_cf(app, "AXFocusedUIElement") {
        if let Ok(element) = value.downcast::<AXUIElement>() {
            return Some(element);
        }
    }
    if let Ok(Some(value)) = copy_cf(app, "AXFocusedWindow") {
        if let Ok(window) = value.downcast::<AXUIElement>() {
            if let Ok(Some(inner)) = copy_cf(&window, "AXFocusedUIElement") {
                if let Ok(element) = inner.downcast::<AXUIElement>() {
                    return Some(element);
                }
            }
        }
    }
    None
}

/// Frames of sheets or modals attached to the window that matches `bounds`.
///
/// An empty list means Accessibility is off, the window was not joined, or no
/// sheet was found. This read does not write the tree cache or Electron flags.
pub(crate) fn attached_sheets(pid: i32, bounds: WindowBounds) -> Vec<NodeFrame> {
    if !trusted() {
        return Vec::new();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let Ok(app) = application(pid) else {
        return Vec::new();
    };
    ignore_timeout(&app);
    let Ok(windows) = copy_windows(&app, deadline) else {
        return Vec::new();
    };
    let Some(parent_index) = windows.iter().position(|window| {
        window
            .frame
            .is_some_and(|frame| bounds_match_frame(bounds, frame))
    }) else {
        return Vec::new();
    };
    let mut sheets = Vec::new();
    collect_sheet_frames(&windows[parent_index].element, &mut sheets, 0, deadline);
    for (index, window) in windows.iter().enumerate() {
        if index == parent_index {
            continue;
        }
        if frame_is_sheet(&window.element) {
            if let Some(frame) = window.frame {
                push_sheet(&mut sheets, frame);
            }
        }
    }
    sheets
}

fn collect_sheet_frames(
    element: &AXUIElement,
    sheets: &mut Vec<NodeFrame>,
    depth: usize,
    deadline: Instant,
) {
    if depth > 4 || Instant::now() >= deadline {
        return;
    }
    if let Ok((elements, _)) = copy_elements(element, "AXSheets", 8, deadline) {
        for child in &elements {
            if let Some(frame) = copy_frame(child) {
                push_sheet(sheets, frame);
            }
        }
    }
    let Ok((children, _)) = copy_child_elements(element, 16, deadline) else {
        return;
    };
    for child in &children {
        if frame_is_sheet(child) {
            if let Some(frame) = copy_frame(child) {
                push_sheet(sheets, frame);
            }
        }
        collect_sheet_frames(child, sheets, depth + 1, deadline);
    }
}

fn frame_is_sheet(element: &AXUIElement) -> bool {
    let role = copy_string(element, "AXRole")
        .ok()
        .flatten()
        .unwrap_or_default();
    let subrole = copy_string(element, "AXSubrole")
        .ok()
        .flatten()
        .unwrap_or_default();
    role == "AXSheet" || role == "AXModal" || subrole.contains("Sheet") || subrole.contains("Modal")
}

fn push_sheet(sheets: &mut Vec<NodeFrame>, frame: NodeFrame) {
    let duplicate = sheets.iter().any(|existing| {
        (existing.x - frame.x).abs() <= 2.0
            && (existing.y - frame.y).abs() <= 2.0
            && (existing.width - frame.width).abs() <= 2.0
            && (existing.height - frame.height).abs() <= 2.0
    });
    if !duplicate && frame.width > 0.0 && frame.height > 0.0 {
        sheets.push(frame);
    }
}

pub(crate) fn ensure_trusted() -> Result<(), WalkError> {
    if trusted() {
        Ok(())
    } else {
        Err(WalkError::Permission(permission_subject()))
    }
}

/// P9 menu/dock helpers reuse this. The desktop view of the menu bar and the
/// Dock items is gated behind Accessibility like the tree.
pub(crate) fn ensure_trusted_for_menu() -> Result<(), crate::error::ComputerError> {
    if trusted() {
        Ok(())
    } else {
        Err(crate::error::ComputerError::PermissionRequired(
            permission_subject(),
        ))
    }
}

fn permission_subject() -> String {
    let subject = apps::responsible_app()
        .map(|app| app.name)
        .unwrap_or_else(|| "computer-use".into());
    permission_message_for(&subject)
}

pub(crate) fn application(pid: i32) -> Result<CFRetained<AXUIElement>, WalkError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: pid is a process id. The wrapper returns a +1 element or panics if the
        // system returns null. The panic is caught and becomes execution_failed.
        unsafe { AXUIElement::new_application(pid) }
    })) {
        Ok(element) => Ok(element),
        Err(_) => Err(WalkError::failed(None, 0)),
    }
}

fn ignore_timeout(element: &AXUIElement) {
    // SAFETY: element is a live AXUIElement. A rejected timeout leaves the default and is ignored.
    let _ = unsafe { element.set_messaging_timeout(ELEMENT_TIMEOUT_SECS) };
}

pub(crate) fn copy_cf(
    element: &AXUIElement,
    name: &str,
) -> Result<Option<CFRetained<CFType>>, AttrError> {
    let attribute = CFString::from_str(name);
    let mut slot: *const CFType = std::ptr::null();
    // SAFETY: slot is a stack out-parameter. On success it holds a +1 CFType, which from_raw owns.
    let error =
        unsafe { element.copy_attribute_value(&attribute, NonNull::new(&mut slot).expect("slot")) };
    match classify(error) {
        CopyClass::Ok => match NonNull::new(slot.cast_mut()) {
            Some(ptr) => Ok(Some(unsafe { CFRetained::from_raw(ptr) })),
            None => Ok(None),
        },
        CopyClass::Missing => Ok(None),
        CopyClass::Permission => Err(AttrError::Permission),
        CopyClass::Timeout => Err(AttrError::Timeout),
        CopyClass::Failed => Err(AttrError::Failed),
    }
}

pub(crate) fn copy_string(element: &AXUIElement, name: &str) -> Result<Option<String>, AttrError> {
    Ok(copy_cf(element, name)?.as_deref().and_then(cf_text))
}

/// Read a numeric attribute such as a scroll bar's `AXValue`.
///
/// `Ok(None)` is a missing attribute or a non-numeric value, which the scroll
/// verifier treats as "this bar cannot be measured", not as a zero.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn copy_f64(element: &AXUIElement, name: &str) -> Result<Option<f64>, AttrError> {
    Ok(copy_cf(element, name)?
        .as_deref()
        .and_then(|value| value.downcast_ref::<CFNumber>())
        .and_then(|number| number.as_f64()))
}

/// Write a numeric attribute. Used for the scroll bar's `AXValue`.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn set_f64(element: &AXUIElement, name: &str, value: f64) -> Result<(), WalkError> {
    let number = CFNumber::new_cgfloat(value);
    let reference: &CFNumber = &number;
    // SAFETY: CFNumber is a CFType that lives to the end of this call. element is
    // live, and the attribute name is a CFString we own for this call.
    let error = unsafe {
        element.set_attribute_value(
            &CFString::from_str(name),
            &*(reference as *const CFNumber).cast::<CFType>(),
        )
    };
    match classify(error) {
        CopyClass::Ok | CopyClass::Missing => Ok(()),
        CopyClass::Permission => Err(WalkError::Permission(permission_subject())),
        CopyClass::Timeout => Err(WalkError::Timeout),
        CopyClass::Failed => Err(WalkError::failed(None, 0)),
    }
}

/// Width and height of an `AXSize` attribute. Used for the scroll area and
/// its scroll bar when estimating the visible fraction.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn ax_size_of(element: &AXUIElement) -> Option<(f64, f64)> {
    ax_size(&*copy_cf(element, "AXSize").ok().flatten()?)
}

/// The `AXParent` of `element`, or `None` at the root.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn parent(element: &AXUIElement) -> Option<CFRetained<AXUIElement>> {
    copy_cf(element, "AXParent")
        .ok()
        .flatten()
        .and_then(|value| value.downcast::<AXUIElement>().ok())
}

/// Children of `element` as live elements, up to `limit`.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn children(
    element: &AXUIElement,
    limit: usize,
    deadline: Instant,
) -> Result<Vec<CFRetained<AXUIElement>>, AttrError> {
    copy_child_elements(element, limit, deadline).map(|(elements, _)| elements)
}

pub(crate) fn copy_bool(element: &AXUIElement, name: &str) -> Result<Option<bool>, AttrError> {
    let Some(value) = copy_cf(element, name)? else {
        return Ok(None);
    };
    if let Some(boolean) = value.downcast_ref::<CFBoolean>() {
        return Ok(Some(boolean.as_bool()));
    }
    if let Some(number) = value.downcast_ref::<CFNumber>() {
        return Ok(Some(number.as_i64().unwrap_or(0) != 0));
    }
    Ok(None)
}

fn set_bool(element: &AXUIElement, name: &str, value: bool) -> Result<(), WalkError> {
    let attribute = CFString::from_str(name);
    let boolean = CFBoolean::new(value);
    // SAFETY: CFBoolean is a CFType. The static lives for the process. element is live,
    // and the attribute name is a CFString we own for this call.
    let error = unsafe {
        element.set_attribute_value(&attribute, &*(boolean as *const CFBoolean).cast::<CFType>())
    };
    match classify(error) {
        CopyClass::Ok | CopyClass::Missing => Ok(()),
        CopyClass::Permission => Err(WalkError::Permission(permission_subject())),
        CopyClass::Timeout => Err(WalkError::Timeout),
        CopyClass::Failed => Err(WalkError::failed(None, 0)),
    }
}

fn cf_text(value: &CFType) -> Option<String> {
    if let Some(text) = value.downcast_ref::<CFString>() {
        let text = text.to_string();
        return (!text.is_empty()).then_some(text);
    }
    if let Some(url) = value.downcast_ref::<CFURL>() {
        let text = url.string().to_string();
        return (!text.is_empty()).then_some(text);
    }
    if let Some(number) = value.downcast_ref::<CFNumber>() {
        return number.as_f64().map(|number| number.to_string());
    }
    None
}

fn display_value(element: &AXUIElement) -> Option<String> {
    for name in ["AXValue", "AXURL", "AXSelectedText"] {
        if let Ok(Some(text)) = copy_string(element, name) {
            return Some(text);
        }
    }
    None
}

fn copy_frame(element: &AXUIElement) -> Option<NodeFrame> {
    let position = copy_cf(element, "AXPosition").ok().flatten()?;
    let size = copy_cf(element, "AXSize").ok().flatten()?;
    let (x, y) = ax_point(&position)?;
    let (width, height) = ax_size(&size)?;
    Some(NodeFrame {
        x,
        y,
        width,
        height,
    })
}

fn ax_point(value: &CFType) -> Option<(f64, f64)> {
    let ax = value.downcast_ref::<AXValue>()?;
    let mut point = CGPoint::default();
    // SAFETY: point is a stack CGPoint. AXValueGetValue writes it only when the type matches.
    let ok = unsafe { ax.value(AXValueType::CGPoint, NonNull::new(&mut point)?.cast()) };
    ok.then_some((point.x, point.y))
}

fn ax_size(value: &CFType) -> Option<(f64, f64)> {
    let ax = value.downcast_ref::<AXValue>()?;
    let mut size = CGSize::default();
    // SAFETY: size is a stack CGSize. AXValueGetValue writes it only when the type matches.
    let ok = unsafe { ax.value(AXValueType::CGSize, NonNull::new(&mut size)?.cast()) };
    ok.then_some((size.width, size.height))
}

fn copy_actions(element: &AXUIElement) -> Vec<String> {
    let attribute_slot: *const CFArray = std::ptr::null();
    let mut slot = attribute_slot;
    // SAFETY: slot is a stack out-parameter. On success it is a +1 CFArray of action names.
    let error = unsafe { element.copy_action_names(NonNull::new(&mut slot).expect("slot")) };
    if classify(error) != CopyClass::Ok {
        return Vec::new();
    }
    let Some(ptr) = NonNull::new(slot.cast_mut()) else {
        return Vec::new();
    };
    // SAFETY: Success returned a +1 array. from_raw takes that ownership.
    let array = unsafe { CFRetained::from_raw(ptr) };
    array_strings(&array)
}

fn array_strings(array: &CFArray) -> Vec<String> {
    let mut names = Vec::new();
    for index in 0..array.count() {
        // SAFETY: index is in range. The pointer is +0 and the array keeps it alive for this call.
        let raw = unsafe { array.value_at_index(index) };
        let Some(ptr) = NonNull::new(raw.cast::<CFType>().cast_mut()) else {
            continue;
        };
        // SAFETY: retain() adds a +1 so the string outlives this iteration.
        let retained: CFRetained<CFType> = unsafe { CFRetained::retain(ptr) };
        if let Some(text) = cf_text(&retained) {
            names.push(text);
        }
    }
    names
}

fn child_total(element: &AXUIElement) -> Result<usize, AttrError> {
    let attribute = CFString::from_str("AXChildren");
    let mut count = 0isize;
    // SAFETY: count is a stack CFIndex. The call writes the array length or returns an error.
    let error = unsafe {
        element.attribute_value_count(&attribute, NonNull::new(&mut count).expect("count"))
    };
    match classify(error) {
        CopyClass::Ok => Ok(usize::try_from(count).unwrap_or(0)),
        CopyClass::Missing => Ok(0),
        CopyClass::Permission => Err(AttrError::Permission),
        CopyClass::Timeout => Err(AttrError::Timeout),
        CopyClass::Failed => Err(AttrError::Failed),
    }
}

fn copy_child_elements(
    element: &AXUIElement,
    limit: usize,
    deadline: Instant,
) -> Result<(Vec<CFRetained<AXUIElement>>, usize), AttrError> {
    copy_elements(element, "AXChildren", limit, deadline)
}

fn copy_elements(
    element: &AXUIElement,
    name: &str,
    limit: usize,
    deadline: Instant,
) -> Result<(Vec<CFRetained<AXUIElement>>, usize), AttrError> {
    if Instant::now() >= deadline {
        return Err(AttrError::Timeout);
    }
    let total = match child_total_named(element, name) {
        Ok(total) => total,
        Err(AttrError::Failed) | Err(AttrError::Missing) => {
            return copy_elements_fallback(element, name, limit);
        }
        Err(error) => return Err(error),
    };
    let take = total.min(limit);
    let mut out = Vec::new();
    let mut index = 0isize;
    while (index as usize) < take {
        if Instant::now() >= deadline {
            return Ok((out, total));
        }
        let batch = ((take as isize) - index).min(VALUE_BATCH);
        let chunk = match copy_range(element, name, index, batch) {
            Ok(chunk) => chunk,
            Err(AttrError::Timeout) => return Ok((out, total)),
            Err(error) => return Err(error),
        };
        if chunk.is_empty() {
            break;
        }
        let count = chunk.len() as isize;
        out.extend(chunk);
        index += count;
    }
    Ok((out, total))
}

fn child_total_named(element: &AXUIElement, name: &str) -> Result<usize, AttrError> {
    let attribute = CFString::from_str(name);
    let mut count = 0isize;
    // SAFETY: count is a stack CFIndex written by AXUIElementGetAttributeValueCount.
    let error = unsafe {
        element.attribute_value_count(&attribute, NonNull::new(&mut count).expect("count"))
    };
    match classify(error) {
        CopyClass::Ok => Ok(usize::try_from(count).unwrap_or(0)),
        CopyClass::Missing => Err(AttrError::Missing),
        CopyClass::Permission => Err(AttrError::Permission),
        CopyClass::Timeout => Err(AttrError::Timeout),
        CopyClass::Failed => Err(AttrError::Failed),
    }
}

fn copy_elements_fallback(
    element: &AXUIElement,
    name: &str,
    limit: usize,
) -> Result<(Vec<CFRetained<AXUIElement>>, usize), AttrError> {
    let Some(value) = copy_cf(element, name)? else {
        return Ok((Vec::new(), 0));
    };
    let Some(array) = value.downcast_ref::<CFArray>() else {
        return Ok((Vec::new(), 0));
    };
    let elements = array_elements(array);
    let total = elements.len();
    let limited: Vec<_> = elements.into_iter().take(limit).collect();
    Ok((limited, total))
}

fn copy_range(
    element: &AXUIElement,
    name: &str,
    index: isize,
    max_values: isize,
) -> Result<Vec<CFRetained<AXUIElement>>, AttrError> {
    let attribute = CFString::from_str(name);
    let mut slot: *const CFArray = std::ptr::null();
    // SAFETY: slot is a stack out-parameter. On success it is a +1 CFArray of elements.
    let error = unsafe {
        element.copy_attribute_values(
            &attribute,
            index,
            max_values,
            NonNull::new(&mut slot).expect("slot"),
        )
    };
    match classify(error) {
        CopyClass::Ok => {}
        CopyClass::Missing => return Ok(Vec::new()),
        CopyClass::Permission => return Err(AttrError::Permission),
        CopyClass::Timeout => return Err(AttrError::Timeout),
        CopyClass::Failed => return Err(AttrError::Failed),
    }
    let Some(ptr) = NonNull::new(slot.cast_mut()) else {
        return Ok(Vec::new());
    };
    // SAFETY: Success returned a +1 array. from_raw takes that ownership. A null slot is handled above.
    let array = unsafe { CFRetained::from_raw(ptr) };
    Ok(array_elements(&array))
}

pub(crate) fn array_elements(array: &CFArray) -> Vec<CFRetained<AXUIElement>> {
    let mut out = Vec::new();
    for index in 0..array.count() {
        // SAFETY: index is in range. value_at_index is +0. Null entries are skipped.
        let raw = unsafe { array.value_at_index(index) };
        if raw.is_null() {
            continue;
        }
        let Some(ptr) = NonNull::new(raw.cast::<CFType>().cast_mut()) else {
            continue;
        };
        // SAFETY: retain() takes a +1 so the element stays valid after the array drops.
        let retained: CFRetained<CFType> = unsafe { CFRetained::retain(ptr) };
        if let Ok(element) = retained.downcast::<AXUIElement>() {
            out.push(element);
        }
    }
    out
}

fn element_pid(element: &AXUIElement) -> Option<i32> {
    let mut pid = 0i32;
    // SAFETY: pid is a stack pid_t. AXUIElementGetPid writes the hosting process id.
    let error = unsafe { element.pid(NonNull::new(&mut pid).expect("pid")) };
    (error == AXError::Success && pid > 0).then_some(pid)
}

fn touch_listed(element: &AXUIElement, deadline: Instant) {
    for name in [
        "AXRoleDescription",
        "AXPlaceholderValue",
        "AXHelp",
        "AXFocused",
        "AXEnabled",
        "AXParent",
    ] {
        if Instant::now() >= deadline {
            return;
        }
        let _ = copy_cf(element, name);
    }
}

fn unavailable() -> UiNode {
    UiNode::bare("unavailable")
}

fn ensure_observer(pid: i32, session: Option<&str>) {
    if lock(failed_observers()).contains(&pid) || lock(observers()).contains_key(&pid) {
        return;
    }
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stop_thread = std::sync::Arc::clone(&stop);
    let (sender, receiver) = std::sync::mpsc::channel();
    let thread = thread::spawn(move || observer_thread(pid, stop_thread, sender));
    let ready = receiver
        .recv_timeout(Duration::from_millis(800))
        .unwrap_or(false);
    if ready {
        lock(observers()).insert(
            pid,
            ObserverState {
                stop,
                thread: Some(thread),
                session: session.map(str::to_string),
            },
        );
        return;
    }
    stop.store(true, Ordering::SeqCst);
    let _ = thread.join();
    lock(failed_observers()).insert(pid);
}

fn observer_thread(
    pid: i32,
    stop: std::sync::Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<bool>,
) {
    let Ok(element) = application(pid) else {
        let _ = ready.send(false);
        return;
    };
    ignore_timeout(&element);
    let pid_box = Box::into_raw(Box::new(pid));
    let mut raw: *mut AXObserver = std::ptr::null_mut();
    // SAFETY: raw is a stack out-parameter. On success it is a +1 AXObserver. The callback
    // only reads the pid box, which this thread frees after the run loop source is gone.
    let error = unsafe {
        AXObserver::create(
            pid,
            Some(on_ax_notification),
            NonNull::new(&mut raw).expect("observer"),
        )
    };
    if error != AXError::Success {
        // SAFETY: the callback was never registered, so freeing the pid box is safe.
        unsafe { drop(Box::from_raw(pid_box)) };
        let _ = ready.send(false);
        return;
    }
    let Some(ptr) = NonNull::new(raw) else {
        unsafe { drop(Box::from_raw(pid_box)) };
        let _ = ready.send(false);
        return;
    };
    // SAFETY: AXObserverCreate returned +1. from_raw takes that ownership.
    let observer = unsafe { CFRetained::from_raw(ptr) };
    let mut registered = 0;
    for name in ["AXFocusedWindowChanged", "AXValueChanged"] {
        let notification = CFString::from_str(name);
        // SAFETY: refcon points at the pid box owned by this thread. It stays valid until
        // after the run loop source is removed below. element is alive for this call.
        let added =
            unsafe { observer.add_notification(&element, &notification, pid_box.cast::<c_void>()) };
        if added == AXError::Success || added == AXError::NotificationAlreadyRegistered {
            registered += 1;
        }
    }
    if registered == 0 {
        drop(observer);
        unsafe { drop(Box::from_raw(pid_box)) };
        let _ = ready.send(false);
        return;
    }
    // SAFETY: observer is a live AXObserver. The source stays on this thread's run loop.
    let source = unsafe { observer.run_loop_source() };
    let Some(run_loop) = CFRunLoop::current() else {
        drop(observer);
        unsafe { drop(Box::from_raw(pid_box)) };
        let _ = ready.send(false);
        return;
    };
    // SAFETY: kCFRunLoopDefaultMode is a process-lifetime mode string.
    let mode = unsafe { kCFRunLoopDefaultMode };
    run_loop.add_source(Some(&source), mode);
    let _ = ready.send(true);
    while !stop.load(Ordering::SeqCst) {
        let _ = CFRunLoop::run_in_mode(mode, 0.5, false);
    }
    run_loop.remove_source(Some(&source), mode);
    drop(source);
    drop(observer);
    drop(element);
    // SAFETY: the run loop source is removed, so the callback cannot run. The box is still owned here.
    unsafe { drop(Box::from_raw(pid_box)) };
}

unsafe extern "C-unwind" fn on_ax_notification(
    _observer: NonNull<AXObserver>,
    _element: NonNull<AXUIElement>,
    _notification: NonNull<CFString>,
    refcon: *mut c_void,
) {
    if refcon.is_null() {
        return;
    }
    // SAFETY: refcon is the pid allocated by the observer thread. That thread frees it only
    // after removing the run loop source, so this read cannot race the free.
    let pid = unsafe { *refcon.cast::<i32>() };
    lock(dirty_set()).insert(pid);
}

fn take_dirty(pid: i32) -> bool {
    if lock(failed_observers()).contains(&pid) || !lock(observers()).contains_key(&pid) {
        return true;
    }
    lock(dirty_set()).remove(&pid)
}

fn stop_observers(mut keep: impl FnMut(i32, &ObserverState) -> bool) {
    let mut join = Vec::new();
    {
        let mut map = lock(observers());
        let pids: Vec<i32> = map
            .iter()
            .filter(|(pid, state)| keep(**pid, state))
            .map(|(pid, _)| *pid)
            .collect();
        for pid in pids {
            if let Some(mut state) = map.remove(&pid) {
                state.stop.store(true, Ordering::SeqCst);
                if let Some(thread) = state.thread.take() {
                    join.push(thread);
                }
            }
        }
    }
    for thread in join {
        let _ = thread.join();
    }
}

impl WalkError {
    fn from_computer(error: ComputerError) -> Self {
        match error {
            ComputerError::PermissionRequired(message) => Self::Permission(message),
            ComputerError::AppNotFound(message) => Self::AppNotFound(message),
            ComputerError::WindowNotFound(message) => Self::WindowNotFound(message),
            ComputerError::InvalidArguments(message) => Self::Invalid(message),
            ComputerError::ExecutionFailed(message) => Self::Message(message),
            other => Self::Message(other.to_string()),
        }
    }

    pub(crate) fn into_computer(self) -> ComputerError {
        match self {
            Self::Permission(message) => ComputerError::PermissionRequired(message),
            Self::Timeout => ComputerError::ExecutionFailed("messaging timeout".into()),
            Self::WindowNotFound(message) => ComputerError::WindowNotFound(message),
            Self::AppNotFound(message) => ComputerError::AppNotFound(message),
            Self::Invalid(message) => ComputerError::InvalidArguments(message),
            Self::Message(message) => ComputerError::ExecutionFailed(message),
            Self::Failed { .. } => {
                ComputerError::ExecutionFailed("accessibility tree could not be read".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_permission_timeout_and_missing() {
        assert_eq!(classify(AXError::Success), CopyClass::Ok);
        assert_eq!(classify(AXError::APIDisabled), CopyClass::Permission);
        assert_eq!(classify(AXError::CannotComplete), CopyClass::Timeout);
        assert_eq!(classify(AXError::NoValue), CopyClass::Missing);
        assert_eq!(classify(AXError::AttributeUnsupported), CopyClass::Missing);
        assert_eq!(classify(AXError::InvalidUIElement), CopyClass::Failed);
    }

    #[test]
    fn permission_message_names_the_settings_pane() {
        let message = permission_message_for("Grok Bot");
        assert!(message.contains("Accessibility permission is required"));
        assert!(message.contains("System Settings → Privacy & Security → Accessibility"));
        assert!(message.contains("Grok Bot"));
    }
}
