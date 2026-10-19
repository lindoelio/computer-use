//! Actions, JSON requests, and the shared dispatcher.

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::{self, Deserializer};
use serde::ser::{Serialize, Serializer};
use serde::{Deserialize, Serialize as SerializeDerive};
use serde_json::{json, Map, Value};

use crate::backend::create_desktop_backend;
use crate::backend::{validate_key, DesktopBackend};
use crate::error::{ComputerError, ErrorBody};
use crate::image_util::{encode_base64, encode_image};
use crate::keys::parse_key_combo;
use crate::state::collect_state_for;
use crate::target::{filter_windows_for_pid, parse_app_selector, resolve_app};
use crate::tree::TreeQuery;
use crate::types::{ImageFormat, Modifier, MouseButton, Region, ScrollDirection};

const MAX_WAIT_MS: u128 = 24 * 60 * 60 * 1000;

/// One desktop action. Serialized with an `action` tag.
///
/// ```json
/// {"action":"click","x":10,"y":20}
/// ```
#[derive(Debug, Clone, PartialEq, SerializeDerive, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Capture the screen or a region.
    Screenshot {
        /// File path. A temp path is used when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Logical region, as `x,y,width,height` or an object.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<Region>,
        /// `png` (default) or `jpeg`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<ImageFormat>,
        /// Scale the image down so its width is at most this many pixels.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<u32>,
        /// Include standard base64 bytes in `data`.
        #[serde(default, skip_serializing_if = "is_false")]
        base64: bool,
    },
    /// Move to a point and click.
    Click {
        /// Logical x.
        x: i32,
        /// Logical y.
        y: i32,
        /// Button. Defaults to left.
        #[serde(default)]
        button: MouseButton,
        /// Click twice when true.
        #[serde(default, skip_serializing_if = "is_false")]
        double: bool,
    },
    /// Click twice. Equivalent to `click` with `double: true`.
    #[serde(rename = "double_click")]
    DoubleClick {
        /// Logical x.
        x: i32,
        /// Logical y.
        y: i32,
        /// Button. Defaults to left.
        #[serde(default)]
        button: MouseButton,
    },
    /// Move the pointer without clicking.
    #[serde(alias = "move_mouse")]
    Move {
        /// Logical x.
        x: i32,
        /// Logical y.
        y: i32,
    },
    /// Type text into the focused control.
    Type {
        /// Text to type.
        text: String,
    },
    /// Press a key combination such as `cmd+shift+4`.
    Key {
        /// Combo string.
        keys: String,
    },
    /// Scroll the wheel. Positive `dy` is down. Positive `dx` is right.
    Scroll {
        /// Optional pointer x. Must be paired with `y`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Optional pointer y. Must be paired with `x`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Horizontal wheel steps.
        #[serde(default, alias = "scroll_x", skip_serializing_if = "Option::is_none")]
        dx: Option<i32>,
        /// Vertical wheel steps.
        #[serde(default, alias = "scroll_y", skip_serializing_if = "Option::is_none")]
        dy: Option<i32>,
        /// Direction used with `amount`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<ScrollDirection>,
        /// Wheel steps used with `direction`. Defaults to 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        amount: Option<i32>,
    },
    /// Screen, cursor, frontmost window, and a best-effort accessibility tree.
    #[serde(alias = "get-state")]
    GetState,
    /// Sleep, then return.
    Wait {
        /// Milliseconds. Added to `seconds` when both are set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
        /// Seconds, including fractions. Added to `ms`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seconds: Option<f64>,
    },
    /// List displays without capturing pixels.
    Displays,
    /// List running applications. `all` includes accessory and prohibited apps.
    ListApps {
        /// Include accessory and prohibited activation policies.
        #[serde(default, rename = "all")]
        all: bool,
    },
    /// List on-screen windows. `app` comes from the shared request context.
    ListWindows,
    /// Launch an application named by the shared `app` field.
    Launch,
    /// Activate an application named by the shared `app` field.
    Activate,
    /// Ask an application to quit. Does not force-terminate.
    Quit,
    /// Report accessibility and screen-capture permission.
    Doctor {
        /// Prompt for Accessibility and Screen Recording when true.
        #[serde(default)]
        request: bool,
    },
    /// Numbered accessibility tree for the key window.
    Tree,
    /// Restore Electron accessibility flags and stop observers.
    SessionEnd,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Flags shared by every action. JSON keys are removed before the action is decoded.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CallContext {
    /// Target application: name, bundle id, or pid.
    pub app: Option<String>,
    /// Target window: title, z-order index, or `id:<number>`.
    pub window: Option<String>,
    /// Element index in the current revision. Used from P3.
    pub element: Option<u32>,
    /// Emit a tree diff when a baseline exists.
    pub diff: bool,
    /// Replace the tree baseline with the full tree.
    pub full: bool,
    /// Attach post-action state. Used from P5.
    pub return_state: bool,
    /// Attach a screenshot with post-action state. Used from P5.
    pub include_screenshot: bool,
    /// Coordinate space name. Used from P5.
    pub coord: Option<String>,
    /// Cache namespace. Defaults to the process id in later phases.
    pub session: Option<String>,
    /// Input tier name. Used from P7.
    pub tier: Option<String>,
    /// Adapter name. Used from P5.
    pub adapter: Option<String>,
    /// Display filter. Used from P14.
    pub display: Option<String>,
}

