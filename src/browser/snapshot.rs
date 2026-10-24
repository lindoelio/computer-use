//! Compact page snapshots with stable refs.
//!
//! A snapshot is the browser counterpart of the desktop accessibility tree:
//! one line per interesting node, numbered, with a `ref=eN` handle that the
//! action commands accept. The full snapshot becomes the diff baseline; the
//! next `--diff` snapshot returns only what changed.
//!
//! The cache lives in the `--session` directory, next to the desktop tree
//! snapshot. Refs are only valid while the page document and the cached
//! baseline agree; a navigation or a new snapshot invalidates them and the
//! next action returns `stale_element`.

#![cfg(feature = "cdp")]

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use chromiumoxide::cdp::browser_protocol::accessibility::{
    AxNode, AxProperty, AxValue, EnableParams as AccessibilityEnableParams, GetFullAxTreeParams,
};
use serde::{Deserialize, Serialize};

use crate::cache;
use crate::error::ComputerError;

/// One rendered node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotNode {
    /// Sequential number in the rendered text.
    pub index: usize,
    /// `e1`, `e2`, ... when the node is actionable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    /// CDP backend node id, used to resolve the element later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_node_id: Option<i64>,
    /// Accessibility role, lowercased.
    pub role: String,
    /// Accessible name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Accessible value. Redacted for protected fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Nesting depth in the accessibility tree.
    pub depth: usize,
    /// Whether the role is an actionable control.
    pub interactive: bool,
    /// Disabled state.
    pub disabled: bool,
    /// Checked state for checkboxes, radios, and switches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    /// Focused state.
    pub focused: bool,
    /// Link target, when the role is a link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// One captured snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserSnapshot {
    /// CDP target id.
    pub target_id: String,
    /// Page URL at capture time.
    pub url: String,
    /// Page title.
    pub title: String,
    /// `document.readyState`.
    pub ready_state: String,
    /// Capture timestamp in unix milliseconds.
    pub captured_unix_ms: u64,
    /// Rendered nodes.
    pub nodes: Vec<SnapshotNode>,
    /// Rendered text, one line per node.
    pub text: String,
    /// Whether a cap truncated the tree.
    pub truncated: bool,
}

impl BrowserSnapshot {
    /// Find a node by its ref.
    pub fn find_ref(&self, ref_id: &str) -> Option<&SnapshotNode> {
        let wanted = ref_id.trim();
        self.nodes
            .iter()
            .find(|node| node.ref_id.as_deref() == Some(wanted))
    }
}

/// Roles that count as actionable controls.
fn is_interactive_role(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "textbox"
            | "searchbox"
            | "combobox"
            | "listbox"
            | "option"
            | "checkbox"
            | "radio"
            | "switch"
            | "slider"
            | "spinbutton"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "tab"
            | "treeitem"
            | "gridcell"
            | "cell"
            | "input"
            | "textarea"
            | "select"
    )
}

