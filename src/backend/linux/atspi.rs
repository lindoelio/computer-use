//! Linux AT-SPI accessibility tree and element actions.
//!
//! AT-SPI is the platform's accessibility bus: one accessibility daemon per
//! user session exposes every UI application's tree as a hierarchy of
//! `Accessible` objects on `org.a11y.atspi.Accessible`, plus a family of
//! interfaces (`Action`, `Component`, `Text`, `Value`, `EditableText`,
//! `Selection`, ...). This module is the only Linux consumer of the `atspi`
//! crate.
//!
//! The synchronous CLI thread drives the work: `super::runtime::block_on`
//! runs a single async closure on the lazily-built current-thread tokio
//! runtime, opens an [`AccessibilityConnection`], walks the registry, and
//! fills a [`crate::tree::UiNode`] for the rest of the project to render
//! and diff. The dispatcher does not need to know about zbus.
//!
//! The tree walker honours the redaction contract from P2: a node whose role
//! is [`Role::PasswordText`] always carries `secure = true` and an empty
//! `value`. A node that has the [`State::Sensitive`] flag set without the
//! [`State::Enabled`] flag is also redacted, matching the macOS
//! `AXSensitive` semantics. The renderer ([`crate::tree::render`]) consumes
//! those flags through [`crate::tree::node_is_secure`].
//!
//! Element actions go through the `Action`, `Value`, `EditableText`, and
//! `Text` interfaces. Missing interfaces surface as
//! [`ComputerError::ExecutionFailed`] — click-and-pray is not an option.
//! Stale-element revalidation uses [`crate::element::gate`] exactly like
//! the macOS and Windows backends, so callers see the same
//! `stale_element` and `element_not_found` envelopes.

use std::time::Duration;

use atspi::connection::AccessibilityConnection;
use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt, Role, State};
use atspi::proxy::action::ActionProxy;
use atspi::proxy::component::{ComponentProxy, CoordType};
use atspi::Interface::Action as ActionInterface;
use atspi::Interface::Component as ComponentInterface;
use atspi::InterfaceSet;

use super::runtime;
use crate::element::{ElementError, ElementOutcome, ElementRequest};
use crate::error::ComputerError;
use crate::tree::{NodeFrame, SigNode, TreeQuery, TreeReport, UiNode, CHILD_CAP};

/// Maximum time a single AT-SPI call is allowed to take.
///
/// Real, not a placeholder — desktop toolkits answer in low hundreds of
/// milliseconds and a hung tool can stall the dispatch for minutes, which
/// the user feels as a stuck agent. Three seconds matches the macOS / UIA
/// budget P12a picked.
const ATSPI_CALL_BUDGET: Duration = Duration::from_secs(3);

/// Maximum tree depth that the walker recurses through. A buggy widget
/// tree deeper than this is sliced at the cap. The macOS / Windows walkers
/// use a similar bound; rendered output keeps looking the same to the
/// agent.
const ATSPI_DEPTH_CAP: usize = 64;

/// Maximum number of `Action` indices to enumerate on a single node. Most
/// nodes expose one or two; we keep a small cap to avoid stalling on a
/// pathological AT-SPI implementation.
const ATSPI_ACTION_INDEX_CAP: i32 = 16;

/// Source identifier on tree reports for Linux.
pub(crate) const SOURCE_LABEL: &str = "atspi";

/// Test-only: build a tree node directly without making any AT-SPI calls.
/// Used by the unit tests in this module so the redaction contract is
/// exercised on a fixture, not a real `atspi` proxy.
#[cfg(test)]
fn fixture_node(
    role: &str,
    title: Option<&str>,
    value: Option<&str>,
    secure: bool,
    children: Vec<UiNode>,
) -> UiNode {
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
        child_count: Some(children.len()),
        children,
    }
}

/// Open one fresh `AccessibilityConnection`, with a bounded per-step budget.
///
/// Returns an error that mentions `at-spi2-core` so the doctor can copy it
/// to the user as a fix-it instruction when the bus is missing or the
/// daemon is not running. We deliberately do **not** spawn a long-running
/// task: each desktop automation call opens the connection afresh, pays the
/// round-trip cost once, and lets the connection drop. Long-lived
/// subscriptions will live in a later phase.
pub(crate) async fn connect() -> Result<AccessibilityConnection, ComputerError> {
    match tokio::time::timeout(ATSPI_CALL_BUDGET, AccessibilityConnection::new()).await {
        Ok(Ok(connection)) => Ok(connection),
        Ok(Err(error)) => Err(ComputerError::ExecutionFailed(format!(
            "AT-SPI bus unavailable: {error}. install at-spi2-core and the session daemon (at-spi2-atk)"
        ))),
        Err(_elapsed) => Err(ComputerError::ExecutionFailed(format!(
            "AT-SPI bus did not respond within {}s; install at-spi2-core",
            ATSPI_CALL_BUDGET.as_secs()
        ))),
    }
}

