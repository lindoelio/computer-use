//! Page actions: element resolution, actionability, click, type, keys,
//! hover, scroll, select, check, upload, dialogs, and evaluation.
//!
//! Everything routes through raw CDP so the same code works for a CSS
//! selector and for a snapshot `ref`. A ref resolves to a backend node id;
//! a selector resolves to a `DOM.NodeId`. Both become a `RemoteObjectId`
//! that `Runtime.callFunctionOn` can use.

#![cfg(feature = "cdp")]

use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, GetBoxModelParams, GetDocumentParams, NodeId, QuerySelectorParams,
    ResolveNodeParams, ScrollIntoViewIfNeededParams, SetFileInputFilesParams,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchMouseEventParams, DispatchMouseEventType, MouseButton as CdpMouseButton,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallArgument, CallFunctionOnParams, EvaluateParams, RemoteObjectId,
};
use chromiumoxide::Page;
use serde_json::{json, Value};

use crate::browser::keys;
use crate::error::ComputerError;
use crate::types::MouseButton;

/// A resolved element. At least one of the ids is set.
#[derive(Debug, Clone)]
pub struct ElementHandle {
    /// DOM node id when the element came from a selector.
    pub node_id: Option<NodeId>,
    /// Backend node id, always set when resolvable.
    pub backend_node_id: Option<BackendNodeId>,
    /// JavaScript object id for `Runtime.callFunctionOn`.
    pub object_id: RemoteObjectId,
}

/// Resolve a CSS selector or a snapshot backend node id.
pub async fn resolve(
    page: &Page,
    selector: Option<&str>,
    backend_node_id: Option<i64>,
) -> Result<ElementHandle, ComputerError> {
    let (node_id, backend) = match (selector, backend_node_id) {
        (Some(selector), _) => {
            let node_id = node_for_selector(page, selector).await?;
            (Some(node_id), None)
        }
        (None, Some(id)) => (None, Some(BackendNodeId::new(id))),
        (None, None) => {
            return Err(ComputerError::InvalidArguments(
                "browser action needs --selector or --ref".into(),
            ))
        }
    };
    let mut params = ResolveNodeParams::builder();
    if let Some(node_id) = &node_id {
        params = params.node_id(*node_id);
    }
    if let Some(backend) = &backend {
        params = params.backend_node_id(*backend);
    }
    let params = params.build();
    let resolved = page
        .execute(params)
        .await
        .map_err(|error| map_action_error(error, "resolve element"))?;
    let object_id =
        resolved.object.object_id.clone().ok_or_else(|| {
            ComputerError::ElementNotFound("element has no JavaScript object".into())
        })?;
    Ok(ElementHandle {
        node_id,
        backend_node_id: backend,
        object_id,
    })
}

async fn node_for_selector(page: &Page, selector: &str) -> Result<NodeId, ComputerError> {
    let document = page
        .execute(GetDocumentParams::builder().depth(0).build())
        .await
        .map_err(|error| map_action_error(error, "DOM.getDocument"))?;
    let found = page
        .execute(
            QuerySelectorParams::builder()
                .node_id(document.root.node_id)
                .selector(selector.to_string())
                .build()
                .map_err(ComputerError::InvalidArguments)?,
        )
        .await
        .map_err(|error| map_action_error(error, "querySelector"))?;
    if *found.node_id.inner() == 0 {
        return Err(ComputerError::ElementNotFound(format!(
            "selector matched no element: {selector}"
        )));
    }
    Ok(found.node_id)
}

fn map_action_error(error: chromiumoxide::error::CdpError, what: &str) -> ComputerError {
    let text = error.to_string();
    if text.contains("no node") || text.contains("node with given id") {
        ComputerError::ElementNotFound(format!("{what}: {text}"))
    } else {
        ComputerError::ExecutionFailed(format!("{what}: {text}"))
    }
}

