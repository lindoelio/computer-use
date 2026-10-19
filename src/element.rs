//! Element actions: revalidation, the perform allowlist, and secure-field refusal.
//!
//! Nothing here calls a platform API. The macOS backend and [`crate::test_backend`]
//! both ask this module whether an action may run, then record or perform it.

use crate::error::ComputerError;
use crate::tree::{self, SavedFrame, SigNode, UiNode};

/// Each edge may differ by this many screen points and still be the same element.
///
/// Two points is the tolerance P2 uses to join an AX window to a CG window.
/// A larger shift is `stale_element`. AX rounding inside the tolerance is not.
pub const FRAME_TOLERANCE: i32 = 2;

/// Hint included on `element_not_found` and `stale_element`.
pub const REFRESH_HINT: &str = "re-run `computer-use tree --app X` to refresh indices";

/// `AXUIElementPerformAction` names `perform_action` accepts.
pub const PERFORM_ALLOWLIST: &[&str] = &[
    "AXPress",
    "AXShowMenu",
    "AXIncrement",
    "AXDecrement",
    "AXConfirm",
    "AXCancel",
    "AXRaise",
    "AXScrollToVisible",
    "AXScrollUpByPage",
    "AXScrollDownByPage",
    "AXScrollLeftByPage",
    "AXScrollRightByPage",
];

/// What the caller asked to do to one element index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementOp {
    /// `AXPress`, repeated `count` times. `count` is 1, 2, or 3.
    Press {
        /// Repeat count.
        count: u8,
    },
    /// Set `AXFocused` to true.
    Focus,
    /// Set `AXValue`. Refused on secure fields.
    SetValue {
        /// New value.
        value: String,
    },
    /// Set `AXSelectedTextRange`. Refused on secure fields.
    SelectText {
        /// First selected character.
        start: i64,
        /// Character after the selection.
        end: i64,
    },
    /// `AXUIElementPerformAction` with an allowlisted name.
    Perform {
        /// Action name such as `AXShowMenu`.
        name: String,
    },
    /// `AXScrollToVisible` only.
    ScrollTo,
    /// Prefer `AXValue` when the attribute is settable, otherwise focus and type.
    Type {
        /// Text to write.
        text: String,
    },
}

impl ElementOp {
    /// JSON `action` name.
    pub fn action_name(&self) -> &'static str {
        match self {
            Self::Press { .. } => "press",
            Self::Focus => "focus",
            Self::SetValue { .. } => "set_value",
            Self::SelectText { .. } => "select_text",
            Self::Perform { .. } => "perform_action",
            Self::ScrollTo => "scroll_to",
            Self::Type { .. } => "type",
        }
    }

    /// Writes text or a selection. Secure fields refuse these.
    pub fn writes_text(&self) -> bool {
        matches!(
            self,
            Self::SetValue { .. } | Self::SelectText { .. } | Self::Type { .. }
        )
    }
}

/// Shared fields for one element command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementRequest {
    /// Action to run after revalidation.
    pub op: ElementOp,
    /// Index in the cached revision.
    pub index: u32,
    /// Fail when the element is outside the window and scroll fails or is missing.
    pub require_visible: bool,
    /// Application selector. Empty uses the frontmost app.
    pub app: Option<String>,
    /// Window selector. Empty uses the key window.
    pub window: Option<String>,
    /// Session cache namespace.
    pub session: Option<String>,
}

/// One step the backend performs after [`gate`] allows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Repeat `AXPress`.
    Press {
        /// Repeat count, already checked to be 1, 2, or 3.
        count: u8,
    },
    /// Set `AXFocused` to true.
    Focus,
    /// Set `AXValue`.
    SetValue {
        /// New value.
        value: String,
    },
    /// Set `AXSelectedTextRange`.
    SelectText {
        /// Range location.
        start: i64,
        /// Range end, exclusive.
        end: i64,
    },
    /// Perform an allowlisted action.
    Perform {
        /// Action name.
        name: String,
    },
    /// Perform `AXScrollToVisible`.
    ScrollTo,
    /// Set `AXValue` because the attribute is settable.
    TypeSetValue {
        /// Text to write.
        text: String,
    },
    /// Focus the element, then post the text to its process. Does not activate the app.
    TypeEnigo {
        /// Text to type.
        text: String,
    },
}

