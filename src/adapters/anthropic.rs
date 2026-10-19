//! Anthropic `computer_20251124` actions.
//!
//! Coordinates stay in image pixels. `zoom` becomes a screenshot region.
//! Drag, mouse buttons held down, triple-click, and `hold_key` are refused
//! until P8. `cursor_position` reads the pointer the backend already exposes.

use serde_json::Value;

use super::{as_object, coordinate_pair, optional_i32, require_string, unsupported_until_p8};
use crate::error::ComputerError;
use crate::protocol::Action;
use crate::types::{MouseButton, Region, ScrollDirection};

pub fn translate(value: &Value) -> Result<Action, ComputerError> {
    let object = as_object(value)?;
    let kind = match object.get("action").and_then(Value::as_str) {
        Some(kind) => kind,
        None => return Err(ComputerError::InvalidArguments("action is required".into())),
    };
    match kind {
        "screenshot" => Ok(screenshot(None)),
        "left_click" => click_at(object, MouseButton::Left, false),
        "right_click" => click_at(object, MouseButton::Right, false),
        "middle_click" => click_at(object, MouseButton::Middle, false),
        "double_click" => click_at(object, MouseButton::Left, true),
        "triple_click" => Err(unsupported_until_p8("triple_click")),
        "mouse_move" => {
            let (x, y) = require_coordinate(object)?;
            Ok(Action::Move { x, y })
        }
        "left_click_drag" => Err(unsupported_until_p8("left_click_drag")),
        "left_mouse_down" => Err(unsupported_until_p8("left_mouse_down")),
        "left_mouse_up" => Err(unsupported_until_p8("left_mouse_up")),
        "scroll" => scroll(object),
        "type" => Ok(Action::Type {
            text: require_string(object, "text")?,
        }),
        "key" => Ok(Action::Key {
            keys: require_string(object, "text")?,
        }),
        "hold_key" => Err(unsupported_until_p8("hold_key")),
        "wait" => {
            let seconds = match object.get("duration").and_then(Value::as_f64) {
                Some(seconds) => seconds,
                None => {
                    return Err(ComputerError::InvalidArguments(
                        "duration is required".into(),
                    ))
                }
            };
            Ok(Action::Wait {
                ms: None,
                seconds: Some(seconds),
            })
        }
        "zoom" => zoom(object),
        "cursor_position" => Ok(Action::CursorPosition),
        other => Err(ComputerError::InvalidArguments(format!(
            "unknown anthropic action {other:?}"
        ))),
    }
}

fn screenshot(region: Option<Region>) -> Action {
    Action::Screenshot {
        output: None,
        region,
        format: None,
        max_width: None,
        canvas: None,
        base64: false,
    }
}

fn click_at(
    object: &serde_json::Map<String, Value>,
    button: MouseButton,
    double: bool,
) -> Result<Action, ComputerError> {
    let (x, y) = require_coordinate(object)?;
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
        })
    }
}

fn require_coordinate(
    object: &serde_json::Map<String, Value>,
) -> Result<(i32, i32), ComputerError> {
    match object.get("coordinate") {
        Some(value) => coordinate_pair(value, "coordinate"),
        None => Err(ComputerError::InvalidArguments(
            "coordinate is required".into(),
        )),
    }
}

fn scroll(object: &serde_json::Map<String, Value>) -> Result<Action, ComputerError> {
    let (x, y) = match object.get("coordinate") {
        Some(value) => {
            let (x, y) = coordinate_pair(value, "coordinate")?;
            (Some(x), Some(y))
        }
        None => (None, None),
    };
    let direction = object
        .get("scroll_direction")
        .or_else(|| object.get("direction"))
        .and_then(Value::as_str)
        .ok_or_else(|| ComputerError::InvalidArguments("scroll_direction is required".into()))?;
    let direction = match direction.to_ascii_lowercase().as_str() {
        "up" => ScrollDirection::Up,
        "down" => ScrollDirection::Down,
        "left" => ScrollDirection::Left,
        "right" => ScrollDirection::Right,
        other => {
            return Err(ComputerError::InvalidArguments(format!(
                "unknown scroll_direction {other:?}"
            )))
        }
    };
    let amount = optional_i32(object, "scroll_amount")?.or(optional_i32(object, "amount")?);
    Ok(Action::Scroll {
        x,
        y,
        dx: None,
        dy: None,
        direction: Some(direction),
        amount,
    })
}

fn zoom(object: &serde_json::Map<String, Value>) -> Result<Action, ComputerError> {
    let region = match object.get("region") {
        Some(Value::Array(values)) if values.len() == 4 => values,
        Some(_) => {
            return Err(ComputerError::InvalidArguments(
                "region must be [x1, y1, x2, y2]".into(),
            ))
        }
        None => return Err(ComputerError::InvalidArguments("region is required".into())),
    };
    let mut edges = [0i32; 4];
    for (index, value) in region.iter().enumerate() {
        edges[index] = super::number_i32(value, "region")?;
    }
    let (x1, y1, x2, y2) = (edges[0], edges[1], edges[2], edges[3]);
    if x2 <= x1 || y2 <= y1 {
        return Err(ComputerError::InvalidArguments(
            "region must have a positive width and height".into(),
        ));
    }
    let width = u32::try_from(x2 - x1)
        .map_err(|_| ComputerError::InvalidArguments("region width is out of range".into()))?;
    let height = u32::try_from(y2 - y1)
        .map_err(|_| ComputerError::InvalidArguments("region height is out of range".into()))?;
    Ok(screenshot(Some(Region::new(x1, y1, width, height)?)))
}
