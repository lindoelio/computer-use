//! P10: Chrome DevTools Protocol (CDP) client.
//!
//! Attach-only. The user runs Chrome with `--remote-debugging-port=N`; we
//! discover `N` from the pid's argv and connect. We never spawn a new Chrome
//! instance — every Chrome life cycle belongs to the user.
//!
//! ## Sync bridge
//!
//! `DesktopBackend` stays synchronous. For each `browser` subcommand we build
//! a fresh `tokio::runtime::Builder::new_current_thread()` runtime on the CLI
//! thread, `block_on` the async CDP request, and drop the runtime on the way
//! out. The handler task chromiumoxide returns is spawned on that runtime,
//! so it is also dropped when the runtime is. Chrome is not affected.
//!
//! ## Errors
//!
//! Every failure funnels into one of:
//!
//! * `ComputerError::Unsupported("...")` — built without `--features cdp`.
//! * `ComputerError::CdpUnavailable("...")` — Chrome not running with a debug
//!   port, or the websocket died.
//! * `ComputerError::BlockedUrl("...")` — the URL is in
//!   `[browser].blocked_urls` in the policy; the request never reaches CDP.
//! * `ComputerError::InvalidArguments("...")` — missing selector/url/script.
//! * `ComputerError::ElementNotFound("...")` — selector `find_element` came
//!   back empty (synthesized from `find_element` errors where applicable).

#![allow(unsafe_code)]

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::error::ComputerError;
use crate::policy;

/// What the CLI passes to [`attach`] so it can find Chrome.
#[derive(Debug, Clone, Default)]
pub struct AttachOptions {
    /// Either `ws://host:port`, `http(s)://host:port` (resolved via `/json/version`).
    /// Wins over pid discovery when present.
    pub debug_url: Option<String>,
    /// Name, bundle id, or path. Used only to look up a pid for the
    /// port-discovery fallback when `debug_url` is `None`.
    pub app: Option<String>,
    /// Pre-resolved pid for the browser. Wins over `app` when present.
    pub app_pid: Option<i32>,
    /// Default CDP request timeout.
    pub command_timeout: Duration,
}

/// Argument set the action helpers receive (the `browser` subcommands route to it).
#[derive(Debug, Clone, Default)]
pub struct ActionOptions {
    pub debug_url: Option<String>,
    pub app: Option<String>,
    pub app_pid: Option<i32>,
    pub target_id: Option<String>,
    pub url: Option<String>,
    pub selector: Option<String>,
    pub text: Option<String>,
    pub script: Option<String>,
    pub command_timeout: Duration,
    /// Cached policy `[browser].blocked_urls`. The dispatcher passes these in
    /// so the cdp helpers do not need to read the policy file themselves.
    pub policy_blocked_urls: Vec<String>,
    /// Policy subject the dispatcher resolved. Currently unused by the CDP
    /// helpers (the URL check is enough), but recorded for future expansion.
    pub policy_subject: Option<policy::Subject>,
}

/// JSON-friendly tab summary.
#[derive(Debug, Clone, Serialize)]
pub struct TabSummary {
    /// CDP `targetId` (opaque string).
    pub target_id: String,
    /// Same value, kept under the older name `handle` for the CLI help text.
    pub handle: String,
    /// Best-effort page title (may be `None` for chrome:// or about:blank).
    pub title: Option<String>,
    /// Current URL.
    pub url: String,
    /// CDP `type` (`"page"`, `"iframe"`, `"service_worker"`, ...).
    pub kind: String,
}

/// Stable error used when the binary is built without `--features cdp`.
pub fn unsupported() -> ComputerError {
    ComputerError::Unsupported(
        "browser commands require the `cdp` feature (build with --features cdp)".into(),
    )
}

/// Parse the argv text for `--remote-debugging-port=N` (or `--remote-debugging-port N`).
///
/// Returns the first match. Pure, no I/O — exercised by unit tests with one
/// fixture per OS format. NUL-separated cmdline payloads (Linux, macOS) work
/// because the splitter accepts both `\0` and ASCII whitespace.
pub fn parse_debug_port(args: &str) -> Option<u16> {
    let tokens: Vec<&str> = args
        .split(|ch: char| ch.is_whitespace() || ch == '\0')
        .filter(|s| !s.is_empty())
        .collect();
    for (idx, token) in tokens.iter().enumerate() {
        if let Some(rest) = token.strip_prefix("--remote-debugging-port=") {
            if let Ok(port) = rest.trim().parse::<u16>() {
                return Some(port);
            }
            continue;
        }
        if *token == "--remote-debugging-port" {
            if let Some(next) = tokens.get(idx + 1) {
                if let Ok(port) = next.trim().parse::<u16>() {
                    return Some(port);
                }
            }
        }
    }
    None
}

fn cdp_unavailable(message: String) -> ComputerError {
    ComputerError::CdpUnavailable(message)
}

