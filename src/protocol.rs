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
use crate::backend::{validate_key, DesktopBackend, WindowTarget};
use crate::cache;
use crate::element::{self, ElementError, ElementOp, ElementOutcome, ElementRequest};
use crate::error::{ComputerError, ErrorBody};
use crate::image_util::{self, encode_base64};
use crate::keys::parse_key_combo;
use crate::policy;
use crate::state::collect_state_for;
use crate::target::{
    filter_windows_for_pid, parse_app_selector, parse_window_selector, resolve_app, resolve_window,
    WindowSelector,
};
use crate::tier::{InputJob, InputKind, TierName};
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
        ///
        /// Absent means 1440. `0` keeps the source width.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<u32>,
        /// Letterbox into `WxH` pixels, for example `1440x900`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        canvas: Option<String>,
        /// Include standard base64 bytes in `data`.
        #[serde(default, skip_serializing_if = "is_false")]
        base64: bool,
    },
    /// Move to a point and click, or press an element when `element` is set.
    Click {
        /// Logical x. Omit it when `element` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Logical y. Omit it when `element` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Button. Defaults to left.
        #[serde(default)]
        button: MouseButton,
        /// Click twice when true. With `element`, this is `press` count 2.
        #[serde(default, skip_serializing_if = "is_false")]
        double: bool,
    },
    /// Click twice. Equivalent to `click` with `double: true`.
    #[serde(rename = "double_click")]
    DoubleClick {
        /// Logical x. Omit it when `element` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Logical y. Omit it when `element` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Button. Defaults to left.
        #[serde(default)]
        button: MouseButton,
    },
    /// Press an element. `count` is 1, 2, or 3.
    Press {
        /// Repeat count. Defaults to 1.
        #[serde(default = "default_press_count")]
        count: u8,
    },
    /// Set `AXFocused` on an element.
    Focus,
    /// Set an element's value. Refused for secure fields.
    #[serde(alias = "set-value")]
    SetValue {
        /// New value.
        value: String,
    },
    /// Set the selected character range. Refused for secure fields.
    #[serde(alias = "select-text")]
    SelectText {
        /// First selected character.
        start: i64,
        /// Character after the selection.
        end: i64,
    },
    /// Perform an allowlisted accessibility action.
    #[serde(alias = "perform-action")]
    PerformAction {
        /// Action name such as `AXShowMenu`.
        name: String,
    },
    /// Scroll an element into view.
    #[serde(
        alias = "scroll-to",
        alias = "scroll_to_visible",
        alias = "scroll-to-visible"
    )]
    ScrollTo,
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
    /// Ask an application to quit. `force` requests a forced terminate.
    Quit {
        /// Force-quit when the platform supports it. Still requires confirmation.
        #[serde(default, skip_serializing_if = "is_false")]
        force: bool,
    },
    /// Report whether policy would allow `app`. Does not perform an action.
    #[serde(rename = "policy_check")]
    PolicyCheck,
    /// Pause mutating actions until `resume`.
    Pause,
    /// Clear the pause flag.
    Resume,
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
    /// Read the pointer. The Anthropic adapter maps `cursor_position` here.
    CursorPosition,
    /// Run several actions that share one target and one cache revision sequence.
    Batch {
        /// Stop after the first failed step. Defaults to true.
        #[serde(default = "default_stop_on_error")]
        stop_on_error: bool,
        /// Native action objects. A nested `batch` is rejected at dispatch.
        actions: Vec<Value>,
    },
    /// Remove empty session directories and stale ones.
    #[serde(alias = "session-prune")]
    SessionPrune {
        /// Report matches without deleting them.
        #[serde(default)]
        dry_run: bool,
        /// Age in seconds after which a dead owner's session is stale. Absent means 24 hours.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_secs: Option<u64>,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn default_press_count() -> u8 {
    1
}

fn default_stop_on_error() -> bool {
    true
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
    /// Fail when the element is outside the window and scroll-into-view cannot run.
    pub require_visible: bool,
    /// User policy file from `--policy`. Absent uses the default path.
    pub policy: Option<String>,
    /// Confirm hook from `--confirm-cmd`. Wins over the environment and the policy file.
    pub confirm_cmd: Option<String>,
    /// Skip `foreground.lock` for tier `activate`.
    pub allow_foreground: bool,
    /// Refuse tier `activate`, and treat cursor motion as failure.
    pub require_background: bool,
    /// Do not send the agent point to the overlay helper.
    pub no_overlay: bool,
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
        require_visible: take_bool(object, "require_visible")?,
        policy: None,
        confirm_cmd: None,
        allow_foreground: take_bool(object, "allow_foreground")?,
        require_background: take_bool(object, "require_background")?,
        no_overlay: take_bool(object, "no_overlay")?,
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
/// Parse errors return `ok: false` and do not panic. The adapter is native
/// unless the object sets `adapter`.
pub fn handle_request_line(line: &str) -> Response {
    handle_request_line_with(line, None)
}

/// Options from the CLI that JSON requests do not carry.
#[derive(Debug, Clone, Copy, Default)]
pub struct LineOptions<'a> {
    /// Process `--adapter` flag. A request field wins.
    pub adapter: Option<&'a str>,
    /// Process `--policy` path.
    pub policy: Option<&'a str>,
    /// Process `--confirm-cmd` program.
    pub confirm_cmd: Option<&'a str>,
    /// Process `--tier`. A request field wins. `--synthetic-focus` arrives as `synthetic`.
    pub tier: Option<&'a str>,
    /// Process `--allow-foreground`. Combined with the request by or.
    pub allow_foreground: bool,
    /// Process `--require-background`. Combined with the request by or.
    pub require_background: bool,
    /// Process `--no-overlay`. Combined with the request by or.
    pub no_overlay: bool,
}

/// Decode one JSON line. `cli_adapter` is the process `--adapter` flag.
///
/// A request field `adapter` wins. `native` skips translation. There is no
/// detection when both are absent.
pub fn handle_request_line_with(line: &str, cli_adapter: Option<&str>) -> Response {
    handle_request_line_opts(
        line,
        LineOptions {
            adapter: cli_adapter,
            ..LineOptions::default()
        },
    )
}

/// Decode one JSON line and apply CLI `--policy` and `--confirm-cmd`.
pub fn handle_request_line_opts(line: &str, options: LineOptions<'_>) -> Response {
    let value = match serde_json::from_str::<Value>(line) {
        Ok(value) => value,
        Err(error) => {
            return Response::failure("invalid_arguments", format!("invalid JSON: {error}"))
        }
    };
    let id = value.get("id").cloned();
    let echo = match effective_adapter(&value, options.adapter) {
        Ok(name) => name,
        Err(error) => return Response::from_error(error).with_id(id),
    };
    match build_request(value, echo.as_deref()) {
        Ok(mut request) => {
            if let Some(policy) = options.policy {
                request.context.policy = Some(policy.to_string());
            }
            if let Some(confirm) = options.confirm_cmd {
                request.context.confirm_cmd = Some(confirm.to_string());
            }
            if request.context.tier.is_none() {
                if let Some(tier) = options.tier {
                    request.context.tier = Some(tier.to_string());
                }
            }
            request.context.allow_foreground |= options.allow_foreground;
            request.context.require_background |= options.require_background;
            request.context.no_overlay |= options.no_overlay;
            dispatch_context(request.action, &request.context).with_id(request.id)
        }
        Err(error) => {
            let mut response = Response::from_error(error).with_id(id);
            stamp_adapter_name(&mut response, echo.as_deref());
            response
        }
    }
}

fn effective_adapter(
    value: &Value,
    cli_adapter: Option<&str>,
) -> Result<Option<String>, ComputerError> {
    if !value.is_object() {
        return Ok(None);
    }
    let from_request = match value.get("adapter") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.as_str()),
        Some(_) => {
            return Err(ComputerError::InvalidArguments(
                "adapter must be a string".into(),
            ))
        }
    };
    let name = from_request.or(cli_adapter);
    match name {
        None | Some("native") => Ok(None),
        Some("openai" | "anthropic") => Ok(name.map(str::to_string)),
        Some(other) => Err(ComputerError::InvalidArguments(format!(
            "unknown adapter {other:?}"
        ))),
    }
}

fn build_request(value: Value, adapter: Option<&str>) -> Result<Request, ComputerError> {
    let Some(name) = adapter else {
        return parse_request(value);
    };
    if !value.is_object() {
        return Err(ComputerError::InvalidArguments(
            "request must be a JSON object".into(),
        ));
    }
    let mut value = value;
    let object = value
        .as_object_mut()
        .expect("object check above guarantees a map");
    let id = object.remove("id");
    let mut context = take_context(object)?;
    context.coord = Some("image".into());
    context.adapter = Some(name.to_string());
    let action = crate::adapters::translate(name, &value)?;
    Ok(Request {
        id,
        action,
        context,
    })
}

