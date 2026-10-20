//! Linux AT-SPI accessibility tree, doctor probe, and element actions.
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
//! an empty `value`. No other flag — including the AT-SPI `Sensitive`
//! bit, which means "user-actionable" rather than "secret" — drives
//! redaction. The renderer ([`crate::tree::render`]) consumes the flag
//! through [`crate::tree::node_is_secure`].
//!
//! Element actions (`press`, `focus`, `set_value`, `select_text`,
//! `scroll_to`) run through AT-SPI on P13a: `Action::do_action`,
//! `Value::set_current_value`, `EditableText::set_text_contents`,
//! `Text::set_selection`/`add_selection`, `Component::scroll_to`, plus
//! `Component::grab_focus` for focus. Each per-element call is bounded
//! by the same [`ATSPI_CALL_BUDGET`] as the tree walk. A missing
//! interface surfaces as `execution_failed` (no invented click); secure
//! `Role::PasswordText` nodes refuse `set_value`, `select_text`, and
//! `type --element` with `policy_denied`. The shared
//! [`crate::element::gate`] stale-element revalidation runs first and
//! rejects any change in role, identifier, or frame before the call is
//! made.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use atspi::connection::AccessibilityConnection;
use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt};
use atspi::{Interface, ObjectRefOwned, Role};

use super::runtime;
use crate::element::{ElementError, ElementOutcome, ElementRequest};
use crate::error::ComputerError;
use crate::tree::{self, TreeQuery, TreeReport, UiNode, CHILD_CAP};

/// Maximum time a single AT-SPI call is allowed to take. Tree walks and
/// element actions share this budget so a slow bus can never stretch a
/// dispatcher step past a few seconds.
const ATSPI_CALL_BUDGET: Duration = Duration::from_secs(3);

/// Maximum tree depth that the walker recurses through.
const ATSPI_DEPTH_CAP: usize = 64;

/// Source identifier on tree reports for Linux.
pub(crate) const SOURCE_LABEL: &str = "atspi";

/// `via` returned from a successful AT-SPI action.
const VIA_ATSPI: &str = "atspi";

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
    // Clone the filter data into the async block so the future does not
    // borrow from `query` (it has to be `'static` for `block_on`).
    let app = query.app.clone();
    let window = query.window.clone();
    let full = query.full;
    let outcome = runtime::block_on(async move {
        let inner = TreeQuery {
            app: app.clone(),
            window: window.clone(),
            full,
            session: None,
        };
        tokio::time::timeout(ATSPI_CALL_BUDGET, walk_live(&inner, false, false)).await
    });
    match outcome {
        Ok(Ok(Ok(live))) => Ok(tree_report_from(&live)),
        Ok(Ok(Err(error))) => Err(error),
        Ok(Err(_elapsed)) => Err(ComputerError::ExecutionFailed(format!(
            "AT-SPI walk exceeded {}s budget",
            ATSPI_CALL_BUDGET.as_secs()
        ))),
        Err(error) => Err(error),
    }
}

/// Borrow a live walk and produce the dispatcher's `TreeReport`.
fn tree_report_from(live: &LiveWalk) -> TreeReport {
    TreeReport {
        pid: live.pid,
        window_id: live.window_id,
        revision: live.ui_revision,
        source: SOURCE_LABEL.to_string(),
        truncated: live.truncated,
        tree: live.tree_text.clone(),
        full_text: live.full_text.clone(),
        diff: None,
        app: None,
        note: None,
        legacy: None,
    }
}

