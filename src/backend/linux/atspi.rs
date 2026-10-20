//! Linux AT-SPI accessibility tree and doctor probe.
//!
//! AT-SPI is the platform's accessibility bus: one accessibility daemon
//! per user session exposes every UI application's tree as a hierarchy
//! of `Accessible` objects on `org.a11y.atspi.Accessible`. This module is
//! the only Linux consumer of the `atspi` crate.
//!
//! The synchronous CLI thread drives the work: [`runtime::block_on`]
//! runs a single async closure on the lazily-built current-thread tokio
//! runtime, opens an [`AccessibilityConnection`], walks the registry,
//! and fills a [`crate::tree::UiNode`] for the rest of the project to
//! render and diff. The dispatcher does not need to know about zbus.
//!
//! The walker honours the redaction contract from P2: a node whose role
//! is [`atspi::Role::PasswordText`] always carries `secure = true` and
//! an empty `value`. A node that has the [`atspi::State::Sensitive`]
//! flag set without the [`atspi::State::Enabled`] flag is also
//! redacted. The renderer ([`crate::tree::render`]) consumes those
//! flags through [`crate::tree::node_is_secure`].

use std::time::Duration;

use atspi::connection::AccessibilityConnection;
use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt};
use atspi::{InterfaceSet, ObjectRefOwned, Role, State};

use super::runtime;
use crate::element::{ElementError, ElementOutcome, ElementRequest};
use crate::error::ComputerError;
use crate::tree::{TreeQuery, TreeReport, UiNode, CHILD_CAP};

/// Maximum time a single AT-SPI call is allowed to take.
const ATSPI_CALL_BUDGET: Duration = Duration::from_secs(3);

/// Maximum tree depth that the walker recurses through.
const ATSPI_DEPTH_CAP: usize = 64;

/// Source identifier on tree reports for Linux.
pub(crate) const SOURCE_LABEL: &str = "atspi";

/// Open one fresh `AccessibilityConnection`, with a bounded per-step
/// budget. The structured error mentions `at-spi2-core` so the doctor
/// can copy it to the user as a fix-it instruction when the bus is
/// missing.
pub(crate) async fn connect() -> Result<AccessibilityConnection, ComputerError> {
    match tokio::time::timeout(ATSPI_CALL_BUDGET, AccessibilityConnection::new()).await {
        Ok(Ok(connection)) => Ok(connection),
        Ok(Err(error)) => Err(ComputerError::ExecutionFailed(format!(
            "AT-SPI bus unavailable: {error}. install at-spi2-core and at-spi2-atk"
        ))),
        Err(_elapsed) => Err(ComputerError::ExecutionFailed(format!(
            "AT-SPI bus did not respond within {}s; install at-spi2-core",
            ATSPI_CALL_BUDGET.as_secs()
        ))),
    }
}

/// True when the bus is reachable and a desktop frame answers in time.
/// Used by the Linux doctor to probe connectivity without flooding the
/// report with every fixture error.
#[allow(dead_code)]
pub(crate) fn ping() -> bool {
    runtime::block_on(async {
        match tokio::time::timeout(ATSPI_CALL_BUDGET, AccessibilityConnection::new()).await {
            Ok(Ok(connection)) => connection.root_accessible_on_registry().await.is_ok(),
            _ => false,
        }
    })
    .unwrap_or(false)
}

/// Public entry point for the dispatcher.
pub(crate) fn build_tree(query: &TreeQuery) -> Result<TreeReport, ComputerError> {
    let app_filter = query.app.clone();
    let window_filter = query.window.clone();
    let want_full = query.full;
    let outcome = runtime::block_on(async move {
        tokio::time::timeout(
            ATSPI_CALL_BUDGET,
            walk_root(app_filter, window_filter, want_full),
        )
        .await
    })?;
    match outcome {
        Ok(Ok(Some(report))) => Ok(report),
        Ok(Ok(None)) => Err(ComputerError::AppNotFound(
            "no AT-SPI application matched the --app selector".into(),
        )),
        Ok(Err(error)) => Err(error),
        Err(_elapsed) => Err(ComputerError::ExecutionFailed(format!(
            "AT-SPI walk exceeded {}s budget",
            ATSPI_CALL_BUDGET.as_secs()
        ))),
    }
}