fn stamp_adapter_name(response: &mut Response, adapter: Option<&str>) {
    if let Some(name @ ("openai" | "anthropic")) = adapter {
        response.fields.insert("adapter".into(), json!(name));
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
    let mut response = match catch_unwind(AssertUnwindSafe(|| {
        dispatch_inner(backend, action, context)
    })) {
        Ok(response) => response,
        Err(_) => Response::failure("execution_failed", "action panicked and was recovered"),
    };
    stamp_adapter_name(&mut response, context.adapter.as_deref());
    response
}

enum PostKind {
    /// Successful or failed mutating action. `return_state` snapshots afterward.
    Mutating,
    /// `return_state` and `include_screenshot` are ignored.
    ReadOnly,
    /// The action already captured. `include_screenshot` reuses that body.
    Screenshot,
}

fn dispatch_inner(backend: &dyn DesktopBackend, action: Action, context: &CallContext) -> Response {
    let label = action_label(&action);
    let action = match remap_coordinates(context, action) {
        Ok(action) => action,
        Err(error) => {
            return finish_action(
                context,
                label,
                &policy::Subject::default(),
                None,
                "allow",
                "default",
                Response::from_error(error),
            )
        }
    };
    let prepared = prepare_action(backend, &action, context);
    if let Some(response) = prepared.stop {
        return finish_action(
            context,
            label,
            &prepared.subject,
            prepared.warning.as_deref(),
            prepared.decision,
            prepared.source,
            response,
        );
    }
    let kind = post_state_kind(&action);
    let response = match action {
        Action::Batch {
            actions,
            stop_on_error,
        } => {
            return finish_action(
                context,
                label,
                &prepared.subject,
                prepared.warning.as_deref(),
                prepared.decision,
                prepared.source,
                run_batch(backend, context, actions, stop_on_error),
            )
        }
        Action::Screenshot {
            output,
            region,
            format,
            max_width,
            canvas,
            base64,
        } => result_response(screenshot(
            backend,
            context,
            ScreenshotArgs {
                output,
                region,
                format,
                max_width,
                canvas,
                include_base64: base64,
            },
        )),
        Action::Click {
            x,
            y,
            button,
            double,
        } => click(backend, context, x, y, button, double),
        Action::DoubleClick { x, y, button } => click(backend, context, x, y, button, true),
        Action::Press { count } => element_command(backend, context, ElementOp::Press { count }),
        Action::Focus => element_command(backend, context, ElementOp::Focus),
        Action::SetValue { value } => {
            element_command(backend, context, ElementOp::SetValue { value })
        }
        Action::SelectText { start, end } => {
            element_command(backend, context, ElementOp::SelectText { start, end })
        }
        Action::PerformAction { name } => {
            element_command(backend, context, ElementOp::Perform { name })
        }
        Action::ScrollTo => element_command(backend, context, ElementOp::ScrollTo),
        Action::Move { x, y } => move_mouse(backend, context, x, y),
        Action::Type { text } => type_command(backend, context, text),
        Action::Key { keys } => press_key(backend, context, &keys),
        Action::Scroll {
            x,
            y,
            dx,
            dy,
            direction,
            amount,
        } => scroll(backend, context, (x, y), (dx, dy), direction, amount),
        Action::GetState => result_response(Ok(collect_state_for(backend, &tree_query(context)))),
        Action::Tree => result_response(tree_action(backend, context)),
        Action::SessionEnd => result_response(session_end(backend, context)),
        Action::Wait { ms, seconds } => result_response(wait(ms, seconds)),
        Action::Displays => result_response(displays(backend)),
        Action::ListApps { all } => result_response(list_apps(backend, all)),
        Action::ListWindows => result_response(list_windows(backend, context.app.as_deref())),
        Action::Launch => result_response(launch(backend, context.app.as_deref())),
        Action::Activate => result_response(activate(
            backend,
            context.app.as_deref(),
            context.window.as_deref(),
        )),
        Action::Quit { force } => result_response(quit(backend, context.app.as_deref(), force)),
        Action::Doctor { request } => result_response(doctor(backend, request)),
        Action::CursorPosition => result_response(cursor_position(backend)),
        Action::SessionPrune { dry_run, ttl_secs } => {
            result_response(session_prune(dry_run, ttl_secs))
        }
        Action::PolicyCheck => {
            let (response, subject) = policy_check(backend, context);
            let response = attach_post_state(backend, context, kind, response);
            return finish_action(
                context,
                label,
                &subject,
                prepared.warning.as_deref(),
                prepared.decision,
                prepared.source,
                response,
            );
        }
        Action::Pause => pause_response(true),
        Action::Resume => pause_response(false),
    };
    let response = attach_post_state(backend, context, kind, response);
    finish_action(
        context,
        label,
        &prepared.subject,
        prepared.warning.as_deref(),
        prepared.decision,
        prepared.source,
        response,
    )
}

fn finish_action(
    context: &CallContext,
    label: &str,
    subject: &policy::Subject,
    warning: Option<&str>,
    decision: &str,
    source: &str,
    mut response: Response,
) -> Response {
    if let Some(warning) = warning {
        push_warning(&mut response, warning);
    }
    let code = if response.ok {
        "ok"
    } else {
        response
            .error
            .as_ref()
            .map(|error| error.code.as_str())
            .unwrap_or("execution_failed")
    };
    let mut decision = decision.to_string();
    let mut source = source.to_string();
    if label == "policy_check" {
        if let Some(value) = response
            .fields
            .get("decision")
            .and_then(|value| value.as_str())
        {
            decision = value.to_string();
        }
        if let Some(value) = response
            .fields
            .get("source")
            .and_then(|value| value.as_str())
        {
            source = value.to_string();
        }
    }
    let tier = response
        .fields
        .get("tier")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let session = context
        .session
        .clone()
        .unwrap_or_else(|| std::process::id().to_string());
    if let Err(error) = crate::audit::record(&crate::audit::AuditRecord {
        session: &session,
        action: label,
        bundle_id: subject.bundle_id.as_deref(),
        pid: subject.pid,
        window_id: subject.window_id,
        element: context.element,
        code,
        decision: &decision,
        source: &source,
        tier: tier.as_deref(),
    }) {
        push_warning(
            &mut response,
            &format!("audit log was not written: {error}"),
        );
    }
    response
}

fn push_warning(response: &mut Response, warning: &str) {
    let warnings = response
        .fields
        .entry("warnings")
        .or_insert_with(|| json!([]));
    if let Some(items) = warnings.as_array_mut() {
        items.push(json!(warning));
    }
}

fn action_label(action: &Action) -> &'static str {
    match action {
        Action::Screenshot { .. } => "screenshot",
        Action::Click { .. } => "click",
        Action::DoubleClick { .. } => "double_click",
        Action::Press { .. } => "press",
        Action::Focus => "focus",
        Action::SetValue { .. } => "set_value",
        Action::SelectText { .. } => "select_text",
        Action::PerformAction { .. } => "perform_action",
        Action::ScrollTo => "scroll_to",
        Action::Move { .. } => "move",
        Action::Type { .. } => "type",
        Action::Key { .. } => "key",
        Action::Scroll { .. } => "scroll",
        Action::GetState => "get_state",
        Action::Wait { .. } => "wait",
        Action::Displays => "displays",
        Action::ListApps { .. } => "list_apps",
        Action::ListWindows => "list_windows",
        Action::Launch => "launch",
        Action::Activate => "activate",
        Action::Quit { .. } => "quit",
        Action::Doctor { .. } => "doctor",
        Action::Tree => "tree",
        Action::SessionEnd => "session_end",
        Action::CursorPosition => "cursor_position",
        Action::Batch { .. } => "batch",
        Action::SessionPrune { .. } => "session_prune",
        Action::PolicyCheck => "policy_check",
        Action::Pause => "pause",
        Action::Resume => "resume",
    }
}

#[derive(Clone, Copy)]
enum PolicyClass {
    Always,
    Benign,
    Observe,
    Mutate,
    Quit,
    Key,
}

fn policy_class(action: &Action) -> PolicyClass {
    match action {
        Action::Wait { .. }
        | Action::Doctor { .. }
        | Action::Displays
        | Action::ListApps { .. }
        | Action::ListWindows
        | Action::Pause
        | Action::Resume => PolicyClass::Always,
        Action::SessionEnd | Action::SessionPrune { .. } | Action::CursorPosition => {
            PolicyClass::Benign
        }
        Action::Screenshot { .. } | Action::Tree | Action::GetState | Action::PolicyCheck => {
            PolicyClass::Observe
        }
        Action::Quit { .. } => PolicyClass::Quit,
        Action::Key { .. } => PolicyClass::Key,
        _ => PolicyClass::Mutate,
    }
}

struct Prepared {
    stop: Option<Response>,
    warning: Option<String>,
    subject: policy::Subject,
    decision: &'static str,
    source: &'static str,
}

fn early_stop(warning: Option<String>, subject: policy::Subject, response: Response) -> Prepared {
    Prepared {
        stop: Some(response),
        warning,
        subject,
        decision: "allow",
        source: "default",
    }
}

fn prepare_action(
    backend: &dyn DesktopBackend,
    action: &Action,
    context: &CallContext,
) -> Prepared {
    let class = policy_class(action);
    let loaded = match policy::load_policy(context.policy.as_deref().map(Path::new)) {
        Ok(policy) => policy,
        Err(error) => {
            if matches!(class, PolicyClass::Always) {
                policy::LoadedPolicy::default()
            } else {
                return early_stop(
                    None,
                    policy::Subject::default(),
                    Response::from_error(error),
                );
            }
        }
    };
    let warning = if loaded.pause_on_user_input {
        backend.user_input_pause_warning().map(str::to_string)
    } else {
        None
    };
    if matches!(action, Action::Pause | Action::Resume | Action::PolicyCheck) {
        return Prepared {
            stop: None,
            warning,
            subject: policy::Subject::default(),
            decision: "allow",
            source: "default",
        };
    }
    let subject = match resolve_subject(backend, action, context) {
        Ok(subject) => subject,
        Err(error) => {
            return early_stop(
                warning,
                policy::Subject::default(),
                Response::from_error(error),
            )
        }
    };
    let policy_action = match action {
        Action::Quit { .. } => policy::PolicyAction::Quit,
        Action::Key { keys } => policy::PolicyAction::Key {
            combo: keys.clone(),
        },
        Action::Screenshot { .. } | Action::Tree | Action::GetState => {
            policy::PolicyAction::Observe
        }
        _ => match class {
            PolicyClass::Always => policy::PolicyAction::Always,
            PolicyClass::Benign => policy::PolicyAction::Benign,
            PolicyClass::Mutate => policy::PolicyAction::Mutate,
            PolicyClass::Observe => policy::PolicyAction::Observe,
            PolicyClass::Quit => policy::PolicyAction::Quit,
            PolicyClass::Key => policy::PolicyAction::Key {
                combo: String::new(),
            },
        },
    };
    let decision = policy::decide(&loaded, &subject, &policy_action);
    if let policy::Decision::Deny { message, source } = &decision {
        return Prepared {
            stop: Some(Response::from_error(ComputerError::PolicyDenied(
                message.clone(),
            ))),
            warning,
            subject,
            decision: "deny",
            source,
        };
    }
    if let Err(error) = refuse_blocked_url(backend, &loaded, &subject) {
        let (decision_name, source) = decision_record(&decision);
        return Prepared {
            stop: Some(Response::from_error(error)),
            warning,
            subject,
            decision: decision_name,
            source,
        };
    }
    let mutating = matches!(
        class,
        PolicyClass::Mutate | PolicyClass::Quit | PolicyClass::Key
    );
    if mutating && policy::is_paused() {
        let (decision_name, source) = decision_record(&decision);
        return Prepared {
            stop: Some(Response::from_error(ComputerError::Paused(
                "local input paused mutating actions; run computer-use resume".into(),
            ))),
            warning,
            subject,
            decision: decision_name,
            source,
        };
    }
    if let Action::Key { .. } = action {
        if let Some(index) = context.element {
            if let Ok(Some(peek)) = backend.peek_element(
                context.app.as_deref(),
                context.window.as_deref(),
                context.session.as_deref(),
                index,
            ) {
                if peek.secure {
                    return Prepared {
                        stop: Some(Response::from_error(ComputerError::PolicyDenied(
                            "refusing a key event aimed at a secure field".into(),
                        ))),
                        warning,
                        subject,
                        decision: "deny",
                        source: "builtin",
                    };
                }
            }
        }
    }
    if let policy::Decision::Confirm { reason } = &decision {
        if let Err(error) = confirm_action(context, &loaded, &subject, action_label(action), reason)
        {
            return Prepared {
                stop: Some(Response::from_error(error)),
                warning,
                subject,
                decision: "confirm",
                source: "builtin",
            };
        }
    }
    let (decision_name, source) = decision_record(&decision);
    Prepared {
        stop: None,
        warning,
        subject,
        decision: decision_name,
        source,
    }
}

fn decision_record(decision: &policy::Decision) -> (&'static str, &'static str) {
    match decision {
        policy::Decision::Allow { source } => ("allow", *source),
        policy::Decision::Deny { source, .. } => ("deny", *source),
        policy::Decision::Confirm { .. } => ("confirm", "builtin"),
    }
}

fn confirm_action(
    context: &CallContext,
    loaded: &policy::LoadedPolicy,
    subject: &policy::Subject,
    action: &str,
    reason: &str,
) -> Result<(), ComputerError> {
    let command = policy::confirm_program(loaded, context.confirm_cmd.as_deref());
    policy::run_confirm(
        command.as_deref(),
        loaded.timeout_secs(),
        &policy::ConfirmDocument {
            action,
            bundle_id: subject.bundle_id.as_deref(),
            path: subject.path.as_deref(),
            pid: subject.pid,
            window_id: subject.window_id,
            element: context.element,
            reason,
        },
    )
}