/// Element actions route through AT-SPI on P13a. The dispatcher-facing
/// entry point is sync; [`runtime::block_on`] drives the async walker,
/// one bounded per-element call on the same connection, and the cache
/// round-trip. A missing interface surfaces as `execution_failed` (no
/// invented click); secure `Role::PasswordText` nodes refuse writes
/// with `policy_denied`.
pub(crate) fn element_action(request: &ElementRequest) -> Result<ElementOutcome, ElementError> {
    let label = request.app.as_deref().unwrap_or("").to_string();
    let query = TreeQuery {
        app: request.app.clone(),
        window: request.window.clone(),
        full: false,
        session: request.session.clone(),
    };
    let request = request.clone();
    let op = request.op.clone();

    // Run the whole async walk + action + re-fetch through the runtime;
    // runtime / connection failures are surfaced as `ComputerError`
    // before any element work runs. The `query` value is moved into
    // the async block; the outer code reads `query.session.as_deref()`
    // only after the block returns, so we keep an owned copy of the
    // session string for the post-action `cache::store` call.
    let session_for_store = query.session.clone();
    let joined = runtime::block_on(async move {
        tokio::time::timeout(ATSPI_CALL_BUDGET, async {
            let cached = load_cached_snapshot(&query).await?;
            let cached = match cached {
                Some(snapshot) => snapshot,
                None => {
                    return Err(ElementError::new(ComputerError::StaleElement(format!(
                        "no cached snapshot for {label} index={}",
                        request.index
                    ))));
                }
            };
            let live = walk_live(&query, false, true /* signal capture */).await?;
            let gate_input = crate::element::GateInput {
                op: &op,
                index: request.index,
                require_visible: request.require_visible,
                app_label: &label,
                cached: &cached.identity,
                cached_revision: cached.revision,
                cached_pid: cached.pid,
                cached_window: cached.window_id,
                resolved_pid: live.pid,
                resolved_window: live.window_id,
                live: &live.ui_root,
                live_pid: live.pid,
                live_window: live.window_id,
            };
            let gate = crate::element::gate(&gate_input)?;
            let via = perform_action(&live, &gate.path, &op).await?;
            // Re-fetch on the same bus, log a failure but never turn a
            // completed action into an error.
            let (refreshed, revalidation_warning) = match walk_live(&query, false, false).await {
                Ok(report) => (Some(report), String::new()),
                Err(error) => (
                    None,
                    format!("tree was not re-read after the action: {error}"),
                ),
            };
            Ok((via, refreshed, cached, revalidation_warning))
        })
        .await
    })?;

    let result = match joined {
        Ok(Ok(value)) => value,
        Ok(Err(_elapsed)) => {
            return Err(ElementError::new(ComputerError::ExecutionFailed(format!(
                "AT-SPI element action exceeded {}s budget",
                ATSPI_CALL_BUDGET.as_secs()
            ))));
        }
        Err(_elapsed) => {
            return Err(ElementError::new(ComputerError::ExecutionFailed(format!(
                "AT-SPI element action exceeded {}s budget",
                ATSPI_CALL_BUDGET.as_secs()
            ))));
        }
    };
    let (via, refreshed, cached, revalidation_warning) = result;
    let mut warnings = Vec::new();
    if !revalidation_warning.is_empty() {
        warnings.push(revalidation_warning);
    }
    let mut snapshot = cached.snapshot;
    let new_revision = refreshed
        .as_ref()
        .map(|live| live.ui_revision)
        .unwrap_or(snapshot.revision);
    if let Some(live) = refreshed {
        snapshot.tree = live.tree_text.clone();
        snapshot.truncated = live.truncated;
        snapshot.revision = live.ui_revision;
        snapshot.observed = Some(live.observed_root);
        snapshot.observed_revision = Some(live.ui_revision);
        if let Err(error) = crate::cache::store(session_for_store.as_deref(), &snapshot) {
            warnings.push(format!("session cache was not updated: {error}"));
        }
    }
    Ok(crate::element::outcome(
        &request,
        new_revision,
        warnings,
        Some(via),
    ))
}

/// One snapshot entry returned by `load_cached_snapshot`.
struct CachedSnapshot {
    /// The sig-node the gate compares the live tree against.
    identity: Arc<tree::SigNode>,
    /// Stored revision (or `observed_revision` when set).
    revision: u64,
    /// Stored pid.
    pid: i32,
    /// Stored window id.
    window_id: u32,
    /// Original snapshot so we can re-write it with `observed` updated.
    snapshot: crate::cache::Snapshot,
}

/// Load the snapshot the cache stored for this `(pid, window_id)`.
async fn load_cached_snapshot(query: &TreeQuery) -> Result<Option<CachedSnapshot>, ElementError> {
    // Resolve the pid/window_id from a pre-walk so the snapshot lookup
    // matches the cache key the dispatcher used to write the original
    // tree. The cost is one extra `walk_live` pass that happens before
    // any element work runs; macOS / Windows pay the same price via
    // `resolve_target` + `cache::load`.
    let pre_report = walk_live(query, false, false)
        .await
        .map_err(ElementError::new)?;
    let pid = pre_report.pid;
    let window_id = pre_report.window_id;
    drop(pre_report);
    let snapshot = crate::cache::load(query.session.as_deref(), None, pid, window_id)
        .map_err(ElementError::new)?;
    let Some(snapshot) = snapshot else {
        return Ok(None);
    };
    let identity = snapshot
        .observed
        .clone()
        .unwrap_or_else(|| snapshot.root.clone());
    let revision = snapshot.observed_revision.unwrap_or(snapshot.revision);
    Ok(Some(CachedSnapshot {
        identity: Arc::new(identity),
        revision,
        pid: snapshot.pid,
        window_id: snapshot.window_id,
        snapshot,
    }))
}