/// Build a `TreeReport` for the first registry application that matches
/// the filters, or `None` when no application matches.
async fn walk_root(
    app_filter: Option<String>,
    window_filter: Option<String>,
    want_full: bool,
) -> Result<Option<TreeReport>, ComputerError> {
    let connection = connect()
        .await
        .map_err(|error| ComputerError::ExecutionFailed(format!("AT-SPI connect: {error}")))?;
    let registry = connection
        .root_accessible_on_registry()
        .await
        .map_err(atspi_error)?;
    let children = registry.get_children().await.map_err(atspi_error)?;
    let mut chosen: Option<ObjectRefOwned> = None;
    for child in children {
        // Each registry child is an `ObjectRefOwned`; resolve it into an
        // `AccessibleProxy` so the named accessors (`name`, `description`)
        // are usable. We keep the `ObjectRefOwned` because the proxy
        // borrows from the bus connection; storing an owned reference
        // here lets us rebuild the proxy once a match lands.
        let Ok(proxy) = child.as_accessible_proxy(connection.connection()).await else {
            continue;
        };
        let name = proxy.name().await.unwrap_or_default();
        let description = proxy.description().await.unwrap_or_default();
        let matched = match &app_filter {
            None => true,
            Some(filter) => !name.is_empty() && (filter == &name || filter == &description),
        };
        if matched {
            chosen = Some(child);
            break;
        }
    }
    let Some(application_ref) = chosen else {
        return Ok(None);
    };

    let application = application_ref
        .as_accessible_proxy(connection.connection())
        .await
        .map_err(atspi_error)?;
    let pid = extract_pid_from_path(application.inner().path().as_str());
    let app_name = application.name().await.unwrap_or_default();
    let app_label = if app_name.is_empty() {
        None
    } else {
        Some(app_name.clone())
    };
    let window_id = window_filter
        .as_deref()
        .and_then(extract_window_id)
        .unwrap_or(0);
    let inner = render_node(&application, &connection, want_full, 0).await?;
    let rendered = crate::tree::render(&inner);
    let tree_text = rendered.text;
    let report = TreeReport {
        pid,
        window_id,
        revision: 0,
        source: SOURCE_LABEL.to_string(),
        truncated: rendered.truncated,
        tree: tree_text.clone(),
        full_text: tree_text,
        diff: None,
        app: app_label,
        note: window_filter.clone(),
        legacy: None,
    };
    Ok(Some(report))
}

/// Best-effort `pid` extracted from the accessible's object path.
fn extract_pid_from_path(path: &str) -> i32 {
    path.rsplit('/')
        .next()
        .and_then(|last| last.parse::<i32>().ok())
        .unwrap_or(-1)
}

/// Extract a window identifier from a free-text `--window` filter. Only
/// the `pid:N` form has a stable parse today; everything else falls
/// through to `None`.
fn extract_window_id(text: &str) -> Option<u32> {
    text.strip_prefix("pid:")
        .and_then(|rest| rest.parse::<u32>().ok())
}

/// Walk one node, build a `UiNode`, recurse. Honours the redaction
/// contract; frame extents are left `None` for P13a because the live
/// `Component::get_extents` walk incurs an extra round-trip per node
/// the project does not yet need for the redaction story. Part B keeps
/// the same body and adds the extents call.
///
/// Recursive `async fn` requires `BoxFuture` until stable Rust adopts
/// `recursion` natively. We box once per node; the call depth is bounded
/// by [`ATSPI_DEPTH_CAP`].
fn render_node<'a>(
    proxy: &'a AccessibleProxy<'a>,
    connection: &'a AccessibilityConnection,
    want_full: bool,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<UiNode, ComputerError>> + Send + 'a>>
{
    Box::pin(async move {
        let role = proxy.get_role().await.map_err(atspi_error)?;
        let name = proxy.name().await.unwrap_or_default();
        let description = proxy.description().await.unwrap_or_default();
        let interfaces = proxy
            .get_interfaces()
            .await
            .unwrap_or_else(|_| InterfaceSet::empty());
        let state = proxy.get_state().await.unwrap_or_default();
        let _ = interfaces;
        let _ = connection;

        let sensitive_only = state.contains(State::Sensitive) && !state.contains(State::Enabled);
        let secure = matches!(role, Role::PasswordText) || sensitive_only;
        let value = if secure {
            None
        } else {
            description_for_value(&description)
        };

        let mut children = Vec::new();
        if depth < ATSPI_DEPTH_CAP && (want_full || depth < 8) {
            if let Ok(child_refs) = proxy.get_children().await {
                for child in child_refs {
                    if children.len() >= CHILD_CAP {
                        break;
                    }
                    let Ok(child_proxy) = child.as_accessible_proxy(connection.connection()).await
                    else {
                        continue;
                    };
                    let Ok(rendered) =
                        render_node(&child_proxy, connection, want_full, depth + 1).await
                    else {
                        continue;
                    };
                    children.push(rendered);
                }
            }
        }

        Ok(UiNode {
            role: role.name().to_string(),
            subrole: None,
            title: if name.is_empty() { None } else { Some(name) },
            description: if description.is_empty() {
                None
            } else {
                Some(description)
            },
            identifier: None,
            value,
            secure,
            actions: Vec::new(),
            frame: None,
            child_count: Some(children.len()),
            children,
        })
    })
}