/// Call a function on the element. Arguments are JSON values.
pub async fn call_on(
    page: &Page,
    handle: &ElementHandle,
    function: &str,
    args: Vec<Value>,
) -> Result<Value, ComputerError> {
    let mut builder = CallFunctionOnParams::builder()
        .function_declaration(function.to_string())
        .object_id(handle.object_id.clone())
        .return_by_value(true)
        .await_promise(true)
        .user_gesture(true);
    if !args.is_empty() {
        let arguments: Vec<CallArgument> = args
            .into_iter()
            .map(|value| CallArgument::builder().value(value).build())
            .collect();
        builder = builder.arguments(arguments);
    }
    let params = builder.build().map_err(ComputerError::InvalidArguments)?;
    let result = page
        .execute(params)
        .await
        .map_err(|error| map_action_error(error, "callFunctionOn"))?;
    if let Some(exception) = result.result.exception_details {
        return Err(ComputerError::ExecutionFailed(format!(
            "page exception: {}",
            exception.text
        )));
    }
    Ok(result.result.result.value.unwrap_or(Value::Null))
}

/// Evaluate an expression in the page context.
pub async fn evaluate(
    page: &Page,
    expression: &str,
    await_promise: bool,
) -> Result<Value, ComputerError> {
    let params = EvaluateParams::builder()
        .expression(expression.to_string())
        .return_by_value(true)
        .await_promise(await_promise)
        .user_gesture(true)
        .build()
        .map_err(ComputerError::InvalidArguments)?;
    let result = page
        .execute(params)
        .await
        .map_err(|error| map_action_error(error, "evaluate"))?;
    if let Some(exception) = result.result.exception_details {
        return Err(ComputerError::ExecutionFailed(format!(
            "page exception: {}",
            exception.text
        )));
    }
    Ok(result.result.result.value.unwrap_or(Value::Null))
}

/// True when the element is a password or one-time-code field.
pub async fn is_secure_field(page: &Page, handle: &ElementHandle) -> Result<bool, ComputerError> {
    let state = call_on(
        page,
        handle,
        r#"function () {
            const type = (this.getAttribute && this.getAttribute('type')) || this.type || '';
            const autocomplete = (this.getAttribute && this.getAttribute('autocomplete')) || this.autocomplete || '';
            return {type: String(type).toLowerCase(), autocomplete: String(autocomplete).toLowerCase()};
        }"#,
        vec![],
    )
    .await?;
    let field_type = state
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if field_type == "password" {
        return Ok(true);
    }
    let autocomplete = state
        .get("autocomplete")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(autocomplete.contains("current-password")
        || autocomplete.contains("new-password")
        || autocomplete.contains("one-time-code"))
}

/// Visible box centre in CSS pixels.
async fn box_center(page: &Page, handle: &ElementHandle) -> Result<(f64, f64), ComputerError> {
    let mut builder = GetBoxModelParams::builder();
    if let Some(node_id) = &handle.node_id {
        builder = builder.node_id(*node_id);
    }
    if let Some(backend) = &handle.backend_node_id {
        builder = builder.backend_node_id(*backend);
    }
    let params = builder.build();
    let response = page
        .execute(params)
        .await
        .map_err(|error| map_action_error(error, "getBoxModel"))?;
    let quad = response.model.content.inner();
    if quad.len() < 8 {
        return Err(ComputerError::ElementNotFound(
            "element has no visible box".into(),
        ));
    }
    let x = (quad[0] + quad[2] + quad[4] + quad[6]) / 4.0;
    let y = (quad[1] + quad[3] + quad[5] + quad[7]) / 4.0;
    Ok((x, y))
}

async fn scroll_into_view(page: &Page, handle: &ElementHandle) -> Result<(), ComputerError> {
    let mut builder = ScrollIntoViewIfNeededParams::builder();
    if let Some(node_id) = &handle.node_id {
        builder = builder.node_id(*node_id);
    }
    if let Some(backend) = &handle.backend_node_id {
        builder = builder.backend_node_id(*backend);
    }
    let params = builder.build();
    page.execute(params)
        .await
        .map_err(|error| map_action_error(error, "scrollIntoViewIfNeeded"))?;
    Ok(())
}