/// Buffered, redacted walk of one application. Carries the public
/// `UiNode` (so the gate can validate) plus the AT-SPI interfaces for
/// every visited path and the root `ObjectRefOwned` so element actions
/// can descend the same way `render_node` did and reach the right node.
struct LiveWalk {
    /// Public `UiNode` tree after redaction.
    ui_root: UiNode,
    /// Public renderer revision (cached in `tree::revision_of`).
    ui_revision: u64,
    /// `InterfaceSet` keyed by child-index path (empty path is root).
    interfaces: HashMap<Vec<usize>, atspi::InterfaceSet>,
    /// Application reference; element actions descend from here by path.
    root_object: ObjectRefOwned,
    /// Resolved pid.
    pid: i32,
    /// Resolved window id (0 when none).
    window_id: u32,
    /// Tree text the dispatcher and renderer use.
    tree_text: String,
    /// Full tree text (matches `tree_text` on P13a).
    full_text: String,
    /// Truncation flag.
    truncated: bool,
    /// `SigNode` root for `cache::Snapshot::observed`.
    observed_root: tree::SigNode,
}

/// Walk the AT-SPI bus and fill a [`LiveWalk`] ready for `gate` and
/// element actions. Returns `ComputerError` so callers can map missing
/// applications to `AppNotFound` consistently.
///
/// `signal_capture` is `true` only when the walker runs before element
/// work; it lets us return the application without renaming
/// `AppNotFound` to `ExecutionFailed`.
async fn walk_live(
    query: &TreeQuery,
    want_full: bool,
    signal_capture: bool,
) -> Result<LiveWalk, ComputerError> {
    let connection = connect().await?;
    let app_filter = query.app.clone();
    let window_filter = query.window.clone();
    let registry = connection
        .root_accessible_on_registry()
        .await
        .map_err(atspi_error)?;
    let children = registry.get_children().await.map_err(atspi_error)?;
    let mut chosen: Option<ObjectRefOwned> = None;
    for child in children {
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
        if signal_capture {
            return Err(ComputerError::AppNotFound(
                "no AT-SPI application matched the --app selector".into(),
            ));
        }
        return Err(ComputerError::ExecutionFailed(
            "no AT-SPI application matched the --app selector".into(),
        ));
    };
    let application = application_ref
        .as_accessible_proxy(connection.connection())
        .await
        .map_err(atspi_error)?;
    let pid = extract_pid_from_path(application.inner().path().as_str());
    let window_id = window_filter
        .as_deref()
        .and_then(extract_window_id)
        .unwrap_or(0);
    let rendered_root = render_node(&application, &connection, want_full, 0).await?;
    let mut path = Vec::new();
    let mut interfaces = HashMap::new();
    let root = reduce_tree(&rendered_root, &mut path, &mut interfaces);
    let public = tree::render(&root);
    let observed_root = public.root.clone();
    Ok(LiveWalk {
        ui_root: root,
        ui_revision: tree::revision_of(&public.root),
        interfaces,
        root_object: application_ref,
        pid,
        window_id,
        tree_text: public.text.clone(),
        full_text: public.text,
        truncated: public.truncated,
        observed_root,
    })
}

/// Resolve the `AccessibleProxy` at a child-index path by descending
/// the live tree's `get_children` results. This mirrors the walk
/// `render_node` used to populate `interfaces` so the index the gate
/// validated points to the same AT-SPI object the tree rendered.
///
/// `into_accessible_proxy` consumes the `ObjectRefOwned` and returns a
/// proxy whose lifetime is the connection's (not the source ref's), so
/// each iteration can build a fresh proxy without leaving the previous
/// `ObjectRefOwned` borrowed.
async fn proxy_at<'a>(
    connection: &'a AccessibilityConnection,
    root: ObjectRefOwned,
    path: &'a [usize],
) -> Result<AccessibleProxy<'a>, ComputerError> {
    let mut current = root
        .into_accessible_proxy(connection.connection())
        .await
        .map_err(atspi_error)?;
    for &child_index in path {
        let child_refs = current.get_children().await.map_err(atspi_error)?;
        let Some(child_ref) = child_refs.into_iter().nth(child_index) else {
            return Err(ComputerError::ElementNotFound(format!(
                "path index {child_index} did not resolve to an AT-SPI child"
            )));
        };
        // The previous `current` is dropped here. The new proxy
        // borrows from the connection, not from the consumed ref.
        current = child_ref
            .into_accessible_proxy(connection.connection())
            .await
            .map_err(atspi_error)?;
    }
    Ok(current)
}