/// A JSON request: an optional `id`, shared flags, and an [`Action`].
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Caller-supplied id, echoed on the response when present.
    pub id: Option<Value>,
    /// Action to run.
    pub action: Action,
    /// Flags that were siblings of `action`.
    pub context: CallContext,
}

/// Parse one JSON object. `id` and the shared flag keys are removed before the action is decoded.
pub fn parse_request(mut value: Value) -> Result<Request, ComputerError> {
    if !value.is_object() {
        return Err(ComputerError::InvalidArguments(
            "request must be a JSON object".into(),
        ));
    }
    let object = value
        .as_object_mut()
        .expect("object check above guarantees a map");
    let id = object.remove("id");
    let context = take_context(object)?;
    let action = serde_json::from_value(value)
        .map_err(|error| ComputerError::InvalidArguments(error.to_string()))?;
    Ok(Request {
        id,
        action,
        context,
    })
}

fn take_context(object: &mut Map<String, Value>) -> Result<CallContext, ComputerError> {
    Ok(CallContext {
        app: take_stringish(object, "app")?,
        window: take_stringish(object, "window")?,
        element: take_optional_u32(object, "element")?,
        diff: take_bool(object, "diff")?,
        full: take_bool(object, "full")?,
        return_state: take_bool(object, "return_state")?,
        include_screenshot: take_bool(object, "include_screenshot")?,
        coord: take_stringish(object, "coord")?,
        session: take_stringish(object, "session")?,
        tier: take_stringish(object, "tier")?,
        adapter: take_stringish(object, "adapter")?,
        display: take_stringish(object, "display")?,
    })
}

fn take_stringish(
    object: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<String>, ComputerError> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(Value::Number(number)) => Ok(Some(number.to_string())),
        Some(_) => Err(ComputerError::InvalidArguments(format!(
            "{key} must be a string or number"
        ))),
    }
}

fn take_bool(object: &mut Map<String, Value>, key: &str) -> Result<bool, ComputerError> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(value),
        Some(_) => Err(ComputerError::InvalidArguments(format!(
            "{key} must be a boolean"
        ))),
    }
}

fn take_optional_u32(
    object: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<u32>, ComputerError> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| ComputerError::InvalidArguments(format!("{key} is out of range"))),
        Some(Value::String(text)) => text
            .parse::<u32>()
            .map(Some)
            .map_err(|_| ComputerError::InvalidArguments(format!("{key} must be an integer"))),
        Some(_) => Err(ComputerError::InvalidArguments(format!(
            "{key} must be an integer"
        ))),
    }
}

/// Decode one JSON line and dispatch it.
///
/// Parse errors return `ok: false` and do not panic.
pub fn handle_request_line(line: &str) -> Response {
    let value = match serde_json::from_str::<Value>(line) {
        Ok(value) => value,
        Err(error) => {
            return Response::failure("invalid_arguments", format!("invalid JSON: {error}"))
        }
    };
    let id = value.get("id").cloned();
    match parse_request(value) {
        Ok(request) => dispatch_context(request.action, &request.context).with_id(request.id),
        Err(error) => Response::from_error(error).with_id(id),
    }
}

/// Run `action` on the platform backend.
pub fn dispatch(action: Action) -> Response {
    dispatch_context(action, &CallContext::default())
}

/// Run `action` with shared flags on the platform backend.
pub fn dispatch_context(action: Action, context: &CallContext) -> Response {
    let backend = create_desktop_backend();
    dispatch_with_context(backend.as_ref(), action, context)
}

/// Run `action` on `backend`. Panics inside the backend become an error response.
pub fn dispatch_with(backend: &dyn DesktopBackend, action: Action) -> Response {
    dispatch_with_context(backend, action, &CallContext::default())
}

/// Run `action` on `backend` with shared flags.
pub fn dispatch_with_context(
    backend: &dyn DesktopBackend,
    action: Action,
    context: &CallContext,
) -> Response {
    match catch_unwind(AssertUnwindSafe(|| {
        dispatch_inner(backend, action, context)
    })) {
        Ok(response) => response,
        Err(_) => Response::failure("execution_failed", "action panicked and was recovered"),
    }
}