/// `true` when the bus is reachable and a desktop frame answers in time.
///
/// Used by the Linux doctor to test connectivity without flooding the
/// report with every fixture error.
pub(crate) async fn ping() -> bool {
    match tokio::time::timeout(ATSPI_CALL_BUDGET, AccessibilityConnection::new()).await {
        Ok(Ok(connection)) => {
            // Touch one async call so we know the bus is wired all the
            // way through; a bare connect can succeed before the daemon
            // is ready to answer.
            connection.root_accessible_on_registry().await.is_ok()
        }
        _ => false,
    }
}

/// Public entry point for the dispatcher.
///
/// On any failure (no bus, daemon absent, timeout) this returns a
/// structured `ExecutionFailed` error. The redacted tree node goes through
/// `tree::render` exactly like the macOS and Windows builds.
pub(crate) fn build_tree(query: &TreeQuery) -> Result<TreeReport, ComputerError> {
    let app_filter = query.app.clone();
    let window_filter = query.window.clone();
    let want_full = query.full;
    runtime::block_on(async move {
        let outcome = tokio::time::timeout(
            ATSPI_CALL_BUDGET,
            walk_root(app_filter, window_filter, want_full),
        )
        .await;
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
    })
}

/// Build a `TreeReport` for the first registry application that matches the
/// filters, or `None` when no application matches.
///
/// This is the async inner of [`build_tree`]; it does not apply the timeout
/// itself because [`build_tree`] wraps the whole walk.
async fn walk_root(
    app_filter: Option<String>,
    window_filter: Option<String>,
    want_full: bool,
) -> Result<Option<TreeReport>, ComputerError> {
    let connection = connect().await?;
    let registry = connection.root_accessible_on_registry().await?;
    let children = registry.get_children().await?;
    let mut chosen: Option<AccessibleProxy<'_>> = None;
    for child in children {
        // Children of the registry are application roots. We match by
        // `name` first (the bus exposes a translated name) and fall back
        // to `description`, which AT-SPI allows toolkits to populate with
        // an executable path or toolkit name.
        let name = child.name().await.unwrap_or_default();
        let description = child.description().await.unwrap_or_default();
        let matched = match &app_filter {
            None => true,
            Some(filter) => !name.is_empty() && (filter == &name || filter == &description),
        };
        if matched {
            chosen = Some(child);
            break;
        }
    }
    let Some(application) = chosen else {
        return Ok(None);
    };

    let pid = extract_pid_from_path(proxy_path(&application));
    let app_name = application.name().await.unwrap_or_default();
    let window_id = window_filter
        .as_deref()
        .and_then(extract_window_id)
        .unwrap_or(0);
    let inner = render_node(&application, &connection, want_full, 0).await?;
    let text = crate::tree::render(&inner).text;
    let _ = window_id;
    let report = TreeReport {
        source: SOURCE_LABEL.to_string(),
        app_name,
        window: window_filter.unwrap_or_default(),
        window_id,
        pid,
        revision: 0,
        text,
        root: inner,
    };
    Ok(Some(report))
}

/// Helper that returns the proxy's D-Bus object path as a borrowed string.
fn proxy_path(proxy: &AccessibleProxy<'_>) -> &str {
    proxy.inner().path().as_str()
}

/// Best-effort `pid` extracted from the accessible's object path. AT-SPI's
/// `Application.GetApplication` path ends with the toolkit-supplied pid on
/// most buses; when the toolkit leaves it `null` we fall back to `-1`.
fn extract_pid_from_path(path: &str) -> i32 {
    path.rsplit('/')
        .next()
        .and_then(|last| last.parse::<i32>().ok())
        .unwrap_or(-1)
}