fn refuse_blocked_url(
    backend: &dyn DesktopBackend,
    loaded: &policy::LoadedPolicy,
    subject: &policy::Subject,
) -> Result<(), ComputerError> {
    if loaded.blocked_urls().is_empty() {
        return Ok(());
    }
    if !policy::is_browser(
        subject.bundle_id.as_deref(),
        subject.path.as_deref().map(Path::new),
        subject.name.as_deref(),
    ) {
        return Ok(());
    }
    let from_title = subject
        .window_title
        .as_deref()
        .and_then(policy::url_in_text)
        .map(str::to_string);
    let url = if let Some(url) = from_title {
        Some(url)
    } else if let Some(pid) = subject.pid {
        backend.browser_url(pid)?
    } else {
        None
    };
    if let Some(url) = url.as_deref() {
        if let Some(pattern) = policy::url_blocked(url, loaded.blocked_urls()) {
            return Err(ComputerError::BlockedUrl(format!("matched {pattern}")));
        }
    }
    Ok(())
}

fn resolve_subject(
    backend: &dyn DesktopBackend,
    action: &Action,
    context: &CallContext,
) -> Result<policy::Subject, ComputerError> {
    let class = policy_class(action);
    if matches!(class, PolicyClass::Always | PolicyClass::Benign) {
        return Ok(policy::Subject::default());
    }
    let screenshot = matches!(action, Action::Screenshot { .. });
    let frontmost_observe =
        matches!(action, Action::Tree | Action::GetState) && context.app.is_none();
    let app_text = context.app.clone().or_else(|| {
        if frontmost_observe {
            backend.frontmost().app
        } else {
            None
        }
    });
    if app_text.is_none() && context.window.is_none() {
        return Ok(policy::Subject::default());
    }
    if app_text.is_none() && screenshot {
        return subject_from_window(backend, context);
    }
    let Some(app_text) = app_text else {
        return Ok(policy::Subject::default());
    };
    let selector = parse_app_selector(&app_text)?;
    let fallback = subject_from_selector(&selector);
    let apps = match backend.list_apps(true) {
        Ok(apps) => apps,
        Err(error) => {
            if policy::selector_denial(&selector).is_some() || matches!(action, Action::Launch) {
                return Ok(fallback);
            }
            return Err(error);
        }
    };
    let info = match resolve_app(&apps, &selector) {
        Ok(info) => info.clone(),
        Err(error) => {
            if policy::selector_denial(&selector).is_some() || matches!(action, Action::Launch) {
                return Ok(fallback);
            }
            // An implicit frontmost name that is not running is not a target.
            // The backend resolves the key window itself.
            if context.app.is_none() {
                return Ok(policy::Subject::default());
            }
            return Err(error);
        }
    };
    let mut subject = policy::Subject {
        name: Some(info.name.clone()),
        bundle_id: info.bundle_id.clone(),
        path: info.path.clone(),
        pid: Some(info.pid),
        window_id: None,
        window_title: None,
        ax_identifier: None,
    };
    let want_window = context.window.is_some()
        || matches!(
            action,
            Action::Tree | Action::GetState | Action::Screenshot { .. }
        );
    if want_window {
        if let Ok(listed) = backend.list_windows() {
            let owned = filter_windows_for_pid(&listed.windows, info.pid);
            let chosen = match context.window.as_deref() {
                Some(window) => {
                    let selector = parse_window_selector(window)?;
                    Some(resolve_window(&owned, &selector)?.clone())
                }
                None => owned.first().cloned(),
            };
            if let Some(window) = chosen {
                subject.window_id = Some(window.id);
                if context.window.is_some()
                    || matches!(
                        action,
                        Action::Tree | Action::GetState | Action::Screenshot { .. }
                    )
                {
                    subject.window_title = Some(window.title);
                }
            }
        }
    }
    Ok(subject)
}

