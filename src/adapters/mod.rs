//! Schema translators for `exec` and `serve`.
//!
//! Adapters turn a foreign action object into a native [`Action`]. They set
//! `coord` to `image` and do not call the backend. Dispatch remaps image
//! pixels with the stored screenshot map.

mod anthropic;
mod openai;

use serde_json::{Map, Value};

use crate::error::ComputerError;
use crate::protocol::Action;

pub use anthropic::translate as translate_anthropic;
pub use openai::translate as translate_openai;

pub(crate) fn as_object(value: &Value) -> Result<&Map<String, Value>, ComputerError> {
    value.as_object().ok_or_else(|| {
        ComputerError::InvalidArguments("adapter request must be a JSON object".into())
    })
}

pub(crate) fn require_i32(object: &Map<String, Value>, key: &str) -> Result<i32, ComputerError> {
    match object.get(key) {
        Some(value) => number_i32(value, key),
        None => Err(ComputerError::InvalidArguments(format!(
            "{key} is required"
        ))),
    }
}

pub(crate) fn optional_i32(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<i32>, ComputerError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => number_i32(value, key).map(Some),
    }
}

pub(crate) fn require_string(
    object: &Map<String, Value>,
    key: &str,
) -> Result<String, ComputerError> {
    match object.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Ok(text.clone()),
        Some(Value::String(_)) => Err(ComputerError::InvalidArguments(format!(
            "{key} must not be empty"
        ))),
        Some(_) => Err(ComputerError::InvalidArguments(format!(
            "{key} must be a string"
        ))),
        None => Err(ComputerError::InvalidArguments(format!(
            "{key} is required"
        ))),
    }
}

pub(crate) fn coordinate_pair(value: &Value, key: &str) -> Result<(i32, i32), ComputerError> {
    let Some(pair) = value.as_array() else {
        return Err(ComputerError::InvalidArguments(format!(
            "{key} must be [x, y]"
        )));
    };
    if pair.len() != 2 {
        return Err(ComputerError::InvalidArguments(format!(
            "{key} must be [x, y]"
        )));
    }
    Ok((number_i32(&pair[0], key)?, number_i32(&pair[1], key)?))
}

pub(crate) fn number_i32(value: &Value, key: &str) -> Result<i32, ComputerError> {
    let Some(number) = value.as_f64() else {
        return Err(ComputerError::InvalidArguments(format!(
            "{key} must be a number"
        )));
    };
    if !number.is_finite() || number < f64::from(i32::MIN) || number > f64::from(i32::MAX) {
        return Err(ComputerError::InvalidArguments(format!(
            "{key} is out of range"
        )));
    }
    Ok(number.round() as i32)
}

pub(crate) fn unsupported_until_p8(action: &str) -> ComputerError {
    ComputerError::Unsupported(format!("{action} is not available until P8"))
}

/// Translate `value` with `adapter`. `native` is not handled here.
pub fn translate(adapter: &str, value: &Value) -> Result<Action, ComputerError> {
    match adapter {
        "openai" => translate_openai(value),
        "anthropic" => translate_anthropic(value),
        other => Err(ComputerError::InvalidArguments(format!(
            "unknown adapter {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ComputerError;
    use serde_json::Value;
    use std::fs;
    use std::path::Path;

    fn assert_fixtures(dir: &str, translate: fn(&Value) -> Result<Action, ComputerError>) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(dir);
        let mut paths: Vec<_> = fs::read_dir(&root)
            .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .collect();
        paths.sort();
        assert!(!paths.is_empty(), "no fixtures in {}", root.display());
        for path in paths {
            let fixture: Value = serde_json::from_str(
                &fs::read_to_string(&path).unwrap_or_else(|error| panic!("{error}")),
            )
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let translated = translate(&fixture["request"]);
            if let Some(code) = fixture.get("error_code").and_then(Value::as_str) {
                let error = match translated {
                    Err(error) => error,
                    Ok(action) => panic!("{} translated to {action:?}", path.display()),
                };
                assert_eq!(error.code(), code, "{}", path.display());
                if let Some(needle) = fixture.get("error_contains").and_then(Value::as_str) {
                    assert!(
                        error.to_string().contains(needle),
                        "{}: {error}",
                        path.display()
                    );
                }
            } else {
                let expected: Action = serde_json::from_value(fixture["expect"].clone())
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                let action = match translated {
                    Ok(action) => action,
                    Err(error) => panic!("{}: {error}", path.display()),
                };
                assert_eq!(action, expected, "{}", path.display());
            }
        }
    }

    #[test]
    fn openai_fixtures_match_the_expected_action() {
        assert_fixtures("openai", translate_openai);
    }

    #[test]
    fn anthropic_fixtures_match_the_expected_action() {
        assert_fixtures("anthropic", translate_anthropic);
    }

    #[test]
    fn openai_wheel_is_middle_and_back_is_unsupported() {
        let wheel = translate_openai(&serde_json::json!({
            "action": {"type": "click", "x": 1, "y": 2, "button": "wheel"}
        }))
        .unwrap();
        assert_eq!(
            wheel,
            Action::Click {
                x: Some(1),
                y: Some(2),
                button: crate::types::MouseButton::Middle,
                double: false,
            }
        );
        let back = translate_openai(&serde_json::json!({
            "type": "click", "x": 1, "y": 2, "button": "back"
        }))
        .unwrap_err();
        assert_eq!(back.code(), "unsupported");
        assert!(back.to_string().contains("P8"));
        let forward = translate_openai(&serde_json::json!({
            "type": "click", "x": 1, "y": 2, "button": "forward"
        }))
        .unwrap_err();
        assert_eq!(forward.code(), "unsupported");
    }

    #[test]
    fn anthropic_return_key_and_unknown_action() {
        let key = translate_anthropic(&serde_json::json!({
            "action": "key",
            "text": "Return"
        }))
        .unwrap();
        assert_eq!(
            key,
            Action::Key {
                keys: "Return".into()
            }
        );
        let unknown = translate_anthropic(&serde_json::json!({"action": "nope"})).unwrap_err();
        assert_eq!(unknown.code(), "invalid_arguments");
        assert!(unknown.to_string().contains("nope"));
    }
}