async fn actionability(
    page: &Page,
    handle: &ElementHandle,
    force: bool,
) -> Result<(), ComputerError> {
    if force {
        return Ok(());
    }
    let state = call_on(
        page,
        handle,
        r#"function () {
            const style = window.getComputedStyle(this);
            const rect = this.getBoundingClientRect();
            return {
                visible: style.visibility !== 'hidden' && style.display !== 'none' && rect.width > 0 && rect.height > 0,
                disabled: this.disabled === true || this.getAttribute('aria-disabled') === 'true'
            };
        }"#,
        vec![],
    )
    .await?;
    let visible = state
        .get("visible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let disabled = state
        .get("disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !visible {
        return Err(ComputerError::ElementNotFound(
            "element is not visible; pass --force to act anyway".into(),
        ));
    }
    if disabled {
        return Err(ComputerError::PolicyDenied(
            "element is disabled; pass --force to act anyway".into(),
        ));
    }
    Ok(())
}

/// Move the visible agent ring when the page has the marker installed.
/// The call is a no-op when `window.__cuAgent` is absent.
async fn move_marker(page: &Page, x: f64, y: f64, kind: &str) {
    let expression = format!("window.__cuAgent && window.__cuAgent.move({x}, {y}, '{kind}')");
    let _ = evaluate(page, &expression, false).await;
}

fn cdp_button(button: MouseButton) -> CdpMouseButton {
    match button {
        MouseButton::Left => CdpMouseButton::Left,
        MouseButton::Right => CdpMouseButton::Right,
        MouseButton::Middle => CdpMouseButton::Middle,
        MouseButton::Back => CdpMouseButton::Back,
        MouseButton::Forward => CdpMouseButton::Forward,
    }
}

fn buttons_mask(button: MouseButton) -> i64 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
        MouseButton::Back => 8,
        MouseButton::Forward => 16,
    }
}

/// Click an element. `count` may be 1, 2, or 3.
pub async fn click(
    page: &Page,
    handle: &ElementHandle,
    button: MouseButton,
    count: u8,
    force: bool,
) -> Result<(), ComputerError> {
    actionability(page, handle, force).await?;
    scroll_into_view(page, handle).await?;
    let (x, y) = box_center(page, handle).await?;
    move_marker(page, x, y, "click").await;
    for index in 1..=count.max(1) {
        let pressed = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MousePressed)
            .x(x)
            .y(y)
            .button(cdp_button(button))
            .buttons(buttons_mask(button))
            .click_count(index as i64)
            .build()
            .map_err(ComputerError::InvalidArguments)?;
        match tokio::time::timeout(std::time::Duration::from_secs(5), page.execute(pressed)).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(map_action_error(error, "mousePressed")),
            // A JavaScript dialog can block the event response. Give the
            // background dialog handler a moment to accept/dismiss it, then
            // return; the click itself was dispatched.
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                return Ok(());
            }
        }
        let released = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseReleased)
            .x(x)
            .y(y)
            .button(cdp_button(button))
            .buttons(0)
            .click_count(index as i64)
            .build()
            .map_err(ComputerError::InvalidArguments)?;
        match tokio::time::timeout(std::time::Duration::from_secs(5), page.execute(released)).await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(map_action_error(error, "mouseReleased")),
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Type `text` into an element. Refuses secure fields.
pub async fn type_text(
    page: &Page,
    handle: &ElementHandle,
    text: &str,
    clear: bool,
) -> Result<(), ComputerError> {
    if is_secure_field(page, handle).await? {
        return Err(ComputerError::PolicyDenied(
            "refused to type into a password or one-time-code field; ask the user to type it"
                .into(),
        ));
    }
    scroll_into_view(page, handle).await?;
    if let Ok((x, y)) = box_center(page, handle).await {
        move_marker(page, x, y, "type").await;
    }
    call_on(
        page,
        handle,
        r#"function () {
            if (this.focus) this.focus();
            if (typeof this.setSelectionRange === 'function') {
                this.setSelectionRange(0, this.value ? this.value.length : 0);
            } else if (typeof this.select === 'function') {
                this.select();
            }
        }"#,
        vec![],
    )
    .await?;
    if clear && text.is_empty() {
        let backspace = keys::parse("Backspace")?;
        dispatch_raw_key(page, &backspace).await?;
        return Ok(());
    }
    if !text.is_empty() {
        let params = chromiumoxide::cdp::browser_protocol::input::InsertTextParams::builder()
            .text(text.to_string())
            .build()
            .map_err(ComputerError::InvalidArguments)?;
        page.execute(params)
            .await
            .map_err(|error| map_action_error(error, "insertText"))?;
    }
    Ok(())
}

