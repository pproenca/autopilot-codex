use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;

use crate::decision::DecisionError;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClickArguments {
    pub x: i64,
    pub y: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<MouseButton>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DragArguments {
    pub from_x: i64,
    pub from_y: i64,
    pub to_x: i64,
    pub to_y: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FocusClickArguments {
    pub x: i64,
    pub y: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "tool", content = "arguments", rename_all = "snake_case")]
pub enum PlannedAction {
    Click(ClickArguments),
    Drag(DragArguments),
    FocusClick(FocusClickArguments),
}

impl PlannedAction {
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::Click(_) => "click",
            Self::Drag(_) => "drag",
            Self::FocusClick(_) => "focus_click",
        }
    }

    pub fn arguments(&self) -> Value {
        match self {
            Self::Click(arguments) => {
                let mut object = Map::from_iter([
                    ("x".to_string(), Value::from(arguments.x)),
                    ("y".to_string(), Value::from(arguments.y)),
                ]);
                if let Some(button) = arguments.button {
                    object.insert(
                        "button".to_string(),
                        Value::String(
                            match button {
                                MouseButton::Left => "left",
                                MouseButton::Right => "right",
                            }
                            .to_string(),
                        ),
                    );
                }
                if let Some(count) = arguments.count {
                    object.insert("count".to_string(), Value::from(count));
                }
                Value::Object(object)
            }
            Self::Drag(arguments) => json!({
                "from_x": arguments.from_x,
                "from_y": arguments.from_y,
                "to_x": arguments.to_x,
                "to_y": arguments.to_y,
            }),
            Self::FocusClick(arguments) => json!({
                "x": arguments.x,
                "y": arguments.y,
            }),
        }
    }

    pub fn validate(&self, width: u32, height: u32) -> Result<(), DecisionError> {
        match self {
            Self::Click(arguments) => {
                validate_coordinate("x", arguments.x, width)?;
                validate_coordinate("y", arguments.y, height)?;
                if !(1..=3).contains(&arguments.count.unwrap_or(1)) {
                    return Err(DecisionError::InvalidClickCount);
                }
            }
            Self::Drag(arguments) => {
                validate_coordinate("from_x", arguments.from_x, width)?;
                validate_coordinate("from_y", arguments.from_y, height)?;
                validate_coordinate("to_x", arguments.to_x, width)?;
                validate_coordinate("to_y", arguments.to_y, height)?;
            }
            Self::FocusClick(arguments) => {
                validate_coordinate("x", arguments.x, width)?;
                validate_coordinate("y", arguments.y, height)?;
            }
        }
        Ok(())
    }

    pub fn action_sha256(&self) -> Result<String, DecisionError> {
        let envelope = recursively_sort(json!({
            "arguments": self.arguments(),
            "tool": self.tool_name(),
        }));
        let bytes = serde_json::to_vec(&envelope).map_err(|_| DecisionError::ActionEncoding)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

fn validate_coordinate(coordinate: &str, value: i64, dimension: u32) -> Result<(), DecisionError> {
    let upper_bound = i64::from(dimension) - 1;
    if value < 0 || value > upper_bound {
        return Err(DecisionError::CoordinateOutOfBounds {
            coordinate: coordinate.to_string(),
            value,
            upper_bound,
        });
    }
    Ok(())
}

fn recursively_sort(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries = object.into_iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, recursively_sort(value)))
                    .collect::<Map<_, _>>(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(recursively_sort).collect()),
        scalar => scalar,
    }
}