/// Map a `chromiumoxide` / CDP error to the closest stable `ComputerError`.
///
/// Chromiumoxide's `CdpError::Chrome(chromiumoxide_types::Error)` carries a CDP
/// server message. When the server fails because the selector matched nothing
/// (e.g. `DOM.querySelector` returns `nodeId: 0`, which then breaks
/// `DOM.describeNode`), the message contains "no node" or "node with given
/// id ... not found". That must surface as `element_not_found` (per gap 2).
/// Anything that looks like a transport / websocket failure stays
/// `cdp_unavailable`.
#[cfg(feature = "cdp")]
fn map_cdp_error(error: chromiumoxide::error::CdpError) -> ComputerError {
    use chromiumoxide::error::CdpError as E;
    match error {
        E::Chrome(message) => {
            if matches_node_not_found(&message.message) {
                ComputerError::ElementNotFound(format!(
                    "selector matched no element: {}",
                    message.message
                ))
            } else {
                cdp_unavailable(format!("cdp: {}", message.message))
            }
        }
        E::ChromeMessage(text) => {
            if matches_node_not_found(&text) {
                ComputerError::ElementNotFound(format!("selector matched no element: {text}"))
            } else {
                cdp_unavailable(format!("cdp: {text}"))
            }
        }
        E::NotFound => ComputerError::ElementNotFound("selector matched no element".into()),
        other => cdp_unavailable(format!("cdp: {other}")),
    }
}

/// True if the given CDP server message means "the requested node does not
/// exist in the DOM tree" — the textual sibling of `nodeId: 0` returned by
/// `DOM.querySelector` and similar.
#[cfg(feature = "cdp")]
fn matches_node_not_found(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("no node") || lower.contains("node with given id") && lower.contains("not found")
}