async fn dispatch_raw_key(page: &Page, stroke: &keys::KeyStroke) -> Result<(), ComputerError> {
    use chromiumoxide::cdp::browser_protocol::input::{
        DispatchKeyEventParams, DispatchKeyEventType,
    };
    let mut down = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyDown)
        .key(stroke.key.clone())
        .windows_virtual_key_code(stroke.virtual_key_code)
        .native_virtual_key_code(stroke.virtual_key_code)
        .modifiers(stroke.modifiers);
    if let Some(code) = &stroke.code {
        down = down.code(code.clone());
    }
    if let Some(text) = &stroke.text {
        down = down.text(text.clone());
    }
    let down = down.build().map_err(ComputerError::InvalidArguments)?;
    page.execute(down)
        .await
        .map_err(|error| map_action_error(error, "keyDown"))?;
    let mut up = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyUp)
        .key(stroke.key.clone())
        .windows_virtual_key_code(stroke.virtual_key_code)
        .native_virtual_key_code(stroke.virtual_key_code)
        .modifiers(stroke.modifiers);
    if let Some(code) = &stroke.code {
        up = up.code(code.clone());
    }
    let up = up.build().map_err(ComputerError::InvalidArguments)?;
    page.execute(up)
        .await
        .map_err(|error| map_action_error(error, "keyUp"))?;
    Ok(())
}

/// Press a key combination on the page.
pub async fn press(page: &Page, combo: &str) -> Result<(), ComputerError> {
    keys::dispatch(page, combo).await?;
    Ok(())
}

/// Move the mouse over an element.
pub async fn hover(page: &Page, handle: &ElementHandle) -> Result<(), ComputerError> {
    scroll_into_view(page, handle).await?;
    let (x, y) = box_center(page, handle).await?;
    move_marker(page, x, y, "hover").await;
    let moved = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseMoved)
        .x(x)
        .y(y)
        .build()
        .map_err(ComputerError::InvalidArguments)?;
    page.execute(moved)
        .await
        .map_err(|error| map_action_error(error, "mouseMoved"))?;
    Ok(())
}

/// Scroll the page, or the element into view first when one was named.
pub async fn scroll(
    page: &Page,
    handle: Option<&ElementHandle>,
    dx: f64,
    dy: f64,
) -> Result<(), ComputerError> {
    if let Some(handle) = handle {
        scroll_into_view(page, handle).await?;
    }
    let center = evaluate(
        page,
        "({x: window.innerWidth/2, y: window.innerHeight/2})",
        false,
    )
    .await
    .unwrap_or_else(|_| json!({"x": 0.0, "y": 0.0}));
    let x = center.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = center.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    move_marker(page, x, y, "scroll").await;
    let params = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseWheel)
        .x(x)
        .y(y)
        .delta_x(dx)
        .delta_y(dy)
        .build()
        .map_err(ComputerError::InvalidArguments)?;
    page.execute(params)
        .await
        .map_err(|error| map_action_error(error, "mouseWheel"))?;
    Ok(())
}