fn subject_from_window(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<policy::Subject, ComputerError> {
    let Some(window) = context.window.as_deref() else {
        return Ok(policy::Subject::default());
    };
    let listed = backend.list_windows()?;
    let selector = parse_window_selector(window)?;
    let window = resolve_window(&listed.windows, &selector)?.clone();
    let apps = backend.list_apps(true)?;
    let info = apps.into_iter().find(|app| app.pid == window.pid);
    Ok(policy::Subject {
        name: info
            .as_ref()
            .map(|app| app.name.clone())
            .or(Some(window.app.clone())),
        bundle_id: info.as_ref().and_then(|app| app.bundle_id.clone()),
        path: info.and_then(|app| app.path),
        pid: Some(window.pid),
        window_id: Some(window.id),
        window_title: Some(window.title),
        ax_identifier: None,
    })
}

fn subject_from_selector(selector: &crate::target::AppSelector) -> policy::Subject {
    use crate::target::AppSelector;
    match selector {
        AppSelector::BundleId(bundle) => policy::Subject {
            bundle_id: Some(bundle.clone()),
            ..policy::Subject::default()
        },
        AppSelector::Name(name) => policy::Subject {
            name: Some(name.clone()),
            ..policy::Subject::default()
        },
        AppSelector::Path(path) => policy::Subject {
            path: Some(path.display().to_string()),
            ..policy::Subject::default()
        },
        AppSelector::Pid(pid) => policy::Subject {
            pid: Some(*pid),
            ..policy::Subject::default()
        },
    }
}

fn policy_check(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> (Response, policy::Subject) {
    let Some(app) = context
        .app
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return (
            Response::from_error(ComputerError::InvalidArguments("app is required".into())),
            policy::Subject::default(),
        );
    };
    let loaded = match policy::load_policy(context.policy.as_deref().map(Path::new)) {
        Ok(policy) => policy,
        Err(error) => return (Response::from_error(error), policy::Subject::default()),
    };
    let selector = match parse_app_selector(app) {
        Ok(selector) => selector,
        Err(error) => return (Response::from_error(error), policy::Subject::default()),
    };
    let mut subject = subject_from_selector(&selector);
    if let Ok(apps) = backend.list_apps(true) {
        if let Ok(info) = resolve_app(&apps, &selector) {
            subject.name = Some(info.name.clone());
            subject.bundle_id = info.bundle_id.clone();
            subject.path = info.path.clone();
            subject.pid = Some(info.pid);
        }
    }
    let decision = policy::app_verdict(&loaded, &subject);
    let (name, source) = match decision {
        policy::Decision::Allow { source } => ("allow", source),
        policy::Decision::Deny { source, .. } => ("deny", source),
        policy::Decision::Confirm { .. } => ("allow", "default"),
    };
    (
        Response::success(json!({
            "decision": name,
            "source": source,
            "bundle_id": subject.bundle_id,
        })),
        subject,
    )
}

fn pause_response(paused: bool) -> Response {
    match policy::set_paused(paused) {
        Ok(()) => Response::success(json!({
            "action": if paused { "pause" } else { "resume" },
            "paused": paused,
        })),
        Err(error) => Response::from_error(error),
    }
}

fn result_response(result: Result<Value, ComputerError>) -> Response {
    match result {
        Ok(value) => Response::success(value),
        Err(error) => Response::from_error(error),
    }
}

fn post_state_kind(action: &Action) -> PostKind {
    match action {
        Action::Screenshot { .. } => PostKind::Screenshot,
        Action::Click { .. }
        | Action::DoubleClick { .. }
        | Action::Press { .. }
        | Action::Focus
        | Action::SetValue { .. }
        | Action::SelectText { .. }
        | Action::PerformAction { .. }
        | Action::ScrollTo
        | Action::Move { .. }
        | Action::Type { .. }
        | Action::Key { .. }
        | Action::Scroll { .. }
        | Action::Launch
        | Action::Activate
        | Action::Quit { .. }
        | Action::Batch { .. } => PostKind::Mutating,
        Action::GetState
        | Action::Wait { .. }
        | Action::Displays
        | Action::ListApps { .. }
        | Action::ListWindows
        | Action::Doctor { .. }
        | Action::Tree
        | Action::SessionEnd
        | Action::SessionPrune { .. }
        | Action::CursorPosition
        | Action::PolicyCheck
        | Action::Pause
        | Action::Resume => PostKind::ReadOnly,
    }
}

fn remap_coordinates(context: &CallContext, action: Action) -> Result<Action, ComputerError> {
    match context.coord.as_deref() {
        None | Some("screen") => Ok(action),
        Some("image") => remap_image_action(context, action),
        Some(other) => Err(ComputerError::InvalidArguments(format!(
            "coord must be screen or image (got {other})"
        ))),
    }
}

fn remap_image_action(context: &CallContext, action: Action) -> Result<Action, ComputerError> {
    match action {
        Action::Click {
            x,
            y,
            button,
            double,
        } => {
            let (x, y) = remap_optional_point(context, x, y)?;
            Ok(Action::Click {
                x,
                y,
                button,
                double,
            })
        }
        Action::DoubleClick { x, y, button } => {
            let (x, y) = remap_optional_point(context, x, y)?;
            Ok(Action::DoubleClick { x, y, button })
        }
        Action::Move { x, y } => {
            let (x, y) = remap_point(context, x, y)?;
            Ok(Action::Move { x, y })
        }
        Action::Scroll {
            x,
            y,
            dx,
            dy,
            direction,
            amount,
        } => {
            let (x, y) = remap_optional_point(context, x, y)?;
            Ok(Action::Scroll {
                x,
                y,
                dx,
                dy,
                direction,
                amount,
            })
        }
        Action::Screenshot {
            output,
            region,
            format,
            max_width,
            canvas,
            base64,
        } => {
            let region = match region {
                Some(region) => Some(remap_region(context, region)?),
                None => None,
            };
            Ok(Action::Screenshot {
                output,
                region,
                format,
                max_width,
                canvas,
                base64,
            })
        }
        other => Ok(other),
    }
}

fn remap_optional_point(
    context: &CallContext,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(Option<i32>, Option<i32>), ComputerError> {
    match (x, y) {
        (Some(x), Some(y)) => {
            let (x, y) = remap_point(context, x, y)?;
            Ok((Some(x), Some(y)))
        }
        other => Ok(other),
    }
}

fn remap_point(context: &CallContext, x: i32, y: i32) -> Result<(i32, i32), ComputerError> {
    let map = require_screenshot_map(context)?;
    ensure_point_inside(&map, x, y)?;
    let (screen_x, screen_y) = image_util::image_to_screen(&map, f64::from(x), f64::from(y));
    Ok((round_screen(screen_x)?, round_screen(screen_y)?))
}

fn remap_region(context: &CallContext, region: Region) -> Result<Region, ComputerError> {
    let map = require_screenshot_map(context)?;
    ensure_region_inside(&map, region)?;
    let x2 = i64::from(region.x) + i64::from(region.width);
    let y2 = i64::from(region.y) + i64::from(region.height);
    let (screen_x1, screen_y1) =
        image_util::image_to_screen(&map, f64::from(region.x), f64::from(region.y));
    let (screen_x2, screen_y2) = image_util::image_to_screen(&map, x2 as f64, y2 as f64);
    let x = round_screen(screen_x1)?;
    let y = round_screen(screen_y1)?;
    let far_x = round_screen(screen_x2)?;
    let far_y = round_screen(screen_y2)?;
    let width = u32::try_from(i64::from(far_x) - i64::from(x))
        .unwrap_or(0)
        .max(1);
    let height = u32::try_from(i64::from(far_y) - i64::from(y))
        .unwrap_or(0)
        .max(1);
    Region::new(x, y, width, height)
}

fn require_screenshot_map(
    context: &CallContext,
) -> Result<image_util::ScreenshotMap, ComputerError> {
    match cache::load_screenshot_map(context.session.as_deref())? {
        Some(map) => Ok(map),
        None => Err(ComputerError::InvalidArguments(
            "take a screenshot first".into(),
        )),
    }
}

fn content_box(map: &image_util::ScreenshotMap) -> (i64, i64, i64, i64) {
    let left = i64::from(map.pad.x);
    let top = i64::from(map.pad.y);
    let right = left + i64::from(map.content_width);
    let bottom = top + i64::from(map.content_height);
    (left, top, right, bottom)
}

fn ensure_point_inside(
    map: &image_util::ScreenshotMap,
    x: i32,
    y: i32,
) -> Result<(), ComputerError> {
    let (left, top, right, bottom) = content_box(map);
    let x = i64::from(x);
    let y = i64::from(y);
    if x < left || y < top || x >= right || y >= bottom {
        return Err(ComputerError::InvalidArguments(format!(
            "coordinate ({x}, {y}) is outside the screenshot"
        )));
    }
    Ok(())
}

fn ensure_region_inside(
    map: &image_util::ScreenshotMap,
    region: Region,
) -> Result<(), ComputerError> {
    let (left, top, right, bottom) = content_box(map);
    let x1 = i64::from(region.x);
    let y1 = i64::from(region.y);
    let x2 = x1 + i64::from(region.width);
    let y2 = y1 + i64::from(region.height);
    if x1 < left || y1 < top || x1 >= right || y1 >= bottom || x2 > right || y2 > bottom {
        return Err(ComputerError::InvalidArguments(format!(
            "region {},{} {}x{} is outside the screenshot",
            region.x, region.y, region.width, region.height
        )));
    }
    Ok(())
}

fn round_screen(value: f64) -> Result<i32, ComputerError> {
    if !value.is_finite() {
        return Err(ComputerError::InvalidArguments(
            "coordinate remap was not finite".into(),
        ));
    }
    let rounded = value.round();
    if rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        return Err(ComputerError::InvalidArguments(
            "coordinate remap is out of range".into(),
        ));
    }
    Ok(rounded as i32)
}

fn run_batch(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    actions: Vec<Value>,
    stop_on_error: bool,
) -> Response {
    let mut results = Vec::with_capacity(actions.len());
    let mut first_error: Option<(usize, ErrorBody)> = None;
    for (index, step) in actions.into_iter().enumerate() {
        let response = dispatch_batch_step(backend, context, step);
        let failed = !response.ok;
        if failed && first_error.is_none() {
            if let Some(error) = response.error.clone() {
                first_error = Some((index, error));
            }
        }
        results.push(response.to_value());
        if failed && stop_on_error {
            return attach_post_state(
                backend,
                context,
                PostKind::Mutating,
                batch_response(results, Some(index), first_error),
            );
        }
    }
    attach_post_state(
        backend,
        context,
        PostKind::Mutating,
        batch_response(results, None, first_error),
    )
}

fn dispatch_batch_step(
    backend: &dyn DesktopBackend,
    outer: &CallContext,
    value: Value,
) -> Response {
    match prepare_batch_step(outer, value) {
        Ok((action, context)) => dispatch_with_context(backend, action, &context),
        Err(error) => Response::from_error(error),
    }
}

fn prepare_batch_step(
    outer: &CallContext,
    mut value: Value,
) -> Result<(Action, CallContext), ComputerError> {
    let Some(object) = value.as_object_mut() else {
        return Err(ComputerError::InvalidArguments(
            "batch step must be a JSON object".into(),
        ));
    };
    if object.get("action").and_then(Value::as_str) == Some("batch") {
        return Err(ComputerError::InvalidArguments(
            "nested batch is not allowed".into(),
        ));
    }
    let mut context = outer.clone();
    overlay_context(&mut context, object)?;
    context.return_state = false;
    context.include_screenshot = false;
    let action = serde_json::from_value(value)
        .map_err(|error| ComputerError::InvalidArguments(error.to_string()))?;
    Ok((action, context))
}

fn overlay_context(
    context: &mut CallContext,
    object: &mut Map<String, Value>,
) -> Result<(), ComputerError> {
    if object.contains_key("app") {
        context.app = take_stringish(object, "app")?;
    }
    if object.contains_key("window") {
        context.window = take_stringish(object, "window")?;
    }
    if object.contains_key("element") {
        context.element = take_optional_u32(object, "element")?;
    }
    if object.contains_key("diff") {
        context.diff = take_bool(object, "diff")?;
    }
    if object.contains_key("full") {
        context.full = take_bool(object, "full")?;
    }
    let _ = object.remove("return_state");
    let _ = object.remove("include_screenshot");
    let _ = object.remove("id");
    let _ = object.remove("stop_on_error");
    if object.contains_key("coord") {
        context.coord = take_stringish(object, "coord")?;
    }
    if object.contains_key("session") {
        context.session = take_stringish(object, "session")?;
    }
    if object.contains_key("tier") {
        context.tier = take_stringish(object, "tier")?;
    }
    if object.contains_key("adapter") {
        context.adapter = take_stringish(object, "adapter")?;
    }
    if object.contains_key("display") {
        context.display = take_stringish(object, "display")?;
    }
    if object.contains_key("require_visible") {
        context.require_visible = take_bool(object, "require_visible")?;
    }
    if object.contains_key("allow_foreground") {
        context.allow_foreground = take_bool(object, "allow_foreground")?;
    }
    if object.contains_key("require_background") {
        context.require_background = take_bool(object, "require_background")?;
    }
    if object.contains_key("no_overlay") {
        context.no_overlay = take_bool(object, "no_overlay")?;
    }
    Ok(())
}

fn batch_response(
    results: Vec<Value>,
    stopped_at: Option<usize>,
    first_error: Option<(usize, ErrorBody)>,
) -> Response {
    let mut fields = Map::new();
    fields.insert("action".into(), json!("batch"));
    fields.insert("results".into(), Value::Array(results));
    if let Some(index) = stopped_at {
        fields.insert("stopped_at".into(), json!(index));
    }
    match first_error {
        Some((index, error)) => {
            let mut response = Response::failure(
                error.code,
                format!("batch failed at action {index}: {}", error.message),
            );
            response.fields = fields;
            response
        }
        None => Response {
            ok: true,
            id: None,
            error: None,
            fields,
        },
    }
}

fn attach_post_state(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    kind: PostKind,
    mut response: Response,
) -> Response {
    if !context.return_state || matches!(kind, PostKind::ReadOnly) {
        return response;
    }
    let mut warnings = Vec::new();
    let mut state = match backend.tree(&tree_query(context)) {
        Ok(report) => json!({
            "app": report.app,
            "pid": report.pid,
            "window_id": report.window_id,
            "revision": report.revision,
            "tree": report.full_text,
            "diff": report.diff,
            "truncated": report.truncated,
        }),
        Err(error) => {
            if !response.ok {
                return response;
            }
            warnings.push(format!("tree snapshot failed: {error}"));
            json!({
                "app": null,
                "pid": null,
                "window_id": null,
                "revision": null,
                "tree": null,
                "diff": null,
                "truncated": null,
            })
        }
    };
    if context.include_screenshot {
        let shot = if response.ok && matches!(kind, PostKind::Screenshot) {
            Ok(Value::Object(response.fields.clone()))
        } else if matches!(kind, PostKind::Screenshot) {
            Err(ComputerError::ExecutionFailed(
                response
                    .error
                    .as_ref()
                    .map(|error| error.message.clone())
                    .unwrap_or_else(|| "screenshot failed".into()),
            ))
        } else {
            screenshot(
                backend,
                context,
                ScreenshotArgs {
                    output: None,
                    region: None,
                    format: None,
                    max_width: None,
                    canvas: None,
                    include_base64: false,
                },
            )
        };
        match shot {
            Ok(value) => state["screenshot"] = value,
            Err(error) => {
                state["screenshot"] = Value::Null;
                warnings.push(format!("screenshot failed: {error}"));
            }
        }
    }
    response.fields.insert("state".into(), state);
    push_warnings(&mut response, warnings);
    response
}

fn push_warnings(response: &mut Response, extra: Vec<String>) {
    if extra.is_empty() {
        return;
    }
    let mut warnings = match response.fields.remove("warnings") {
        Some(Value::Array(items)) => items,
        Some(other) => vec![other],
        None => Vec::new(),
    };
    for warning in extra {
        warnings.push(json!(warning));
    }
    response
        .fields
        .insert("warnings".into(), Value::Array(warnings));
}

fn cursor_position(backend: &dyn DesktopBackend) -> Result<Value, ComputerError> {
    let (x, y) = backend.cursor_position()?;
    Ok(json!({"action": "cursor_position", "x": x, "y": y}))
}

fn session_prune(dry_run: bool, ttl_secs: Option<u64>) -> Result<Value, ComputerError> {
    let ttl = match ttl_secs {
        Some(seconds) => Duration::from_secs(seconds),
        None => cache::DEFAULT_SESSION_TTL,
    };
    let report = cache::prune_sessions(
        &cache::sessions_root(),
        &cache::PruneOptions {
            dry_run,
            ttl,
            now: SystemTime::now(),
        },
        &cache::pid_alive,
    )?;
    Ok(json!({
        "action": "session_prune",
        "dry_run": dry_run,
        "ttl_secs": ttl.as_secs(),
        "removed": report.removed,
        "kept": report.kept,
    }))
}

struct ScreenshotArgs {
    output: Option<String>,
    region: Option<Region>,
    format: Option<ImageFormat>,
    max_width: Option<u32>,
    canvas: Option<String>,
    include_base64: bool,
}

fn screenshot(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    args: ScreenshotArgs,
) -> Result<Value, ComputerError> {
    let ScreenshotArgs {
        output,
        region,
        format,
        max_width,
        canvas,
        include_base64,
    } = args;
    if region.is_some() && (context.app.is_some() || context.window.is_some()) {
        return Err(ComputerError::InvalidArguments(
            "pass a region or a window target, not both".into(),
        ));
    }
    let format = format.unwrap_or_default();
    let max_width = max_width.unwrap_or(image_util::DEFAULT_SCREENSHOT_MAX_WIDTH);
    let canvas = canvas
        .as_deref()
        .map(image_util::parse_canvas)
        .transpose()?;
    let raw = if context.app.is_some() || context.window.is_some() {
        if let Some(error) = backend.display_block() {
            return Err(error);
        }
        let target = resolve_screenshot_window(backend, context)?;
        backend.capture_window(&target)?
    } else {
        backend.capture_screen(region)?
    };
    let prepared = image_util::prepare_shot(
        raw.frame.image(),
        image_util::ShotFit {
            scale: raw.frame.scale(),
            origin_x: raw.origin_x,
            origin_y: raw.origin_y,
            logical_width: raw.logical_width,
            logical_height: raw.logical_height,
            max_width,
            canvas,
        },
    )?;
    let encoded = image_util::encode_rgba(&prepared.image, format)?;
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
    let mut warnings = raw.warnings;
    if let Err(error) = cache::store_screenshot_map(context.session.as_deref(), &prepared.map) {
        warnings.push(format!("screenshot map was not cached: {error}"));
    }
    let mut body = serde_json::to_value(&prepared.map).map_err(|error| {
        ComputerError::ExecutionFailed(format!("screenshot map encode failed: {error}"))
    })?;
    body["path"] = json!(path_string(&path));
    body["format"] = json!(format.as_str());
    body["capture_source"] = json!(raw.capture_source);
    if raw.occluded {
        body["occluded"] = json!(true);
    }
    if !warnings.is_empty() {
        body["warnings"] = json!(warnings);
    }
    if include_base64 {
        body["data"] = json!(encode_base64(&encoded.bytes));
    }
    Ok(body)
}

fn resolve_screenshot_window(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<WindowTarget, ComputerError> {
    let listed = backend.list_windows()?;
    if let Some(app) = context.app.as_deref() {
        let apps = backend.list_apps(true)?;
        let selector = parse_app_selector(app)?;
        let app = resolve_app(&apps, &selector)?;
        let owned = filter_windows_for_pid(&listed.windows, app.pid);
        let selector = match context.window.as_deref() {
            Some(window) => parse_window_selector(window)?,
            None => WindowSelector::Index(0),
        };
        let window = resolve_window(&owned, &selector)?;
        return Ok(window_target(window));
    }
    let selector = parse_window_selector(context.window.as_deref().unwrap_or(""))?;
    let window = resolve_window(&listed.windows, &selector)?;
    Ok(window_target(window))
}

fn window_target(window: &crate::target::WindowInfo) -> WindowTarget {
    WindowTarget {
        window_id: window.id,
        pid: window.pid,
        bounds: window.bounds,
    }
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
    context: &CallContext,
    x: Option<i32>,
    y: Option<i32>,
    button: MouseButton,
    double: bool,
) -> Response {
    match (context.element, x, y) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            Response::failure("invalid_arguments", "pass element or x and y, not both")
        }
        (None, None, None) => Response::failure(
            "invalid_arguments",
            "click requires element, or both x and y",
        ),
        (None, Some(_), None) | (None, None, Some(_)) => {
            Response::failure("invalid_arguments", "x and y must be passed together")
        }
        (Some(_index), None, None) => {
            let count: u8 = if double { 2 } else { 1 };
            element_command(backend, context, ElementOp::Press { count })
        }
        (None, Some(x), Some(y)) => coordinate_click(backend, context, x, y, button, double),
    }
}