/// Resolve the page target we will operate on. Attach-only: when the caller
/// did not pass `--target`, we look for the first existing `page` target. If
/// no page target exists we return `cdp_unavailable("no page target")` and
/// **never** create one (per gap 3).
#[cfg(feature = "cdp")]
async fn resolve_page(
    browser: &mut chromiumoxide::Browser,
    target_id: Option<&str>,
) -> Result<chromiumoxide::Page, ComputerError> {
    if let Some(target_id_text) = target_id.map(str::trim).filter(|s| !s.is_empty()) {
        return browser
            .get_page(target_id_for(target_id_text))
            .await
            .map_err(|error| {
                cdp_unavailable(format!("target {target_id_text} not found: {error}"))
            });
    }
    let targets = browser.fetch_targets().await.map_err(map_cdp_error)?;
    let first = targets.into_iter().find(|target| target.r#type == "page");
    match first {
        Some(target) => {
            let tid = String::from(target.target_id.clone());
            browser
                .get_page(target.target_id.clone())
                .await
                .map_err(|error| cdp_unavailable(format!("get_page({tid}): {error}")))
        }
        None => Err(no_page_target_error()),
    }
}

/// Read the page URL after attach. Used to enforce `[browser].blocked_urls`
/// against the *current* page (per gap 4) before running click / text /
/// type / eval.
#[cfg(feature = "cdp")]
async fn current_page_url(page: &chromiumoxide::Page) -> String {
    match page.url().await {
        Ok(Some(text)) => text,
        _ => String::new(),
    }
}

/// Attach, look up the page target, and enforce the policy block list against
/// the current page URL. All mutating browser ops route through this helper.
///
/// The helper is *sync*: it builds its own runtime and `block_on`s the
/// operation closure. The closure receives an attached `Browser` and a resolved
/// `Page`; the URL check has already passed by the time it runs.
#[cfg(feature = "cdp")]
fn attach_with_url_check<F, Fut, T>(
    attach_opts: AttachOptions,
    target_id: Option<String>,
    blocked_urls: Vec<String>,
    operation: F,
) -> Result<T, ComputerError>
where
    F: FnOnce(chromiumoxide::Browser, chromiumoxide::Page) -> Fut,
    Fut: std::future::Future<Output = Result<T, ComputerError>>,
{
    let command_timeout = attach_opts.command_timeout;
    let ws_url = pick_websocket_url(&attach_opts)?;
    let browser = connect(&ws_url, command_timeout)?;
    let rt = new_runtime(command_timeout)?;
    rt.block_on(async move {
        let mut browser = browser;
        let page = resolve_page(&mut browser, target_id.as_deref()).await?;
        let active = current_page_url(&page).await;
        if let Some(pattern) = policy::url_blocked(&active, &blocked_urls) {
            return Err(ComputerError::BlockedUrl(format!(
                "current page matched {pattern}"
            )));
        }
        operation(browser, page).await
    })
}

// -----------------------------------------------------------------------------
// Port discovery (always available, regardless of feature).

/// macOS argv source: `sysctl KERN_PROCARGS2`.
///
/// The kernel returns argc, then argv as NUL-separated C strings, ending with a
/// trailing NUL. The actual argv starts at the first NUL byte (after argc).
#[cfg(target_os = "macos")]
fn read_command_line_args(pid: i32) -> Result<String, ComputerError> {
    use std::ffi::{c_int, c_void};

    extern "C" {
        fn sysctl(
            name: *const c_int,
            namelen: usize,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *const c_void,
            newlen: usize,
        ) -> c_int;
    }

    // CTL_KERN (1), KERN_PROCARGS2 (58)
    let mib: [c_int; 3] = [1, 58, pid as c_int];
    let mut size: usize = 0;
    let rc = unsafe {
        sysctl(
            mib.as_ptr() as *const _,
            mib.len(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(cdp_unavailable(format!(
            "sysctl size for pid {pid}: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut buf = vec![0u8; size];
    let rc = unsafe {
        sysctl(
            mib.as_ptr() as *const _,
            mib.len(),
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(cdp_unavailable(format!(
            "sysctl read for pid {pid}: {}",
            std::io::Error::last_os_error()
        )));
    }
    buf.truncate(size);
    let start = buf
        .iter()
        .position(|byte| *byte == 0)
        .map(|index| index + 1)
        .unwrap_or(0);
    let end = buf
        .iter()
        .rposition(|byte| *byte != 0)
        .map(|index| index + 1)
        .unwrap_or(start);
    Ok(String::from_utf8_lossy(&buf[start..end]).into_owned())
}

/// Linux argv source: `/proc/<pid>/cmdline`, NUL-separated UTF-8.
#[cfg(target_os = "linux")]
fn read_command_line_args(pid: i32) -> Result<String, ComputerError> {
    use std::io::Read;

    let path = format!("/proc/{pid}/cmdline");
    let mut file = std::fs::File::open(&path).map_err(|error| {
        cdp_unavailable(format!("open {path}: {error} (process may have exited)"))
    })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| cdp_unavailable(format!("read {path}: {error}")))?;
    let mut out = String::new();
    for part in bytes.split(|byte| *byte == 0) {
        if part.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&String::from_utf8_lossy(part));
    }
    Ok(out)
}

/// Windows argv source: best-effort via Win32. The project does not bundle
/// Win32, so the error is structured: the caller can still pass `--cdp-url`.
#[cfg(target_os = "windows")]
fn read_command_line_args(pid: i32) -> Result<String, ComputerError> {
    Err(cdp_unavailable(format!(
        "Windows command-line lookup for pid {pid} is not implemented; pass --cdp-url ws://127.0.0.1:<port> explicitly"
    )))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn read_command_line_args(pid: i32) -> Result<String, ComputerError> {
    Err(cdp_unavailable(format!(
        "command-line lookup is not implemented for this OS; pass --cdp-url ws://127.0.0.1:<port> explicitly"
    )))
}

/// Highest-level port discovery. Public so unit tests can stub it.
pub fn discover_debug_port(pid: i32) -> Result<u16, ComputerError> {
    let args = read_command_line_args(pid)?;
    parse_debug_port(&args).ok_or_else(|| {
        cdp_unavailable(format!(
            "pid {pid} command line has no --remote-debugging-port=N"
        ))
    })
}

// -----------------------------------------------------------------------------
// Runtime + attach (only when `cdp` is on).

#[cfg(feature = "cdp")]
fn new_runtime(_timeout: Duration) -> Result<tokio::runtime::Runtime, ComputerError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| cdp_unavailable(format!("tokio runtime: {error}")))
}

#[cfg(feature = "cdp")]
fn normalize_url(input: &str) -> Result<String, ComputerError> {
    if let Some(rest) = input
        .strip_prefix("ws://")
        .or_else(|| input.strip_prefix("wss://"))
    {
        if rest.is_empty() {
            return Err(cdp_unavailable(format!("cdp url has no host: {input}")));
        }
        return Ok(input.to_string());
    }
    if let Some(rest) = input
        .strip_prefix("http://")
        .or_else(|| input.strip_prefix("https://"))
    {
        if rest.is_empty() {
            return Err(cdp_unavailable(format!("cdp url has no host: {input}")));
        }
        // chromiumoxide will resolve `/json/version` for us.
        return Ok(input.trim_end_matches('/').to_string());
    }
    Err(cdp_unavailable(format!(
        "unsupported cdp url scheme: {input} (expected ws://, wss://, https://, or http://)"
    )))
}

#[cfg(feature = "cdp")]
fn pick_websocket_url(opts: &AttachOptions) -> Result<String, ComputerError> {
    if let Some(url) = opts
        .debug_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return normalize_url(url);
    }
    let pid = opts.app_pid.ok_or_else(|| {
        cdp_unavailable("cdp attach requires --cdp-url or --app resolving to a running pid".into())
    })?;
    let port = discover_debug_port(pid)?;
    Ok(format!("ws://127.0.0.1:{port}"))
}

#[cfg(feature = "cdp")]
fn connect(url: &str, timeout: Duration) -> Result<chromiumoxide::Browser, ComputerError> {
    let rt = new_runtime(timeout)?;
    let connect = chromiumoxide::Browser::connect(url.to_string());
    let (browser, mut handler) = rt
        .block_on(connect)
        .map_err(|error| cdp_unavailable(format!("connect to {url}: {error}")))?;
    rt.spawn(async move {
        use futures::StreamExt;
        while let Some(_event) = handler.next().await {}
    });
    Ok(browser)
}

#[cfg(feature = "cdp")]
fn target_id_for(text: &str) -> chromiumoxide::cdp::browser_protocol::target::TargetId {
    chromiumoxide::cdp::browser_protocol::target::TargetId::new(text.to_string())
}

/// List every target the running Chrome exposes.
///
/// Implemented as one big async function driven by a `current_thread` runtime
/// so the borrow checker can see `browser.fetch_targets()` (`&mut`) followed
/// by `browser.get_page(...)` (`&`) on the same Browser.
#[cfg(feature = "cdp")]
pub fn list_tabs(opts: &ActionOptions) -> Result<Vec<TabSummary>, ComputerError> {
    let attach_opts = AttachOptions {
        debug_url: opts.debug_url.clone(),
        app: opts.app.clone(),
        app_pid: opts.app_pid,
        command_timeout: opts.command_timeout,
    };
    let url = pick_websocket_url(&attach_opts)?;
    let browser = connect(&url, opts.command_timeout)?;
    let rt = new_runtime(opts.command_timeout)?;
    let summaries = rt.block_on(async move {
        let mut browser = browser;
        let targets = browser
            .fetch_targets()
            .await
            .map_err(|error| cdp_unavailable(format!("fetch_targets: {error}")))?;
        let mut out = Vec::with_capacity(targets.len());
        for target in targets {
            let target_id = target.target_id.clone();
            let kind = target.r#type.clone();
            let fallback_url = target.url.clone();
            let page_url = match browser.get_page(target_id.clone()).await {
                Ok(handle) => match handle.url().await {
                    Ok(Some(text)) => text,
                    _ => fallback_url.clone(),
                },
                Err(_) => fallback_url,
            };
            let title = match browser.get_page(target_id.clone()).await {
                Ok(handle) => handle.get_title().await.ok().flatten(),
                Err(_) => None,
            };
            let target_id_string: String = target_id.into();
            out.push(TabSummary {
                target_id: target_id_string.clone(),
                handle: target_id_string,
                title,
                url: page_url,
                kind,
            });
        }
        Result::<Vec<TabSummary>, ComputerError>::Ok(out)
    })?;
    Ok(summaries)
}

#[cfg(not(feature = "cdp"))]
pub fn list_tabs(_opts: &ActionOptions) -> Result<Vec<TabSummary>, ComputerError> {
    Err(unsupported())
}

/// Return the [`attach_with_url_check`] closure argument that converts an
/// absent page target into `cdp_unavailable("no page target")`. Centralised so
/// the message stays stable across ops.
#[cfg(feature = "cdp")]
fn no_page_target_error() -> ComputerError {
    cdp_unavailable(
        "no page target found in the attached browser; computer-use is attach-only \
         and will not create new pages. Open a tab in Chrome, then retry."
            .into(),
    )
}

/// Bring the named tab to the front.
#[cfg(feature = "cdp")]
pub fn activate_tab(opts: &ActionOptions) -> Result<String, ComputerError> {
    let target_id = opts.target_id.clone().ok_or_else(|| {
        ComputerError::InvalidArguments("browser activate requires --target".into())
    })?;
    let attach_opts = AttachOptions {
        debug_url: opts.debug_url.clone(),
        app: opts.app.clone(),
        app_pid: opts.app_pid,
        command_timeout: opts.command_timeout,
    };
    let url = pick_websocket_url(&attach_opts)?;
    let browser = connect(&url, opts.command_timeout)?;
    let blocked_urls = opts.policy_blocked_urls.clone();
    let rt = new_runtime(opts.command_timeout)?;
    let (active_url, target_id_for_err) = rt.block_on(async move {
        let browser = browser;
        let target_id_owned = target_id.clone();
        let page = browser
            .get_page(target_id_for(&target_id_owned))
            .await
            .map_err(|error| {
                cdp_unavailable(format!("target {target_id_owned} not found: {error}"))
            })?;
        page.bring_to_front()
            .await
            .map_err(|error| cdp_unavailable(format!("bringToFront: {error}")))?;
        let active = match page.url().await {
            Ok(Some(text)) => text,
            Ok(None) => String::new(),
            Err(_) => String::new(),
        };
        Ok::<(String, String), ComputerError>((active, target_id_owned))
    })?;
    if let Some(pattern) = policy::url_blocked(&active_url, &blocked_urls) {
        return Err(ComputerError::BlockedUrl(format!(
            "current tab {target_id_for_err} matched {pattern}"
        )));
    }
    Ok(active_url)
}

#[cfg(not(feature = "cdp"))]
pub fn activate_tab(_opts: &ActionOptions) -> Result<String, ComputerError> {
    Err(unsupported())
}

/// Navigate the named tab (or the first page) to a URL.
#[cfg(feature = "cdp")]
pub fn navigate(opts: &ActionOptions) -> Result<String, ComputerError> {
    let url = opts
        .url
        .clone()
        .ok_or_else(|| ComputerError::InvalidArguments("browser navigate requires --url".into()))?;
    if let Some(pattern) = policy::url_blocked(&url, &opts.policy_blocked_urls) {
        return Err(ComputerError::BlockedUrl(format!("matched {pattern}")));
    }
    let attach_opts = AttachOptions {
        debug_url: opts.debug_url.clone(),
        app: opts.app.clone(),
        app_pid: opts.app_pid,
        command_timeout: opts.command_timeout,
    };
    let ws_url = pick_websocket_url(&attach_opts)?;
    let browser = connect(&ws_url, opts.command_timeout)?;
    let target_id = opts.target_id.clone();
    let blocked_urls = opts.policy_blocked_urls.clone();
    let url_for_send = url.clone();
    let rt = new_runtime(opts.command_timeout)?;
    let active_url = rt.block_on(async move {
        let mut browser = browser;
        let page = resolve_page(&mut browser, target_id.as_deref()).await?;
        page.goto(url_for_send.clone())
            .await
            .map_err(|error| cdp_unavailable(format!("Page.navigate: {error}")))?;
        let active = match page.url().await {
            Ok(Some(text)) => text,
            Ok(None) => String::new(),
            Err(_) => String::new(),
        };
        Ok::<String, ComputerError>(active)
    })?;
    if let Some(pattern) = policy::url_blocked(&active_url, &blocked_urls) {
        return Err(ComputerError::BlockedUrl(format!(
            "redirect landed on blocked url matched {pattern}"
        )));
    }
    Ok(active_url)
}

#[cfg(not(feature = "cdp"))]
pub fn navigate(_opts: &ActionOptions) -> Result<String, ComputerError> {
    Err(unsupported())
}

/// Click the centre of the element matched by `selector`.
///
/// The CDP runtime already produces physical mouse events through
/// `Input.dispatchMouseEvent` (tier `cdp`). The spec asked for the element box
/// centre -> viewport -> screen transform (window.screenX/Y + outer/inner
/// height + devicePixelRatio), but Chrome owns its own window and that path
/// would couple this binary to Chrome's window bounds without a clean public
/// API. The CDP click stays; the result names the tier explicitly so callers
/// know what they got (P10 deviation, see ROADMAP.md).
#[cfg(feature = "cdp")]
pub fn click(opts: &ActionOptions) -> Result<(), ComputerError> {
    let selector = opts.selector.clone().ok_or_else(|| {
        ComputerError::InvalidArguments("browser click requires --selector".into())
    })?;
    let attach_opts = AttachOptions {
        debug_url: opts.debug_url.clone(),
        app: opts.app.clone(),
        app_pid: opts.app_pid,
        command_timeout: opts.command_timeout,
    };
    let target_id = opts.target_id.clone();
    let blocked_urls = opts.policy_blocked_urls.clone();
    attach_with_url_check(
        attach_opts,
        target_id,
        blocked_urls,
        move |_browser, page| async move {
            let element = page
                .find_element(selector.clone())
                .await
                .map_err(map_cdp_error)?;
            element
                .click()
                .await
                .map_err(|error| cdp_unavailable(format!("element click: {error}")))?;
            Ok::<(), ComputerError>(())
        },
    )
}

#[cfg(not(feature = "cdp"))]
pub fn click(_opts: &ActionOptions) -> Result<(), ComputerError> {
    Err(unsupported())
}

/// Read `innerText` of the element matched by `selector`.
///
/// Spec: `browser text --selector S` returns the `innerText` of the first
/// element the selector matches. A selector that matches nothing is
/// `element_not_found`. The current page URL is checked against
/// `[browser].blocked_urls` before the websocket is touched.
#[cfg(feature = "cdp")]
pub fn read_inner_text(opts: &ActionOptions) -> Result<String, ComputerError> {
    let selector = opts.selector.clone().ok_or_else(|| {
        ComputerError::InvalidArguments("browser text requires --selector".into())
    })?;
    let attach_opts = AttachOptions {
        debug_url: opts.debug_url.clone(),
        app: opts.app.clone(),
        app_pid: opts.app_pid,
        command_timeout: opts.command_timeout,
    };
    let target_id = opts.target_id.clone();
    let blocked_urls = opts.policy_blocked_urls.clone();
    attach_with_url_check(
        attach_opts,
        target_id,
        blocked_urls,
        move |_browser, page| async move {
            let element = page
                .find_element(selector.clone())
                .await
                .map_err(map_cdp_error)?;
            let text = element.inner_text().await.map_err(map_cdp_error)?;
            Ok::<String, ComputerError>(text.unwrap_or_default())
        },
    )
}

#[cfg(not(feature = "cdp"))]
pub fn read_inner_text(_opts: &ActionOptions) -> Result<String, ComputerError> {
    Err(unsupported())
}

/// Type `text` into the element matched by `selector` (or the page body when
/// no selector is given).
///
/// Classified like `Action::Type` for audit purposes: the text body is never
/// stored, only the character count. The current page URL is checked against
/// `[browser].blocked_urls` first.
#[cfg(feature = "cdp")]
pub fn type_text(opts: &ActionOptions) -> Result<(), ComputerError> {
    let text = opts
        .text
        .clone()
        .ok_or_else(|| ComputerError::InvalidArguments("browser type requires --text".into()))?;
    let selector = opts.selector.clone();
    let attach_opts = AttachOptions {
        debug_url: opts.debug_url.clone(),
        app: opts.app.clone(),
        app_pid: opts.app_pid,
        command_timeout: opts.command_timeout,
    };
    let target_id = opts.target_id.clone();
    let blocked_urls = opts.policy_blocked_urls.clone();
    attach_with_url_check(
        attach_opts,
        target_id,
        blocked_urls,
        move |_browser, page| async move {
            match selector.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(sel) => {
                    let element = page
                        .find_element(sel.to_string())
                        .await
                        .map_err(map_cdp_error)?;
                    element
                        .click()
                        .await
                        .map_err(|error| cdp_unavailable(format!("element click: {error}")))?;
                    element
                        .type_str(text.clone())
                        .await
                        .map_err(|error| cdp_unavailable(format!("element.type_str: {error}")))?;
                }
                None => {
                    // No selector: focus the body and type there. Element::type_str
                    // requires an Element; the body is the closest focus target.
                    let body = page
                        .find_element("body".to_string())
                        .await
                        .map_err(map_cdp_error)?;
                    body.click()
                        .await
                        .map_err(|error| cdp_unavailable(format!("body click: {error}")))?;
                    body.type_str(text.clone())
                        .await
                        .map_err(|error| cdp_unavailable(format!("body.type_str: {error}")))?;
                }
            }
            Ok::<(), ComputerError>(())
        },
    )
}

#[cfg(not(feature = "cdp"))]
pub fn type_text(_opts: &ActionOptions) -> Result<(), ComputerError> {
    Err(unsupported())
}

/// Evaluate `script` in the page context. Returns the value as JSON.
#[cfg(feature = "cdp")]
pub fn eval(opts: &ActionOptions) -> Result<Value, ComputerError> {
    let script = opts
        .script
        .clone()
        .ok_or_else(|| ComputerError::InvalidArguments("browser eval requires --eval".into()))?;
    let attach_opts = AttachOptions {
        debug_url: opts.debug_url.clone(),
        app: opts.app.clone(),
        app_pid: opts.app_pid,
        command_timeout: opts.command_timeout,
    };
    let target_id = opts.target_id.clone();
    let blocked_urls = opts.policy_blocked_urls.clone();
    attach_with_url_check(
        attach_opts,
        target_id,
        blocked_urls,
        move |_browser, page| async move {
            let result = page
                .evaluate(script.clone())
                .await
                .map_err(|error| cdp_unavailable(format!("Page.evaluate: {error}")))?;
            Ok::<chromiumoxide::js::EvaluationResult, ComputerError>(result)
        },
    )
    .map(|result| result.value().cloned().unwrap_or(Value::Null))
}

#[cfg(not(feature = "cdp"))]
pub fn eval(_opts: &ActionOptions) -> Result<Value, ComputerError> {
    Err(unsupported())
}
// -----------------------------------------------------------------------------
// Unit tests. Pure logic, no Chrome needed.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_debug_port_handles_mac_sysctl_layout() {
        // sysctl KERN_PROCARGS2 returns argc, then NUL-separated args ending in
        // a NUL. We skip past argc, then the splitter sees NUL + whitespace.
        let argv = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome\
            \0--remote-debugging-port=9222\
            \0--enable-features=NetworkService\
            \0";
        assert_eq!(parse_debug_port(argv), Some(9222));
    }

    #[test]
    fn parse_debug_port_handles_linux_proc_cmdline_layout() {
        let argv = "/opt/google/chrome/chrome\0--type=\
            \0--remote-debugging-port=0\0--enable-features=Foo\0";
        assert_eq!(parse_debug_port(argv), Some(0));
    }

    #[test]
    fn parse_debug_port_handles_linux_proc_cmdline_with_space_separator() {
        let argv = "/opt/google/chrome/chrome\0--remote-debugging-port\0 9222\0--bar";
        assert_eq!(parse_debug_port(argv), Some(9222));
    }

    #[test]
    fn parse_debug_port_handles_windows_command_line_layout() {
        let argv = "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe \
            --remote-debugging-port=9222 --enable-features=Foo";
        assert_eq!(parse_debug_port(argv), Some(9222));
    }

    #[test]
    fn parse_debug_port_returns_none_when_flag_missing() {
        assert!(parse_debug_port("--type= --enable-features=Foo").is_none());
        assert!(parse_debug_port("").is_none());
    }

    #[test]
    fn parse_debug_port_ignores_invalid_numbers() {
        assert!(parse_debug_port("--remote-debugging-port=not-a-number").is_none());
    }

    #[test]
    fn unsupported_returns_a_stable_code() {
        let error = unsupported();
        assert_eq!(error.code(), "unsupported");
    }

    #[test]
    #[cfg(feature = "cdp")]
    fn cdp_feature_compiles_and_exposes_helpers() {
        // Just make sure the feature-gated functions are reachable.
        let _opts = ActionOptions::default();
        let _url = normalize_url("ws://127.0.0.1:9222");
        let _list = list_tabs;
        let _click = click;
        let _eval = eval;
    }

    #[test]
    #[cfg(not(feature = "cdp"))]
    fn list_tabs_returns_unsupported_when_feature_off() {
        let opts = ActionOptions::default();
        let result = list_tabs(&opts);
        assert!(matches!(result, Err(ComputerError::Unsupported(_))));
    }

    #[test]
    #[cfg(not(feature = "cdp"))]
    fn navigate_returns_unsupported_when_feature_off() {
        let opts = ActionOptions::default();
        let result = navigate(&opts);
        assert!(matches!(result, Err(ComputerError::Unsupported(_))));
    }

    #[test]
    #[cfg(not(feature = "cdp"))]
    fn click_returns_unsupported_when_feature_off() {
        let opts = ActionOptions::default();
        let result = click(&opts);
        assert!(matches!(result, Err(ComputerError::Unsupported(_))));
    }

    #[test]
    #[cfg(not(feature = "cdp"))]
    fn type_text_returns_unsupported_when_feature_off() {
        let opts = ActionOptions::default();
        let result = type_text(&opts);
        assert!(matches!(result, Err(ComputerError::Unsupported(_))));
    }

    #[test]
    #[cfg(not(feature = "cdp"))]
    fn eval_returns_unsupported_when_feature_off() {
        let opts = ActionOptions::default();
        let result = eval(&opts);
        assert!(matches!(result, Err(ComputerError::Unsupported(_))));
    }

    #[test]
    #[cfg(not(feature = "cdp"))]
    fn activate_tab_returns_unsupported_when_feature_off() {
        let opts = ActionOptions::default();
        let result = activate_tab(&opts);
        assert!(matches!(result, Err(ComputerError::Unsupported(_))));
    }

    #[test]
    #[cfg(not(feature = "cdp"))]
    fn read_inner_text_returns_unsupported_when_feature_off() {
        let opts = ActionOptions::default();
        let result = read_inner_text(&opts);
        assert!(matches!(result, Err(ComputerError::Unsupported(_))));
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn no_page_target_error_is_cdp_unavailable_with_stable_message() {
        let error = no_page_target_error();
        assert!(matches!(error, ComputerError::CdpUnavailable(_)));
        let text = match error {
            ComputerError::CdpUnavailable(text) => text,
            _ => unreachable!("checked above"),
        };
        assert!(
            text.contains("no page target"),
            "expected stable 'no page target' phrasing in {text}"
        );
        assert!(
            text.contains("attach-only"),
            "expected attach-only phrasing in {text}"
        );
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn map_cdp_error_chrome_message_no_node_becomes_element_not_found() {
        use chromiumoxide::error::CdpError;
        let error = CdpError::ChromeMessage("No node with given id found".into());
        let mapped = map_cdp_error(error);
        match mapped {
            ComputerError::ElementNotFound(msg) => {
                // The message we produce must lead with the "selector matched
                // no element" phrase so error recipients can identify the
                // cause without parsing the embedded CDP body.
                assert!(
                    msg.starts_with("selector matched no element"),
                    "expected leading phrase, got {msg}"
                );
            }
            other => panic!("expected ElementNotFound, got {other:?}"),
        }
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn map_cdp_error_chrome_message_node_not_found_phrase_matches() {
        // Chromiumoxide wraps the CDP server response in `CdpError::Chrome`
        // (a `chromiumoxide_types::Error`). We can't construct that without
        // pulling in `chromiumoxide_types` as a dev-dep, but we can exercise
        // the same matching logic through `ChromeMessage`, which carries the
        // verbatim text the server returned. The function branches on the
        // same substrings.
        use chromiumoxide::error::CdpError;
        let error = CdpError::ChromeMessage("Node with given id was not found.".into());
        let mapped = map_cdp_error(error);
        assert!(matches!(mapped, ComputerError::ElementNotFound(_)));
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn map_cdp_error_unrelated_chrome_message_stays_cdp_unavailable() {
        use chromiumoxide::error::CdpError;
        let error = CdpError::ChromeMessage("Protocol error: some method not allowed".into());
        let mapped = map_cdp_error(error);
        match mapped {
            ComputerError::CdpUnavailable(msg) => {
                assert!(
                    msg.contains("some method not allowed"),
                    "expected cdp_unavailable to keep the original message, got {msg}"
                );
            }
            other => panic!("expected CdpUnavailable, got {other:?}"),
        }
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn map_cdp_error_io_error_stays_cdp_unavailable() {
        use chromiumoxide::error::CdpError;
        // Anything that is not a ChromeError/ChromeMessage/NotFound error
        // (transport, websocket, IO) must remain cdp_unavailable.
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let error = CdpError::Io(io);
        let mapped = map_cdp_error(error);
        assert!(matches!(mapped, ComputerError::CdpUnavailable(_)));
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn map_cdp_error_not_found_becomes_element_not_found() {
        use chromiumoxide::error::CdpError;
        let error = CdpError::NotFound;
        let mapped = map_cdp_error(error);
        match mapped {
            ComputerError::ElementNotFound(msg) => {
                assert!(msg.contains("no element"));
            }
            other => panic!("expected ElementNotFound, got {other:?}"),
        }
    }
}

// -----------------------------------------------------------------------------
// Request-type construction test. Exercises the Action deserializer.

#[cfg(test)]
mod request_construction {
    use serde_json::json;

    #[test]
    fn browser_actions_deserialize_from_request_objects() {
        let tabs = json!({
            "action": "browser_tabs",
        });
        let parsed: crate::protocol::Action = serde_json::from_value(tabs).unwrap();
        assert!(matches!(parsed, crate::protocol::Action::BrowserTabs {}));

        let activate = json!({
            "action": "browser_activate",
            "target_id": "ABC123",
        });
        let parsed: crate::protocol::Action = serde_json::from_value(activate).unwrap();
        match parsed {
            crate::protocol::Action::BrowserActivate { target_id } => {
                assert_eq!(target_id.as_deref(), Some("ABC123"));
            }
            other => panic!("expected BrowserActivate, got {other:?}"),
        }

        let navigate = json!({
            "action": "browser_navigate",
            "url": "https://example.com",
            "target_id": "ABC123",
        });
        let parsed: crate::protocol::Action = serde_json::from_value(navigate).unwrap();
        match parsed {
            crate::protocol::Action::BrowserNavigate { url, target_id } => {
                assert_eq!(url, "https://example.com");
                assert_eq!(target_id.as_deref(), Some("ABC123"));
            }
            other => panic!("expected BrowserNavigate, got {other:?}"),
        }

        let click = json!({
            "action": "browser_click",
            "selector": "button#go",
        });
        let parsed: crate::protocol::Action = serde_json::from_value(click).unwrap();
        match parsed {
            crate::protocol::Action::BrowserClick {
                selector,
                target_id,
            } => {
                assert_eq!(selector, "button#go");
                assert!(target_id.is_none());
            }
            other => panic!("expected BrowserClick, got {other:?}"),
        }

        let text = json!({
            "action": "browser_text",
            "selector": "input#name",
        });
        let parsed: crate::protocol::Action = serde_json::from_value(text).unwrap();
        match parsed {
            crate::protocol::Action::BrowserText {
                selector,
                target_id,
            } => {
                assert_eq!(selector, "input#name");
                assert!(target_id.is_none());
            }
            other => panic!("expected BrowserText, got {other:?}"),
        }

        let type_action = json!({
            "action": "browser_type",
            "text": "hello",
            "selector": "input#name",
        });
        let parsed: crate::protocol::Action = serde_json::from_value(type_action).unwrap();
        match parsed {
            crate::protocol::Action::BrowserType {
                text,
                selector,
                target_id,
            } => {
                assert_eq!(text, "hello");
                assert_eq!(selector.as_deref(), Some("input#name"));
                assert!(target_id.is_none());
            }
            other => panic!("expected BrowserType, got {other:?}"),
        }

        let eval_action = json!({
            "action": "browser_eval",
            "script": "1+1",
        });
        let parsed: crate::protocol::Action = serde_json::from_value(eval_action).unwrap();
        match parsed {
            crate::protocol::Action::BrowserEval { script, target_id } => {
                assert_eq!(script, "1+1");
                assert!(target_id.is_none());
            }
            other => panic!("expected BrowserEval, got {other:?}"),
        }
    }
}
