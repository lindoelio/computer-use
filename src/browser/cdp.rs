//! CDP driver: connection, target resolution, navigation with waits, tabs,
//! snapshots, actions, screenshots, and evaluation.
//!
//! The old implementation created a runtime inside `connect` and dropped it
//! before the command ran, which killed the chromiumoxide handler. This module
//! keeps one runtime alive for the whole command: connect, spawn the handler,
//! run the operation, drop the runtime.

#![cfg(feature = "cdp")]
#![allow(unsafe_code)]

use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, CaptureScreenshotFormat,
};
use chromiumoxide::cdp::browser_protocol::target::TargetId;
use chromiumoxide::{Browser, Page};
use futures::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};

use crate::browser::actions;
use crate::browser::launch;
use crate::browser::snapshot;
use crate::browser::{BrowserCommand, BrowserOptions, ScreenshotFormat, WaitSpec};
use crate::error::ComputerError;
use crate::policy;

/// JSON-friendly tab summary.
#[derive(Debug, Clone, Serialize)]
pub struct TabSummary {
    /// CDP `targetId` (opaque string).
    pub target_id: String,
    /// Same value under the older name `handle`.
    pub handle: String,
    /// Best-effort page title.
    pub title: Option<String>,
    /// Current URL, or `<blocked>` when policy blocked it.
    pub url: String,
    /// CDP `type` (`page`, `iframe`, `service_worker`, ...).
    pub kind: String,
    /// `targetId` of the opener, when the tab was opened by another page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opener_id: Option<String>,
    /// True when `[browser].blocked_urls` matched the URL.
    pub blocked: bool,
}

/// Stable error used when the binary is built without `--features cdp`.
pub fn unsupported() -> ComputerError {
    ComputerError::Unsupported(
        "browser commands require the `cdp` feature (build with --features cdp)".into(),
    )
}

fn cdp_unavailable(message: impl Into<String>) -> ComputerError {
    ComputerError::CdpUnavailable(message.into())
}

/// Parse `--remote-debugging-port=N` from argv text.
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

#[cfg(target_os = "macos")]
fn read_command_line_args(pid: i32) -> Result<String, ComputerError> {
    use std::ffi::{c_int, c_void};
    extern "C" {
        fn sysctl(
            name: *const c_int,
            namelen: u32,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }
    const CTL_KERN: c_int = 1;
    const KERN_PROCARGS2: c_int = 49;
    const ARG_MAX: usize = 262_144;
    let mut mib = [CTL_KERN, KERN_PROCARGS2, pid];
    let mut buffer = vec![0u8; ARG_MAX];
    let mut size = buffer.len();
    let result = unsafe {
        sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buffer.as_mut_ptr() as *mut c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(cdp_unavailable(format!(
            "sysctl KERN_PROCARGS2 failed for pid {pid}"
        )));
    }
    buffer.truncate(size);
    let text = String::from_utf8_lossy(&buffer).to_string();
    Ok(text)
}

#[cfg(target_os = "linux")]
fn read_command_line_args(pid: i32) -> Result<String, ComputerError> {
    let path = format!("/proc/{pid}/cmdline");
    let bytes = std::fs::read(&path).map_err(ComputerError::Io)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

#[cfg(target_os = "windows")]
fn read_command_line_args(pid: i32) -> Result<String, ComputerError> {
    Err(cdp_unavailable(format!(
        "Windows command-line lookup for pid {pid} is not implemented; pass --cdp-url http://127.0.0.1:<port> or use `browser launch`"
    )))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn read_command_line_args(pid: i32) -> Result<String, ComputerError> {
    Err(cdp_unavailable(format!(
        "command-line lookup is not implemented for this OS (pid {pid}); pass --cdp-url"
    )))
}

/// Discover the debug port from a browser pid's argv.
pub fn discover_debug_port(pid: i32) -> Result<u16, ComputerError> {
    let args = read_command_line_args(pid)?;
    parse_debug_port(&args).ok_or_else(|| {
        cdp_unavailable(format!(
            "pid {pid} command line has no --remote-debugging-port=N; use `browser launch` or --cdp-url"
        ))
    })
}

fn normalize_url(input: &str) -> Result<String, ComputerError> {
    let trimmed = input.trim();
    if trimmed.starts_with("ws://")
        || trimmed.starts_with("wss://")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
    {
        if trimmed.contains("://") && trimmed.split("://").nth(1).unwrap_or("").is_empty() {
            return Err(cdp_unavailable(format!("cdp url has no host: {input}")));
        }
        return Ok(trimmed.trim_end_matches('/').to_string());
    }
    Err(cdp_unavailable(format!(
        "unsupported cdp url scheme: {input} (expected ws://, wss://, http://, or https://)"
    )))
}

/// Resolve the websocket/http endpoint.
///
/// Order: `--cdp-url` wins; then `--app` pid argv discovery; then the newest
/// managed `browser launch` session. Nothing else is guessed.
pub fn pick_websocket_url(options: &BrowserOptions) -> Result<String, ComputerError> {
    if let Some(url) = options
        .debug_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return normalize_url(url);
    }
    if let Some(pid) = options.app_pid {
        let port = discover_debug_port(pid)?;
        return Ok(format!("http://127.0.0.1:{port}"));
    }
    if let Ok(records) = launch::list_records() {
        if let Some(record) = records.into_iter().next_back() {
            if crate::cache::pid_alive(record.pid) {
                return Ok(record.cdp_url);
            }
        }
    }
    Err(cdp_unavailable(
        "no browser endpoint: pass --cdp-url http://127.0.0.1:<port>, use --app with a browser started with --remote-debugging-port, or run `browser launch`",
    ))
}

fn new_runtime() -> Result<tokio::runtime::Runtime, ComputerError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| cdp_unavailable(format!("tokio runtime: {error}")))
}

/// Run an async closure with a connected browser and a live handler.
pub fn run_browser<T, F, Fut>(options: &BrowserOptions, operation: F) -> Result<T, ComputerError>
where
    F: FnOnce(Browser) -> Fut,
    Fut: std::future::Future<Output = Result<T, ComputerError>>,
{
    let runtime = new_runtime()?;
    let url = pick_websocket_url(options)?;
    let (browser, mut handler) = runtime
        .block_on(Browser::connect(url))
        .map_err(|error| cdp_unavailable(format!("connect: {error}")))?;
    runtime.spawn(async move { while handler.next().await.is_some() {} });
    runtime.block_on(operation(browser))
}