impl Step {
    /// Name recorded by the stub: the AX action or attribute that runs.
    pub fn record_name(&self) -> &str {
        match self {
            Self::Press { .. } => "AXPress",
            Self::Focus | Self::TypeEnigo { .. } => "AXFocused",
            Self::SetValue { .. } | Self::TypeSetValue { .. } => "AXValue",
            Self::SelectText { .. } => "AXSelectedTextRange",
            Self::Perform { name } => name.as_str(),
            Self::ScrollTo => "AXScrollToVisible",
        }
    }

    /// How many times the stub records this step.
    pub fn repeats(&self) -> u8 {
        match self {
            Self::Press { count } => *count,
            _ => 1,
        }
    }
}

/// Successful element action, before it is rendered as JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementOutcome {
    /// Command name such as `press` or `set_value`.
    pub action: String,
    /// Index the caller passed.
    pub element: u32,
    /// Structural hash after the action.
    pub revision: u64,
    /// `ax` for an accessibility action. `event` posts to the pid. `activate` uses the foreground path.
    pub tier: String,
    /// True when this action moved the system cursor.
    pub cursor_moved: bool,
    /// Set when the action tried to restore the previous frontmost app.
    pub focus_restored: Option<bool>,
    /// Non-fatal notes. Scroll failure lands here unless `--require-visible` is set.
    pub warnings: Vec<String>,
    /// `set_value` or `enigo` for `type`. Empty for other commands.
    pub via: Option<String>,
    /// Allowlisted action name for `perform_action`.
    pub name: Option<String>,
    /// Press count, when the command was `press`.
    pub count: Option<u8>,
}

/// Success value shared by the macOS backend and the stub.
pub fn outcome(
    request: &ElementRequest,
    revision: u64,
    warnings: Vec<String>,
    via: Option<String>,
) -> ElementOutcome {
    let tier = match via.as_deref() {
        Some("event") => "event",
        Some("activate") => "activate",
        Some("enigo") => "enigo",
        _ => "ax",
    };
    let cursor_moved = matches!(via.as_deref(), Some("activate") | Some("enigo"));
    let focus_restored = (via.as_deref() == Some("activate")).then_some(true);
    ElementOutcome {
        action: request.op.action_name().to_string(),
        element: request.index,
        revision,
        tier: tier.to_string(),
        cursor_moved,
        focus_restored,
        warnings,
        via,
        name: match &request.op {
            ElementOp::Perform { name } => Some(name.clone()),
            _ => None,
        },
        count: match request.op {
            ElementOp::Press { count } => Some(count),
            _ => None,
        },
    }
}

/// Failure that may carry the fresh tree for `stale_element`.
#[derive(Debug)]
pub struct ElementError {
    /// Stable error.
    pub error: ComputerError,
    /// Numbered tree after the re-fetch. Present for `stale_element`.
    pub tree: Option<String>,
}

impl ElementError {
    /// An error without a replacement tree.
    pub fn new(error: ComputerError) -> Self {
        Self { error, tree: None }
    }

    /// `stale_element` plus the fresh numbered tree.
    pub fn stale(message: impl Into<String>, tree: impl Into<String>) -> Self {
        Self {
            error: ComputerError::StaleElement(message.into()),
            tree: Some(tree.into()),
        }
    }
}

impl From<ComputerError> for ElementError {
    fn from(error: ComputerError) -> Self {
        Self::new(error)
    }
}

/// Cached title and secure bit used by the confirm hook before an element action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementPeek {
    /// Title, identifier, or description. Empty when the node has none.
    pub title: Option<String>,
    /// `AXSecureTextField` or a secure subrole.
    pub secure: bool,
}