fn dispatch_inner(backend: &dyn DesktopBackend, action: Action, context: &CallContext) -> Response {
    let result = match action {
        Action::Screenshot {
            output,
            region,
            format,
            max_width,
            base64,
        } => screenshot(backend, output, region, format, max_width, base64),
        Action::Click {
            x,
            y,
            button,
            double,
        } => click(backend, x, y, button, double),
        Action::DoubleClick { x, y, button } => click(backend, x, y, button, true),
        Action::Move { x, y } => move_mouse(backend, x, y),
        Action::Type { text } => type_text(backend, &text),
        Action::Key { keys } => press_key(backend, &keys),
        Action::Scroll {
            x,
            y,
            dx,
            dy,
            direction,
            amount,
        } => scroll(backend, x, y, dx, dy, direction, amount),
        Action::GetState => Ok(collect_state_for(backend, &tree_query(context))),
        Action::Tree => tree_action(backend, context),
        Action::SessionEnd => session_end(backend, context),
        Action::Wait { ms, seconds } => wait(ms, seconds),
        Action::Displays => displays(backend),
        Action::ListApps { all } => list_apps(backend, all),
        Action::ListWindows => list_windows(backend, context.app.as_deref()),
        Action::Launch => launch(backend, context.app.as_deref()),
        Action::Activate => activate(backend, context.app.as_deref(), context.window.as_deref()),
        Action::Quit => quit(backend, context.app.as_deref()),
        Action::Doctor { request } => doctor(backend, request),
    };
    match result {
        Ok(value) => Response::success(value),
        Err(error) => Response::from_error(error),
    }
}

fn screenshot(
    backend: &dyn DesktopBackend,
    output: Option<String>,
    region: Option<Region>,
    format: Option<ImageFormat>,
    max_width: Option<u32>,
    include_base64: bool,
) -> Result<Value, ComputerError> {
    let format = format.unwrap_or_default();
    let frame = backend.screenshot(region)?;
    let encoded = encode_image(&frame, format, max_width)?;
    let path = match output {
        Some(path) => PathBuf::from(path),
        None => default_screenshot_path(format),
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(&path, &encoded.bytes)?;
    let mut body = json!({
        "path": path_string(&path),
        "width": encoded.width,
        "height": encoded.height,
        "scale": frame.scale(),
        "format": format.as_str(),
    });
    if include_base64 {
        body["data"] = json!(encode_base64(&encoded.bytes));
    }
    Ok(body)
}

fn default_screenshot_path(format: ImageFormat) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        "computer-use-{}-{nanos}-{sequence}.{}",
        std::process::id(),
        format.extension()
    );
    std::env::temp_dir().join(name)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn click(
    backend: &dyn DesktopBackend,
    x: i32,
    y: i32,
    button: MouseButton,
    double: bool,
) -> Result<Value, ComputerError> {
    backend.simulate_click(x, y, button)?;
    if double {
        thread::sleep(Duration::from_millis(50));
        backend.simulate_click(x, y, button)?;
    }
    Ok(json!({
        "action": if double { "double_click" } else { "click" },
        "x": x,
        "y": y,
        "button": button.as_str(),
        "double": double,
    }))
}

fn move_mouse(backend: &dyn DesktopBackend, x: i32, y: i32) -> Result<Value, ComputerError> {
    backend.move_mouse(x, y)?;
    Ok(json!({"action": "move", "x": x, "y": y}))
}

fn type_text(backend: &dyn DesktopBackend, text: &str) -> Result<Value, ComputerError> {
    if text.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "text must not be empty".into(),
        ));
    }
    if text.contains('\0') {
        return Err(ComputerError::InvalidArguments(
            "text must not contain a null byte".into(),
        ));
    }
    backend.simulate_type(text)?;
    Ok(json!({
        "action": "type",
        "text": text,
        "chars": text.chars().count(),
    }))
}

fn press_key(backend: &dyn DesktopBackend, keys: &str) -> Result<Value, ComputerError> {
    let combo = parse_key_combo(keys)?;
    validate_key(&combo.key)?;
    backend.simulate_key(&combo.modifiers, &combo.key)?;
    let modifiers: Vec<&str> = combo
        .modifiers
        .iter()
        .copied()
        .map(Modifier::as_str)
        .collect();
    Ok(json!({
        "action": "key",
        "keys": keys,
        "modifiers": modifiers,
        "key": combo.key,
    }))
}

fn scroll(
    backend: &dyn DesktopBackend,
    x: Option<i32>,
    y: Option<i32>,
    dx: Option<i32>,
    dy: Option<i32>,
    direction: Option<ScrollDirection>,
    amount: Option<i32>,
) -> Result<Value, ComputerError> {
    if x.is_some() != y.is_some() {
        return Err(ComputerError::InvalidArguments(
            "scroll x and y must be set together".into(),
        ));
    }
    let (scroll_dx, scroll_dy) = resolve_scroll(dx, dy, direction, amount)?;
    if let (Some(x), Some(y)) = (x, y) {
        backend.move_mouse(x, y)?;
    }
    backend.simulate_scroll(scroll_dx, scroll_dy)?;
    Ok(json!({
        "action": "scroll",
        "x": x,
        "y": y,
        "dx": scroll_dx,
        "dy": scroll_dy,
    }))
}

