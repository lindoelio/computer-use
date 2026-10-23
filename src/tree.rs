//! Numbered accessibility tree text, diff, and the pure helpers the AX walker uses.
//!
//! Nothing in this module calls a platform API. The macOS walker fills [`UiNode`]
//! and the tests build the same nodes by hand.

use serde::{Deserialize, Serialize};

/// Children kept under one parent. Further children become `… +N`.
pub const CHILD_CAP: usize = 40;
/// Deepest indented level. The window is depth 0.
pub const DEPTH_CAP: usize = 12;
/// Rendered lines before the tree stops. `truncated=true` is an extra line.
pub const LINE_CAP: usize = 400;
/// Maximum characters kept in a displayed value.
pub const VALUE_CAP: usize = 120;
/// Maximum characters kept from a URL path.
pub const URL_PATH_CAP: usize = 60;

/// One accessibility node before numbering.
#[derive(Clone, Debug, PartialEq)]
pub struct UiNode {
    /// Role, or `unavailable` when a copy failed for this node only.
    pub role: String,
    /// `AXSubrole`, when the platform sent one.
    pub subrole: Option<String>,
    /// `AXTitle`.
    pub title: Option<String>,
    /// `AXDescription`, used when the title is empty.
    pub description: Option<String>,
    /// `AXIdentifier`. Preferred over the title in the diff signature.
    pub identifier: Option<String>,
    /// Display value. Secure nodes must leave this empty; the renderer never reads a secret.
    pub value: Option<String>,
    /// `AXEnabled`/UIA `IsEnabled`. True when the platform does not expose it.
    pub enabled: bool,
    /// `AXSecureTextField`, or a subrole containing `Secure`.
    pub secure: bool,
    /// Action names, already copied off the element.
    pub actions: Vec<String>,
    /// Screen frame in points. Missing when the element has no position.
    pub frame: Option<NodeFrame>,
    /// Child count before the child cap. `None` means `children.len()`.
    pub child_count: Option<usize>,
    /// Children in platform order. The renderer keeps at most [`CHILD_CAP`].
    pub children: Vec<UiNode>,
}

/// Screen rectangle in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeFrame {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// Frame stored for revalidation. Points are rounded the same way the text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedFrame {
    /// Left edge in screen points.
    pub x: i32,
    /// Top edge in screen points.
    pub y: i32,
    /// Width in screen points.
    pub width: u32,
    /// Height in screen points.
    pub height: u32,
}

impl From<NodeFrame> for SavedFrame {
    fn from(frame: NodeFrame) -> Self {
        Self {
            x: round_i32(frame.x),
            y: round_i32(frame.y),
            width: round_u32(frame.width),
            height: round_u32(frame.height),
        }
    }
}

/// Element index bound to one process, window, and structural revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundElement {
    /// Owning process.
    pub pid: i32,
    /// Joined window id.
    pub window_id: u32,
    /// Hash of the structural signatures.
    pub revision: u64,
    /// Preorder index in that revision.
    pub index: u32,
}

impl UiNode {
    /// A node with a role and no other fields.
    pub fn bare(role: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            subrole: None,
            title: None,
            description: None,
            identifier: None,
            value: None,
            enabled: true,
            secure: false,
            actions: Vec::new(),
            frame: None,
            child_count: None,
            children: Vec::new(),
        }
    }

    fn real_child_count(&self) -> usize {
        self.child_count.unwrap_or(self.children.len())
    }
}

/// Preorder lookup. Index 0 is `root`, matching [`render`].
pub fn find_ui(root: &UiNode, index: u32) -> Option<&UiNode> {
    let mut cursor = 0u32;
    fn walk<'a>(node: &'a UiNode, index: u32, cursor: &mut u32) -> Option<&'a UiNode> {
        if *cursor == index {
            return Some(node);
        }
        *cursor = cursor.saturating_add(1);
        for child in &node.children {
            if let Some(found) = walk(child, index, cursor) {
                return Some(found);
            }
        }
        None
    }
    walk(root, index, &mut cursor)
}

/// Lookup by the index stored when the signature was rendered.
pub fn find_sig(root: &SigNode, index: u32) -> Option<&SigNode> {
    if root.index == index {
        return Some(root);
    }
    for child in &root.children {
        if let Some(found) = find_sig(child, index) {
            return Some(found);
        }
    }
    None
}