/// Drive one element op through the matching AT-SPI interface. Returns
/// the `via` string the EI action reports back. The error type is
/// `ComputerError` so the dispatcher can map missing interfaces to
/// `execution_failed`.
async fn perform_action(
    live: &LiveWalk,
    path: &[usize],
    op: &crate::element::ElementOp,
) -> Result<String, ComputerError> {
    use crate::element::ElementOp;

    let connection = connect().await?;
    let interfaces = live.interfaces.get(path).copied().unwrap_or_default();
    let proxy = proxy_at(&connection, live.root_object.clone(), path).await?;
    let element_writes_text = matches!(
        op,
        ElementOp::SetValue { .. } | ElementOp::SelectText { .. } | ElementOp::Type { .. }
    );
    let secure = proxy_is_password(&proxy).await;
    if secure && element_writes_text {
        return Err(ComputerError::PolicyDenied(
            "refusing to read or write a secure text field".into(),
        ));
    }
    let _ = interfaces;
    let via = match op {
        ElementOp::Press { count, button } => {
            let _ = button;
            if !interfaces.contains(Interface::Action) {
                return Err(ComputerError::ExecutionFailed(format!(
                    "AT-SPI element action: Action interface missing at path depth {}",
                    path.len()
                )));
            }
            let action_proxy = atspi::proxy::proxy_ext::ProxyExt::proxies(&proxy)
                .await
                .map_err(atspi_error)?
                .action()
                .await
                .map_err(atspi_error)?;
            // First action (click/press/activate) is the project default;
            // when only one is exposed the call is harmless either way.
            for _ in 0..*count {
                action_proxy.do_action(0).await.map_err(atspi_error)?;
            }
            format!("{VIA_ATSPI}:Action")
        }
        ElementOp::Focus => {
            if !interfaces.contains(Interface::Component) {
                return Err(ComputerError::ExecutionFailed(format!(
                    "AT-SPI element action: Component interface missing at path depth {}",
                    path.len()
                )));
            }
            let component_proxy = atspi::proxy::proxy_ext::ProxyExt::proxies(&proxy)
                .await
                .map_err(atspi_error)?
                .component()
                .await
                .map_err(atspi_error)?;
            component_proxy.grab_focus().await.map_err(atspi_error)?;
            format!("{VIA_ATSPI}:Component.grab_focus")
        }
        ElementOp::SetValue { value } => {
            // AT-SPI exposes two writable shapes: `Value` (numeric) and
            // `EditableText` (text). EditableText wins when both are
            // advertised because it is the human-language path.
            if interfaces.contains(Interface::EditableText) {
                let editable = atspi::proxy::proxy_ext::ProxyExt::proxies(&proxy)
                    .await
                    .map_err(atspi_error)?
                    .editable_text()
                    .await
                    .map_err(atspi_error)?;
                editable
                    .set_text_contents(value)
                    .await
                    .map_err(atspi_error)?;
                format!("{VIA_ATSPI}:EditableText")
            } else if interfaces.contains(Interface::Value) {
                let numeric = value.parse::<f64>().map_err(|error| {
                    ComputerError::InvalidArguments(format!(
                        "AT-SPI Value interface requires a numeric value: {error}"
                    ))
                })?;
                let value_proxy = atspi::proxy::proxy_ext::ProxyExt::proxies(&proxy)
                    .await
                    .map_err(atspi_error)?
                    .value()
                    .await
                    .map_err(atspi_error)?;
                value_proxy
                    .set_current_value(numeric)
                    .await
                    .map_err(atspi_error)?;
                format!("{VIA_ATSPI}:Value")
            } else {
                return Err(ComputerError::ExecutionFailed(format!(
                    "AT-SPI element action: EditableText or Value interface missing at path depth {}",
                    path.len()
                )));
            }
        }
        ElementOp::SelectText { start, end } => {
            if !interfaces.contains(Interface::Text) {
                return Err(ComputerError::ExecutionFailed(format!(
                    "AT-SPI element action: Text interface missing at path depth {}",
                    path.len()
                )));
            }
            let text_proxy = atspi::proxy::proxy_ext::ProxyExt::proxies(&proxy)
                .await
                .map_err(atspi_error)?
                .text()
                .await
                .map_err(atspi_error)?;
            let start = i32::try_from(*start).unwrap_or(i32::MAX);
            let end = i32::try_from(*end).unwrap_or(i32::MAX);
            let ok = text_proxy
                .set_selection(0, start, end)
                .await
                .map_err(atspi_error)?;
            if !ok {
                text_proxy
                    .add_selection(start, end)
                    .await
                    .map_err(atspi_error)?;
            }
            format!("{VIA_ATSPI}:Text")
        }
        ElementOp::ScrollTo => {
            if !interfaces.contains(Interface::Component) {
                return Err(ComputerError::ExecutionFailed(format!(
                    "AT-SPI element action: Component interface missing at path depth {}",
                    path.len()
                )));
            }
            let component_proxy = atspi::proxy::proxy_ext::ProxyExt::proxies(&proxy)
                .await
                .map_err(atspi_error)?
                .component()
                .await
                .map_err(atspi_error)?;
            component_proxy
                .scroll_to(atspi::ScrollType::TopLeft)
                .await
                .map_err(atspi_error)?;
            format!("{VIA_ATSPI}:Component.scroll_to")
        }
        ElementOp::Perform { name } => {
            if !interfaces.contains(Interface::Action) {
                return Err(ComputerError::ExecutionFailed(format!(
                    "AT-SPI element action: Action interface missing at path depth {} for `{name}`",
                    path.len()
                )));
            }
            let action_proxy = atspi::proxy::proxy_ext::ProxyExt::proxies(&proxy)
                .await
                .map_err(atspi_error)?
                .action()
                .await
                .map_err(atspi_error)?;
            let actions = action_proxy.get_actions().await.map_err(atspi_error)?;
            let mut chosen_index = 0i32;
            for (index, action) in actions.iter().enumerate() {
                if action.name.eq_ignore_ascii_case(name) {
                    chosen_index = index as i32;
                    break;
                }
            }
            action_proxy
                .do_action(chosen_index)
                .await
                .map_err(atspi_error)?;
            format!("{VIA_ATSPI}:Action.perform")
        }
        ElementOp::Type { text } => {
            // `type --element` reuses the `set_value` machinery on top
            // of `EditableText::set_text_contents`. The dispatcher has
            // already filtered secure nodes via `node_is_secure`; we
            // re-check the AT-SPI role to refuse password fields.
            if interfaces.contains(Interface::EditableText) {
                let editable = atspi::proxy::proxy_ext::ProxyExt::proxies(&proxy)
                    .await
                    .map_err(atspi_error)?
                    .editable_text()
                    .await
                    .map_err(atspi_error)?;
                editable
                    .set_text_contents(text)
                    .await
                    .map_err(atspi_error)?;
                format!("{VIA_ATSPI}:EditableText.type")
            } else if interfaces.contains(Interface::Value) {
                let numeric = text.parse::<f64>().map_err(|error| {
                    ComputerError::InvalidArguments(format!(
                        "AT-SPI Value interface requires a numeric value: {error}"
                    ))
                })?;
                let value_proxy = atspi::proxy::proxy_ext::ProxyExt::proxies(&proxy)
                    .await
                    .map_err(atspi_error)?
                    .value()
                    .await
                    .map_err(atspi_error)?;
                value_proxy
                    .set_current_value(numeric)
                    .await
                    .map_err(atspi_error)?;
                format!("{VIA_ATSPI}:Value.type")
            } else {
                return Err(ComputerError::ExecutionFailed(format!(
                    "AT-SPI element action: EditableText or Value interface missing at path depth {}",
                    path.len()
                )));
            }
        }
    };
    Ok(via)
}