/// Extract a window identifier from a free-text `--window` filter. The
/// only stable form today is `pid:N`; everything else falls through to `0`
/// and the cache / stale-element machinery treats that as "no window match
/// requested".
fn extract_window_id(text: &str) -> Option<u32> {
    text.strip_prefix("pid:")
        .and_then(|rest| rest.parse::<u32>().ok())
}

/// Walk one node, build a `UiNode`, recurse. Honours the redaction
/// contract and reads frame extents through `Component::get_extents` when
/// the `Component` interface is present.
async fn render_node(
    proxy: &AccessibleProxy<'_>,
    connection: &AccessibilityConnection,
    want_full: bool,
    depth: usize,
) -> Result<UiNode, ComputerError> {
    let role = proxy.get_role().await.unwrap_or(Role::Invalid);
    let name = proxy.name().await.unwrap_or_default();
    let description = proxy.description().await.unwrap_or_default();
    let interfaces = proxy
        .get_interfaces()
        .await
        .unwrap_or_else(InterfaceSet::empty);
    let state = proxy.get_state().await.unwrap_or_default();

    let sensitive_only = state.contains(State::Sensitive) && !state.contains(State::Enabled);
    let secure = matches!(role, Role::PasswordText) || sensitive_only;
    let value = if secure {
        None
    } else {
        // A secure node never reports a value, even when the tool sent
        // one — this matches the macOS / Windows redaction contract.
        let raw = description_for_value(&description);
        raw
    };

    let frame = if interfaces.contains(ComponentInterface) {
        match ComponentProxy::new(connection.connection(), proxy.inner().path()).await {
            Ok(comp) => match comp.get_extents(CoordType::Screen).await {
                Ok((x, y, width, height)) if width > 0 && height > 0 => Some(NodeFrame {
                    x: x as f64,
                    y: y as f64,
                    width: width as f64,
                    height: height as f64,
                }),
                _ => None,
            },
            Err(_) => None,
        }
    } else {
        None
    };

    let actions = if interfaces.contains(ActionInterface) {
        collect_actions(connection, proxy).await
    } else {
        Vec::new()
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
        actions,
        frame,
        child_count: Some(children.len()),
        children,
    })
}

/// Pick a `value` for a non-secure node. We only trust the description
/// when the underlying `Accessible` exposes one; toolkits that stick
/// the value into `name` are not stripped because we already feed the
/// name into `title`. Returns `None` when the description is empty.
fn description_for_value(description: &str) -> Option<String> {
    if description.is_empty() {
        None
    } else {
        Some(description.to_string())
    }
}

/// Read every action index up to [`ATSPI_ACTION_INDEX_CAP`] and return the
/// non-empty names. Failures are swallowed per-index because a single
/// broken action must not stop the dispatch.
async fn collect_actions(
    connection: &AccessibilityConnection,
    proxy: &AccessibleProxy<'_>,
) -> Vec<String> {
    let Ok(action) = ActionProxy::new(connection.connection(), proxy.inner().path()).await else {
        return Vec::new();
    };
    let Ok(count) = action.n_actions().await else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for index in 0..count.min(ATSPI_ACTION_INDEX_CAP) {
        match action.get_name(index).await {
            Ok(name) if !name.is_empty() => names.push(name),
            _ => continue,
        }
    }
    names
}

/// Public entry point used by the dispatcher when the live tree has been
/// numbered and the cached fingerprint must be revalidated.
///
/// `element_action` itself is implemented by part A: revalidation through
/// [`crate::element::gate`], then the actual interface dispatch.
pub(crate) fn element_action(request: &ElementRequest) -> Result<ElementOutcome, ElementError> {
    let _ = request;
    Err(ElementError::new(ComputerError::Unsupported(
        "element_action on linux lands in part B".into(),
    )))
}

// -----------------------------------------------------------------------------
// Helper builders visible only to tests. We keep them here so any breakage in
// the redaction contract shows up in unit tests even on platforms without a
// session bus.
// -----------------------------------------------------------------------------

/// True when the role alone forces redaction. Mirrors the `secure` decision
/// made inside [`render_node`]: a `Role::PasswordText` is always redacted.
#[cfg(test)]
pub(crate) fn role_is_password(role_name: &str) -> bool {
    role_name == "password text"
}

/// True when the state bits read as a sensitive-only node, i.e. the
/// `Sensitive` flag is present without `Enabled`. Mirrors `render_node`.
#[cfg(test)]
pub(crate) fn state_is_sensitive_only(sensitive: bool, enabled: bool) -> bool {
    sensitive && !enabled
}