pub(crate) fn resolve_scroll(
    dx: Option<i32>,
    dy: Option<i32>,
    direction: Option<ScrollDirection>,
    amount: Option<i32>,
) -> Result<(i32, i32), ComputerError> {
    let has_delta = dx.is_some() || dy.is_some();
    let has_direction = direction.is_some() || amount.is_some();
    if has_delta && has_direction {
        return Err(ComputerError::InvalidArguments(
            "use either dx/dy or direction/amount, not both".into(),
        ));
    }
    if let Some(direction) = direction {
        let amount = amount.unwrap_or(1);
        return Ok(match direction {
            ScrollDirection::Up => (0, -amount),
            ScrollDirection::Down => (0, amount),
            ScrollDirection::Left => (-amount, 0),
            ScrollDirection::Right => (amount, 0),
        });
    }
    if amount.is_some() {
        return Err(ComputerError::InvalidArguments(
            "amount requires direction".into(),
        ));
    }
    if !has_delta {
        return Err(ComputerError::InvalidArguments(
            "scroll requires dx/dy or direction".into(),
        ));
    }
    Ok((dx.unwrap_or(0), dy.unwrap_or(0)))
}

fn wait(ms: Option<u64>, seconds: Option<f64>) -> Result<Value, ComputerError> {
    let waited = resolve_wait(ms, seconds)?;
    thread::sleep(Duration::from_millis(waited));
    Ok(json!({"waited_ms": waited}))
}

pub(crate) fn resolve_wait(ms: Option<u64>, seconds: Option<f64>) -> Result<u64, ComputerError> {
    if ms.is_none() && seconds.is_none() {
        return Err(ComputerError::InvalidArguments(
            "wait requires ms, seconds, or both".into(),
        ));
    }
    let mut total: u128 = u128::from(ms.unwrap_or(0));
    if let Some(seconds) = seconds {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(ComputerError::InvalidArguments(
                "seconds must be a finite number greater than or equal to 0".into(),
            ));
        }
        let extra = (seconds * 1000.0).round();
        if extra > u128::MAX as f64 {
            return Err(ComputerError::InvalidArguments("wait is too long".into()));
        }
        total = total.saturating_add(extra as u128);
    }
    if total > MAX_WAIT_MS {
        return Err(ComputerError::InvalidArguments(
            "wait must be at most 24 hours".into(),
        ));
    }
    u64::try_from(total).map_err(|_| ComputerError::InvalidArguments("wait is too long".into()))
}

fn displays(backend: &dyn DesktopBackend) -> Result<Value, ComputerError> {
    let displays = backend.list_displays()?;
    Ok(json!({"displays": displays}))
}

fn tree_query(context: &CallContext) -> TreeQuery {
    TreeQuery {
        app: context.app.clone(),
        window: context.window.clone(),
        full: context.full,
        session: context.session.clone(),
    }
}