/// Structural signature stored in the session cache. Values are already redacted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigNode {
    /// Preorder index in the render that produced this node.
    pub index: u32,
    /// Role string.
    pub role: String,
    /// Identifier, otherwise title, otherwise description.
    pub identity: String,
    /// Child count before the cap.
    pub child_count: usize,
    /// Value as it was printed, without the surrounding quotes.
    pub value: String,
    /// Rounded frame. A shift here does not change [`revision_of`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<SavedFrame>,
    /// Secure text field. The raw characters are never stored.
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub secure: bool,
    /// Children that were numbered. Capped children are omitted.
    pub children: Vec<SigNode>,
}

/// Text plus the signature tree used for the next diff.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    /// Numbered text, ending in a newline. Includes `truncated=true` when a cap fired.
    pub text: String,
    /// Whether a child, depth, or line cap fired.
    pub truncated: bool,
    /// Signature of the numbered tree.
    pub root: SigNode,
}

/// Previous accessibility flags for `AXManualAccessibility` and `AXEnhancedUserInterface`.
///
/// `None` means the attribute was missing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagState {
    /// Previous `AXManualAccessibility` value.
    pub manual: Option<bool>,
    /// Previous `AXEnhancedUserInterface` value.
    pub enhanced: Option<bool>,
}

/// What to write on attach, and what to write back on teardown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElectronEdit {
    /// Set `AXManualAccessibility` to this value. `None` means leave it alone.
    pub set_manual: Option<bool>,
    /// Set `AXEnhancedUserInterface` to this value. `None` means leave it alone.
    pub set_enhanced: Option<bool>,
    /// Values to restore. Only fields we changed are `Some`.
    pub restore: FlagState,
    /// Values observed before the edit.
    pub previous: FlagState,
}

/// Request shared by `tree` and `get-state`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeQuery {
    /// Application selector. Empty uses the frontmost app.
    pub app: Option<String>,
    /// Window selector. Empty uses the key window.
    pub window: Option<String>,
    /// Print the full tree and replace the baseline.
    pub full: bool,
    /// Request a diff explicitly. The renderer already diffs once a baseline
    /// exists; this flag makes the request visible and is a no-op otherwise.
    pub diff: bool,
    /// Session cache namespace. Empty uses the process id.
    pub session: Option<String>,
}

/// Observation returned to the dispatcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeReport {
    /// Target pid.
    pub pid: i32,
    /// Joined window id. Zero when no CG window matched.
    pub window_id: u32,
    /// Hash of the structural signatures.
    pub revision: u64,
    /// `ax` or `osascript`.
    pub source: String,
    /// Whether the numbered text hit a cap.
    pub truncated: bool,
    /// Chosen text for the `tree` command: the diff once a baseline exists, otherwise the full text.
    pub tree: String,
    /// Numbered text, even when [`Self::tree`] is a diff.
    pub full_text: String,
    /// Diff against the previous baseline. `None` when this observation created the baseline.
    pub diff: Option<String>,
    /// Bundle id when known, otherwise the application name.
    pub app: Option<String>,
    /// Extra note. The osascript path sets `source=osascript`.
    pub note: Option<String>,
    /// JXA JSON when the native tree was not built.
    pub legacy: Option<String>,
}

/// Render `root` with the tree-text rules.
pub fn render(root: &UiNode) -> Rendered {
    let mut lines = Vec::new();
    let mut truncated = false;
    let mut index = 0;
    let signature = walk(root, 0, &mut index, &mut lines, &mut truncated);
    if truncated {
        lines.push("truncated=true".to_string());
    }
    let mut text = lines.join("\n");
    text.push('\n');
    Rendered {
        text,
        truncated,
        root: signature,
    }
}

/// Diff `current` against `previous` with an LCS on `(role, identity, child_count)`.
pub fn diff_trees(previous: &SigNode, current: &SigNode) -> String {
    let mut lines = Vec::new();
    diff_level(
        std::slice::from_ref(previous),
        std::slice::from_ref(current),
        &mut lines,
    );
    if lines.is_empty() {
        String::new()
    } else {
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }
}

