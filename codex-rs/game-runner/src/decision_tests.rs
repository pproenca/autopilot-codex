use pretty_assertions::assert_eq;
use serde_json::json;

use super::ClickArguments;
use super::DecisionError;
use super::DragArguments;
use super::FocusClickArguments;
use super::MouseButton;
use super::PlannedAction;

#[test]
fn click_action_has_exact_arguments_and_stable_hash() -> anyhow::Result<()> {
    let action = PlannedAction::Click(ClickArguments {
        x: 120,
        y: 240,
        button: None,
        count: Some(1),
    });

    assert_eq!(
        (
            action.tool_name(),
            action.arguments(),
            action.action_sha256()?,
        ),
        (
            "click",
            json!({"count": 1, "x": 120, "y": 240}),
            "bd1c262b95a3f95eaf81bc17481f5dcc19a66895cd96af45145e6fcd6363f01e"
                .to_string(),
        )
    );
    Ok(())
}

#[test]
fn drag_and_focus_click_actions_have_complete_arguments() {
    assert_eq!(
        PlannedAction::Drag(DragArguments {
            from_x: 10,
            from_y: 20,
            to_x: 30,
            to_y: 40,
        })
        .arguments(),
        json!({"from_x": 10, "from_y": 20, "to_x": 30, "to_y": 40})
    );
    assert_eq!(
        PlannedAction::FocusClick(FocusClickArguments { x: 50, y: 60 }).arguments(),
        json!({"x": 50, "y": 60})
    );
}

#[test]
fn planned_actions_validate_complete_image_bounds() {
    let action = PlannedAction::Click(ClickArguments {
        x: 1051,
        y: 819,
        button: Some(MouseButton::Left),
        count: Some(1),
    });

    assert_eq!(
        action.validate(/*width*/ 1051, /*height*/ 820),
        Err(DecisionError::CoordinateOutOfBounds {
            coordinate: "x".to_string(),
            value: 1051,
            upper_bound: 1050,
        })
    );
}

#[test]
fn planned_action_decoding_rejects_unknown_or_invalid_values() {
    let fixtures = [
        json!({"tool": "click", "arguments": {"x": 1, "y": 2, "extra": true}}),
        json!({"tool": "click", "arguments": {"x": 1, "y": 2, "button": "middle"}}),
        json!({"tool": "drag", "arguments": {
            "from_x": 1, "from_y": 2, "to_x": 3, "to_y": 4, "extra": true
        }}),
    ];

    for fixture in fixtures {
        assert!(serde_json::from_value::<PlannedAction>(fixture).is_err());
    }
}

#[test]
fn planned_actions_reject_invalid_counts_and_coordinates() {
    for count in [0, 4] {
        let action = PlannedAction::Click(ClickArguments {
            x: 1,
            y: 2,
            button: None,
            count: Some(count),
        });
        assert_eq!(action.validate(/*width*/ 10, /*height*/ 10), Err(DecisionError::InvalidClickCount));
    }

    let action = PlannedAction::FocusClick(FocusClickArguments { x: -1, y: 2 });
    assert_eq!(
        action.validate(/*width*/ 10, /*height*/ 10),
        Err(DecisionError::CoordinateOutOfBounds {
            coordinate: "x".to_string(),
            value: -1,
            upper_bound: 9,
        })
    );
}