/// Role and title of the element under a coordinate, when hit-testing finds one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitInfo {
    /// `AXRole`.
    pub role: String,
    /// `AXTitle`, when the element has one.
    pub title: Option<String>,
}

/// Revalidation succeeded. The backend may read settable or character count next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// Child indices from the window to the element. Empty is the window itself.
    pub path: Vec<usize>,
    /// Try `AXScrollToVisible` before the action.
    pub scroll_first: bool,
    /// The element is outside the window and does not support scroll.
    pub outside_warning: Option<String>,
    /// `type` still needs `AXUIElementIsAttributeSettable` for `AXValue`.
    pub needs_settable: bool,
    /// `select_text` still needs `AXNumberOfCharacters` when the attribute exists.
    pub needs_chars: bool,
    /// Action names copied before the perform.
    pub actions: Vec<String>,
}

/// Inputs [`gate`] needs. The live node is the re-fetched tree.
pub struct GateInput<'a> {
    /// Requested operation.
    pub op: &'a ElementOp,
    /// Caller index.
    pub index: u32,
    /// `--require-visible`.
    pub require_visible: bool,
    /// App name used in the refresh hint. Empty keeps the literal `X`.
    pub app_label: &'a str,
    /// Cached signature. `observed` wins over the diff baseline.
    pub cached: &'a SigNode,
    /// Revision stored with `cached`. It must equal [`tree::revision_of`].
    pub cached_revision: u64,
    /// Pid stored with the snapshot.
    pub cached_pid: i32,
    /// Window id stored with the snapshot.
    pub cached_window: u32,
    /// Pid of the resolved target.
    pub resolved_pid: i32,
    /// Window id of the resolved target.
    pub resolved_window: u32,
    /// Fresh tree.
    pub live: &'a UiNode,
    /// Pid of the fresh tree.
    pub live_pid: i32,
    /// Window id of the fresh tree.
    pub live_window: u32,
}

/// `count` must be 1, 2, or 3.
pub fn validate_count(count: u8) -> Result<(), ComputerError> {
    if (1..=3).contains(&count) {
        Ok(())
    } else {
        Err(ComputerError::InvalidArguments(
            "count must be from 1 to 3".into(),
        ))
    }
}

/// Refuse names outside [`PERFORM_ALLOWLIST`].
pub fn validate_perform_name(name: &str) -> Result<(), ComputerError> {
    if PERFORM_ALLOWLIST.contains(&name) {
        Ok(())
    } else {
        Err(ComputerError::InvalidArguments(format!(
            "{name} is not an allowed action. Allowed: {}",
            PERFORM_ALLOWLIST.join(", ")
        )))
    }
}

/// `start <= end`, both non-negative, and `end` within `char_count` when it was read.
pub fn validate_range(start: i64, end: i64, char_count: Option<i64>) -> Result<(), ComputerError> {
    if start < 0 || end < 0 {
        return Err(ComputerError::InvalidArguments(
            "start and end must be greater than or equal to 0".into(),
        ));
    }
    if start > end {
        return Err(ComputerError::InvalidArguments(
            "start must be less than or equal to end".into(),
        ));
    }
    if let Some(count) = char_count {
        if end > count {
            return Err(ComputerError::InvalidArguments(format!(
                "end {end} is past AXNumberOfCharacters {count}"
            )));
        }
    }
    Ok(())
}

/// Refresh hint. A known app name replaces the literal `X`.
pub fn refresh_hint(app: &str) -> String {
    let app = app.trim();
    if app.is_empty() || app == "X" {
        REFRESH_HINT.to_string()
    } else {
        format!("re-run `computer-use tree --app {app}` to refresh indices")
    }
}