/// Full text when there is no baseline or `force_full` is set. Otherwise the diff.
pub fn choose_text(previous: Option<&SigNode>, rendered: &Rendered, force_full: bool) -> String {
    match previous {
        Some(previous) if !force_full => diff_trees(previous, &rendered.root),
        _ => rendered.text.clone(),
    }
}

/// Stable hash of the structural signatures.
pub fn revision_of(root: &SigNode) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    hash_node(root, &mut hash);
    hash
}

/// Enable the two Electron attributes when they are false or missing.
pub fn electron_enable(current: FlagState) -> ElectronEdit {
    let set_manual = (current.manual != Some(true)).then_some(true);
    let set_enhanced = (current.enhanced != Some(true)).then_some(true);
    ElectronEdit {
        set_manual,
        set_enhanced,
        restore: FlagState {
            manual: restore_value(set_manual.is_some(), current.manual),
            enhanced: restore_value(set_enhanced.is_some(), current.enhanced),
        },
        previous: current,
    }
}

/// Values to write back. `None` means that attribute was not changed.
pub fn electron_restore(edit: &ElectronEdit) -> FlagState {
    edit.restore.clone()
}

/// Fake `AXManualAccessibility` / `AXEnhancedUserInterface` map.
///
/// The macOS walker uses the same [`electron_enable`] decision. Tests drive
/// this map instead of a live application element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeMap {
    /// Current `AXManualAccessibility` value. `None` means the attribute is missing.
    pub manual: Option<bool>,
    /// Current `AXEnhancedUserInterface` value. `None` means the attribute is missing.
    pub enhanced: Option<bool>,
}

impl AttributeMap {
    /// Set both flags when they are false or missing. Returns the edit to persist.
    pub fn enable_electron(&mut self) -> ElectronEdit {
        let edit = electron_enable(FlagState {
            manual: self.manual,
            enhanced: self.enhanced,
        });
        if edit.set_manual == Some(true) {
            self.manual = Some(true);
        }
        if edit.set_enhanced == Some(true) {
            self.enhanced = Some(true);
        }
        edit
    }

    /// Write back only the attributes [`enable_electron`] changed.
    pub fn restore_electron(&mut self, edit: &ElectronEdit) {
        if let Some(value) = edit.restore.manual {
            self.manual = Some(value);
        }
        if let Some(value) = edit.restore.enhanced {
            self.enhanced = Some(value);
        }
    }
}

/// Helper pids to walk under a window.
///
/// Foreign child pids come first and win over the process scan. Duplicates are
/// dropped. The result is capped at 3.
pub fn hosted_helper_pids(app_pid: i32, child_pids: &[i32], scanned: &[i32]) -> Vec<i32> {
    let mut out = Vec::new();
    push_unique(&mut out, app_pid, child_pids);
    if out.is_empty() {
        push_unique(&mut out, app_pid, scanned);
    }
    out.truncate(3);
    out
}

/// Whether `child_bundle` looks like a helper of `parent_name` / `parent_bundle`.
pub fn bundle_matches_parent(
    parent_name: &str,
    parent_bundle: Option<&str>,
    child_bundle: &str,
) -> bool {
    let child = child_bundle.to_ascii_lowercase();
    let squashed = parent_name.to_ascii_lowercase().replace(' ', "");
    if squashed.len() >= 3 && child.contains(&squashed) {
        return true;
    }
    let Some(parent_bundle) = parent_bundle else {
        return false;
    };
    let parent_bundle = parent_bundle.to_ascii_lowercase();
    if child != parent_bundle && child.contains(&parent_bundle) {
        return true;
    }
    parent_bundle
        .rsplit('.')
        .next()
        .is_some_and(|label| label.len() >= 3 && child.contains(label))
}