fn value_text(value: &AxValue) -> Option<String> {
    match &value.value {
        Some(serde_json::Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(serde_json::Value::Bool(flag)) => Some(flag.to_string()),
        Some(serde_json::Value::Number(number)) => Some(number.to_string()),
        Some(serde_json::Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

fn property_name(property: &AxProperty) -> String {
    serde_json::to_value(&property.name)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{:?}", property.name).to_ascii_lowercase())
}

fn property_flag(node: &AxNode, name: &str) -> bool {
    node.properties
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|property| property_name(property) == name)
        .and_then(|property| property.value.value.as_ref())
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn role_text(node: &AxNode) -> String {
    node.role
        .as_ref()
        .and_then(value_text)
        .unwrap_or_else(|| "unknown".to_string())
        .to_ascii_lowercase()
}

fn node_is_ignored(node: &AxNode) -> bool {
    node.ignored
}

fn should_emit(node: &AxNode, interactive_only: bool) -> bool {
    if node_is_ignored(node) {
        return false;
    }
    let role = role_text(node);
    if role == "none" || role == "generic" || role == "ignored" || role == "unknown" {
        return false;
    }
    if interactive_only && !is_interactive_role(&role) {
        return false;
    }
    if is_interactive_role(&role) {
        return true;
    }
    let name = node.name.as_ref().and_then(value_text);
    let value = node.value.as_ref().and_then(value_text);
    name.is_some() || value.is_some()
}

/// Capture the accessibility tree of `page`.
pub async fn capture(
    page: &chromiumoxide::Page,
    target_id: &str,
    interactive_only: bool,
    max_nodes: usize,
) -> Result<BrowserSnapshot, ComputerError> {
    let _ = page.execute(AccessibilityEnableParams::default()).await;
    let tree = page
        .execute(GetFullAxTreeParams::default())
        .await
        .map_err(|error| ComputerError::ExecutionFailed(format!("accessibility tree: {error}")))?;
    let nodes = tree.result.nodes;
    let url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page.get_title().await.ok().flatten().unwrap_or_default();
    let ready_state = read_ready_state(page).await.unwrap_or_default();

    let by_id: std::collections::HashMap<String, &AxNode> = nodes
        .iter()
        .map(|node| (node.node_id.as_ref().to_string(), node))
        .collect();
    let mut roots: Vec<&AxNode> = nodes
        .iter()
        .filter(|node| {
            node.parent_id
                .as_ref()
                .map(|parent| !by_id.contains_key(parent.as_ref()))
                .unwrap_or(true)
        })
        .collect();
    roots.sort_by(|a, b| a.node_id.as_ref().cmp(b.node_id.as_ref()));

    let mut rendered: Vec<SnapshotNode> = Vec::new();
    let mut truncated = false;
    for root in roots {
        walk(
            root,
            &by_id,
            0,
            interactive_only,
            max_nodes,
            &mut rendered,
            &mut truncated,
        );
        if rendered.len() >= max_nodes {
            truncated = true;
            break;
        }
    }
    for (index, node) in rendered.iter_mut().enumerate() {
        node.index = index + 1;
    }
    let text = render_text(&url, &title, &ready_state, &rendered, truncated);
    Ok(BrowserSnapshot {
        target_id: target_id.to_string(),
        url,
        title,
        ready_state,
        captured_unix_ms: now_ms(),
        nodes: rendered,
        text,
        truncated,
    })
}

#[allow(clippy::too_many_arguments)]
fn walk(
    node: &AxNode,
    by_id: &std::collections::HashMap<String, &AxNode>,
    depth: usize,
    interactive_only: bool,
    max_nodes: usize,
    out: &mut Vec<SnapshotNode>,
    truncated: &mut bool,
) {
    if out.len() >= max_nodes {
        *truncated = true;
        return;
    }
    if depth > 16 {
        *truncated = true;
        return;
    }
    if should_emit(node, interactive_only) {
        let role = role_text(node);
        let interactive = is_interactive_role(&role);
        let protected = property_flag(node, "protected");
        let value = node.value.as_ref().and_then(value_text).map(|text| {
            if protected {
                "[redacted]".to_string()
            } else {
                text
            }
        });
        let name = node.name.as_ref().and_then(value_text);
        let backend_node_id = node.backend_dom_node_id.as_ref().map(|id| *id.inner());
        let checked = node
            .properties
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|property| property_name(property) == "checked")
            .and_then(|property| property.value.value.as_ref())
            .and_then(|value| value.as_bool());
        let focused = property_flag(node, "focused");
        let disabled = property_flag(node, "disabled");
        let url = node
            .properties
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|property| property_name(property) == "url")
            .and_then(|property| property.value.value.as_ref())
            .and_then(|value| value.as_str())
            .map(str::to_string);
        out.push(SnapshotNode {
            index: 0,
            ref_id: if interactive && backend_node_id.is_some() {
                Some(format!("e{}", out.len() + 1))
            } else {
                None
            },
            backend_node_id,
            role,
            name,
            value,
            depth,
            interactive,
            disabled,
            checked,
            focused,
            url,
        });
    }
    if let Some(children) = &node.child_ids {
        for child in children {
            if let Some(child) = by_id.get(child.as_ref()) {
                walk(
                    child,
                    by_id,
                    depth + 1,
                    interactive_only,
                    max_nodes,
                    out,
                    truncated,
                );
            }
        }
    }
}

async fn read_ready_state(page: &chromiumoxide::Page) -> Option<String> {
    let result = page.evaluate("document.readyState").await.ok()?;
    result
        .value()
        .and_then(|value| value.as_str().map(str::to_string))
}

/// Render the text form. `baseline` turns this into a diff.
pub fn render_text(
    url: &str,
    title: &str,
    ready_state: &str,
    nodes: &[SnapshotNode],
    truncated: bool,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("url: {url}\n"));
    out.push_str(&format!("title: {title}\n"));
    out.push_str(&format!("ready: {ready_state}\n"));
    for node in nodes {
        out.push_str(&format_node(node));
        out.push('\n');
    }
    if truncated {
        out.push_str("… truncated\n");
    }
    out
}

/// Render a diff against `baseline`.
pub fn render_diff(current: &BrowserSnapshot, baseline: &BrowserSnapshot) -> String {
    let mut out = String::new();
    if current.url != baseline.url {
        out.push_str(&format!("~ url: {} -> {}\n", baseline.url, current.url));
    }
    if current.title != baseline.title {
        out.push_str(&format!(
            "~ title: {} -> {}\n",
            baseline.title, current.title
        ));
    }
    let key = |node: &SnapshotNode| {
        node.backend_node_id
            .map(|id| format!("b{id}"))
            .unwrap_or_else(|| format!("{}|{}", node.role, node.name.clone().unwrap_or_default()))
    };
    let old: std::collections::HashMap<String, &SnapshotNode> = baseline
        .nodes
        .iter()
        .map(|node| (key(node), node))
        .collect();
    let new: std::collections::HashMap<String, &SnapshotNode> =
        current.nodes.iter().map(|node| (key(node), node)).collect();
    for node in &current.nodes {
        match old.get(&key(node)) {
            None => out.push_str(&format!("+ {}\n", format_node(node))),
            Some(previous) => {
                let changed = previous.role != node.role
                    || previous.name != node.name
                    || previous.value != node.value
                    || previous.disabled != node.disabled
                    || previous.checked != node.checked;
                if changed {
                    out.push_str(&format!(
                        "~ {} -> {}\n",
                        format_node(previous),
                        format_node(node)
                    ));
                }
            }
        }
    }
    for node in &baseline.nodes {
        if !new.contains_key(&key(node)) {
            out.push_str(&format!("- {}\n", format_node(node)));
        }
    }
    if current.truncated {
        out.push_str("… truncated\n");
    }
    out
}

fn format_node(node: &SnapshotNode) -> String {
    let indent = "  ".repeat(node.depth.min(8));
    let mut line = format!("{}[{}] {}", indent, node.index, node.role);
    if let Some(name) = &node.name {
        line.push_str(&format!(" \"{}\"", name.replace('"', "'")));
    }
    if let Some(ref_id) = &node.ref_id {
        line.push_str(&format!(" ref={ref_id}"));
    }
    if let Some(value) = &node.value {
        line.push_str(&format!(" value=\"{}\"", value.replace('"', "'")));
    }
    if node.disabled {
        line.push_str(" disabled");
    }
    if let Some(checked) = node.checked {
        line.push_str(if checked { " checked" } else { " unchecked" });
    }
    if node.focused {
        line.push_str(" focused");
    }
    if let Some(url) = &node.url {
        line.push_str(&format!(" url=\"{url}\""));
    }
    line
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn cache_path(session: Option<&str>, target_id: &str) -> Result<PathBuf, ComputerError> {
    let dir = cache::session_dir(session)?;
    let safe: String = target_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    Ok(dir.join(format!("browser-{safe}.json")))
}

/// Store a snapshot as the diff baseline for `--session`.
pub fn store(session: Option<&str>, snapshot: &BrowserSnapshot) -> Result<(), ComputerError> {
    let path = cache_path(session, &snapshot.target_id)?;
    write_private(&path, snapshot)
}

/// Load the last snapshot for `--session` and target.
pub fn load(
    session: Option<&str>,
    target_id: &str,
) -> Result<Option<BrowserSnapshot>, ComputerError> {
    let path = cache_path(session, target_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(ComputerError::Io)?;
    serde_json::from_str(&text).map(Some).map_err(|error| {
        ComputerError::ExecutionFailed(format!(
            "browser snapshot {} is not readable: {error}",
            path.display()
        ))
    })
}

/// Remove the cached snapshot for a target.
pub fn clear(session: Option<&str>, target_id: &str) -> Result<(), ComputerError> {
    let path = cache_path(session, target_id)?;
    if path.exists() {
        fs::remove_file(path).map_err(ComputerError::Io)?;
    }
    Ok(())
}

fn write_private(path: &std::path::Path, value: &impl Serialize) -> Result<(), ComputerError> {
    let text = serde_json::to_string(value).map_err(|error| {
        ComputerError::ExecutionFailed(format!("browser snapshot encode failed: {error}"))
    })?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(ComputerError::Io)?;
    file.write_all(text.as_bytes()).map_err(ComputerError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_load_and_clear_cover_missing_corrupt_and_present() {
        let cache = crate::cache::CacheEnv::acquire();
        let session = Some("snapshot-cache");
        assert!(load(session, "target").unwrap().is_none());
        clear(session, "target").unwrap();

        let path = cache_path(session, "target").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json").unwrap();
        let error = load(session, "target").unwrap_err();
        assert!(error.to_string().contains("not readable"), "{error}");
        clear(session, "target").unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(cache.dir());
    }

    fn node(index: usize, role: &str, name: &str) -> SnapshotNode {
        SnapshotNode {
            index,
            ref_id: Some(format!("e{index}")),
            backend_node_id: Some(index as i64),
            role: role.into(),
            name: Some(name.into()),
            value: None,
            depth: 0,
            interactive: true,
            disabled: false,
            checked: None,
            focused: false,
            url: None,
        }
    }

    #[test]
    fn diff_reports_added_removed_and_changed() {
        let baseline = BrowserSnapshot {
            target_id: "t".into(),
            url: "https://example.com/".into(),
            title: "Example".into(),
            ready_state: "complete".into(),
            captured_unix_ms: 0,
            nodes: vec![node(1, "button", "Save"), node(2, "link", "Help")],
            text: String::new(),
            truncated: false,
        };
        let current = BrowserSnapshot {
            target_id: "t".into(),
            url: "https://example.com/next".into(),
            title: "Next".into(),
            ready_state: "complete".into(),
            captured_unix_ms: 1,
            nodes: vec![node(1, "button", "Save"), node(3, "button", "Send")],
            text: String::new(),
            truncated: false,
        };
        let diff = render_diff(&current, &baseline);
        assert!(diff.contains("+ [3] button \"Send\""));
        assert!(diff.contains("- [2] link \"Help\""));
        assert!(diff.contains("~ url:"));
    }

    #[test]
    fn value_text_covers_every_json_shape() {
        fn ax(value: serde_json::Value) -> AxValue {
            serde_json::from_value(serde_json::json!({"type": "string", "value": value}))
                .expect("AxValue")
        }
        assert_eq!(
            value_text(&ax(serde_json::json!("  hi "))),
            Some("hi".into())
        );
        assert_eq!(value_text(&ax(serde_json::json!("   "))), None);
        assert_eq!(
            value_text(&ax(serde_json::json!(true))),
            Some("true".into())
        );
        assert_eq!(value_text(&ax(serde_json::json!(4))), Some("4".into()));
        assert_eq!(value_text(&ax(serde_json::Value::Null)), None);
        assert_eq!(value_text(&ax(serde_json::json!([1]))), Some("[1]".into()));
    }

    #[test]
    fn render_text_and_diff_cover_truncation_and_changes() {
        let mut changed = node(1, "button", "Send");
        changed.disabled = true;
        changed.checked = Some(false);
        changed.focused = true;
        changed.url = Some("https://example.com/".into());
        changed.value = Some("v\"q".into());
        changed.depth = 20;
        let text = render_text("u", "t", "complete", &[changed.clone()], true);
        assert!(text.contains("… truncated"), "{text}");
        assert!(text.contains("disabled"), "{text}");
        assert!(text.contains("unchecked"), "{text}");
        assert!(text.contains("focused"), "{text}");
        assert!(text.contains("url=\"https://example.com/\""), "{text}");
        assert!(text.contains("value=\"v'q\""), "{text}");

        let baseline = BrowserSnapshot {
            target_id: "t".into(),
            url: String::new(),
            title: String::new(),
            ready_state: String::new(),
            captured_unix_ms: 0,
            nodes: vec![node(1, "button", "Save")],
            text: String::new(),
            truncated: false,
        };
        let mut current = baseline.clone();
        current.nodes = vec![changed];
        current.truncated = true;
        let diff = render_diff(&current, &baseline);
        assert!(
            diff.contains("~ ") && diff.contains("\"Save\"") && diff.contains("\"Send\""),
            "{diff}"
        );
        assert!(diff.contains("… truncated"), "{diff}");
    }

    #[test]
    fn cache_path_sanitizes_the_target_id() {
        let cache = crate::cache::CacheEnv::acquire();
        let path = cache_path(Some("s"), "a/b c").unwrap();
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("browser-a_b_c.json"));
        let _ = std::fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn refs_are_found_by_name() {
        let snapshot = BrowserSnapshot {
            target_id: "t".into(),
            url: String::new(),
            title: String::new(),
            ready_state: String::new(),
            captured_unix_ms: 0,
            nodes: vec![node(1, "button", "Save")],
            text: String::new(),
            truncated: false,
        };
        assert!(snapshot.find_ref("e1").is_some());
        assert!(snapshot.find_ref("e2").is_none());
    }
}