/// Resolve the raw websocket URL for a browser endpoint.
fn raw_browser_ws_url(options: &BrowserOptions) -> Result<String, ComputerError> {
    if let Some(url) = options.debug_url.as_deref() {
        if url.starts_with("ws://") || url.starts_with("wss://") {
            return Ok(url.to_string());
        }
        if url.starts_with("http://") || url.starts_with("https://") {
            return http_ws_url(url);
        }
    }
    if let Some(pid) = options.app_pid {
        let port = discover_debug_port(pid)?;
        return http_ws_url(&format!("http://127.0.0.1:{port}"));
    }
    if let Ok(records) = launch::list_records() {
        if let Some(record) = records.into_iter().next_back() {
            if let Some(ws) = record.ws_url {
                return Ok(ws);
            }
            return http_ws_url(&record.cdp_url);
        }
    }
    Err(cdp_unavailable("no browser endpoint for the dialog rescue"))
}

fn http_ws_url(base: &str) -> Result<String, ComputerError> {
    let rest = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .ok_or_else(|| cdp_unavailable(format!("not an http url: {base}")))?;
    let (hostport, path) = match rest.split_once('/') {
        Some((host, tail)) => (host, format!("/{tail}")),
        None => (rest, "/".to_string()),
    };
    let address = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:80")
    };
    let path = if path.ends_with('/') {
        format!("{path}json/version")
    } else {
        format!("{path}/json/version")
    };
    let mut stream = std::net::TcpStream::connect(&address)
        .map_err(|error| cdp_unavailable(format!("connect {address}: {error}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(ComputerError::Io)?;
    use std::io::{ErrorKind, Read, Write};
    let request = format!("GET {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(ComputerError::Io)?;
    // Read until EOF, a timeout, or a sane cap. Chrome does not always close
    // the connection promptly, and a read timeout must not discard the body we
    // already have (it used to surface as an EAGAIN `io_error`).
    let mut response = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                response.extend_from_slice(&buffer[..read]);
                if response.len() > 64 * 1024 {
                    break;
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == ErrorKind::TimedOut => break,
            Err(error) => return Err(ComputerError::Io(error)),
        }
    }
    let response = String::from_utf8_lossy(&response);
    let body = response.split("\r\n\r\n").nth(1).unwrap_or("");
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| cdp_unavailable(format!("json/version from {base}: {error}")))?;
    value
        .get("webSocketDebuggerUrl")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| cdp_unavailable(format!("json/version from {base} had no websocket url")))
}

async fn read_raw_response<T>(
    connection: &mut chromiumoxide::Connection<T>,
    id: chromiumoxide::types::CallId,
) -> Result<serde_json::Value, ComputerError>
where
    T: chromiumoxide::types::EventMessage + Unpin,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, connection.next()).await {
            Err(_) => return Err(ComputerError::Timeout("raw cdp response timed out".into())),
            Ok(None) => return Err(cdp_unavailable("raw cdp connection closed")),
            Ok(Some(Err(error))) => return Err(cdp_unavailable(format!("raw cdp: {error}"))),
            Ok(Some(Ok(chromiumoxide::types::Message::Response(response)))) => {
                if response.id != id {
                    continue;
                }
                if let Some(error) = response.error {
                    return Err(ComputerError::ExecutionFailed(error.message));
                }
                return Ok(response.result.unwrap_or(serde_json::Value::Null));
            }
            Ok(Some(Ok(_))) => continue,
        }
    }
}

/// Send `Page.handleJavaScriptDialog` over a fresh raw connection. This works
/// even when the main chromiumoxide target is blocked by the dialog.
pub fn rescue_dialog(
    options: &BrowserOptions,
    target_id: &str,
    accept: bool,
    prompt_text: Option<&str>,
) -> Result<(), ComputerError> {
    let ws = raw_browser_ws_url(options)?;
    let target = target_id.to_string();
    let prompt = prompt_text.map(str::to_string);
    let runtime = new_runtime()?;
    runtime.block_on(async move {
        let mut connection =
            chromiumoxide::Connection::<chromiumoxide::cdp::CdpEventMessage>::connect(&ws)
                .await
                .map_err(|error| cdp_unavailable(format!("raw cdp connect: {error}")))?;
        let attach = connection
            .submit_command(
                "Target.attachToTarget".into(),
                None,
                serde_json::json!({"targetId": target, "flatten": true}),
            )
            .map_err(|error| cdp_unavailable(format!("raw attach encode: {error}")))?;
        let response = read_raw_response(&mut connection, attach).await?;
        let session = response
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| cdp_unavailable("raw attach returned no sessionId"))?;
        let session_id = chromiumoxide::cdp::browser_protocol::target::SessionId::new(session);
        let mut params = serde_json::json!({"accept": accept});
        if let Some(prompt) = prompt {
            params["promptText"] = serde_json::Value::String(prompt);
        }
        let dialog = connection
            .submit_command(
                "Page.handleJavaScriptDialog".into(),
                Some(session_id),
                params,
            )
            .map_err(|error| cdp_unavailable(format!("raw dialog encode: {error}")))?;
        let _ = read_raw_response(&mut connection, dialog).await;
        Ok(())
    })
}

/// Call the raw dialog rescue after an action that may have opened one.
fn maybe_rescue_dialog(options: &BrowserOptions, result: &serde_json::Value) {
    if options.on_dialog == crate::browser::DialogMode::Fail {
        return;
    }
    let Some(target) = result.get("target_id").and_then(serde_json::Value::as_str) else {
        return;
    };
    let accept = options.on_dialog == crate::browser::DialogMode::Accept;
    let _ = rescue_dialog(options, target, accept, None);
}

/// Handle `Page.javascriptDialogOpening` in the background so actions that
/// trigger an alert/confirm/prompt do not hang.
///
/// The listener is registered before this returns: spawning a task that
/// subscribes later races the action that opens the dialog, and the dialog
/// then blocks the load wait until it times out.
async fn spawn_dialog_handler(page: &Page, mode: crate::browser::DialogMode) {
    let Ok(mut events) = page
        .event_listener::<chromiumoxide::cdp::browser_protocol::page::EventJavascriptDialogOpening>(
        )
        .await
    else {
        return;
    };
    let dialog_page = page.clone();
    tokio::spawn(async move {
        while let Some(_event) = events.next().await {
            let accept = mode == crate::browser::DialogMode::Accept;
            let params =
                chromiumoxide::cdp::browser_protocol::page::HandleJavaScriptDialogParams::new(
                    accept,
                );
            let _ = dialog_page.execute(params).await;
        }
    });
}