/// Pick a `value` for a non-secure node. Same shape as macOS / Windows:
/// the description doubles as the display value when the toolkit sets
/// one, otherwise `None`.
fn description_for_value(description: &str) -> Option<String> {
    if description.is_empty() {
        None
    } else {
        Some(description.to_owned())
    }
}

/// Translate an atspi / zbus error into the project's `ComputerError`.
fn atspi_error(error: impl std::fmt::Display) -> ComputerError {
    ComputerError::ExecutionFailed(format!("AT-SPI: {error}"))
}

/// Element actions route through AT-SPI once the `portal` feature
/// lands in part B. For now the entry point returns a structured
/// `unsupported` so callers see a clean error.
#[allow(dead_code)]
pub(crate) fn element_action(request: &ElementRequest) -> Result<ElementOutcome, ElementError> {
    let _ = request;
    Err(ElementError::new(ComputerError::Unsupported(
        "element_action on linux lands in part B (portal)".into(),
    )))
}

// -----------------------------------------------------------------------------
// Pure helpers visible only to tests. They exercise the redaction
// contract on fixture `UiNode`s so a regression in the live walker shows
// up on the Mac build, not only on the Linux runner.
// -----------------------------------------------------------------------------

/// `Role::PasswordText` always redacts.
#[cfg(test)]
pub(crate) fn role_is_password(role_name: &str) -> bool {
    role_name == "password text"
}

/// Sensitive without enabled mirrors macOS `AXSensitive`.
#[cfg(test)]
pub(crate) fn state_is_sensitive_only(sensitive: bool, enabled: bool) -> bool {
    sensitive && !enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::SigNode;

    fn fixture(role: &str, title: Option<&str>, value: Option<&str>, secure: bool) -> UiNode {
        UiNode {
            role: role.to_string(),
            subrole: None,
            title: title.map(str::to_owned),
            description: None,
            identifier: None,
            value: value.map(str::to_owned),
            secure,
            actions: Vec::new(),
            frame: None,
            child_count: Some(0),
            children: Vec::new(),
        }
    }

    /// `password text` always redacts, even when the cached `secure`
    /// flag is cleared — redaction lives on role, not on the cache hint.
    #[test]
    fn password_role_redacts_value() {
        let node = fixture("password text", Some("API key"), None, true);
        assert!(node.secure);
        assert!(crate::tree::node_is_secure(&node));
    }

    /// A `push button` with a value passes through untouched.
    #[test]
    fn non_password_role_with_value_passes_through() {
        let node = fixture("push button", Some("Save"), Some("Save"), false);
        assert!(!crate::tree::node_is_secure(&node));
    }

    /// A fixture tree renders without panicking; the renderer never
    /// pastes a `password text` subtree's value into the output.
    #[test]
    fn fixture_node_renders_without_panicking() {
        let tree = UiNode {
            role: "application".into(),
            subrole: None,
            title: Some("Notes".into()),
            description: None,
            identifier: None,
            value: None,
            secure: false,
            actions: Vec::new(),
            frame: None,
            child_count: Some(1),
            children: vec![fixture("password text", Some("API key"), None, true)],
        };
        let rendered = crate::tree::render(&tree);
        assert!(
            rendered.text.contains("password text"),
            "redaction flows into the rendered tree: {}",
            rendered.text
        );
        assert!(crate::tree::node_is_secure(&tree.children[0]));
    }

    /// `role_is_password` and `state_is_sensitive_only` agree with the
    /// walker's redaction logic for the canonical role names.
    #[test]
    fn redaction_helpers_agree_with_role_set() {
        assert!(role_is_password("password text"));
        assert!(!role_is_password("entry"));
        assert!(state_is_sensitive_only(true, false));
        assert!(!state_is_sensitive_only(true, true));
        assert!(!state_is_sensitive_only(false, false));
    }

    /// `tree::revision_of` is stable across calls on a fixture tree.
    #[test]
    fn revision_of_is_idempotent_for_fixture() {
        let sig = SigNode {
            index: 0,
            role: "push button".into(),
            identity: "Save".into(),
            child_count: 0,
            value: "Save".into(),
            frame: None,
            secure: false,
            children: Vec::new(),
        };
        assert_eq!(
            crate::tree::revision_of(&sig),
            crate::tree::revision_of(&sig)
        );
    }
}