fn tree_action(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<Value, ComputerError> {
    let report = backend.tree(&tree_query(context))?;
    let mut body = json!({
        "pid": report.pid,
        "window_id": report.window_id,
        "revision": report.revision,
        "source": report.source,
        "truncated": report.truncated,
        "tree": report.tree,
    });
    if let Some(note) = report.note {
        body["note"] = json!(note);
    }
    if let Some(legacy) = report.legacy.as_deref() {
        body["accessibility_legacy"] = serde_json::from_str(legacy).unwrap_or(Value::Null);
    }
    Ok(body)
}

fn session_end(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<Value, ComputerError> {
    let restored = backend.end_session(context.session.as_deref())?;
    Ok(json!({
        "action": "session_end",
        "restored": restored,
    }))
}

fn list_apps(backend: &dyn DesktopBackend, all: bool) -> Result<Value, ComputerError> {
    let apps = backend.list_apps(all)?;
    Ok(json!({"apps": apps}))
}

fn list_windows(backend: &dyn DesktopBackend, app: Option<&str>) -> Result<Value, ComputerError> {
    let listed = backend.list_windows()?;
    let windows = match app {
        None => listed.windows,
        Some(app) => {
            let selector = parse_app_selector(app)?;
            let apps = backend.list_apps(true)?;
            let info = resolve_app(&apps, &selector)?;
            filter_windows_for_pid(&listed.windows, info.pid)
        }
    };
    let mut body = json!({"windows": windows});
    if let Some(note) = listed.note {
        body["note"] = json!(note);
    }
    Ok(body)
}

fn launch(backend: &dyn DesktopBackend, app: Option<&str>) -> Result<Value, ComputerError> {
    let app = require_app(app)?;
    let result = backend.launch(app)?;
    Ok(json!({
        "action": "launch",
        "name": result.name,
        "bundle_id": result.bundle_id,
        "pid": result.pid,
        "path": result.path,
    }))
}

fn activate(
    backend: &dyn DesktopBackend,
    app: Option<&str>,
    window: Option<&str>,
) -> Result<Value, ComputerError> {
    let app = require_app(app)?;
    let result = backend.activate(app, window)?;
    Ok(json!({
        "action": "activate",
        "name": result.name,
        "bundle_id": result.bundle_id,
        "pid": result.pid,
        "path": result.path,
        "window_id": result.window_id,
        "window_title": result.window_title,
    }))
}

fn quit(backend: &dyn DesktopBackend, app: Option<&str>) -> Result<Value, ComputerError> {
    let app = require_app(app)?;
    let result = backend.quit(app)?;
    Ok(json!({
        "action": "quit",
        "name": result.name,
        "bundle_id": result.bundle_id,
        "pid": result.pid,
        "accepted": result.accepted,
    }))
}

fn doctor(backend: &dyn DesktopBackend, request: bool) -> Result<Value, ComputerError> {
    let report = backend.doctor(request)?;
    serde_json::to_value(report).map_err(|error| {
        ComputerError::ExecutionFailed(format!("failed to encode doctor report: {error}"))
    })
}

fn require_app(app: Option<&str>) -> Result<&str, ComputerError> {
    match app.map(str::trim).filter(|text| !text.is_empty()) {
        Some(app) => Ok(app),
        None => Err(ComputerError::InvalidArguments("app is required".into())),
    }
}

/// JSON result printed by the CLI and returned by [`dispatch`].
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// Whether the action completed.
    pub ok: bool,
    /// Request id, when the caller sent one.
    pub id: Option<Value>,
    /// Present when `ok` is false.
    pub error: Option<ErrorBody>,
    /// Extra fields flattened into the JSON object.
    pub fields: Map<String, Value>,
}

impl Response {
    /// Successful response. `data` must be a JSON object; other values are stored as `result`.
    pub fn success(data: Value) -> Self {
        let fields = match data {
            Value::Object(map) => map,
            other => {
                let mut map = Map::new();
                map.insert("result".into(), other);
                map
            }
        };
        Self {
            ok: true,
            id: None,
            error: None,
            fields,
        }
    }

    /// Failed response with a stable `code` and `message`.
    pub fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            id: None,
            error: Some(ErrorBody {
                code: code.into(),
                message: message.into(),
            }),
            fields: Map::new(),
        }
    }

    /// Failed response from a [`ComputerError`].
    pub fn from_error(error: ComputerError) -> Self {
        Self::failure(error.code(), error.to_string())
    }

    /// Attach a request id.
    pub fn with_id(mut self, id: Option<Value>) -> Self {
        self.id = id;
        self
    }

    /// JSON object for this response.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("ok".into(), Value::Bool(self.ok));
        if let Some(id) = &self.id {
            map.insert("id".into(), id.clone());
        }
        if let Some(error) = &self.error {
            map.insert(
                "error".into(),
                serde_json::to_value(error).unwrap_or_else(
                    |_| json!({"code": "execution_failed", "message": "failed to encode error"}),
                ),
            );
        }
        for (key, value) in &self.fields {
            if key != "ok" && key != "id" && key != "error" {
                map.insert(key.clone(), value.clone());
            }
        }
        Value::Object(map)
    }

    /// Serialize as compact or pretty JSON, followed by a newline.
    pub fn to_json_line(&self, pretty: bool) -> String {
        let value = self.to_value();
        let body = if pretty {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{\"ok\":false}".to_string())
        } else {
            serde_json::to_string(&value).unwrap_or_else(|_| "{\"ok\":false}".to_string())
        };
        format!("{body}\n")
    }
}

impl From<ComputerError> for Response {
    fn from(error: ComputerError) -> Self {
        Self::from_error(error)
    }
}