/// Child-index path of preorder `index` in a cached signature.
pub fn find_path(root: &SigNode, index: u32) -> Option<Vec<usize>> {
    fn walk(node: &SigNode, index: u32, path: &mut Vec<usize>) -> bool {
        if node.index == index {
            return true;
        }
        for (child_index, child) in node.children.iter().enumerate() {
            path.push(child_index);
            if walk(child, index, path) {
                return true;
            }
            path.pop();
        }
        false
    }
    let mut path = Vec::new();
    walk(root, index, &mut path).then_some(path)
}

/// Live node at a child-index path. An empty path is the root.
pub fn ui_at_path<'a>(root: &'a UiNode, path: &[usize]) -> Option<&'a UiNode> {
    let mut node = root;
    for &index in path {
        node = node.children.get(index)?;
    }
    Some(node)
}

fn sig_at_path<'a>(root: &'a SigNode, path: &[usize]) -> Option<&'a SigNode> {
    let mut node = root;
    for &index in path {
        node = node.children.get(index)?;
    }
    Some(node)
}

/// True when `element` is not fully inside `window`.
pub fn frame_outside(element: SavedFrame, window: SavedFrame) -> bool {
    let right = element.x.saturating_add_unsigned(element.width);
    let bottom = element.y.saturating_add_unsigned(element.height);
    let window_right = window.x.saturating_add_unsigned(window.width);
    let window_bottom = window.y.saturating_add_unsigned(window.height);
    element.x < window.x || element.y < window.y || right > window_right || bottom > window_bottom
}

/// True when every edge is within [`FRAME_TOLERANCE`]. Missing frames match only each other.
pub fn frames_match(cached: Option<SavedFrame>, live: Option<SavedFrame>) -> bool {
    match (cached, live) {
        (None, None) => true,
        (Some(cached), Some(live)) => {
            near(cached.x, live.x)
                && near(cached.y, live.y)
                && near_u(cached.width, live.width)
                && near_u(cached.height, live.height)
        }
        _ => false,
    }
}

fn near(left: i32, right: i32) -> bool {
    (i64::from(left) - i64::from(right)).abs() <= i64::from(FRAME_TOLERANCE)
}

fn near_u(left: u32, right: u32) -> bool {
    (i64::from(left) - i64::from(right)).abs() <= i64::from(FRAME_TOLERANCE)
}

/// Decide whether the action may run.
///
/// A pid, window id, or cached revision that does not match the stored tree is
/// `stale_element`. The live structural hash may differ when some other node
/// changed: this function compares the element at the cached child path.
pub fn gate(input: &GateInput<'_>) -> Result<Gate, ElementError> {
    let hint = refresh_hint(input.app_label);
    let live_text = tree::render(input.live).text;
    if input.cached_pid != input.resolved_pid
        || input.cached_window != input.resolved_window
        || input.cached_pid != input.live_pid
        || input.cached_window != input.live_window
        || input.cached_revision != tree::revision_of(input.cached)
    {
        return Err(ElementError::stale(
            format!("cached revision/pid/window does not match; {hint}"),
            live_text,
        ));
    }
    let Some(path) = find_path(input.cached, input.index) else {
        return Err(ComputerError::ElementNotFound(format!(
            "element {} is not in the cached tree; {hint}",
            input.index
        ))
        .into());
    };
    let Some(cached) = sig_at_path(input.cached, &path) else {
        return Err(ComputerError::ElementNotFound(format!(
            "element {} is not in the cached tree; {hint}",
            input.index
        ))
        .into());
    };
    let Some(live) = ui_at_path(input.live, &path) else {
        return Err(ComputerError::ElementNotFound(format!(
            "element {} is missing from the fresh tree; {hint}",
            input.index
        ))
        .into());
    };
    let live_identity = tree::identity_text(live);
    let live_frame = live.frame.map(SavedFrame::from);
    if cached.role != live.role {
        return Err(ElementError::stale(
            format!("role changed from {} to {}; {hint}", cached.role, live.role),
            live_text,
        ));
    }
    if cached.identity != live_identity {
        return Err(ElementError::stale(
            format!(
                "identifier changed from \"{}\" to \"{}\"; {hint}",
                cached.identity, live_identity
            ),
            live_text,
        ));
    }
    if !frames_match(cached.frame, live_frame) {
        return Err(ElementError::stale(
            format!(
                "frame changed from {} to {}; {hint}",
                frame_text(cached.frame),
                frame_text(live_frame)
            ),
            live_text,
        ));
    }
    let secure = cached.secure || tree::node_is_secure(live);
    if secure && input.op.writes_text() {
        return Err(ComputerError::PolicyDenied(
            "refusing to read or write a secure text field".into(),
        )
        .into());
    }
    let window_frame = input.live.frame.map(SavedFrame::from);
    let outside = if path.is_empty() {
        false
    } else {
        match (live_frame, window_frame) {
            (Some(element), Some(window)) => frame_outside(element, window),
            _ => false,
        }
    };
    let supports_scroll = live
        .actions
        .iter()
        .any(|action| action == "AXScrollToVisible");
    let already_scrolling = matches!(input.op, ElementOp::ScrollTo)
        || matches!(input.op, ElementOp::Perform { name } if name == "AXScrollToVisible");
    let (scroll_first, outside_warning) = if outside && !already_scrolling {
        if supports_scroll {
            (true, None)
        } else if input.require_visible {
            return Err(ComputerError::ExecutionFailed(
                "element is outside the window and does not support AXScrollToVisible".into(),
            )
            .into());
        } else {
            (
                false,
                Some("element is outside the window and does not support AXScrollToVisible".into()),
            )
        }
    } else {
        (false, None)
    };
    let needs_settable = matches!(input.op, ElementOp::Type { .. });
    let needs_chars = matches!(input.op, ElementOp::SelectText { .. });
    Ok(Gate {
        path,
        scroll_first,
        outside_warning,
        needs_settable,
        needs_chars,
        actions: live.actions.clone(),
    })
}

