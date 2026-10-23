//! OpenAI computer-use tool actions.
//!
//! Accepts the action object itself, or that object nested under `action`.
//! `wheel` is an alias of `middle`. `back` and `forward` are mouse buttons 4 and 5.
//! A bare `wait` sleeps 0 ms because the schema has no duration.

use serde_json::{Map, Value};

use super::{as_object, optional_i32, require_i32, require_string};
use crate::error::ComputerError;
use crate::protocol::Action;
use crate::types::{MouseButton, Point};

pub fn translate(value: &Value) -> Result<Action, ComputerError> {
    let object = as_object(value)?;
    let inner = match object.get("action") {
        Some(Value::Object(inner)) => inner,
        Some(_) => object,
        None => object,
    };
    let kind = match inner.get("type").and_then(Value::as_str) {
        Some(kind) => kind,
        None => return Err(ComputerError::InvalidArguments("type is required".into())),
    };
    match kind {
        "screenshot" => Ok(screenshot()),
        "click" => click(inner, false),
        "double_click" => click(inner, true),
        "scroll" => scroll(inner),
        "keypress" => keypress(inner),
        "type" => Ok(Action::Type {
            text: require_string(inner, "text")?,
        }),
        "move" => Ok(Action::Move {
            x: require_i32(inner, "x")?,
            y: require_i32(inner, "y")?,
        }),
        "drag" => drag(inner),
        "wait" => wait(inner),
        other => Err(ComputerError::InvalidArguments(format!(
            "unknown openai action type {other:?}"
        ))),
    }
}

fn screenshot() -> Action {
    Action::Screenshot {
        output: None,
        region: None,
        format: None,
        max_width: None,
        canvas: None,
        base64: false,
        marks: false,
    }
}

fn click(object: &Map<String, Value>, double: bool) -> Result<Action, ComputerError> {
    let button = match object
        .get("button")
        .and_then(Value::as_str)
        .unwrap_or("left")
        .to_ascii_lowercase()
        .as_str()
    {
        "left" => MouseButton::Left,
        "right" => MouseButton::Right,
        "middle" | "wheel" => MouseButton::Middle,
        "back" => MouseButton::Back,
        "forward" => MouseButton::Forward,
        other => {
            return Err(ComputerError::InvalidArguments(format!(
                "unknown click button {other:?}"
            )))
        }
    };
    let x = require_i32(object, "x")?;
    let y = require_i32(object, "y")?;
    if double {
        Ok(Action::DoubleClick {
            x: Some(x),
            y: Some(y),
            button,
        })
    } else {
        Ok(Action::Click {
            x: Some(x),
            y: Some(y),
            button,
            double: false,
            count: 1,
        })
    }
}

fn drag(object: &Map<String, Value>) -> Result<Action, ComputerError> {
    let Some(points) = object.get("path").and_then(Value::as_array) else {
        return Err(ComputerError::InvalidArguments(
            "path must be an array of points".into(),
        ));
    };
    if points.len() < 2 {
        return Err(ComputerError::InvalidArguments(
            "drag needs at least 2 points".into(),
        ));
    }
    let mut path = Vec::with_capacity(points.len());
    for point in points {
        let Some(point) = point.as_object() else {
            return Err(ComputerError::InvalidArguments(
                "path points must be objects".into(),
            ));
        };
        path.push(Point {
            x: require_i32(point, "x")?,
            y: require_i32(point, "y")?,
        });
    }
    Ok(Action::Drag {
        path,
        button: MouseButton::Left,
    })
}

fn scroll(object: &Map<String, Value>) -> Result<Action, ComputerError> {
    let scroll_x = optional_i32(object, "scroll_x")?.or(optional_i32(object, "dx")?);
    let scroll_y = optional_i32(object, "scroll_y")?.or(optional_i32(object, "dy")?);
    if scroll_x.is_none() && scroll_y.is_none() {
        return Err(ComputerError::InvalidArguments(
            "scroll_x or scroll_y is required".into(),
        ));
    }
    Ok(Action::Scroll {
        x: optional_i32(object, "x")?,
        y: optional_i32(object, "y")?,
        dx: scroll_x,
        dy: scroll_y,
        direction: None,
        amount: None,
    })
}

fn keypress(object: &Map<String, Value>) -> Result<Action, ComputerError> {
    let keys = match object.get("keys") {
        Some(Value::Array(keys)) if !keys.is_empty() => keys,
        Some(Value::Array(_)) => {
            return Err(ComputerError::InvalidArguments(
                "keys must not be empty".into(),
            ))
        }
        Some(_) => {
            return Err(ComputerError::InvalidArguments(
                "keys must be an array".into(),
            ))
        }
        None => return Err(ComputerError::InvalidArguments("keys is required".into())),
    };
    let mut parts = Vec::with_capacity(keys.len());
    for key in keys {
        let Some(text) = key.as_str() else {
            return Err(ComputerError::InvalidArguments(
                "keys must be an array of strings".into(),
            ));
        };
        if text.is_empty() {
            return Err(ComputerError::InvalidArguments(
                "keys must not contain an empty string".into(),
            ));
        }
        parts.push(text.to_string());
    }
    Ok(Action::Key {
        keys: parts.join("+"),
    })
}

fn wait(object: &Map<String, Value>) -> Result<Action, ComputerError> {
    let ms = optional_i32(object, "ms")?;
    let seconds = object.get("seconds").and_then(Value::as_f64);
    if ms.is_none() && seconds.is_none() {
        return Ok(Action::wait_sleep(Some(0), None));
    }
    if let Some(value) = ms {
        if value < 0 {
            return Err(ComputerError::InvalidArguments(
                "ms must be greater than or equal to 0".into(),
            ));
        }
    }
    Ok(Action::wait_sleep(ms.map(|value| value as u64), seconds))
}