/// Keep scheme, host, and at most 60 characters of the path. Drop the query.
pub fn shorten_url(value: &str) -> String {
    let (scheme, rest) = if let Some(rest) = value.strip_prefix("https://") {
        ("https://", rest)
    } else if let Some(rest) = value.strip_prefix("http://") {
        ("http://", rest)
    } else {
        return value.to_string();
    };
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (host, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    format!("{scheme}{host}{}", truncate_chars(path, URL_PATH_CAP))
}

fn restore_value(changed: bool, previous: Option<bool>) -> Option<bool> {
    if changed {
        Some(previous.unwrap_or(false))
    } else {
        None
    }
}

fn push_unique(out: &mut Vec<i32>, app_pid: i32, pids: &[i32]) {
    for pid in pids {
        if *pid == app_pid || *pid <= 0 || out.contains(pid) {
            continue;
        }
        out.push(*pid);
        if out.len() == 3 {
            break;
        }
    }
}

fn walk(
    node: &UiNode,
    depth: usize,
    index: &mut u32,
    lines: &mut Vec<String>,
    truncated: &mut bool,
) -> SigNode {
    let my_index = *index;
    *index += 1;
    let value = display_value(node);
    let identity = identity_of(node);
    if lines.len() < LINE_CAP {
        lines.push(format_line(node, my_index, depth, &value));
    } else {
        *truncated = true;
    }
    let total = node.real_child_count();
    let mut children = Vec::new();
    if depth >= DEPTH_CAP {
        if total > 0 {
            *truncated = true;
        }
    } else {
        let show = total.min(CHILD_CAP).min(node.children.len());
        for child in node.children.iter().take(show) {
            if lines.len() >= LINE_CAP {
                *truncated = true;
                break;
            }
            children.push(walk(child, depth + 1, index, lines, truncated));
        }
        if total > CHILD_CAP {
            *truncated = true;
            if lines.len() < LINE_CAP {
                let indent = "  ".repeat(depth + 1);
                lines.push(format!("{indent}… +{}", total - CHILD_CAP));
            }
        } else if node.children.len() < total {
            // The walker stopped early (line budget) and did not attach every child.
            *truncated = true;
        }
    }
    SigNode {
        index: my_index,
        role: node.role.clone(),
        identity,
        child_count: total,
        value,
        frame: node.frame.map(SavedFrame::from),
        secure: node.secure || is_secure(node),
        children,
    }
}

/// Identifier, otherwise title, otherwise description. Empty when all are missing.
pub fn identity_text(node: &UiNode) -> String {
    identity_of(node)
}

/// `AXSecureTextField`, or a subrole containing `Secure`.
pub fn node_is_secure(node: &UiNode) -> bool {
    node.secure || is_secure(node)
}

fn is_false_bool(value: &bool) -> bool {
    !*value
}

fn format_line(node: &UiNode, index: u32, depth: usize, value: &str) -> String {
    let indent = "  ".repeat(depth);
    let mut line = format!("{indent}[{index}] {}", node.role);
    if let Some(label) = quoted_label(node) {
        line.push(' ');
        line.push_str(&quote(&label));
    }
    if let Some(frame) = node.frame.map(SavedFrame::from) {
        line.push_str(&format!(
            " @{x},{y} {width}x{height}",
            x = frame.x,
            y = frame.y,
            width = frame.width,
            height = frame.height
        ));
    }
    if node.secure || is_secure(node) {
        line.push_str(" value=\"••••\"");
    } else if node.value.is_some() {
        line.push_str(" value=");
        line.push_str(&quote(value));
    }
    if !node.actions.is_empty() {
        line.push_str(" actions=");
        line.push_str(&node.actions.join(","));
    }
    line
}

fn quoted_label(node: &UiNode) -> Option<String> {
    nonempty(&node.title).or_else(|| nonempty(&node.description))
}

fn identity_of(node: &UiNode) -> String {
    nonempty(&node.identifier)
        .or_else(|| nonempty(&node.title))
        .or_else(|| nonempty(&node.description))
        .unwrap_or_default()
}

fn nonempty(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn display_value(node: &UiNode) -> String {
    if node.secure || is_secure(node) {
        return "••••".to_string();
    }
    let Some(value) = node.value.as_deref() else {
        return String::new();
    };
    truncate_chars(&shorten_url(value), VALUE_CAP)
}

fn is_secure(node: &UiNode) -> bool {
    // macOS reports `AXSecureTextField`; Linux reports the AT-SPI
    // `Role::PasswordText` (its `Role::name()` is `"password text"`).
    // Both redacts the value. Windows uses `IsPassword` on the
    // `IRawElementProviderFragmentRoot`; UIA sets `node.secure` directly
    // from `IsPassword`, so the dedicated bool field covers it.
    node.role == "AXSecureTextField"
        || node.role == "password text"
        || node
            .subrole
            .as_deref()
            .is_some_and(|subrole| subrole.contains("Secure"))
}

fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn round_i32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    let clamped = value.round().clamp(i32::MIN as f64, i32::MAX as f64);
    clamped as i32
}

fn round_u32(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    let clamped = value.round().min(u32::MAX as f64);
    clamped as u32
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

fn diff_level(previous: &[SigNode], current: &[SigNode], lines: &mut Vec<String>) {
    let pairs = lcs(previous, current);
    let mut prev_index = 0;
    let mut curr_index = 0;
    for (left, right) in pairs {
        while prev_index < left {
            lines.push(removed(&previous[prev_index]));
            prev_index += 1;
        }
        while curr_index < right {
            lines.push(added(&current[curr_index]));
            curr_index += 1;
        }
        let before = &previous[left];
        let after = &current[right];
        if before.value != after.value {
            lines.push(format!(
                "~ [{}] {} value: {} -> {}",
                after.index,
                after.role,
                quote(&before.value),
                quote(&after.value)
            ));
        }
        diff_level(&before.children, &after.children, lines);
        prev_index = left + 1;
        curr_index = right + 1;
    }
    while prev_index < previous.len() {
        lines.push(removed(&previous[prev_index]));
        prev_index += 1;
    }
    while curr_index < current.len() {
        lines.push(added(&current[curr_index]));
        curr_index += 1;
    }
}

fn added(node: &SigNode) -> String {
    format!("+ [{}] {}", node.index, labeled(node))
}

fn removed(node: &SigNode) -> String {
    format!("- [{}] {}", node.index, labeled(node))
}

fn labeled(node: &SigNode) -> String {
    if node.identity.is_empty() {
        node.role.clone()
    } else {
        format!("{} {}", node.role, quote(&node.identity))
    }
}

fn same_signature(left: &SigNode, right: &SigNode) -> bool {
    left.role == right.role
        && left.identity == right.identity
        && left.child_count == right.child_count
}

fn lcs(previous: &[SigNode], current: &[SigNode]) -> Vec<(usize, usize)> {
    let rows = previous.len() + 1;
    let cols = current.len() + 1;
    let mut score = vec![0u32; rows * cols];
    let at = |row: usize, col: usize| row * cols + col;
    for row in 1..rows {
        for col in 1..cols {
            if same_signature(&previous[row - 1], &current[col - 1]) {
                score[at(row, col)] = score[at(row - 1, col - 1)] + 1;
            } else {
                score[at(row, col)] = score[at(row - 1, col)].max(score[at(row, col - 1)]);
            }
        }
    }
    let mut pairs = Vec::new();
    let mut row = previous.len();
    let mut col = current.len();
    while row > 0 && col > 0 {
        if same_signature(&previous[row - 1], &current[col - 1]) {
            pairs.push((row - 1, col - 1));
            row -= 1;
            col -= 1;
        } else if score[at(row - 1, col)] >= score[at(row, col - 1)] {
            row -= 1;
        } else {
            col -= 1;
        }
    }
    pairs.reverse();
    pairs
}

fn hash_node(node: &SigNode, hash: &mut u64) {
    hash_bytes(hash, node.role.as_bytes());
    hash_bytes(hash, node.identity.as_bytes());
    hash_bytes(hash, &node.child_count.to_le_bytes());
    for child in &node.children {
        hash_node(child, hash);
    }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
    *hash ^= 0xff;
    *hash = hash.wrapping_mul(0x100000001b3);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(x: f64, y: f64, width: f64, height: f64) -> NodeFrame {
        NodeFrame {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn numbering_matches_the_contract_example() {
        let mut window = UiNode::bare("AXWindow");
        window.title = Some("Notes".into());
        window.frame = Some(frame(0.0, 0.0, 800.0, 600.0));
        let mut button = UiNode::bare("AXButton");
        button.title = Some("New Note".into());
        button.frame = Some(frame(12.0, 10.0, 88.0, 24.0));
        button.actions = vec!["AXPress".into()];
        let mut text = UiNode::bare("AXTextArea");
        text.frame = Some(frame(12.0, 40.0, 776.0, 500.0));
        text.value = Some("hello".into());
        window.children = vec![button, text];
        let rendered = render(&window);
        assert_eq!(
            rendered.text,
            "[0] AXWindow \"Notes\" @0,0 800x600\n  [1] AXButton \"New Note\" @12,10 88x24 actions=AXPress\n  [2] AXTextArea @12,40 776x500 value=\"hello\"\n"
        );
        assert!(!rendered.truncated);
        assert_eq!(rendered.root.index, 0);
        assert_eq!(rendered.root.children[1].index, 2);
    }

    #[test]
    fn child_cap_depth_cap_and_line_cap_set_truncated() {
        let mut parent = UiNode::bare("AXGroup");
        parent.children = (0..42)
            .map(|index| {
                let mut child = UiNode::bare("AXStaticText");
                child.title = Some(format!("n{index}"));
                child
            })
            .collect();
        parent.child_count = Some(42);
        let rendered = render(&parent);
        assert!(rendered.truncated);
        assert!(rendered.text.contains("… +2"));
        assert!(rendered.text.contains("truncated=true"));
        assert_eq!(rendered.root.children.len(), 40);
        assert!(rendered.text.contains("[40]"));
        assert!(!rendered.text.contains("[41]"));

        let mut deep = UiNode::bare("AXGroup");
        let mut cursor = &mut deep;
        for _ in 0..14 {
            cursor.children.push(UiNode::bare("AXGroup"));
            cursor = cursor.children.last_mut().unwrap();
        }
        let rendered = render(&deep);
        assert!(rendered.truncated);
        assert!(rendered
            .text
            .lines()
            .any(|line| line.starts_with("truncated=")));
        let deepest = rendered
            .text
            .lines()
            .filter(|line| line.contains("AXGroup"))
            .count();
        assert_eq!(deepest, DEPTH_CAP + 1);

        let mut stopped = UiNode::bare("AXGroup");
        stopped.children = vec![UiNode::bare("AXStaticText"), UiNode::bare("AXStaticText")];
        stopped.child_count = Some(5);
        let rendered = render(&stopped);
        assert!(rendered.truncated);
        assert!(rendered.text.contains("truncated=true"));
        assert!(
            !rendered.text.contains('…'),
            "a line-budget stop is not the 40-child marker"
        );

        let mut wide = UiNode::bare("AXGroup");
        grow_binary(&mut wide, 0);
        let rendered = render(&wide);
        let content: Vec<_> = rendered
            .text
            .lines()
            .filter(|line| *line != "truncated=true")
            .collect();
        assert_eq!(content.len(), LINE_CAP);
        assert!(rendered.truncated);
    }

    #[test]
    fn url_strip_and_secure_redaction_drop_the_secret() {
        assert_eq!(
            shorten_url("https://example.com/path/to/resource?q=secret"),
            "https://example.com/path/to/resource"
        );
        let long_path = format!("/{}", "a".repeat(80));
        let shortened = shorten_url(&format!("http://example.com{long_path}?x=1"));
        assert_eq!(shortened, format!("http://example.com/{}", "a".repeat(59)));
        assert!(!shortened.contains('?'));

        let mut secret = UiNode::bare("AXSecureTextField");
        secret.value = Some("hunter2".into());
        secret.secure = true;
        let rendered = render(&secret);
        assert!(rendered.text.contains("value=\"••••\""));
        assert!(!rendered.text.contains("hunter2"));
        assert!(!rendered.root.value.contains("hunter2"));

        let mut sub = UiNode::bare("AXTextField");
        sub.subrole = Some("AXSecureTextField".into());
        sub.value = Some("hunter2".into());
        let rendered = render(&sub);
        assert!(!rendered.text.contains("hunter2"));
        assert!(rendered.text.contains("••••"));
    }

    #[test]
    fn diff_uses_lcs_and_full_replaces_the_printed_text() {
        let mut before = UiNode::bare("AXWindow");
        before.identifier = Some("win".into());
        before.children = vec![
            labeled("AXButton", "A", "one"),
            labeled("AXButton", "B", "two"),
            labeled("AXTextArea", "C", "a"),
        ];
        let mut after = UiNode::bare("AXWindow");
        after.identifier = Some("win".into());
        after.children = vec![
            labeled("AXButton", "A", "one"),
            labeled("AXTextArea", "C", "ab"),
            labeled("AXButton", "D", "new"),
        ];
        let first = render(&before);
        let second = render(&after);
        let diff = diff_trees(&first.root, &second.root);
        assert!(diff.contains("- [2] AXButton \"B\""), "{diff}");
        assert!(
            diff.contains("~ [2] AXTextArea value: \"a\" -> \"ab\""),
            "{diff}"
        );
        assert!(diff.contains("+ [3] AXButton \"D\""), "{diff}");
        assert!(!diff.contains("+ [1]"));

        assert_eq!(choose_text(None, &second, false), second.text);
        assert_eq!(
            choose_text(Some(&first.root), &second, true),
            second.text,
            "full replaces the baseline text"
        );
        let incremental = choose_text(Some(&first.root), &second, false);
        assert!(incremental.starts_with('-') || incremental.starts_with('~'));
        assert_ne!(revision_of(&first.root), revision_of(&second.root));
    }

    #[test]
    fn electron_set_and_restore_returns_the_previous_values() {
        let edit = electron_enable(FlagState {
            manual: Some(false),
            enhanced: None,
        });
        assert_eq!(edit.set_manual, Some(true));
        assert_eq!(edit.set_enhanced, Some(true));
        assert_eq!(
            electron_restore(&edit),
            FlagState {
                manual: Some(false),
                enhanced: Some(false),
            }
        );
        let untouched = electron_enable(FlagState {
            manual: Some(true),
            enhanced: Some(true),
        });
        assert_eq!(untouched.set_manual, None);
        assert_eq!(untouched.set_enhanced, None);
        assert_eq!(
            electron_restore(&untouched),
            FlagState {
                manual: None,
                enhanced: None,
            }
        );
    }

    #[test]
    fn attribute_map_sets_and_restores_only_changed_flags() {
        let mut attrs = AttributeMap {
            manual: Some(false),
            enhanced: None,
        };
        let edit = attrs.enable_electron();
        assert_eq!(
            attrs,
            AttributeMap {
                manual: Some(true),
                enhanced: Some(true),
            }
        );
        assert_eq!(edit.previous.manual, Some(false));
        assert_eq!(edit.previous.enhanced, None);
        attrs.restore_electron(&edit);
        assert_eq!(
            attrs,
            AttributeMap {
                manual: Some(false),
                enhanced: Some(false),
            }
        );

        let mut already = AttributeMap {
            manual: Some(true),
            enhanced: Some(true),
        };
        let untouched = already.enable_electron();
        assert_eq!(untouched.set_manual, None);
        assert_eq!(untouched.set_enhanced, None);
        already.manual = Some(true);
        already.restore_electron(&untouched);
        assert_eq!(already.manual, Some(true));
        assert_eq!(already.enhanced, Some(true));
    }

    #[test]
    fn bound_element_keeps_pid_window_revision_and_index() {
        let bound = BoundElement {
            pid: 412,
            window_id: 88,
            revision: 3,
            index: 2,
        };
        let text = serde_json::to_string(&bound).unwrap();
        let back: BoundElement = serde_json::from_str(&text).unwrap();
        assert_eq!(back, bound);
    }

    #[test]
    fn hosted_pid_walker_includes_a_foreign_child_once() {
        let helpers = hosted_helper_pids(10, &[10, 77, 77, 88], &[77, 99]);
        assert_eq!(helpers, vec![77, 88]);
        let scanned = hosted_helper_pids(10, &[], &[10, 55, 55, 66, 67]);
        assert_eq!(scanned, vec![55, 66, 67]);
        assert!(bundle_matches_parent(
            "Google Chrome",
            Some("com.google.Chrome"),
            "com.google.Chrome.helper"
        ));
    }

    fn grow_binary(node: &mut UiNode, depth: usize) {
        if depth >= DEPTH_CAP {
            return;
        }
        node.children = vec![UiNode::bare("AXGroup"), UiNode::bare("AXGroup")];
        for child in &mut node.children {
            grow_binary(child, depth + 1);
        }
    }

    fn labeled(role: &str, identity: &str, value: &str) -> UiNode {
        let mut node = UiNode::bare(role);
        node.identifier = Some(identity.into());
        node.title = Some(identity.into());
        node.value = Some(value.into());
        node
    }
}