fn coordinate_click(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    x: i32,
    y: i32,
    button: MouseButton,
    double: bool,
) -> Response {
    let kind = InputKind::Click {
        x,
        y,
        button,
        count: if double { 2 } else { 1 },
    };
    // Tier rejection, including synthetic without the feature, happens before hit-testing.
    if let Err(error) = planned_tiers(backend, context) {
        return Response::from_error(error);
    }
    let mut warnings = Vec::new();
    let hit = match backend.hit_test(x, y) {
        Ok(hit) => hit,
        Err(error) => {
            warnings.push(format!("hit test failed: {error}"));
            None
        }
    };
    if let Some(title) = hit.as_ref().and_then(|hit| hit.title.as_deref()) {
        if policy::sensitive_title(title) {
            if let Err(error) = confirm_title(context, "click", "sensitive control title") {
                return Response::from_error(error);
            }
        }
    }
    match run_attempts(backend, context, kind) {
        Ok(report) => {
            let body = json!({
                "action": if double { "double_click" } else { "click" },
                "x": x,
                "y": y,
                "button": button.as_str(),
                "double": double,
                "hit": hit_value(hit),
            });
            Response::success(finish_input(body, report, warnings))
        }
        Err(error) => Response::from_error(error),
    }
}

fn hit_value(hit: Option<crate::element::HitInfo>) -> Value {
    match hit {
        Some(hit) => json!({"role": hit.role, "title": hit.title}),
        None => Value::Null,
    }
}

fn type_command(backend: &dyn DesktopBackend, context: &CallContext, text: String) -> Response {
    if context.element.is_some() {
        return element_command(backend, context, ElementOp::Type { text });
    }
    if text.is_empty() {
        return Response::from_error(ComputerError::InvalidArguments(
            "text must not be empty".into(),
        ));
    }
    if text.contains('\0') {
        return Response::from_error(ComputerError::InvalidArguments(
            "text must not contain a null byte".into(),
        ));
    }
    let chars = text.chars().count();
    match run_attempts(backend, context, InputKind::Type { text: text.clone() }) {
        Ok(report) => {
            let body = json!({
                "action": "type",
                "text": text,
                "chars": chars,
            });
            Response::success(finish_input(body, report, Vec::new()))
        }
        Err(error) => Response::from_error(error),
    }
}

fn element_command(backend: &dyn DesktopBackend, context: &CallContext, op: ElementOp) -> Response {
    if let Err(error) = precheck(&op) {
        return Response::from_error(error);
    }
    if let Err(error) = confirm_element_title(backend, context, &op) {
        return Response::from_error(error);
    }
    let index = match context.element {
        Some(index) => index,
        None => {
            return Response::from_error(ComputerError::InvalidArguments(
                "element is required".into(),
            ))
        }
    };
    let request = ElementRequest {
        op,
        index,
        require_visible: context.require_visible,
        app: context.app.clone(),
        window: context.window.clone(),
        session: context.session.clone(),
    };
    match backend.element_action(&request) {
        Ok(outcome) => Response::success(outcome_value(&outcome)),
        Err(error) => element_error_response(error),
    }
}

fn confirm_element_title(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    op: &ElementOp,
) -> Result<(), ComputerError> {
    let press = matches!(op, ElementOp::Press { .. })
        || matches!(op, ElementOp::Perform { name } if name.eq_ignore_ascii_case("AXPress"));
    if !press {
        return Ok(());
    }
    let Some(index) = context.element else {
        return Ok(());
    };
    let Ok(Some(peek)) = backend.peek_element(
        context.app.as_deref(),
        context.window.as_deref(),
        context.session.as_deref(),
        index,
    ) else {
        return Ok(());
    };
    if peek.title.as_deref().is_some_and(policy::sensitive_title) {
        confirm_title(context, "press", "sensitive control title")?;
    }
    Ok(())
}

fn confirm_title(context: &CallContext, action: &str, reason: &str) -> Result<(), ComputerError> {
    let loaded = policy::load_policy(context.policy.as_deref().map(Path::new))?;
    let command = policy::confirm_program(&loaded, context.confirm_cmd.as_deref());
    policy::run_confirm(
        command.as_deref(),
        loaded.timeout_secs(),
        &policy::ConfirmDocument {
            action,
            bundle_id: None,
            path: None,
            pid: None,
            window_id: None,
            element: context.element,
            reason,
        },
    )
}

fn precheck(op: &ElementOp) -> Result<(), ComputerError> {
    match op {
        ElementOp::Press { count } => element::validate_count(*count),
        ElementOp::SetValue { value } => element::validate_text(value, true),
        ElementOp::Type { text } => element::validate_text(text, false),
        ElementOp::SelectText { start, end } => element::validate_range(*start, *end, None),
        ElementOp::Perform { name } => element::validate_perform_name(name),
        ElementOp::Focus | ElementOp::ScrollTo => Ok(()),
    }
}

fn outcome_value(outcome: &ElementOutcome) -> Value {
    let mut body = json!({
        "action": outcome.action,
        "element": outcome.element,
        "revision": outcome.revision,
        "tier": outcome.tier,
        "cursor_moved": outcome.cursor_moved,
    });
    if let Some(restored) = outcome.focus_restored {
        body["focus_restored"] = json!(restored);
    }
    if !outcome.warnings.is_empty() {
        body["warnings"] = json!(outcome.warnings);
    }
    if let Some(via) = &outcome.via {
        body["via"] = json!(via);
    }
    if let Some(name) = &outcome.name {
        body["name"] = json!(name);
    }
    if let Some(count) = outcome.count {
        body["count"] = json!(count);
    }
    body
}

fn element_error_response(error: ElementError) -> Response {
    let mut response = Response::from_error(error.error);
    if let Some(tree) = error.tree {
        response.fields.insert("tree".into(), json!(tree));
    }
    response
}

fn move_mouse(backend: &dyn DesktopBackend, context: &CallContext, x: i32, y: i32) -> Response {
    match run_attempts(backend, context, InputKind::Move { x, y }) {
        Ok(report) => Response::success(finish_input(
            json!({"action": "move", "x": x, "y": y}),
            report,
            Vec::new(),
        )),
        Err(error) => Response::from_error(error),
    }
}

fn press_key(backend: &dyn DesktopBackend, context: &CallContext, keys: &str) -> Response {
    let combo = match parse_key_combo(keys) {
        Ok(combo) => combo,
        Err(error) => return Response::from_error(error),
    };
    if let Err(error) = validate_key(&combo.key) {
        return Response::from_error(error);
    }
    let modifiers: Vec<&str> = combo
        .modifiers
        .iter()
        .copied()
        .map(Modifier::as_str)
        .collect();
    let body = json!({
        "action": "key",
        "keys": keys,
        "modifiers": modifiers,
        "key": combo.key,
    });
    match run_attempts(
        backend,
        context,
        InputKind::Key {
            modifiers: combo.modifiers,
            key: combo.key,
        },
    ) {
        Ok(report) => Response::success(finish_input(body, report, Vec::new())),
        Err(error) => Response::from_error(error),
    }
}

fn scroll(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    point: (Option<i32>, Option<i32>),
    delta: (Option<i32>, Option<i32>),
    direction: Option<ScrollDirection>,
    amount: Option<i32>,
) -> Response {
    let (x, y) = point;
    let (dx, dy) = delta;
    if x.is_some() != y.is_some() {
        return Response::from_error(ComputerError::InvalidArguments(
            "scroll x and y must be set together".into(),
        ));
    }
    let (scroll_dx, scroll_dy) = match resolve_scroll(dx, dy, direction, amount) {
        Ok(delta) => delta,
        Err(error) => return Response::from_error(error),
    };
    match run_attempts(
        backend,
        context,
        InputKind::Scroll {
            x,
            y,
            dx: scroll_dx,
            dy: scroll_dy,
        },
    ) {
        Ok(report) => Response::success(finish_input(
            json!({
                "action": "scroll",
                "x": x,
                "y": y,
                "dx": scroll_dx,
                "dy": scroll_dy,
            }),
            report,
            Vec::new(),
        )),
        Err(error) => Response::from_error(error),
    }
}