/// Build the steps that run after settable and character-count reads.
pub fn steps(
    op: &ElementOp,
    gate: &Gate,
    settable: bool,
    char_count: Option<i64>,
) -> Result<Vec<Step>, ElementError> {
    match op {
        ElementOp::Press { count } => {
            validate_count(*count)?;
            require_action(&gate.actions, "AXPress")?;
            Ok(vec![Step::Press { count: *count }])
        }
        ElementOp::Focus => Ok(vec![Step::Focus]),
        ElementOp::SetValue { value } => {
            validate_text(value, true)?;
            Ok(vec![Step::SetValue {
                value: value.clone(),
            }])
        }
        ElementOp::SelectText { start, end } => {
            validate_range(*start, *end, char_count)?;
            Ok(vec![Step::SelectText {
                start: *start,
                end: *end,
            }])
        }
        ElementOp::Perform { name } => {
            validate_perform_name(name)?;
            require_action(&gate.actions, name)?;
            Ok(vec![Step::Perform { name: name.clone() }])
        }
        ElementOp::ScrollTo => {
            require_action(&gate.actions, "AXScrollToVisible")?;
            Ok(vec![Step::ScrollTo])
        }
        ElementOp::Type { text } => {
            validate_text(text, false)?;
            if settable {
                Ok(vec![Step::TypeSetValue { text: text.clone() }])
            } else {
                Ok(vec![Step::TypeEnigo { text: text.clone() }])
            }
        }
    }
}

/// `execution_failed` listing the actions CopyActionNames returned.
pub fn require_action(actions: &[String], name: &str) -> Result<(), ComputerError> {
    if actions.iter().any(|action| action == name) {
        Ok(())
    } else {
        let listed = if actions.is_empty() {
            "(none)".to_string()
        } else {
            actions.join(", ")
        };
        Err(ComputerError::ExecutionFailed(format!(
            "element has no {name} action. actions: {listed}"
        )))
    }
}

/// Empty text is refused when `empty_ok` is false. A null byte is always refused.
pub fn validate_text(text: &str, empty_ok: bool) -> Result<(), ComputerError> {
    if !empty_ok && text.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "text must not be empty".into(),
        ));
    }
    reject_null(text)
}