impl Serialize for Response {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Response {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Value::Object(mut object) = value else {
            return Err(de::Error::custom("response must be a JSON object"));
        };
        let ok = object
            .remove("ok")
            .ok_or_else(|| de::Error::missing_field("ok"))?;
        let ok = ok
            .as_bool()
            .ok_or_else(|| de::Error::custom("ok must be a boolean"))?;
        let id = object.remove("id");
        let error = match object.remove("error") {
            Some(value) => Some(serde_json::from_value(value).map_err(de::Error::custom)?),
            None => None,
        };
        Ok(Self {
            ok,
            id,
            error,
            fields: object,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_backend::StubBackend;
    use std::sync::atomic::Ordering;

    #[test]
    fn action_roundtrip_and_aliases() {
        let click: Action = serde_json::from_str(
            r#"{"action":"click","x":10,"y":20,"button":"right","double":true}"#,
        )
        .unwrap();
        match &click {
            Action::Click {
                x,
                y,
                button,
                double,
            } => {
                assert_eq!(
                    (*x, *y, *button, *double),
                    (10, 20, MouseButton::Right, true)
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        let encoded = serde_json::to_string(&click).unwrap();
        let decoded: Action = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, click);

        let moved: Action = serde_json::from_str(r#"{"action":"move_mouse","x":1,"y":2}"#).unwrap();
        assert!(matches!(moved, Action::Move { x: 1, y: 2 }));

        let state: Action = serde_json::from_str(r#"{"action":"get-state"}"#).unwrap();
        assert!(matches!(state, Action::GetState));
        let state: Action = serde_json::from_str(r#"{"action":"get_state"}"#).unwrap();
        assert!(matches!(state, Action::GetState));

        let shot: Action = serde_json::from_str(
            r#"{"action":"screenshot","region":"0,0,8,9","format":"jpeg","max_width":640,"base64":true}"#,
        )
        .unwrap();
        match shot {
            Action::Screenshot {
                region,
                format,
                max_width,
                base64,
                ..
            } => {
                assert_eq!(region.unwrap().width, 8);
                assert_eq!(format, Some(ImageFormat::Jpeg));
                assert_eq!(max_width, Some(640));
                assert!(base64);
            }
            other => panic!("unexpected {other:?}"),
        }
        let shot: Action = serde_json::from_str(
            r#"{"action":"screenshot","region":{"x":0,"y":1,"width":2,"height":3}}"#,
        )
        .unwrap();
        match shot {
            Action::Screenshot { region, .. } => {
                assert_eq!(region.unwrap().height, 3);
            }
            other => panic!("unexpected {other:?}"),
        }

        assert!(serde_json::from_str::<Action>(r#"{"action":"fly"}"#).is_err());
        assert!(serde_json::from_str::<Action>(r#"{"action":"click"}"#).is_err());
    }

    #[test]
    fn response_shape_and_roundtrip() {
        let ok = Response::success(json!({"waited_ms": 1})).with_id(Some(json!(7)));
        let value = ok.to_value();
        assert_eq!(value["ok"], true);
        assert!(value.get("error").is_none());
        assert_eq!(value["waited_ms"], 1);
        assert_eq!(value["id"], 7);
        let text = serde_json::to_string(&ok).unwrap();
        let back: Response = serde_json::from_str(&text).unwrap();
        assert_eq!(back.to_value(), value);

        let err = Response::failure("invalid_arguments", "bad region");
        let value = err.to_value();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "invalid_arguments");
        assert_eq!(value["error"]["message"], "bad region");
        assert!(value.get("waited_ms").is_none());
        let line = err.to_json_line(false);
        assert!(line.ends_with('\n'));
        assert!(!line[..line.len() - 1].contains('\n'));
    }

    #[test]
    fn scroll_and_wait_resolution() {
        assert_eq!(
            resolve_scroll(Some(1), Some(-2), None, None).unwrap(),
            (1, -2)
        );
        assert_eq!(
            resolve_scroll(None, None, Some(ScrollDirection::Up), Some(3)).unwrap(),
            (0, -3)
        );
        assert_eq!(
            resolve_scroll(None, None, Some(ScrollDirection::Right), None).unwrap(),
            (1, 0)
        );
        assert_eq!(
            resolve_scroll(None, None, Some(ScrollDirection::Left), Some(4)).unwrap(),
            (-4, 0)
        );
        assert_eq!(
            resolve_scroll(None, None, Some(ScrollDirection::Down), Some(2)).unwrap(),
            (0, 2)
        );
        assert!(resolve_scroll(Some(1), None, Some(ScrollDirection::Up), None).is_err());
        assert!(resolve_scroll(None, None, None, Some(2)).is_err());
        assert!(resolve_scroll(None, None, None, None).is_err());

        assert_eq!(resolve_wait(Some(10), None).unwrap(), 10);
        assert_eq!(resolve_wait(Some(10), Some(0.5)).unwrap(), 510);
        assert_eq!(resolve_wait(None, Some(1.0)).unwrap(), 1000);
        assert!(resolve_wait(None, None).is_err());
        assert!(resolve_wait(None, Some(-1.0)).is_err());
        assert!(resolve_wait(None, Some(f64::NAN)).is_err());
    }

    #[test]
    fn json_line_dispatch_and_stub_actions() {
        let wait = handle_request_line(r#"{"id":1,"action":"wait","ms":0}"#);
        assert!(wait.ok);
        assert_eq!(wait.to_value()["id"], 1);
        assert_eq!(wait.to_value()["waited_ms"], 0);

        let bad = handle_request_line("not-json");
        assert!(!bad.ok);
        assert_eq!(bad.to_value()["error"]["code"], "invalid_arguments");

        let missing = handle_request_line(r#"{"id":"x","action":"nope"}"#);
        assert!(!missing.ok);
        assert_eq!(missing.to_value()["id"], "x");

        let request = parse_request(json!({"id": 3, "action": "displays"})).unwrap();
        assert_eq!(request.id, Some(json!(3)));
        assert!(matches!(request.action, Action::Displays));

        let backend = StubBackend::default();
        let response = dispatch_with(
            &backend,
            Action::Click {
                x: 3,
                y: 4,
                button: MouseButton::Left,
                double: true,
            },
        );
        assert!(response.ok);
        assert_eq!(response.to_value()["action"], "double_click");
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 2);

        let response = dispatch_with(&backend, Action::Move { x: 8, y: 9 });
        assert!(response.ok);
        assert_eq!(backend.moves.load(Ordering::SeqCst), 1);

        let response = dispatch_with(&backend, Action::Type { text: "hi".into() });
        assert_eq!(response.to_value()["chars"], 2);
        assert_eq!(backend.typed.lock().unwrap().as_slice(), ["hi"]);

        let response = dispatch_with(
            &backend,
            Action::Key {
                keys: "cmd+shift+4".into(),
            },
        );
        assert!(response.ok);
        assert_eq!(response.to_value()["key"], "4");
        let keys = backend.keys.lock().unwrap();
        assert_eq!(keys[0].1, "4");
        assert_eq!(keys[0].0, vec![Modifier::Command, Modifier::Shift]);
        drop(keys);

        let response = dispatch_with(
            &backend,
            Action::Key {
                keys: "not+a+key".into(),
            },
        );
        assert!(!response.ok);

        let response = dispatch_with(
            &backend,
            Action::Scroll {
                x: Some(1),
                y: Some(2),
                dx: None,
                dy: None,
                direction: Some(ScrollDirection::Down),
                amount: Some(3),
            },
        );
        assert!(response.ok);
        assert_eq!(backend.scrolls.lock().unwrap().as_slice(), [(0, 3)]);
        assert_eq!(backend.moves.load(Ordering::SeqCst), 2);

        let response = dispatch_with(&backend, Action::Displays);
        assert_eq!(response.to_value()["displays"][0]["name"], "Stub");

        let response = dispatch_with(&backend, Action::GetState);
        assert_eq!(response.to_value()["cursor"]["x"], 4);
        assert!(response.ok);

        let shot = dispatch_with(
            &backend,
            Action::Screenshot {
                output: None,
                region: None,
                format: Some(ImageFormat::Png),
                max_width: Some(1),
                base64: true,
            },
        );
        assert!(shot.ok, "{shot:?}");
        let value = shot.to_value();
        assert_eq!(value["width"], 1);
        assert_eq!(value["height"], 1);
        assert_eq!(value["scale"], 2.0);
        assert_eq!(value["format"], "png");
        let path = value["path"].as_str().unwrap();
        let bytes = fs::read(path).unwrap();
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
        assert!(value["data"].as_str().unwrap().starts_with("iVBOR"));
        let _ = fs::remove_file(path);

        let failed = StubBackend {
            fail_click: true,
            ..StubBackend::default()
        };
        let response = dispatch_with(
            &failed,
            Action::Click {
                x: 0,
                y: 0,
                button: MouseButton::Left,
                double: false,
            },
        );
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
    }

    #[test]
    fn dispatch_wait_does_not_need_a_display() {
        let response = dispatch(Action::Wait {
            ms: Some(0),
            seconds: None,
        });
        assert!(response.ok);
        assert_eq!(response.to_value()["waited_ms"], 0);
    }

    #[test]
    fn shared_flags_are_stripped_before_the_action_decodes() {
        let request = parse_request(json!({
            "id": 4,
            "action": "list_windows",
            "app": "Foo \"Bar\"",
            "window": 0,
            "element": "3",
            "diff": true,
            "full": false,
            "return_state": true
        }))
        .unwrap();
        assert_eq!(request.id, Some(json!(4)));
        assert!(matches!(request.action, Action::ListWindows));
        assert_eq!(request.context.app.as_deref(), Some("Foo \"Bar\""));
        assert_eq!(request.context.window.as_deref(), Some("0"));
        assert_eq!(request.context.element, Some(3));
        assert!(request.context.diff);
        assert!(!request.context.full);
        assert!(request.context.return_state);
    }

    fn ctx(app: &str) -> CallContext {
        CallContext {
            app: Some(app.to_string()),
            ..CallContext::default()
        }
    }

    #[test]
    fn stub_inventory_launch_and_doctor() {
        let backend = StubBackend::default();

        let response = dispatch_with(&backend, Action::ListApps { all: false });
        assert!(response.ok);
        let value = response.to_value();
        let apps = value["apps"].as_array().unwrap();
        assert!(apps.iter().any(|app| app["name"] == "Notes"));
        assert!(apps.iter().all(|app| app["name"] != "Widget"));

        let response = dispatch_with(&backend, Action::ListApps { all: true });
        let value = response.to_value();
        assert!(value["apps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|app| app["pid"] == 77));

        let response = dispatch_with_context(&backend, Action::ListWindows, &ctx("Notes"));
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        let windows = &value["windows"];
        assert_eq!(windows.as_array().unwrap().len(), 2);
        assert_eq!(windows[0]["z"], 0);
        assert_eq!(windows[0]["id"], 100);
        assert_eq!(windows[1]["z"], 1);
        assert_eq!(windows[1]["title"], "Welcome");
        assert_eq!(windows[0]["ax_matched"], true);
        assert!(value.get("note").is_none());

        let response = dispatch_with_context(&backend, Action::Launch, &ctx("Notes"));
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["pid"], 10);
        assert_eq!(response.to_value()["action"], "launch");
        assert_eq!(backend.launched.lock().unwrap().as_slice(), ["Notes"]);

        let response = dispatch_with_context(&backend, Action::Launch, &ctx("com.apple.Terminal"));
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
        assert_eq!(backend.launched.lock().unwrap().len(), 1);

        let response = dispatch_with_context(&backend, Action::Activate, &ctx("10"));
        assert!(response.ok, "{response:?}");
        assert!(response.to_value()["window_id"].is_null());
        assert_eq!(backend.activated.lock().unwrap().len(), 1);

        let mut context = ctx("Notes");
        context.window = Some("Welcome".into());
        let response = dispatch_with_context(&backend, Action::Activate, &context);
        assert_eq!(response.to_value()["window_id"], 101);
        assert_eq!(response.to_value()["window_title"], "Welcome");

        let response = dispatch_with_context(&backend, Action::Quit, &ctx("Terminal"));
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
        assert!(backend.quit.lock().unwrap().is_empty());

        let response = dispatch_with_context(&backend, Action::Quit, &ctx("412"));
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["accepted"], true);
        assert_eq!(response.to_value()["name"], "Foo \"Bar\"");

        let response = dispatch_with(&backend, Action::Launch);
        assert_eq!(response.to_value()["error"]["code"], "invalid_arguments");

        let response = dispatch_with(&backend, Action::Doctor { request: false });
        let value = response.to_value();
        assert_eq!(value["ok"], true);
        assert_eq!(value["accessibility"], true);
        assert_eq!(value["screen_capture"], false);
        assert!(value["input_monitor"].is_null());
        assert_eq!(value["responsible_app"]["name"], "Terminal");
        assert!(value["fixes"][0]
            .as_str()
            .unwrap()
            .contains("Screen Recording"));

        let response = dispatch_with(&backend, Action::GetState);
        assert!(response.ok);
        let value = response.to_value();
        assert_eq!(value["pid"], 10);
        assert_eq!(value["source"], "stub");
        assert!(value["tree"].as_str().unwrap().contains("AXWindow"));
        assert!(value["accessibility_legacy"].is_null());

        let response = dispatch_with_context(&backend, Action::GetState, &ctx("412"));
        assert_eq!(response.to_value()["pid"], 412);
        assert_eq!(response.to_value()["source"], "stub");
        assert!(response.to_value()["tree"]
            .as_str()
            .unwrap()
            .contains("AXWindow"));
        let seen = backend.accessibility_apps.lock().unwrap().clone();
        assert_eq!(seen.last().and_then(|name| name.as_deref()), Some("412"));

        // Get-state already stored a baseline. Clear it so this sequence
        // still covers "no baseline prints the full tree".
        backend.baselines.lock().unwrap().clear();
        let first = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert!(first.ok, "{first:?}");
        let full = first.to_value()["tree"].as_str().unwrap().to_string();
        assert!(full.contains("value=\"a\""), "{full}");
        assert_eq!(first.to_value()["source"], "stub");
        *backend.stub_value.lock().unwrap() = "ab".into();
        let diff = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        let diff_text = diff.to_value()["tree"].as_str().unwrap().to_string();
        assert!(
            diff_text.contains("~ [1] AXTextArea value: \"a\" -> \"ab\""),
            "{diff_text}"
        );
        let mut full_context = ctx("Notes");
        full_context.full = true;
        let replaced = dispatch_with_context(&backend, Action::Tree, &full_context);
        let replaced_text = replaced.to_value()["tree"].as_str().unwrap().to_string();
        assert!(replaced_text.contains("value=\"ab\""), "{replaced_text}");
        assert!(!replaced_text.contains("~ [1]"));
        let after = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert_eq!(after.to_value()["tree"].as_str().unwrap(), "");

        let ended = dispatch_with(&backend, Action::SessionEnd);
        assert!(ended.ok, "{ended:?}");
        assert_eq!(ended.to_value()["action"], "session_end");
        assert!(ended.to_value()["restored"].as_array().unwrap().is_empty());
    }

    #[test]
    fn stub_refuses_privacy_window_and_self_quit() {
        let mut backend = StubBackend::default();
        backend.apps.push(crate::target::AppInfo {
            name: "System Settings".into(),
            bundle_id: Some("com.apple.systempreferences".into()),
            pid: 50,
            path: Some(
                "/System/Applications/System Settings.app/Contents/MacOS/System Settings".into(),
            ),
            active: false,
            hidden: false,
            activation_policy: crate::target::ActivationPolicy::Regular,
        });
        backend.windows.push(crate::target::WindowInfo {
            id: 9,
            pid: 50,
            app: "System Settings".into(),
            title: "Privacy & Security".into(),
            z: 4,
            bounds: crate::target::WindowBounds {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            layer: 0,
            on_screen: true,
            ax_matched: false,
        });
        let mut context = ctx("com.apple.systempreferences");
        context.window = Some("Privacy".into());
        let response = dispatch_with_context(&backend, Action::Activate, &context);
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
        assert!(backend.activated.lock().unwrap().is_empty());

        let response = dispatch_with_context(&backend, Action::Activate, &ctx("System Settings"));
        assert!(response.ok, "{response:?}");

        let self_pid = std::process::id() as i32;
        backend.apps.retain(|app| app.pid != self_pid);
        backend.apps.push(crate::target::AppInfo {
            name: "computer-use".into(),
            bundle_id: None,
            pid: self_pid,
            path: None,
            active: false,
            hidden: false,
            activation_policy: crate::target::ActivationPolicy::Regular,
        });
        let response = dispatch_with_context(&backend, Action::Quit, &ctx(&self_pid.to_string()));
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
    }
}