/// The visible agent marker. It is idempotent: the guard stops a second
/// injection from stacking badges or double-prefixing the title.
const AGENT_MARKER_JS: &str = r##"(() => {
  if (window.__cuAgentMarker) return;
  window.__cuAgentMarker = true;
  const install = () => {
    if (!document.documentElement || document.getElementById('__cu-agent-host')) return;
    const host = document.createElement('div');
    host.id = '__cu-agent-host';
    host.style.cssText = 'all:initial;position:fixed;inset:auto 16px 16px auto;z-index:2147483647;pointer-events:none;';
    const root = host.attachShadow({mode: 'open'});
    root.innerHTML = '<style>' +
      '.badge{font:600 12px/1.4 -apple-system,BlinkMacSystemFont,Segoe UI,sans-serif;color:#fff;background:rgba(17,17,17,.82);padding:6px 10px;border-radius:999px;display:flex;align-items:center;gap:6px;box-shadow:0 2px 12px rgba(0,0,0,.35)}' +
      '.dot{width:8px;height:8px;border-radius:50%;background:#34d399;animation:cu-pulse 1.6s infinite}' +
      '@keyframes cu-pulse{0%{box-shadow:0 0 0 0 rgba(52,211,153,.7)}70%{box-shadow:0 0 0 8px rgba(52,211,153,0)}100%{box-shadow:0 0 0 0 rgba(52,211,153,0)}}' +
      '.ring{position:fixed;left:0;top:0;width:28px;height:28px;margin:-14px 0 0 -14px;border:2px solid rgba(52,211,153,.9);border-radius:50%;box-shadow:0 0 0 6px rgba(52,211,153,.18);opacity:0;transform:scale(.6);transition:transform .15s ease,opacity .15s ease;pointer-events:none}' +
      '.ring.on{opacity:1;transform:scale(1)}' +
      '.ring.click{transform:scale(.75);background:rgba(52,211,153,.25)}' +
      '</style><div class="badge"><span class="dot"></span>computer-use agent</div><div class="ring"></div>';
    document.documentElement.appendChild(host);
    const ring = root.querySelector('.ring');
    window.__cuAgent = {
      move(x, y, kind) {
        ring.style.left = x + 'px';
        ring.style.top = y + 'px';
        ring.classList.add('on');
        ring.classList.toggle('click', kind === 'click');
        clearTimeout(window.__cuAgent._t);
        window.__cuAgent._t = setTimeout(() => ring.classList.remove('on', 'click'), kind === 'click' ? 900 : 1400);
      },
      hide() { ring.classList.remove('on', 'click'); }
    };
    if (!document.title.startsWith('\u{1F916} ')) document.title = '\u{1F916} ' + document.title;
    const robot = 'data:image/svg+xml,' + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><text y="50" font-size="52">\u{1F916}</text></svg>');
    let icon = document.querySelector('link[rel~="icon"]');
    if (!icon) { icon = document.createElement('link'); icon.rel = 'icon'; (document.head || document.documentElement).appendChild(icon); }
    icon.href = robot;
    const title = document.querySelector('title');
    if (title) {
      new MutationObserver(() => {
        if (!document.title.startsWith('\u{1F916} ')) document.title = '\u{1F916} ' + document.title;
      }).observe(title, {childList: true, characterData: true, subtree: true});
    }
  };
  if (document.documentElement) install();
  else document.addEventListener('DOMContentLoaded', install, {once: true});
})();"##;

/// Whether the endpoint is the newest managed `browser launch` session.
fn endpoint_is_managed(options: &BrowserOptions) -> bool {
    if options.debug_url.is_some() || options.app_pid.is_some() {
        return false;
    }
    launch::list_records()
        .ok()
        .and_then(|records| records.into_iter().next_back())
        .map(|record| crate::cache::pid_alive(record.pid))
        .unwrap_or(false)
}

fn marker_enabled(options: &BrowserOptions) -> bool {
    match options.agent_overlay {
        crate::browser::AgentOverlay::On => true,
        crate::browser::AgentOverlay::Off => false,
        crate::browser::AgentOverlay::Auto => endpoint_is_managed(options),
    }
}

async fn install_agent_marker(browser: &Browser, page: &Page) {
    let params = AddScriptToEvaluateOnNewDocumentParams::builder()
        .source(AGENT_MARKER_JS)
        .run_immediately(true)
        .build()
        .expect("marker params");
    let _ = browser.execute(params).await;
    let _ = actions::evaluate(page, AGENT_MARKER_JS, false).await;
}

/// Run an async closure with a connected browser and a resolved page.
pub fn run_page<T, F, Fut>(
    options: &BrowserOptions,
    target_id: Option<String>,
    operation: F,
) -> Result<T, ComputerError>
where
    F: FnOnce(Browser, Page) -> Fut,
    Fut: std::future::Future<Output = Result<T, ComputerError>>,
{
    let blocked = options.blocked_urls.clone();
    let dialog_mode = options.on_dialog;
    run_browser(options, move |mut browser| async move {
        let page = resolve_page(&mut browser, target_id.as_deref()).await?;
        let url = current_url(&page).await;
        if let Some(pattern) = policy::url_blocked(&url, &blocked) {
            return Err(ComputerError::BlockedUrl(format!(
                "current page matched {pattern}"
            )));
        }
        if marker_enabled(options) {
            install_agent_marker(&browser, &page).await;
        }
        if dialog_mode != crate::browser::DialogMode::Fail {
            spawn_dialog_handler(&page, dialog_mode).await;
        }
        operation(browser, page).await
    })
}