fn planned_tiers(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<Vec<TierName>, ComputerError> {
    let requested = crate::tier::parse_tier(context.tier.as_deref())?;
    crate::tier::plan(
        requested,
        crate::tier::InputClass::Background,
        backend.input_support(),
        crate::tier::skylight_compiled(),
    )
}

fn run_attempts(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    kind: InputKind,
) -> Result<crate::tier::InputReport, ComputerError> {
    let attempts = planned_tiers(backend, context)?;
    let mut event_error: Option<ComputerError> = None;
    for (index, tier) in attempts.iter().copied().enumerate() {
        if tier == TierName::Activate {
            match activate_block(context)? {
                Some(blocked) if index > 0 => {
                    return Err(event_error.unwrap_or(blocked));
                }
                Some(blocked) => return Err(blocked),
                None => {}
            }
        }
        let job = InputJob {
            tier,
            kind: kind.clone(),
            app: context.app.clone(),
            window: context.window.clone(),
            allow_foreground: context.allow_foreground,
            require_background: context.require_background,
        };
        match backend.perform_input(&job) {
            Ok(mut report) => {
                if !context.no_overlay {
                    if let Some((x, y)) = crate::tier::input_point(&kind) {
                        let session = context
                            .session
                            .clone()
                            .unwrap_or_else(|| std::process::id().to_string());
                        if let Some(warning) = crate::overlay::note_point(x, y, &session) {
                            report.warnings.push(warning.to_string());
                        }
                    }
                }
                return Ok(report);
            }
            Err(error) => {
                let next_is_activate = attempts.get(index + 1) == Some(&TierName::Activate);
                if tier == TierName::Event && error.code() == "execution_failed" && next_is_activate
                {
                    let blocked = activate_block(context)?;
                    let lock_held = blocked.as_ref().is_some_and(is_lock_error);
                    if crate::tier::fallback_to_activate(lock_held, context.require_background)
                        && blocked.is_none()
                    {
                        event_error = Some(error);
                        continue;
                    }
                }
                return Err(error);
            }
        }
    }
    Err(ComputerError::ExecutionFailed("no input tier ran".into()))
}

fn activate_block(context: &CallContext) -> Result<Option<ComputerError>, ComputerError> {
    if context.require_background {
        return Ok(Some(ComputerError::ExecutionFailed(
            "--require-background refuses tier activate because it uses the foreground cursor"
                .into(),
        )));
    }
    if context.allow_foreground {
        return Ok(None);
    }
    let Some(path) = crate::foreground::lock_path() else {
        return Ok(None);
    };
    match crate::foreground::holder_if_taken(&path) {
        Ok(None) => Ok(None),
        Ok(Some(pid)) if pid > 0 => Ok(Some(ComputerError::ExecutionFailed(format!(
            "foreground.lock is held by pid {pid}"
        )))),
        Ok(Some(_)) => Ok(Some(ComputerError::ExecutionFailed(
            "foreground.lock is held by another session".into(),
        ))),
        Err(error) => Err(error.into_computer()),
    }
}

fn is_lock_error(error: &ComputerError) -> bool {
    error.to_string().contains("foreground.lock")
}

fn finish_input(
    mut body: Value,
    report: crate::tier::InputReport,
    mut warnings: Vec<String>,
) -> Value {
    warnings.extend(report.warnings);
    body["tier"] = json!(report.tier);
    body["cursor_moved"] = json!(report.cursor_moved);
    if let Some(restored) = report.focus_restored {
        body["focus_restored"] = json!(restored);
    }
    if !warnings.is_empty() {
        body["warnings"] = json!(warnings);
    }
    body
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
    let mut body = json!({
        "action": "session_end",
        "restored": restored,
    });
    if let Err(error) = cache::prune_empty_sessions() {
        body["warnings"] = json!([format!("session prune failed: {error}")]);
    }
    Ok(body)
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

fn quit(
    backend: &dyn DesktopBackend,
    app: Option<&str>,
    force: bool,
) -> Result<Value, ComputerError> {
    let app = require_app(app)?;
    let result = backend.quit(app, force)?;
    Ok(json!({
        "action": "quit",
        "name": result.name,
        "bundle_id": result.bundle_id,
        "pid": result.pid,
        "accepted": result.accepted,
        "force": force,
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
                    (Some(10), Some(20), MouseButton::Right, true)
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
        let bare: Action = serde_json::from_str(r#"{"action":"click"}"#).unwrap();
        assert!(matches!(
            bare,
            Action::Click {
                x: None,
                y: None,
                ..
            }
        ));
        let press: Action = serde_json::from_str(r#"{"action":"press"}"#).unwrap();
        assert!(matches!(press, Action::Press { count: 1 }));
        let renamed: Action =
            serde_json::from_str(r#"{"action":"set-value","value":"hi"}"#).unwrap();
        assert!(matches!(renamed, Action::SetValue { .. }));
        let scrolled: Action = serde_json::from_str(r#"{"action":"scroll-to-visible"}"#).unwrap();
        assert!(matches!(scrolled, Action::ScrollTo));
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
                x: Some(3),
                y: Some(4),
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
        assert_eq!(backend.moves.load(Ordering::SeqCst), 1);
        assert_eq!(response.to_value()["tier"], "event");
        assert_eq!(response.to_value()["cursor_moved"], false);

        let response = dispatch_with(&backend, Action::Displays);
        assert_eq!(response.to_value()["displays"][0]["name"], "Stub");

        let response = dispatch_with(&backend, Action::GetState);
        assert_eq!(response.to_value()["cursor"]["x"], 4);
        assert!(response.ok);

        let _cache = crate::cache::CacheEnv::acquire();
        let shot = dispatch_with(
            &backend,
            Action::Screenshot {
                output: None,
                region: None,
                format: Some(ImageFormat::Png),
                max_width: Some(1),
                canvas: None,
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
        assert_eq!(value["capture_source"], "stub");
        assert_eq!(value["origin"]["x"], 0);
        assert_eq!(value["origin"]["y"], 0);
        assert_eq!(value["fit"], "width");
        assert!(value["map"]["image_to_screen"].is_string());
        let _ = fs::remove_file(path);
        if let Ok(dir) = crate::cache::session_dir(None) {
            let _ = fs::remove_file(dir.join("screenshot-map.json"));
        }

        let failed = StubBackend {
            fail_click: true,
            ..StubBackend::default()
        };
        let response = dispatch_with(
            &failed,
            Action::Click {
                x: Some(0),
                y: Some(0),
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

    fn shot(max_width: Option<u32>, canvas: Option<&str>) -> Action {
        Action::Screenshot {
            output: None,
            region: None,
            format: Some(ImageFormat::Png),
            max_width,
            canvas: canvas.map(str::to_string),
            base64: false,
        }
    }

    #[test]
    fn dispatch_applies_default_max_width_and_keeps_explicit_zero() {
        let _cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend {
            capture_pixels: Some((1600, 1000)),
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &backend,
            shot(None, None),
            &CallContext {
                session: Some("p4-default-width".into()),
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["width"], 1440);
        assert_eq!(value["height"], 900);
        assert_eq!(value["scale"], 2.0);
        assert_eq!(value["logical_width"], 800);
        assert_eq!(value["logical_height"], 500);
        assert_eq!(value["fit"], "width");
        assert_eq!(value["capture_source"], "stub");
        assert_eq!(value["content_width"], 1440);
        let path = value["path"].as_str().unwrap();
        let _ = fs::remove_file(path);
        let map = crate::cache::load_screenshot_map(Some("p4-default-width"))
            .unwrap()
            .expect("map");
        assert_eq!(map.width, 1440);
        let _ = fs::remove_dir_all(crate::cache::session_dir(Some("p4-default-width")).unwrap());

        let full = dispatch_with_context(
            &backend,
            shot(Some(0), None),
            &CallContext {
                session: Some("p4-zero-width".into()),
                ..CallContext::default()
            },
        );
        assert!(full.ok, "{full:?}");
        assert_eq!(full.to_value()["width"], 1600);
        assert_eq!(full.to_value()["height"], 1000);
        assert_eq!(full.to_value()["fit"], "none");
        let path = full.to_value()["path"].as_str().unwrap().to_string();
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir_all(crate::cache::session_dir(Some("p4-zero-width")).unwrap());
    }

    #[test]
    fn dispatch_window_capture_records_origin_and_faults() {
        let _cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            shot(Some(0), None),
            &CallContext {
                app: Some("Notes".into()),
                window: Some("0".into()),
                session: Some("p4-window".into()),
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["capture_source"], "stub");
        assert_eq!(value["origin"]["x"], 10);
        assert_eq!(value["origin"]["y"], 20);
        assert_eq!(value["logical_width"], 400);
        assert_eq!(value["logical_height"], 300);
        assert_eq!(value["width"], 800);
        assert_eq!(value["fit"], "none");
        let path = value["path"].as_str().unwrap();
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir_all(crate::cache::session_dir(Some("p4-window")).unwrap());

        let denied = StubBackend {
            capture_fault: crate::test_backend::StubCaptureFault::Permission,
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &denied,
            shot(Some(0), None),
            &CallContext {
                app: Some("Notes".into()),
                ..CallContext::default()
            },
        );
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "permission_required");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Screen Recording"));

        let missing = StubBackend {
            capture_fault: crate::test_backend::StubCaptureFault::MissingWindow,
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &missing,
            shot(Some(0), None),
            &CallContext {
                window: Some("id:100".into()),
                ..CallContext::default()
            },
        );
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "window_not_found");

        let both = dispatch_with_context(
            &backend,
            Action::Screenshot {
                output: None,
                region: Some(Region::new(0, 0, 8, 9).unwrap()),
                format: None,
                max_width: Some(0),
                canvas: None,
                base64: false,
            },
            &CallContext {
                app: Some("Notes".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(both.to_value()["error"]["code"], "invalid_arguments");

        let locked = StubBackend {
            block_display: true,
            windows: Vec::new(),
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &locked,
            shot(Some(0), None),
            &CallContext {
                app: Some("Notes".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(response.to_value()["error"]["code"], "display_unavailable");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("locked"));
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
            "return_state": true,
            "require_visible": true
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
        assert!(request.context.require_visible);
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

        let response =
            dispatch_with_context(&backend, Action::Quit { force: false }, &ctx("Terminal"));
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
        assert!(backend.quit.lock().unwrap().is_empty());

        let response = dispatch_with_context(&backend, Action::Quit { force: false }, &ctx("412"));
        assert_eq!(response.to_value()["error"]["code"], "confirm_required");
        assert!(backend.quit.lock().unwrap().is_empty());

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

        let _cache = crate::cache::CacheEnv::acquire();
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
        let response = dispatch_with_context(
            &backend,
            Action::Quit { force: false },
            &ctx(&self_pid.to_string()),
        );
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
    }

    fn notes_button() -> crate::tree::UiNode {
        let mut button = crate::tree::UiNode::bare("AXButton");
        button.title = Some("Save".into());
        button.actions = vec!["AXPress".into(), "AXShowMenu".into()];
        button.frame = Some(crate::tree::NodeFrame {
            x: 12.0,
            y: 10.0,
            width: 80.0,
            height: 24.0,
        });
        let mut root = crate::tree::UiNode::bare("AXWindow");
        root.title = Some("Notes".into());
        root.frame = Some(crate::tree::NodeFrame {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        });
        root.children = vec![button];
        root
    }

    fn element_context(action_element: u32) -> CallContext {
        CallContext {
            app: Some("Notes".into()),
            element: Some(action_element),
            ..CallContext::default()
        }
    }

    #[test]
    fn element_json_shape_revalidation_and_hit_test() {
        let backend = StubBackend::default();
        backend.seed_element(notes_button());
        let request = parse_request(json!({
            "action": "press",
            "app": "Notes",
            "element": 1,
            "count": 1
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["action"], "press");
        assert_eq!(value["element"], 1);
        assert_eq!(value["tier"], "ax");
        assert_eq!(value["count"], 1);
        assert!(value["revision"].is_number());
        assert!(value.get("warnings").is_none());

        let request = parse_request(json!({
            "action": "perform-action",
            "app": "Notes",
            "element": 1,
            "name": "AXShowMenu"
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["action"], "perform_action");
        assert_eq!(response.to_value()["name"], "AXShowMenu");
        assert_eq!(response.to_value()["tier"], "ax");

        let before = backend.performs.lock().unwrap().len();
        let request = parse_request(json!({
            "action": "perform_action",
            "app": "Notes",
            "element": 1,
            "name": "AXDelete"
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert_eq!(response.to_value()["error"]["code"], "invalid_arguments");
        assert_eq!(backend.performs.lock().unwrap().len(), before);

        let request = parse_request(json!({
            "action": "set_value",
            "app": "Notes",
            "element": 1,
            "value": "hello"
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["action"], "set_value");

        let request = parse_request(json!({
            "action": "select_text",
            "app": "Notes",
            "element": 1,
            "start": 0,
            "end": 5
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["action"], "select_text");

        let request = parse_request(json!({
            "action": "scroll_to_visible",
            "app": "Notes",
            "element": 1
        }))
        .unwrap();
        assert!(matches!(request.action, Action::ScrollTo));
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("AXScrollToVisible"));

        let clicks = backend.clicks.load(Ordering::SeqCst);
        let request = parse_request(json!({
            "action": "click",
            "app": "Notes",
            "element": 1,
            "double": true
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["action"], "press");
        assert_eq!(response.to_value()["count"], 2);
        assert_eq!(backend.clicks.load(Ordering::SeqCst), clicks);

        *backend.hit.lock().unwrap() = Some(crate::element::HitInfo {
            role: "AXButton".into(),
            title: Some("Save".into()),
        });
        let response = dispatch_with(
            &backend,
            Action::Click {
                x: Some(12),
                y: Some(10),
                button: MouseButton::Left,
                double: false,
            },
        );
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["hit"]["role"], "AXButton");
        assert_eq!(response.to_value()["hit"]["title"], "Save");
        assert_eq!(backend.clicks.load(Ordering::SeqCst), clicks + 1);

        let mut changed = notes_button();
        changed.children[0].role = "AXTextArea".into();
        backend.set_live(changed);
        let response = dispatch_with_context(&backend, Action::Focus, &element_context(1));
        assert_eq!(response.to_value()["error"]["code"], "stale_element");
        assert!(response.to_value()["tree"]
            .as_str()
            .unwrap()
            .contains("AXTextArea"));

        let empty = StubBackend::default();
        let response = dispatch_with_context(&empty, Action::Focus, &element_context(0));
        assert_eq!(response.to_value()["error"]["code"], "element_not_found");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("refresh indices"));

        let bare = dispatch_with(
            &empty,
            Action::Click {
                x: None,
                y: None,
                button: MouseButton::Left,
                double: false,
            },
        );
        assert_eq!(bare.to_value()["error"]["code"], "invalid_arguments");
    }

    #[test]
    fn element_action_does_not_replace_the_tree_diff_baseline() {
        let backend = StubBackend::default();
        let first = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert!(first.ok, "{first:?}");
        let key = (String::new(), 10, 100);
        let baseline = backend.baselines.lock().unwrap().get(&key).cloned();
        let response = dispatch_with_context(&backend, Action::Focus, &element_context(0));
        assert!(response.ok, "{response:?}");
        assert_eq!(
            backend.baselines.lock().unwrap().get(&key),
            baseline.as_ref()
        );
        *backend.stub_value.lock().unwrap() = "ab".into();
        let diff = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        let text = diff.to_value()["tree"].as_str().unwrap().to_string();
        assert!(
            text.contains("~ [1] AXTextArea value: \"a\" -> \"ab\""),
            "{text}"
        );
    }

    fn sample_map() -> crate::image_util::ScreenshotMap {
        crate::image_util::ScreenshotMap {
            width: 1440,
            height: 900,
            scale: 2.0,
            logical_width: 800,
            logical_height: 500,
            origin: crate::image_util::MapPoint { x: 120, y: 40 },
            fit: "width".into(),
            pad: crate::image_util::MapPoint { x: 0, y: 0 },
            content_width: 1440,
            content_height: 900,
            map: crate::image_util::MapFormulas {
                image_to_screen:
                    "screen = origin + (image - pad) * logical_size / image_content_size".into(),
                screen_to_image:
                    "image = pad + (screen - origin) * image_content_size / logical_size".into(),
            },
        }
    }

    #[test]
    fn image_coordinate_on_a_stored_map_becomes_the_screen_point() {
        let cache = crate::cache::CacheEnv::acquire();
        assert!(cache.dir().is_dir());
        let map = sample_map();
        cache::store_screenshot_map(Some("p5-map"), &map).unwrap();
        let backend = StubBackend::default();
        let context = CallContext {
            coord: Some("image".into()),
            session: Some("p5-map".into()),
            ..CallContext::default()
        };
        let response = dispatch_with_context(&backend, Action::Move { x: 100, y: 50 }, &context);
        assert!(response.ok, "{response:?}");
        let (screen_x, screen_y) = image_util::image_to_screen(&map, 100.0, 50.0);
        let value = response.to_value();
        assert_eq!(value["x"], json!(screen_x.round() as i64));
        assert_eq!(value["y"], json!(screen_y.round() as i64));
        assert_eq!(value["x"], 176);
        assert_eq!(value["y"], 68);
        assert_eq!(backend.moves.load(Ordering::SeqCst), 1);

        let outside = dispatch_with_context(&backend, Action::Move { x: 1440, y: 50 }, &context);
        assert_eq!(outside.to_value()["error"]["code"], "invalid_arguments");
        assert!(outside.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside"));
        assert_eq!(backend.moves.load(Ordering::SeqCst), 1);

        let missing = dispatch_with_context(
            &backend,
            Action::Move { x: 1, y: 2 },
            &CallContext {
                coord: Some("image".into()),
                session: Some("no-map".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(missing.to_value()["error"]["code"], "invalid_arguments");
        assert!(missing.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("take a screenshot first"));

        let action =
            crate::adapters::translate("openai", &json!({"type": "move", "x": 100, "y": 50}))
                .unwrap();
        let adapted = dispatch_with_context(
            &backend,
            action,
            &CallContext {
                coord: Some("image".into()),
                session: Some("p5-map".into()),
                adapter: Some("openai".into()),
                ..CallContext::default()
            },
        );
        assert!(adapted.ok, "{adapted:?}");
        assert_eq!(adapted.to_value()["adapter"], "openai");
        assert_eq!(adapted.to_value()["x"], 176);
        assert_eq!(adapted.to_value()["y"], 68);
        assert!(adapted.to_value().get("state").is_none());

        let missing_adapter = handle_request_line_with(
            r#"{"adapter":"anthropic","action":"mouse_move","coordinate":[1,2],"session":"no-map"}"#,
            None,
        );
        assert!(!missing_adapter.ok);
        assert_eq!(
            missing_adapter.to_value()["error"]["code"],
            "invalid_arguments"
        );
        assert!(missing_adapter.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("take a screenshot first"));
        assert_eq!(missing_adapter.to_value()["adapter"], "anthropic");

        let waited = handle_request_line_with(r#"{"adapter":"openai","type":"wait"}"#, None);
        assert!(waited.ok, "{waited:?}");
        assert_eq!(waited.to_value()["adapter"], "openai");
        assert_eq!(waited.to_value()["waited_ms"], 0);
    }

    #[test]
    fn return_state_includes_the_numbered_tree_and_ignores_read_only() {
        let backend = StubBackend::default();
        let before = backend.accessibility_apps.lock().unwrap().len();
        let response = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(3),
                y: Some(4),
                button: MouseButton::Left,
                double: false,
            },
            &CallContext {
                app: Some("Notes".into()),
                return_state: true,
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["action"], "click");
        let tree = value["state"]["tree"].as_str().unwrap();
        assert!(tree.contains("[0] AXWindow"), "{tree}");
        assert!(tree.contains("AXTextArea"), "{tree}");
        assert!(value["state"]["diff"].is_null());
        assert_eq!(value["state"]["app"], "com.apple.Notes");
        assert_eq!(value["state"]["pid"], 10);
        assert_eq!(value["state"]["window_id"], 100);
        assert!(value.get("warnings").is_none());
        assert!(backend.accessibility_apps.lock().unwrap().len() > before);

        let displays = dispatch_with_context(
            &backend,
            Action::Displays,
            &CallContext {
                return_state: true,
                include_screenshot: true,
                ..CallContext::default()
            },
        );
        assert!(displays.ok, "{displays:?}");
        assert!(displays.to_value().get("state").is_none());

        let cursor = dispatch_with(&backend, Action::CursorPosition);
        assert_eq!(cursor.to_value()["x"], 4);
        assert_eq!(cursor.to_value()["y"], 5);
        assert!(cursor.to_value().get("adapter").is_none());
    }

    #[test]
    fn return_state_refreshes_the_tree_diff_baseline() {
        let backend = StubBackend::default();
        let first = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert!(first.ok, "{first:?}");
        *backend.stub_value.lock().unwrap() = "ab".into();
        let mut context = element_context(0);
        context.return_state = true;
        let response = dispatch_with_context(&backend, Action::Focus, &context);
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        let diff = value["state"]["diff"].as_str().unwrap();
        assert!(
            diff.contains("~ [1] AXTextArea value: \"a\" -> \"ab\""),
            "{diff}"
        );
        assert!(value["state"]["tree"]
            .as_str()
            .unwrap()
            .contains("value=\"ab\""));
        *backend.stub_value.lock().unwrap() = "abc".into();
        let next = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        let value = next.to_value();
        let text = value["tree"].as_str().unwrap();
        assert!(
            text.contains("~ [1] AXTextArea value: \"ab\" -> \"abc\""),
            "{text}"
        );
        assert!(!text.contains("\"a\" -> \"abc\""), "{text}");
    }

    #[test]
    fn include_screenshot_failure_keeps_the_action_result() {
        let backend = StubBackend {
            capture_fault: crate::test_backend::StubCaptureFault::Permission,
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(1),
                y: Some(2),
                button: MouseButton::Left,
                double: false,
            },
            &CallContext {
                app: Some("Notes".into()),
                return_state: true,
                include_screenshot: true,
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["action"], "click");
        assert_eq!(value["x"], 1);
        assert_eq!(value["y"], 2);
        assert!(value["state"]["tree"]
            .as_str()
            .unwrap()
            .contains("AXWindow"));
        assert!(value["state"]["screenshot"].is_null());
        let warnings = value["warnings"].as_array().unwrap();
        assert!(warnings
            .iter()
            .any(|item| { item.as_str().unwrap().contains("screenshot failed") }));
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 1);
    }

    fn batch_clicks(stop_on_error: bool) -> Value {
        json!({
            "action": "batch",
            "stop_on_error": stop_on_error,
            "actions": [
                {"action": "click", "x": 1, "y": 2},
                {"action": "click", "x": 3, "y": 4}
            ]
        })
    }

    #[test]
    fn batch_stop_on_error_controls_the_second_attempt() {
        let stopped = StubBackend {
            fail_click: true,
            ..StubBackend::default()
        };
        let request = parse_request(batch_clicks(true)).unwrap();
        let response = dispatch_with_context(&stopped, request.action, &request.context);
        assert!(!response.ok);
        let value = response.to_value();
        assert_eq!(value["error"]["code"], "execution_failed");
        assert_eq!(value["stopped_at"], 0);
        assert_eq!(value["results"].as_array().unwrap().len(), 1);
        assert!(!value["results"][0]["ok"].as_bool().unwrap());
        assert_eq!(stopped.click_attempts.load(Ordering::SeqCst), 2);
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("batch failed at action 0"));

        let both = StubBackend {
            fail_click: true,
            ..StubBackend::default()
        };
        let request = parse_request(batch_clicks(false)).unwrap();
        let response = dispatch_with_context(&both, request.action, &request.context);
        assert!(!response.ok);
        let value = response.to_value();
        assert!(value.get("stopped_at").is_none(), "{value}");
        assert_eq!(value["results"].as_array().unwrap().len(), 2);
        assert!(!value["results"][1]["ok"].as_bool().unwrap());
        assert_eq!(both.click_attempts.load(Ordering::SeqCst), 4);

        let nested = parse_request(json!({
            "action": "batch",
            "actions": [{"action": "batch", "actions": []}]
        }))
        .unwrap();
        let response = dispatch_with(&StubBackend::default(), nested.action);
        assert_eq!(response.to_value()["error"]["code"], "invalid_arguments");
        assert_eq!(response.to_value()["stopped_at"], 0);
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nested batch"));

        let backend = StubBackend::default();
        backend.seed_element(notes_button());
        let request = parse_request(json!({
            "action": "batch",
            "app": "Notes",
            "actions": [
                {"action": "press", "element": 1},
                {"action": "focus", "element": 1}
            ]
        }))
        .unwrap();
        assert!(matches!(
            request.action,
            Action::Batch {
                stop_on_error: true,
                ..
            }
        ));
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        let results = response.to_value()["results"].clone();
        assert_eq!(results[0]["action"], "press");
        assert_eq!(results[0]["element"], 1);
        assert_eq!(results[1]["action"], "focus");
        assert!(response.to_value().get("state").is_none());
    }

    #[test]
    fn session_prune_action_removes_an_empty_temp_session() {
        let _cache = crate::cache::CacheEnv::acquire();
        let dir = cache::session_dir(Some("empty-session")).unwrap();
        assert!(dir.join("owner.json").is_file());
        let response = dispatch(Action::SessionPrune {
            dry_run: true,
            ttl_secs: Some(0),
        });
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["dry_run"], true);
        assert!(response.to_value()["removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "empty-session"));
        assert!(dir.exists());
        let response = dispatch(Action::SessionPrune {
            dry_run: false,
            ttl_secs: Some(0),
        });
        assert!(response.ok, "{response:?}");
        assert!(!dir.exists());
    }

    #[test]
    fn pause_short_circuits_before_the_stub_clicks() {
        let backend = StubBackend::default();
        let _paused = policy::override_paused(true);
        let response = dispatch_with(
            &backend,
            Action::Click {
                x: Some(1),
                y: Some(2),
                button: MouseButton::Left,
                double: false,
            },
        );
        assert_eq!(response.to_value()["error"]["code"], "paused");
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 0);
        assert_eq!(backend.click_attempts.load(Ordering::SeqCst), 0);
        let waited = dispatch_with(
            &backend,
            Action::Wait {
                ms: Some(0),
                seconds: None,
            },
        );
        assert!(waited.ok, "{waited:?}");
    }

    #[test]
    fn confirm_hook_allows_quit_and_a_failure_does_not() {
        let backend = StubBackend::default();
        let dir = std::env::temp_dir().join(format!(
            "computer-use-hook-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        let (allow, deny) = {
            let allow = dir.join("allow.sh");
            let deny = dir.join("deny.sh");
            std::fs::write(&allow, "#!/bin/sh\ncat >/dev/null\nexit 0\n").unwrap();
            std::fs::write(&deny, "#!/bin/sh\ncat >/dev/null\nexit 1\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&allow, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(&deny, std::fs::Permissions::from_mode(0o700)).unwrap();
            (allow.display().to_string(), deny.display().to_string())
        };
        #[cfg(windows)]
        let (allow, deny) = ("cmd /C exit 0".to_string(), "cmd /C exit 1".to_string());
        let mut context = ctx("412");
        context.confirm_cmd = Some(deny);
        let denied = dispatch_with_context(&backend, Action::Quit { force: false }, &context);
        assert_eq!(denied.to_value()["error"]["code"], "confirm_denied");
        assert!(backend.quit.lock().unwrap().is_empty());
        context.confirm_cmd = Some(allow);
        let allowed = dispatch_with_context(&backend, Action::Quit { force: false }, &context);
        assert!(allowed.ok, "{allowed:?}");
        assert_eq!(backend.quit.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocked_url_prevents_the_click() {
        let mut backend = StubBackend::default();
        backend.apps.push(crate::target::AppInfo {
            name: "Safari".into(),
            bundle_id: Some("com.apple.Safari".into()),
            pid: 80,
            path: Some("/Applications/Safari.app/Contents/MacOS/Safari".into()),
            active: false,
            hidden: false,
            activation_policy: crate::target::ActivationPolicy::Regular,
        });
        backend
            .browser_urls
            .lock()
            .unwrap()
            .insert(80, "https://accounts.example.com/login".into());
        let dir = std::env::temp_dir().join(format!(
            "computer-use-url-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let policy_path = dir.join("policy.toml");
        std::fs::write(
            &policy_path,
            "version = 1\n[browser]\nblocked_urls = [\"https://accounts.example.com/*\"]\n",
        )
        .unwrap();
        let mut context = ctx("80");
        context.policy = Some(policy_path.display().to_string());
        let response = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(3),
                y: Some(4),
                button: MouseButton::Left,
                double: false,
            },
            &context,
        );
        assert_eq!(response.to_value()["error"]["code"], "blocked_url");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("https://accounts.example.com/*"));
        assert!(!response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("login"));
        assert_eq!(backend.click_attempts.load(Ordering::SeqCst), 0);
        backend
            .browser_urls
            .lock()
            .unwrap()
            .insert(80, "https://example.com/".into());
        let allowed = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(3),
                y: Some(4),
                button: MouseButton::Left,
                double: false,
            },
            &context,
        );
        assert!(allowed.ok, "{allowed:?}");
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn click_at(x: i32, y: i32) -> Action {
        Action::Click {
            x: Some(x),
            y: Some(y),
            button: MouseButton::Left,
            double: false,
        }
    }

    fn last_audit(dir: &std::path::Path) -> Value {
        let text = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        serde_json::from_str(text.lines().last().unwrap()).unwrap()
    }

    #[test]
    fn auto_event_falls_back_to_activate_unless_blocked() {
        let backend = StubBackend {
            fail_tiers: std::sync::Mutex::new(vec!["event".into()]),
            ..StubBackend::default()
        };
        let response = dispatch_with(&backend, click_at(3, 4));
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["tier"], "activate");
        assert_eq!(value["cursor_moved"], true);
        assert_eq!(value["focus_restored"], true);
        assert_eq!(
            backend.tiers_run.lock().unwrap().as_slice(),
            ["event", "activate"]
        );
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 1);

        let held = StubBackend {
            fail_tiers: std::sync::Mutex::new(vec!["event".into()]),
            ..StubBackend::default()
        };
        let dir = temp_dir("lock-fallback");
        let path = dir.join("foreground.lock");
        let _slot = crate::foreground::push_lock_path(&path);
        let _lock = crate::foreground::try_lock(&path).unwrap();
        let response = dispatch_with(&held, click_at(3, 4));
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stub tier event failed"));
        assert_eq!(held.tiers_run.lock().unwrap().as_slice(), ["event"]);
        assert_eq!(held.clicks.load(Ordering::SeqCst), 0);
        drop(_lock);
        let _ = std::fs::remove_dir_all(&dir);

        let background = StubBackend {
            fail_tiers: std::sync::Mutex::new(vec!["event".into()]),
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &background,
            click_at(1, 2),
            &CallContext {
                require_background: true,
                ..CallContext::default()
            },
        );
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
        assert_eq!(background.tiers_run.lock().unwrap().as_slice(), ["event"]);
        assert_eq!(background.clicks.load(Ordering::SeqCst), 0);

        let refused = StubBackend::default();
        let response = dispatch_with_context(
            &refused,
            click_at(1, 2),
            &CallContext {
                tier: Some("activate".into()),
                require_background: true,
                ..CallContext::default()
            },
        );
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--require-background"));
        assert!(refused.tiers_run.lock().unwrap().is_empty());
        assert_eq!(refused.clicks.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn activate_names_the_lock_holder_before_it_clicks() {
        let backend = StubBackend::default();
        let dir = temp_dir("lock-held");
        let path = dir.join("foreground.lock");
        let _slot = crate::foreground::push_lock_path(&path);
        let _lock = crate::foreground::try_lock(&path).unwrap();
        let response = dispatch_with_context(
            &backend,
            click_at(9, 8),
            &CallContext {
                tier: Some("activate".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
        let message = response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            message.contains(&format!(
                "foreground.lock is held by pid {}",
                std::process::id()
            )),
            "{message}"
        );
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 0);
        assert_eq!(backend.hit_calls.load(Ordering::SeqCst), 1);
        assert!(backend.tiers_run.lock().unwrap().is_empty());
        drop(_lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn synthetic_without_the_feature_does_not_hit_test_or_click() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            click_at(1, 1),
            &CallContext {
                tier: Some("synthetic".into()),
                ..CallContext::default()
            },
        );
        if crate::tier::skylight_compiled() {
            assert!(response.ok, "{response:?}");
            assert_eq!(response.to_value()["tier"], "synthetic");
            assert_eq!(backend.clicks.load(Ordering::SeqCst), 1);
            assert_eq!(backend.tiers_run.lock().unwrap().as_slice(), ["synthetic"]);
        } else {
            assert_eq!(response.to_value()["error"]["code"], "unsupported");
            assert!(response.to_value()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("built without skylight feature"));
            assert_eq!(backend.hit_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.clicks.load(Ordering::SeqCst), 0);
            assert!(backend.tiers_run.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn missing_overlay_helper_warns_and_no_overlay_skips_it() {
        let dir = temp_dir("overlay-warn");
        let _slot = crate::overlay::push_socket_path(dir.join("missing.sock"));
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            click_at(4, 5),
            &CallContext {
                session: Some("agent".into()),
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["tier"], "event");
        assert_eq!(response.to_value()["cursor_moved"], false);
        assert!(response.to_value().get("focus_restored").is_none());
        let warnings = response.to_value()["warnings"].as_array().unwrap().clone();
        assert!(warnings
            .iter()
            .any(|item| item == "overlay helper not running"));

        let quiet = dispatch_with_context(
            &backend,
            click_at(4, 5),
            &CallContext {
                no_overlay: true,
                ..CallContext::default()
            },
        );
        assert!(quiet.ok, "{quiet:?}");
        assert!(quiet.to_value().get("warnings").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audit_records_decision_source_and_the_real_code() {
        let dir = temp_dir("audit");
        let _state = crate::audit::push_state_dir(&dir);
        let backend = StubBackend::default();
        let response = dispatch_with(&backend, click_at(2, 3));
        assert!(response.ok, "{response:?}");
        let line = last_audit(&dir);
        assert_eq!(line["action"], "click");
        assert_eq!(line["code"], "ok");
        assert_eq!(line["decision"], "allow");
        assert_eq!(line["source"], "default");
        assert_eq!(line["tier"], "event");
        assert!(line.get("text").is_none());

        let denied = dispatch_with_context(&backend, Action::Launch, &ctx("com.apple.Terminal"));
        assert_eq!(denied.to_value()["error"]["code"], "policy_denied");
        let line = last_audit(&dir);
        assert_eq!(line["action"], "launch");
        assert_eq!(line["code"], "policy_denied");
        assert_eq!(line["decision"], "deny");
        assert_eq!(line["source"], "builtin");

        let checked =
            dispatch_with_context(&backend, Action::PolicyCheck, &ctx("com.apple.Terminal"));
        assert!(checked.ok, "{checked:?}");
        assert_eq!(checked.to_value()["decision"], "deny");
        let line = last_audit(&dir);
        assert_eq!(line["action"], "policy_check");
        assert_eq!(line["code"], "ok");
        assert_eq!(line["decision"], "deny");
        assert_eq!(line["source"], "builtin");

        let bad = dispatch_with(
            &backend,
            Action::Click {
                x: None,
                y: None,
                button: MouseButton::Left,
                double: false,
            },
        );
        assert_eq!(bad.to_value()["error"]["code"], "invalid_arguments");
        let line = last_audit(&dir);
        assert_eq!(line["code"], "invalid_arguments");
        assert_eq!(line["decision"], "allow");
        assert_eq!(line["source"], "default");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