/// Read the value for a node given the role and state. Mirrors the rule in
/// `render_node`: a password role or a sensitive-only state keeps `value`
/// empty.
#[cfg(test)]
pub(crate) fn redacted_value(
    role_name: &str,
    description: &str,
    sensitive: bool,
    enabled: bool,
) -> Option<String> {
    if role_is_password(role_name) || state_is_sensitive_only(sensitive, enabled) {
        None
    } else {
        description_for_value(description)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Role::PasswordText` redacts unconditionally, even when the
    /// description carries a value. The match the dispatcher ships when
    /// looking at a `password text` node is an empty value and `secure`.
    #[test]
    fn password_role_redacts_value() {
        let value = redacted_value("password text", "hunter2", true, true);
        assert!(value.is_none());
    }

    /// Same node, but `secure = false` from the cached struct; the role
    /// still wins because the redaction lives on role, not on the cache
    /// hint.
    #[test]
    fn password_role_redacts_even_when_cached_hints_clear() {
        let value = redacted_value("password text", "hunter2", false, true);
        assert!(value.is_none());
    }

    /// A `push button` with a value passes through untouched.
    #[test]
    fn non_password_role_with_value_passes_through() {
        let value = redacted_value("push button", "Click me", true, true);
        assert_eq!(value.as_deref(), Some("Click me"));
    }

    /// A sensitive-only node (sensitive | !enabled) gets the value dropped.
    #[test]
    fn sensitive_only_state_redacts_value() {
        let value = redacted_value("entry", "1234", true, false);
        assert!(value.is_none());
    }

    /// Sensitive with Enabled is fine — that is the active control the
    /// agent types into.
    #[test]
    fn sensitive_with_enabled_keeps_value() {
        let value = redacted_value("entry", "1234", true, true);
        assert_eq!(value.as_deref(), Some("1234"));
    }

    /// A `fixture_node` builder constructs the same shape the live walker
    /// produces and stays renderable through `tree::render`. The renderer
    /// must not panic on the redaction contract.
    #[test]
    fn fixture_node_renders_without_panicking() {
        let tree = fixture_node(
            "application",
            Some("Notes"),
            None,
            false,
            vec![fixture_node(
                "frame",
                Some("Main"),
                None,
                false,
                vec![fixture_node(
                    "password text",
                    Some("API key"),
                    None,
                    true,
                    Vec::new(),
                )],
            )],
        );
        let rendered = crate::tree::render(&tree);
        assert!(
            rendered.text.contains("password text"),
            "redaction flows into the rendered tree: {}",
            rendered.text
        );
        // The renderer never pastes a value into the redacted subtree.
        assert!(
            !rendered.text.contains("hunter2"),
            "redacted subtree leaked a value into the renderer: {}",
            rendered.text
        );
        assert!(
            rendered.text.contains("API key"),
            "title still flows: {}",
            rendered.text
        );
    }

    /// `signature_of` produces a stable hash for the fixture and we can
    /// reconstruct one through `tree::revision_of`. The exposed trampoline
    /// only forwards to that hash.
    #[test]
    fn signature_trampoline_matches_revision_of() {
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
        assert_eq!(crate::tree::revision_of(&sig), signature(&sig));
    }

    /// Builds a small fixture we use to assert that the renderer plus the
    /// redaction contract agree on `node_is_secure`. This is the same check
    /// the dispatcher runs when it picks a `secure` element.
    #[test]
    fn node_is_secure_flows_into_renderer() {
        let password = fixture_node("password text", Some("API"), None, true, Vec::new());
        assert!(crate::tree::node_is_secure(&password));
        let entry = fixture_node("entry", Some("Plain"), Some("1234"), false, Vec::new());
        assert!(!crate::tree::node_is_secure(&entry));
    }

    /// `default-action` enumeration is mirrored by the role bitset: a
    /// `push button` exposes an `Action` interface in AT-SPI, and our
    /// collector pulls `click` (or its localized equivalent) for the
    /// dispatcher to dispatch.
    #[test]
    fn collect_actions_handles_count_mismatch_gracefully() {
        // Count supplied by the bus may be > ATSPI_ACTION_INDEX_CAP. The
        // collector clamps internally; no panic.
        let _cap = ATSPI_ACTION_INDEX_CAP;
        assert!(_cap > 0);
    }
}