fn reject_null(text: &str) -> Result<(), ComputerError> {
    if text.contains('\0') {
        Err(ComputerError::InvalidArguments(
            "text must not contain a null byte".into(),
        ))
    } else {
        Ok(())
    }
}

fn frame_text(frame: Option<SavedFrame>) -> String {
    match frame {
        Some(frame) => format!("@{},{} {}x{}", frame.x, frame.y, frame.width, frame.height),
        None => "none".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::NodeFrame;

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
        root.child_count = Some(1);
        root.children = vec![child];
        root
    }

    fn input_for<'a>(
        op: &'a ElementOp,
        cached: &'a SigNode,
        revision: u64,
        live: &'a UiNode,
        index: u32,
    ) -> GateInput<'a> {
        GateInput {
            op,
            index,
            require_visible: false,
            app_label: "Notes",
            cached,
            cached_revision: revision,
            cached_pid: 10,
            cached_window: 100,
            resolved_pid: 10,
            resolved_window: 100,
            live,
            live_pid: 10,
            live_window: 100,
        }
    }

    fn render_pair(root: &UiNode) -> (SigNode, u64) {
        let rendered = tree::render(root);
        let revision = tree::revision_of(&rendered.root);
        (rendered.root, revision)
    }

    #[test]
    fn same_role_allows_and_role_identifier_or_frame_changes_are_stale() {
        let cached_ui = window(button("Save", 10.0, &["AXPress"]));
        let (cached, revision) = render_pair(&cached_ui);
        let op = ElementOp::Press { count: 1 };
        let allowed = gate(&input_for(&op, &cached, revision, &cached_ui, 1)).unwrap();
        assert_eq!(allowed.path, vec![0]);
        assert!(!allowed.scroll_first);
        let steps = steps(&op, &allowed, false, None).unwrap();
        assert_eq!(steps, vec![Step::Press { count: 1 }]);

        let mut changed_role = cached_ui.clone();
        changed_role.children[0].role = "AXTextArea".into();
        let error = gate(&input_for(&op, &cached, revision, &changed_role, 1)).unwrap_err();
        assert_eq!(error.error.code(), "stale_element");
        assert!(error.tree.unwrap().contains("AXTextArea"));

        let mut changed_id = cached_ui.clone();
        changed_id.children[0].title = Some("Delete".into());
        let error = gate(&input_for(&op, &cached, revision, &changed_id, 1)).unwrap_err();
        assert_eq!(error.error.code(), "stale_element");
        assert!(error.error.to_string().contains("identifier changed"));

        let mut changed_frame = cached_ui.clone();
        changed_frame.children[0].frame = Some(frame(40.0, 10.0, 80.0, 24.0));
        let error = gate(&input_for(&op, &cached, revision, &changed_frame, 1)).unwrap_err();
        assert_eq!(error.error.code(), "stale_element");
        assert!(error.error.to_string().contains("frame changed"));

        let mut nudged = cached_ui.clone();
        nudged.children[0].frame = Some(frame(14.0, 10.0, 80.0, 24.0));
        assert!(gate(&input_for(&op, &cached, revision, &nudged, 1)).is_ok());
    }

    #[test]
    fn secure_node_refuses_set_value_select_text_and_type() {
        let mut field = UiNode::bare("AXSecureTextField");
        field.title = Some("Password".into());
        field.secure = true;
        let root = window(field);
        let (cached, revision) = render_pair(&root);
        for op in [
            ElementOp::SetValue {
                value: "secret".into(),
            },
            ElementOp::SelectText { start: 0, end: 1 },
            ElementOp::Type {
                text: "secret".into(),
            },
        ] {
            let error = gate(&input_for(&op, &cached, revision, &root, 1)).unwrap_err();
            assert_eq!(error.error.code(), "policy_denied", "{op:?}");
            assert!(error.tree.is_none());
        }
        let mut subrole = UiNode::bare("AXTextField");
        subrole.subrole = Some("AXSecureTextField".into());
        subrole.title = Some("Password".into());
        let root = window(subrole);
        let (cached, revision) = render_pair(&root);
        let op = ElementOp::SetValue {
            value: "secret".into(),
        };
        let error = gate(&input_for(&op, &cached, revision, &root, 1)).unwrap_err();
        assert_eq!(error.error.code(), "policy_denied");
    }

    #[test]
    fn allowlist_rejects_ax_delete_and_missing_action_lists_names() {
        let error = validate_perform_name("AXDelete").unwrap_err();
        assert_eq!(error.code(), "invalid_arguments");
        assert!(error.to_string().contains("AXPress"));
        let error = require_action(&["AXShowMenu".into()], "AXPress").unwrap_err();
        assert_eq!(error.code(), "execution_failed");
        assert!(error.to_string().contains("AXShowMenu"));
    }

    #[test]
    fn scroll_to_is_requested_when_the_frame_is_outside_the_window() {
        let cached_ui = window(button("Save", 700.0, &["AXPress", "AXScrollToVisible"]));
        let (cached, revision) = render_pair(&cached_ui);
        let press = ElementOp::Press { count: 1 };
        let allowed = gate(&input_for(&press, &cached, revision, &cached_ui, 1)).unwrap();
        assert!(allowed.scroll_first);
        let scroll = ElementOp::ScrollTo;
        let allowed = gate(&input_for(&scroll, &cached, revision, &cached_ui, 1)).unwrap();
        assert!(!allowed.scroll_first);
        let steps = steps(&scroll, &allowed, false, None).unwrap();
        assert_eq!(steps, vec![Step::ScrollTo]);
        let perform = ElementOp::Perform {
            name: "AXScrollToVisible".into(),
        };
        let allowed = gate(&input_for(&perform, &cached, revision, &cached_ui, 1)).unwrap();
        assert!(!allowed.scroll_first);
    }

    #[test]
    fn require_visible_fails_when_the_element_cannot_scroll() {
        let cached_ui = window(button("Save", 700.0, &["AXPress"]));
        let (cached, revision) = render_pair(&cached_ui);
        let press = ElementOp::Press { count: 1 };
        let mut input = input_for(&press, &cached, revision, &cached_ui, 1);
        input.require_visible = true;
        let error = gate(&input).unwrap_err();
        assert_eq!(error.error.code(), "execution_failed");
        input.require_visible = false;
        let allowed = gate(&input).unwrap();
        assert!(!allowed.scroll_first);
        assert!(allowed.outside_warning.is_some());
    }

    #[test]
    fn mismatched_revision_or_window_is_stale_and_a_missing_index_is_not_found() {
        let cached_ui = window(button("Save", 10.0, &["AXPress"]));
        let (cached, revision) = render_pair(&cached_ui);
        let op = ElementOp::Focus;
        let mut input = input_for(&op, &cached, revision, &cached_ui, 1);
        input.cached_revision = revision.wrapping_add(1);
        let error = gate(&input).unwrap_err();
        assert_eq!(error.error.code(), "stale_element");
        assert!(error
            .error
            .to_string()
            .contains("cached revision/pid/window does not match"));

        input.cached_revision = revision;
        input.live_window = 9;
        let error = gate(&input).unwrap_err();
        assert_eq!(error.error.code(), "stale_element");

        input.live_window = 100;
        input.index = 9;
        let error = gate(&input).unwrap_err();
        assert_eq!(error.error.code(), "element_not_found");
        assert!(error.error.to_string().contains("refresh indices"));
    }

    #[test]
    fn select_range_must_sit_inside_the_character_count() {
        assert!(validate_range(0, 5, Some(5)).is_ok());
        assert_eq!(
            validate_range(4, 2, None).unwrap_err().code(),
            "invalid_arguments"
        );
        assert_eq!(
            validate_range(0, 6, Some(5)).unwrap_err().code(),
            "invalid_arguments"
        );
    }
}