/// Select an option by value or label.
pub async fn select_option(
    page: &Page,
    handle: &ElementHandle,
    value: Option<&str>,
    label: Option<&str>,
) -> Result<Value, ComputerError> {
    call_on(
        page,
        handle,
        r#"function (value, label) {
            const select = this;
            const options = Array.from(select.options || []);
            const match = options.find((option) =>
                (value !== null && value !== undefined && option.value === value) ||
                (label !== null && label !== undefined && option.label === label) ||
                (label !== null && label !== undefined && option.text === label)
            );
            if (!match) return {ok: false, reason: 'option not found'};
            select.value = match.value;
            select.dispatchEvent(new Event('input', {bubbles: true}));
            select.dispatchEvent(new Event('change', {bubbles: true}));
            return {ok: true, value: select.value, label: match.label};
        }"#,
        vec![
            value.map(|value| json!(value)).unwrap_or(Value::Null),
            label.map(|label| json!(label)).unwrap_or(Value::Null),
        ],
    )
    .await
}

/// Check or uncheck a checkbox or radio.
pub async fn set_checked(
    page: &Page,
    handle: &ElementHandle,
    checked: bool,
) -> Result<Value, ComputerError> {
    call_on(
        page,
        handle,
        r#"function (checked) {
            if (this.checked !== checked) {
                this.click();
            }
            return {ok: this.checked === checked, checked: this.checked};
        }"#,
        vec![json!(checked)],
    )
    .await
}

/// Set files on an `<input type=file>`.
pub async fn upload(
    page: &Page,
    handle: &ElementHandle,
    files: &[String],
) -> Result<(), ComputerError> {
    let mut builder = SetFileInputFilesParams::builder().files(files.iter().cloned());
    if let Some(node_id) = &handle.node_id {
        builder = builder.node_id(*node_id);
    }
    if let Some(backend) = &handle.backend_node_id {
        builder = builder.backend_node_id(*backend);
    }
    let params = builder.build().map_err(ComputerError::InvalidArguments)?;
    page.execute(params)
        .await
        .map_err(|error| map_action_error(error, "setFileInputFiles"))?;
    Ok(())
}

/// Read the element's innerText.
pub async fn read_text(page: &Page, handle: &ElementHandle) -> Result<String, ComputerError> {
    let value = call_on(
        page,
        handle,
        r#"function () {
            const tag = (this.tagName || '').toUpperCase();
            if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') {
                return this.value === undefined || this.value === null ? '' : String(this.value);
            }
            if (this.innerText !== undefined && this.innerText !== null) return this.innerText;
            if (this.textContent !== undefined && this.textContent !== null) return this.textContent;
            return '';
        }"#,
        vec![],
    )
    .await?;
    Ok(value.as_str().unwrap_or_default().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_action_error_splits_not_found_from_execution() {
        let not_found = map_action_error(
            chromiumoxide::error::CdpError::msg("No node with given id"),
            "resolve",
        );
        assert_eq!(not_found.code(), "element_not_found");
        assert!(not_found.to_string().contains("resolve"));

        let generic = map_action_error(chromiumoxide::error::CdpError::msg("boom"), "evaluate");
        assert_eq!(generic.code(), "execution_failed");
        assert!(generic.to_string().contains("evaluate: boom"));
    }

    #[test]
    fn button_maps_cover_every_variant() {
        let cases = [
            (MouseButton::Left, CdpMouseButton::Left, 1),
            (MouseButton::Right, CdpMouseButton::Right, 2),
            (MouseButton::Middle, CdpMouseButton::Middle, 4),
            (MouseButton::Back, CdpMouseButton::Back, 8),
            (MouseButton::Forward, CdpMouseButton::Forward, 16),
        ];
        for (button, cdp, mask) in cases {
            assert_eq!(cdp_button(button), cdp, "{button:?}");
            assert_eq!(buttons_mask(button), mask, "{button:?}");
        }
    }
}