async fn get_page_attached(browser: &Browser, target: &TargetId) -> Result<Page, ComputerError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match browser.get_page(target.clone()).await {
            Ok(page) => return Ok(page),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(cdp_unavailable(format!(
                        "target {} is not attachable: {error}",
                        target.as_ref()
                    )));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn resolve_page_attached(
    browser: &mut Browser,
    target_id: Option<&str>,
) -> Result<Page, ComputerError> {
    if let Some(target) = target_id.map(str::trim).filter(|value| !value.is_empty()) {
        return get_page_attached(browser, &TargetId::new(target.to_string())).await;
    }
    let targets = browser
        .fetch_targets()
        .await
        .map_err(|error| cdp_unavailable(format!("fetch_targets: {error}")))?;
    let pages: Vec<_> = targets
        .into_iter()
        .filter(|target| target.r#type == "page")
        .collect();
    match pages.len() {
        0 => Err(cdp_unavailable(
            "no page target found; open a tab in the browser, or run `browser tab-new --url <url>`",
        )),
        1 => get_page_attached(browser, &pages[0].target_id).await,
        _ => {
            let list = pages
                .iter()
                .map(|target| format!("{} ({})", target.target_id.as_ref(), target.url))
                .collect::<Vec<_>>()
                .join(", ");
            Err(ComputerError::InvalidArguments(format!(
                "multiple page targets: {list}. Pass --target <targetId>; list them with `browser tabs`."
            )))
        }
    }
}

async fn get_page_when_attached(
    browser: &Browser,
    target: &TargetId,
) -> Result<Page, ComputerError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match browser.get_page(target.clone()).await {
            Ok(page) => {
                // A freshly attached target has a session before its frame
                // manager has the main frame. `Page.navigate` is dropped when
                // the frame is missing, so wait for it here.
                let frame_deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match page.mainframe().await {
                        Ok(Some(_)) => return Ok(page),
                        Ok(None) if Instant::now() < frame_deadline => {
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        Ok(None) => {
                            return Err(cdp_unavailable(format!(
                                "target {} has no main frame",
                                target.as_ref()
                            )));
                        }
                        Err(_) if Instant::now() < frame_deadline => {
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        Err(error) => {
                            return Err(cdp_unavailable(format!(
                                "target {} frame lookup failed: {error}",
                                target.as_ref()
                            )));
                        }
                    }
                }
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(cdp_unavailable(format!(
                        "target {} is not attachable: {error}",
                        target.as_ref()
                    )));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Resolve the page target. With no `--target` a single page is used;
/// multiple pages are an explicit error instead of a silent first-page pick.
pub async fn resolve_page(
    browser: &mut Browser,
    target_id: Option<&str>,
) -> Result<Page, ComputerError> {
    if let Some(target) = target_id.map(str::trim).filter(|value| !value.is_empty()) {
        return get_page_when_attached(browser, &TargetId::new(target.to_string())).await;
    }
    let targets = browser
        .fetch_targets()
        .await
        .map_err(|error| cdp_unavailable(format!("fetch_targets: {error}")))?;
    let pages: Vec<_> = targets
        .into_iter()
        .filter(|target| target.r#type == "page")
        .collect();
    match pages.len() {
        0 => Err(cdp_unavailable(
            "no page target found; open a tab in the browser, or run `browser tab-new --url <url>`",
        )),
        1 => get_page_when_attached(browser, &pages[0].target_id).await,
        _ => {
            let list = pages
                .iter()
                .map(|target| {
                    format!(
                        "{} ({})",
                        target.target_id.as_ref(),
                        if target.url.is_empty() {
                            "about:blank"
                        } else {
                            target.url.as_str()
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(ComputerError::InvalidArguments(format!(
                "multiple page targets: {list}. Pass --target <targetId>; list them with `browser tabs`."
            )))
        }
    }
}

async fn current_url(page: &Page) -> String {
    page.url().await.ok().flatten().unwrap_or_default()
}

fn target_summary(
    target: &chromiumoxide::cdp::browser_protocol::target::TargetInfo,
    blocked_urls: &[String],
) -> TabSummary {
    let url = target.url.clone();
    let blocked = policy::url_blocked(&url, blocked_urls).is_some();
    TabSummary {
        target_id: target.target_id.as_ref().to_string(),
        handle: target.target_id.as_ref().to_string(),
        title: if blocked {
            None
        } else {
            Some(target.title.clone())
        },
        url: if blocked {
            "<blocked>".to_string()
        } else {
            url
        },
        kind: target.r#type.clone(),
        opener_id: target.opener_id.as_ref().map(|id| id.as_ref().to_string()),
        blocked,
    }
}

async fn list_tabs(browser: &mut Browser, blocked_urls: &[String]) -> Result<Value, ComputerError> {
    let targets = browser
        .fetch_targets()
        .await
        .map_err(|error| cdp_unavailable(format!("fetch_targets: {error}")))?;
    let tabs: Vec<TabSummary> = targets
        .iter()
        .map(|target| target_summary(target, blocked_urls))
        .collect();
    let count = tabs.len();
    Ok(json!({"tabs": tabs, "count": count}))
}

async fn activate(
    browser: &mut Browser,
    target: &str,
    blocked_urls: &[String],
) -> Result<Value, ComputerError> {
    let page = get_page_when_attached(browser, &TargetId::new(target.to_string())).await?;
    let url = current_url(&page).await;
    if let Some(pattern) = policy::url_blocked(&url, blocked_urls) {
        return Err(ComputerError::BlockedUrl(format!(
            "target {target} current page matched {pattern}"
        )));
    }
    page.bring_to_front()
        .await
        .map_err(|error| cdp_unavailable(format!("bringToFront: {error}")))?;
    Ok(json!({"target_id": target, "url": url}))
}

fn escape_selector(selector: &str) -> String {
    serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string())
}

/// Wait for a condition by polling the page. Works even when the event
/// happened before the command started.
pub async fn wait_for(
    page: &Page,
    wait: &WaitSpec,
    timeout: Duration,
) -> Result<(), ComputerError> {
    if wait.is_none() {
        return Ok(());
    }
    let deadline = Instant::now() + timeout;
    loop {
        let done = match wait {
            WaitSpec::None => true,
            WaitSpec::Load => read_ready_state(page).await.as_deref() == Some("complete"),
            WaitSpec::DomContentLoaded => matches!(
                read_ready_state(page).await.as_deref(),
                Some("interactive") | Some("complete")
            ),
            WaitSpec::NetworkIdle => {
                if read_ready_state(page).await.as_deref() == Some("complete") {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    true
                } else {
                    false
                }
            }
            WaitSpec::Selector(selector) => selector_visible(page, selector).await,
            WaitSpec::Url(glob) => policy::glob_match(glob, &current_url(page).await),
        };
        if done {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ComputerError::Timeout(format!(
                "wait {} timed out after {} ms",
                describe_wait(wait),
                timeout.as_millis()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn describe_wait(wait: &WaitSpec) -> String {
    match wait {
        WaitSpec::None => "none".into(),
        WaitSpec::Load => "load".into(),
        WaitSpec::DomContentLoaded => "domcontentloaded".into(),
        WaitSpec::NetworkIdle => "networkidle".into(),
        WaitSpec::Selector(selector) => format!("selector:{selector}"),
        WaitSpec::Url(glob) => format!("url:{glob}"),
    }
}

async fn read_ready_state(page: &Page) -> Option<String> {
    actions::evaluate(page, "document.readyState", false)
        .await
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

async fn selector_visible(page: &Page, selector: &str) -> bool {
    let expression = format!(
        "(() => {{ const el = document.querySelector({}); if (!el) return false; const rect = el.getBoundingClientRect(); const style = window.getComputedStyle(el); return style.visibility !== 'hidden' && style.display !== 'none' && rect.width > 0 && rect.height > 0; }})()",
        escape_selector(selector)
    );
    actions::evaluate(page, &expression, false)
        .await
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

async fn evaluate_ignoring_context_loss(
    page: &Page,
    expression: &str,
) -> Result<(), ComputerError> {
    // A navigation destroys the execution context before the response comes
    // back. That error is expected, not a failure of the navigation itself.
    match actions::evaluate(page, expression, false).await {
        Ok(_) => Ok(()),
        Err(ComputerError::ExecutionFailed(message))
            if message.contains("context")
                || message.contains("destroyed")
                || message.contains("Execution") =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

async fn navigate(
    page: &Page,
    url: &str,
    wait: &WaitSpec,
    timeout: Duration,
) -> Result<(), ComputerError> {
    let mut events = page
        .event_listener::<chromiumoxide::cdp::browser_protocol::page::EventFrameNavigated>()
        .await
        .ok();
    let before = current_url(page).await;
    let target = serde_json::to_string(url).map_err(|error| {
        ComputerError::InvalidArguments(format!("url is not encodable: {error}"))
    })?;
    evaluate_ignoring_context_loss(page, &format!("window.location.assign({target})")).await?;
    if !wait.is_none() {
        if let Some(stream) = events.as_mut() {
            let _ = tokio::time::timeout(timeout.min(Duration::from_secs(10)), stream.next()).await;
        }
    }
    wait_after_navigation(page, &before, wait, timeout).await
}

/// Wait until a navigation started and then until the caller's condition.
async fn wait_after_navigation(
    page: &Page,
    before_url: &str,
    wait: &WaitSpec,
    timeout: Duration,
) -> Result<(), ComputerError> {
    if wait.is_none() {
        tokio::time::sleep(Duration::from_millis(50)).await;
        return Ok(());
    }
    let deadline = Instant::now() + timeout;
    loop {
        let url = current_url(page).await;
        let state = read_ready_state(page).await.unwrap_or_default();
        if url != before_url || state == "loading" {
            break;
        }
        // A same-URL reload or a cached navigation can finish between polls.
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    wait_for(page, wait, timeout).await
}

async fn reload(page: &Page, wait: &WaitSpec, timeout: Duration) -> Result<(), ComputerError> {
    let mut events = page
        .event_listener::<chromiumoxide::cdp::browser_protocol::page::EventFrameNavigated>()
        .await
        .ok();
    let before = current_url(page).await;
    evaluate_ignoring_context_loss(page, "window.location.reload()").await?;
    if !wait.is_none() {
        if let Some(stream) = events.as_mut() {
            let _ = tokio::time::timeout(timeout.min(Duration::from_secs(10)), stream.next()).await;
        }
    }
    wait_after_navigation(page, &before, wait, timeout).await
}

async fn history_move(page: &Page, forward: bool) -> Result<String, ComputerError> {
    let before = current_url(page).await;
    let expression = if forward {
        "window.history.forward()"
    } else {
        "window.history.back()"
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        // A navigation can replace the execution context and swallow the
        // evaluate; retry it until the deadline instead of failing after one
        // lost call.
        let _ = evaluate_ignoring_context_loss(page, expression).await;
        for _ in 0..40 {
            let url = current_url(page).await;
            if url != before {
                return Ok(url);
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if Instant::now() >= deadline {
            return Err(ComputerError::ExecutionFailed(if forward {
                "no forward history entry".into()
            } else {
                "no back history entry".into()
            }));
        }
    }
}

async fn screenshot(
    page: &Page,
    output: Option<&str>,
    full_page: bool,
    format: ScreenshotFormat,
    quality: Option<i64>,
    base64: bool,
) -> Result<Value, ComputerError> {
    let cdp_format = match format {
        ScreenshotFormat::Png => CaptureScreenshotFormat::Png,
        ScreenshotFormat::Jpeg => CaptureScreenshotFormat::Jpeg,
    };
    let mut builder = chromiumoxide::page::ScreenshotParams::builder()
        .format(cdp_format)
        .full_page(full_page);
    if let Some(quality) = quality {
        builder = builder.quality(quality);
    }
    let bytes = page
        .screenshot(builder.build())
        .await
        .map_err(|error| cdp_unavailable(format!("captureScreenshot: {error}")))?;
    let path = match output {
        Some(path) => std::path::PathBuf::from(path),
        None => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0);
            std::env::temp_dir().join(format!(
                "computer-use-browser-{stamp}.{}",
                format.extension()
            ))
        }
    };
    std::fs::write(&path, &bytes).map_err(ComputerError::Io)?;
    let mut result = json!({
        "path": path.display().to_string(),
        "bytes": bytes.len(),
        "format": format.extension(),
        "full_page": full_page,
    });
    if base64 {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        result["base64"] = json!(encoded);
    }
    Ok(result)
}

/// Resolve a snapshot ref into a backend node id for the current target.
pub async fn resolve_ref(
    options: &BrowserOptions,
    page: &Page,
    target_id: &str,
    ref_id: &str,
) -> Result<i64, ComputerError> {
    let snapshot = snapshot::load(options.session.as_deref(), target_id)?.ok_or_else(|| {
        ComputerError::StaleElement(
            "no snapshot in this session; run `browser snapshot` first, then use --ref".into(),
        )
    })?;
    let current = current_url(page).await;
    if !snapshot.url.is_empty() && !same_document_url(&snapshot.url, &current) {
        return Err(ComputerError::StaleElement(format!(
            "the page changed since the snapshot ({} -> {current}); run `browser snapshot` again",
            snapshot.url
        )));
    }
    let node = snapshot.find_ref(ref_id).ok_or_else(|| {
        ComputerError::StaleElement(format!(
            "ref {ref_id} is not in the cached snapshot; run `browser snapshot` again"
        ))
    })?;
    node.backend_node_id
        .ok_or_else(|| ComputerError::StaleElement(format!("ref {ref_id} has no backend node id")))
}

fn same_document_url(left: &str, right: &str) -> bool {
    left.trim_end_matches('/') == right.trim_end_matches('/')
}

fn element_args(
    selector: Option<&str>,
    ref_id: Option<&str>,
) -> Result<(Option<String>, Option<String>), ComputerError> {
    match (selector, ref_id) {
        (Some(_), Some(_)) => Err(ComputerError::InvalidArguments(
            "pass --selector or --ref, not both".into(),
        )),
        (None, None) => Err(ComputerError::InvalidArguments(
            "pass --selector or --ref".into(),
        )),
        (Some(selector), None) => Ok((Some(selector.to_string()), None)),
        (None, Some(ref_id)) => Ok((None, Some(ref_id.to_string()))),
    }
}

/// `--selector`/`--ref` for a command where focusing an element is optional,
/// such as `browser press` (a page-level key press has no target element).
fn optional_element_args(
    selector: Option<&str>,
    ref_id: Option<&str>,
) -> Result<(Option<String>, Option<String>), ComputerError> {
    if selector.is_some() && ref_id.is_some() {
        return Err(ComputerError::InvalidArguments(
            "pass --selector or --ref, not both".into(),
        ));
    }
    Ok((selector.map(str::to_string), ref_id.map(str::to_string)))
}

/// Dispatch one command.
pub fn dispatch(command: BrowserCommand, options: &BrowserOptions) -> Result<Value, ComputerError> {
    match command {
        BrowserCommand::Launch {
            browser,
            headless,
            profile,
            port,
            url,
        } => launch::launch(
            browser.as_deref(),
            headless,
            profile.as_deref(),
            port,
            url.as_deref(),
        ),
        BrowserCommand::Quit { session, purge } => {
            let session = session.or_else(|| options.session.clone());
            launch::quit(session.as_deref(), purge)
        }
        BrowserCommand::Sessions => launch::sessions(),
        BrowserCommand::Tabs => {
            let blocked = options.blocked_urls.clone();
            run_browser(options, move |mut browser| async move {
                list_tabs(&mut browser, &blocked).await
            })
        }
        BrowserCommand::Activate { target } => {
            let blocked = options.blocked_urls.clone();
            run_browser(options, move |mut browser| async move {
                activate(&mut browser, &target, &blocked).await
            })
        }
        BrowserCommand::TabNew { url } => {
            let url = url.unwrap_or_else(|| "about:blank".to_string());
            if let Some(pattern) = policy::url_blocked(&url, &options.blocked_urls) {
                return Err(ComputerError::BlockedUrl(format!("matched {pattern}")));
            }
            run_browser(options, move |browser| async move {
                let page = browser
                    .new_page("about:blank".to_string())
                    .await
                    .map_err(|error| cdp_unavailable(format!("createTarget: {error}")))?;
                let target_id = page.target_id().as_ref().to_string();
                if url != "about:blank" {
                    navigate(&page, &url, &WaitSpec::Load, Duration::from_secs(30)).await?;
                }
                Ok(json!({"target_id": target_id, "url": current_url(&page).await}))
            })
        }
        BrowserCommand::TabClose { target } => run_browser(options, move |browser| async move {
            let page = get_page_when_attached(&browser, &TargetId::new(target.clone())).await?;
            page.close()
                .await
                .map_err(|error| cdp_unavailable(format!("closeTarget: {error}")))?;
            Ok(json!({"target_id": target, "closed": true}))
        }),
        BrowserCommand::Navigate {
            url,
            target,
            wait,
            timeout_ms,
            new_tab,
        } => {
            if let Some(pattern) = policy::url_blocked(&url, &options.blocked_urls) {
                return Err(ComputerError::BlockedUrl(format!("matched {pattern}")));
            }
            if new_tab {
                let blocked = options.blocked_urls.clone();
                run_browser(options, move |browser| async move {
                    let page = browser
                        .new_page("about:blank".to_string())
                        .await
                        .map_err(|error| cdp_unavailable(format!("createTarget: {error}")))?;
                    let target_id = page.target_id().as_ref().to_string();
                    let timeout = timeout_ms
                        .map(|ms| Duration::from_millis(ms.max(1)))
                        .unwrap_or_else(|| Duration::from_secs(30));
                    navigate(&page, &url, &wait, timeout).await?;
                    let final_url = current_url(&page).await;
                    if let Some(pattern) = policy::url_blocked(&final_url, &blocked) {
                        return Err(ComputerError::BlockedUrl(format!(
                            "redirect landed on blocked url matched {pattern}"
                        )));
                    }
                    Ok(json!({"target_id": target_id, "url": final_url}))
                })
            } else {
                let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000).max(1));
                run_page(options, target, move |_browser, page| async move {
                    let target_id = page.target_id().as_ref().to_string();
                    navigate(&page, &url, &wait, timeout).await?;
                    let final_url = current_url(&page).await;
                    Ok(json!({"target_id": target_id, "url": final_url}))
                })
            }
        }
        BrowserCommand::Back { target } => {
            run_page(options, target, move |_browser, page| async move {
                let url = history_move(&page, false).await?;
                Ok(json!({"url": url}))
            })
        }
        BrowserCommand::Forward { target } => {
            run_page(options, target, move |_browser, page| async move {
                let url = history_move(&page, true).await?;
                Ok(json!({"url": url}))
            })
        }
        BrowserCommand::Reload {
            target,
            wait,
            timeout_ms,
        } => {
            let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000).max(1));
            run_page(options, target, move |_browser, page| async move {
                reload(&page, &wait, timeout).await?;
                Ok(json!({"url": current_url(&page).await}))
            })
        }
        BrowserCommand::Wait {
            target,
            wait,
            timeout_ms,
        } => {
            let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000).max(1));
            run_page(options, target, move |_browser, page| async move {
                wait_for(&page, &wait, timeout).await?;
                Ok(
                    json!({"url": current_url(&page).await, "ready_state": read_ready_state(&page).await}),
                )
            })
        }
        BrowserCommand::Snapshot {
            target,
            diff,
            interactive_only,
            max_nodes,
        } => {
            let session = options.session.clone();
            run_page(options, target, move |_browser, page| async move {
                let target_id = page.target_id().as_ref().to_string();
                let captured = snapshot::capture(
                    &page,
                    &target_id,
                    interactive_only,
                    max_nodes.unwrap_or(400),
                )
                .await?;
                let baseline = if diff {
                    snapshot::load(session.as_deref(), &target_id)?
                } else {
                    None
                };
                let text = match &baseline {
                    Some(baseline) => snapshot::render_diff(&captured, baseline),
                    None => captured.text.clone(),
                };
                snapshot::store(session.as_deref(), &captured)?;
                let refs: std::collections::BTreeMap<String, i64> = captured
                    .nodes
                    .iter()
                    .filter_map(|node| {
                        node.ref_id
                            .as_ref()
                            .zip(node.backend_node_id)
                            .map(|(ref_id, backend)| (ref_id.clone(), backend))
                    })
                    .collect();
                Ok(json!({
                    "target_id": captured.target_id,
                    "url": captured.url,
                    "title": captured.title,
                    "ready_state": captured.ready_state,
                    "text": text,
                    "diff": baseline.is_some(),
                    "refs": refs,
                    "node_count": captured.nodes.len(),
                    "truncated": captured.truncated,
                }))
            })
        }
        BrowserCommand::Click {
            target,
            selector,
            ref_id,
            button,
            count,
            wait,
            timeout_ms,
            force,
        } => {
            let (selector, ref_id) = element_args(selector.as_deref(), ref_id.as_deref())?;
            let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000).max(1));
            let session = options.session.clone();
            let blocked = options.blocked_urls.clone();
            let result = run_page(options, target, move |_browser, page| async move {
                let target_id = page.target_id().as_ref().to_string();
                let backend = match &ref_id {
                    Some(ref_id) => Some(
                        resolve_ref(
                            &BrowserOptions {
                                session: session.clone(),
                                ..BrowserOptions::default()
                            },
                            &page,
                            &target_id,
                            ref_id,
                        )
                        .await?,
                    ),
                    None => None,
                };
                let handle = actions::resolve(&page, selector.as_deref(), backend).await?;
                actions::click(&page, &handle, button, count, force).await?;
                wait_for(&page, &wait, timeout).await?;
                let url = current_url(&page).await;
                if let Some(pattern) = policy::url_blocked(&url, &blocked) {
                    return Err(ComputerError::BlockedUrl(format!(
                        "click landed on blocked url matched {pattern}"
                    )));
                }
                Ok(json!({"target_id": target_id, "url": url, "clicked": true}))
            })?;
            maybe_rescue_dialog(options, &result);
            Ok(result)
        }
        BrowserCommand::Text {
            target,
            selector,
            ref_id,
        } => {
            let (selector, ref_id) = element_args(selector.as_deref(), ref_id.as_deref())?;
            let session = options.session.clone();
            run_page(options, target, move |_browser, page| async move {
                let target_id = page.target_id().as_ref().to_string();
                let backend = match &ref_id {
                    Some(ref_id) => Some(
                        resolve_ref(
                            &BrowserOptions {
                                session: session.clone(),
                                ..BrowserOptions::default()
                            },
                            &page,
                            &target_id,
                            ref_id,
                        )
                        .await?,
                    ),
                    None => None,
                };
                let handle = actions::resolve(&page, selector.as_deref(), backend).await?;
                let text = actions::read_text(&page, &handle).await?;
                Ok(
                    json!({"selector": selector, "ref": ref_id, "text": text, "url": current_url(&page).await}),
                )
            })
        }
        BrowserCommand::Type {
            target,
            selector,
            ref_id,
            text,
            clear,
            submit,
            wait,
            timeout_ms,
        } => {
            let (selector, ref_id) = element_args(selector.as_deref(), ref_id.as_deref())?;
            let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000).max(1));
            let session = options.session.clone();
            run_page(options, target, move |_browser, page| async move {
                let target_id = page.target_id().as_ref().to_string();
                let backend = match &ref_id {
                    Some(ref_id) => Some(
                        resolve_ref(
                            &BrowserOptions {
                                session: session.clone(),
                                ..BrowserOptions::default()
                            },
                            &page,
                            &target_id,
                            ref_id,
                        )
                        .await?,
                    ),
                    None => None,
                };
                let handle = actions::resolve(&page, selector.as_deref(), backend).await?;
                actions::type_text(&page, &handle, &text, clear).await?;
                if submit {
                    actions::press(&page, "Enter").await?;
                }
                wait_for(&page, &wait, timeout).await?;
                Ok(
                    json!({"target_id": target_id, "chars": text.chars().count(), "url": current_url(&page).await}),
                )
            })
        }
        BrowserCommand::Press {
            target,
            selector,
            ref_id,
            keys,
        } => {
            // A page-level press (`--keys Tab`) has no element to focus.
            let (selector, ref_id) = optional_element_args(selector.as_deref(), ref_id.as_deref())?;
            let session = options.session.clone();
            let result = run_page(options, target, move |_browser, page| async move {
                let target_id = page.target_id().as_ref().to_string();
                if selector.is_some() || ref_id.is_some() {
                    let backend = match &ref_id {
                        Some(ref_id) => Some(
                            resolve_ref(
                                &BrowserOptions {
                                    session: session.clone(),
                                    ..BrowserOptions::default()
                                },
                                &page,
                                &target_id,
                                ref_id,
                            )
                            .await?,
                        ),
                        None => None,
                    };
                    let handle = actions::resolve(&page, selector.as_deref(), backend).await?;
                    let _ = actions::call_on(
                        &page,
                        &handle,
                        "function () { if (this.focus) this.focus(); }",
                        vec![],
                    )
                    .await;
                }
                actions::press(&page, &keys).await?;
                Ok(json!({"target_id": target_id, "keys": keys, "url": current_url(&page).await}))
            })?;
            maybe_rescue_dialog(options, &result);
            Ok(result)
        }
        BrowserCommand::Hover {
            target,
            selector,
            ref_id,
        } => {
            let (selector, ref_id) = element_args(selector.as_deref(), ref_id.as_deref())?;
            let session = options.session.clone();
            run_page(options, target, move |_browser, page| async move {
                let target_id = page.target_id().as_ref().to_string();
                let backend = match &ref_id {
                    Some(ref_id) => Some(
                        resolve_ref(
                            &BrowserOptions {
                                session: session.clone(),
                                ..BrowserOptions::default()
                            },
                            &page,
                            &target_id,
                            ref_id,
                        )
                        .await?,
                    ),
                    None => None,
                };
                let handle = actions::resolve(&page, selector.as_deref(), backend).await?;
                actions::hover(&page, &handle).await?;
                Ok(json!({"target_id": target_id, "hovered": true}))
            })
        }
        BrowserCommand::Scroll {
            target,
            selector,
            ref_id,
            dx,
            dy,
        } => {
            let (selector, ref_id) = match (selector.as_deref(), ref_id.as_deref()) {
                (Some(_), Some(_)) => {
                    return Err(ComputerError::InvalidArguments(
                        "pass --selector or --ref, not both".into(),
                    ))
                }
                _ => (selector, ref_id),
            };
            let session = options.session.clone();
            run_page(options, target, move |_browser, page| async move {
                let target_id = page.target_id().as_ref().to_string();
                let backend = match &ref_id {
                    Some(ref_id) => Some(
                        resolve_ref(
                            &BrowserOptions {
                                session: session.clone(),
                                ..BrowserOptions::default()
                            },
                            &page,
                            &target_id,
                            ref_id,
                        )
                        .await?,
                    ),
                    None => None,
                };
                let handle = if selector.is_some() || backend.is_some() {
                    Some(actions::resolve(&page, selector.as_deref(), backend).await?)
                } else {
                    None
                };
                actions::scroll(&page, handle.as_ref(), dx, dy).await?;
                Ok(json!({"target_id": target_id, "scrolled": true}))
            })
        }
        BrowserCommand::Select {
            target,
            selector,
            ref_id,
            value,
            label,
        } => {
            let (selector, ref_id) = element_args(selector.as_deref(), ref_id.as_deref())?;
            if value.is_none() && label.is_none() {
                return Err(ComputerError::InvalidArguments(
                    "select needs --value or --label".into(),
                ));
            }
            let session = options.session.clone();
            run_page(options, target, move |_browser, page| async move {
                let target_id = page.target_id().as_ref().to_string();
                let backend = match &ref_id {
                    Some(ref_id) => Some(
                        resolve_ref(
                            &BrowserOptions {
                                session: session.clone(),
                                ..BrowserOptions::default()
                            },
                            &page,
                            &target_id,
                            ref_id,
                        )
                        .await?,
                    ),
                    None => None,
                };
                let handle = actions::resolve(&page, selector.as_deref(), backend).await?;
                let result =
                    actions::select_option(&page, &handle, value.as_deref(), label.as_deref())
                        .await?;
                Ok(json!({"target_id": target_id, "result": result}))
            })
        }
        BrowserCommand::Check {
            target,
            selector,
            ref_id,
            checked,
        } => {
            let (selector, ref_id) = element_args(selector.as_deref(), ref_id.as_deref())?;
            let session = options.session.clone();
            run_page(options, target, move |_browser, page| async move {
                let target_id = page.target_id().as_ref().to_string();
                let backend = match &ref_id {
                    Some(ref_id) => Some(
                        resolve_ref(
                            &BrowserOptions {
                                session: session.clone(),
                                ..BrowserOptions::default()
                            },
                            &page,
                            &target_id,
                            ref_id,
                        )
                        .await?,
                    ),
                    None => None,
                };
                let handle = actions::resolve(&page, selector.as_deref(), backend).await?;
                let result = actions::set_checked(&page, &handle, checked).await?;
                Ok(json!({"target_id": target_id, "result": result}))
            })
        }
        BrowserCommand::Upload {
            target,
            selector,
            ref_id,
            files,
        } => {
            let (selector, ref_id) = element_args(selector.as_deref(), ref_id.as_deref())?;
            if files.is_empty() {
                return Err(ComputerError::InvalidArguments(
                    "upload needs at least one --file".into(),
                ));
            }
            let session = options.session.clone();
            run_page(options, target, move |_browser, page| async move {
                let target_id = page.target_id().as_ref().to_string();
                let backend = match &ref_id {
                    Some(ref_id) => Some(
                        resolve_ref(
                            &BrowserOptions {
                                session: session.clone(),
                                ..BrowserOptions::default()
                            },
                            &page,
                            &target_id,
                            ref_id,
                        )
                        .await?,
                    ),
                    None => None,
                };
                let handle = actions::resolve(&page, selector.as_deref(), backend).await?;
                actions::upload(&page, &handle, &files).await?;
                Ok(json!({"target_id": target_id, "files": files.len(), "uploaded": true}))
            })
        }
        BrowserCommand::Dialog {
            target,
            accept,
            prompt_text,
        } => {
            let target_id = match target {
                Some(target) => target,
                None => run_browser(options, move |mut browser| async move {
                    let page = resolve_page_attached(&mut browser, None).await?;
                    Ok(page.target_id().as_ref().to_string())
                })?,
            };
            rescue_dialog(options, &target_id, accept, prompt_text.as_deref())?;
            Ok(json!({"accepted": accept, "target_id": target_id}))
        }
        BrowserCommand::Screenshot {
            target,
            output,
            full_page,
            format,
            quality,
            base64,
        } => run_page(options, target, move |_browser, page| async move {
            screenshot(&page, output.as_deref(), full_page, format, quality, base64).await
        }),
        BrowserCommand::Eval {
            target,
            script,
            timeout_ms,
            await_promise,
        } => {
            let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000).max(1));
            let script_chars = script.chars().count();
            let script_hash = stable_hash(&script);
            run_page(options, target, move |_browser, page| async move {
                let value =
                    tokio::time::timeout(timeout, actions::evaluate(&page, &script, await_promise))
                        .await
                        .map_err(|_| ComputerError::Timeout("browser eval timed out".into()))??;
                Ok(json!({
                    "result": value,
                    "script_chars": script_chars,
                    "script_hash": script_hash,
                }))
            })
        }
        BrowserCommand::Batch {
            steps,
            stop_on_error,
        } => {
            let mut results = Vec::new();
            let mut failed = 0usize;
            for (index, step) in steps.into_iter().enumerate() {
                match dispatch(step, options) {
                    Ok(value) => results.push(json!({"ok": true, "result": value})),
                    Err(error) => {
                        failed += 1;
                        results.push(json!({
                            "ok": false,
                            "error": {"code": error.code(), "message": error.to_string()},
                        }));
                        if stop_on_error {
                            return Ok(json!({
                                "results": results,
                                "count": results.len(),
                                "failed": failed,
                                "stopped_at": index,
                            }));
                        }
                    }
                }
            }
            Ok(json!({
                "results": results,
                "count": results.len(),
                "failed": failed,
            }))
        }
    }
}

fn stable_hash(value: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_debug_port_handles_mac_and_linux_layouts() {
        let argv = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome\0--remote-debugging-port=9222\0--x\0";
        assert_eq!(parse_debug_port(argv), Some(9222));
        let argv = "/usr/bin/chromium\0--remote-debugging-port\0 9223\0";
        assert_eq!(parse_debug_port(argv), Some(9223));
        assert_eq!(parse_debug_port("--remote-debugging-port=nope"), None);
    }

    #[test]
    fn normalize_url_accepts_http_and_ws() {
        assert_eq!(
            normalize_url("http://127.0.0.1:9222/").unwrap(),
            "http://127.0.0.1:9222"
        );
        assert!(normalize_url("ftp://x").is_err());
    }
}