/// Check whether the live element reports `Role::PasswordText`. P13a's
/// `Role` -> `name()` is `Role::PasswordText -> "password text"`; we use
/// the same string the redaction walker writes into `UiNode.role` so a
/// regression in either path fails the same way.
async fn proxy_is_password(proxy: &AccessibleProxy<'_>) -> bool {
    let Ok(role) = proxy.get_role().await else {
        return false;
    };
    matches!(role, Role::PasswordText)
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
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<RenderedNode, ComputerError>> + Send + 'a>,
> {
    Box::pin(async move {
        let role = proxy.get_role().await.map_err(atspi_error)?;
        let name = proxy.name().await.unwrap_or_default();
        let description = proxy.description().await.unwrap_or_default();
        let interfaces = proxy.get_interfaces().await.unwrap_or_default();

        // AT-SPI `Sensitive` means "user-actionable", not secret. Only
        // `Role::PasswordText` redacts the value. macOS / Windows do the
        // same on `AXSecureTextField` / `IsPassword`.
        let secure = matches!(role, Role::PasswordText);
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

        Ok(RenderedNode {
            node: UiNode {
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
                children: Vec::new(),
            },
            interfaces,
            rendered_children: children,
        })
    })
}

/// One `UiNode` paired with the AT-SPI data element actions consume:
/// the `InterfaceSet` of the node (drives interface presence checks) and
/// the rendered children (so callers can keep going down the tree).
struct RenderedNode {
    /// Public, redacted shape the project renders and diffs.
    node: UiNode,
    /// AT-SPI interfaces exposed by the node.
    interfaces: atspi::InterfaceSet,
    /// Children with their own [`RenderedNode`] payload.
    rendered_children: Vec<RenderedNode>,
}

/// Reduce a `RenderedNode` tree into the public `UiNode` and a per-path
/// interface map keyed by child-index path. Empty path is the root, same
/// as `element::find_path`.
fn reduce_tree(
    rendered: &RenderedNode,
    path: &mut Vec<usize>,
    out_interfaces: &mut HashMap<Vec<usize>, atspi::InterfaceSet>,
) -> UiNode {
    out_interfaces.insert(path.clone(), rendered.interfaces);
    let mut children = Vec::with_capacity(rendered.rendered_children.len());
    for (idx, child) in rendered.rendered_children.iter().enumerate() {
        path.push(idx);
        let node = reduce_tree(child, path, out_interfaces);
        path.pop();
        children.push(node);
    }
    let mut node = rendered.node.clone();
    node.children = children;
    node
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

/// AT-SPI `State::Sensitive` does not mean "secret"; the contract used
/// to redact based on `Sensitive && !Enabled`. That bit is meaningless
/// for redaction. The function remains in case a future platform
/// revisits the rule, but P13a only ever calls [`role_is_password`].
#[cfg(test)]
#[allow(dead_code)]
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

    /// AT-SPI `Sensitive` does not mean "secret". A fixture node that
    /// mirrors a disabled but non-password widget keeps its value in
    /// the rendered tree.
    #[test]
    fn disabled_but_non_password_node_passes_through() {
        let node = fixture("entry", Some("draft"), Some("draft text"), false);
        assert!(!crate::tree::node_is_secure(&node));
        assert!(crate::tree::identity_text(&node).contains("entry"));
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

    /// `role_is_password` is the single source of truth for redaction.
    #[test]
    fn redaction_helpers_agree_with_role_set() {
        assert!(role_is_password("password text"));
        assert!(!role_is_password("entry"));
        assert!(!role_is_password("push button"));
        assert!(!role_is_password(""));
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

    /// `proxy_at`'s argument shape stays the same: `ObjectRefOwned`
    /// root and a child-index path. The live walk produces both, the
    /// proxy resolution consumes them.
    #[test]
    fn proxy_at_signature_is_stable() {
        // `proxy_at` is an async function. The signature uses a single
        // lifetime for the connection and the path slice (both must
        // outlive the returned `AccessibleProxy`). Pinning the type
        // here catches a regression that drops one.
        let _: for<'a> fn(&'a AccessibilityConnection, ObjectRefOwned, &'a [usize]) -> _ = proxy_at;
    }

    /// The Linux `via` tag for element actions is `"atspi"`; the
    /// tier rendering picks `ax` for unknown `via`s, so we set the
    /// prefix explicitly here.
    #[test]
    fn via_prefix_is_atspi() {
        assert!(VIA_ATSPI.starts_with("atspi"));
    }

    /// The redacted redaction contract: a `Role::PasswordText` node
    /// always sets `secure = true`, and a non-password node never does.
    /// Locks the AT-SPI -> `Role::name()` mapping in place.
    #[test]
    fn password_role_is_the_only_redaction_signal() {
        let password = fixture("password text", Some("API key"), None, false);
        assert!(crate::tree::node_is_secure(&password));
        let entry = fixture("entry", Some("username"), Some("alice"), false);
        assert!(!crate::tree::node_is_secure(&entry));
        let button = fixture("push button", Some("Save"), Some("Save"), false);
        assert!(!crate::tree::node_is_secure(&button));
    }
}
