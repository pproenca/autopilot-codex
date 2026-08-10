use pretty_assertions::assert_eq;
use serde_json::json;

use super::DecisionAudit;
use super::DecisionGate;
use super::DecisionSnapshot;
use super::ClickArguments;
use super::DecisionError;
use super::DragArguments;
use super::FocusClickArguments;
use super::InvalidationReason;
use super::MouseButton;
use super::MutationEvidence;
use super::MutationResult;
use super::OutcomeDraft;
use super::OutcomeKind;
use super::PlanCandidate;
use super::PlanDraft;
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

fn click(x: i64, y: i64) -> PlannedAction {
    PlannedAction::Click(ClickArguments {
        x,
        y,
        button: None,
        count: None,
    })
}

fn plan_draft(observation_reference: String, chosen_action: PlannedAction) -> PlanDraft {
    PlanDraft {
        observation_reference,
        objective: "Open a safe menu".to_string(),
        visible_state_summary: "The main menu is visible".to_string(),
        candidates: vec![
            PlanCandidate {
                action: "Open Settings".to_string(),
                predicted_visible_consequence: "Settings appears".to_string(),
            },
            PlanCandidate {
                action: "Open Credits".to_string(),
                predicted_visible_consequence: "Credits appears".to_string(),
            },
        ],
        chosen_action,
        reason: "The action is reversible".to_string(),
        expected_visible_result: "A non-gameplay screen".to_string(),
        invalidation_condition: "The menu changes".to_string(),
    }
}

fn observe(gate: &DecisionGate, call_id: &str, reference: &str) -> anyhow::Result<()> {
    gate.begin_full_observation();
    gate.complete_full_observation(
        call_id.to_string(),
        reference.to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;
    Ok(())
}

#[test]
fn one_observation_plan_mutation_and_after_observation_is_complete() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    gate.begin_full_observation();
    let before = gate.complete_full_observation(
        "capture-before".to_string(),
        "sha256:before".to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;
    let plan = gate.record_plan(plan_draft(before.reference.clone(), click(180, 640)))?;
    let authorized = gate.prepare_mutation(
        "click",
        &json!({"x": 180, "y": 640}),
        "mutation-1",
    )?;
    gate.record_mutation_result("mutation-1", MutationResult::Success)?;
    gate.begin_full_observation();
    let after = gate.complete_full_observation(
        "capture-after".to_string(),
        "sha256:after".to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;

    assert_eq!(
        gate.snapshot(),
        DecisionSnapshot {
            owner_generation: 1,
            next_observation_generation: 3,
            observation: Some(after),
            plan: None,
            mutation: Some(MutationEvidence {
                plan,
                authorization: authorized,
                result: Some(MutationResult::Success),
            }),
            outcome: None,
            requires_post_mutation_observation: false,
            audit: DecisionAudit {
                plans_accepted: 1,
                plan_rejections: 0,
                mutation_attempts: 1,
                mutation_authorizations: 1,
                mutation_denials: 0,
            },
        }
    );
    Ok(())
}

#[test]
fn capture_attempt_and_positive_wait_invalidate_authority() -> anyhow::Result<()> {
    for invalidate in [InvalidationReason::CaptureStarted, InvalidationReason::PositiveWait] {
        let gate = DecisionGate::new(1);
        observe(&gate, "capture-before", "sha256:before")?;
        gate.record_plan(plan_draft("sha256:before".to_string(), click(180, 640)))?;
        match invalidate {
            InvalidationReason::CaptureStarted => gate.begin_full_observation(),
            InvalidationReason::PositiveWait => gate.before_wait(Some(&json!({"seconds": 1}))),
            InvalidationReason::TurnAborted | InvalidationReason::OwnerGenerationReplaced { .. } => {
                unreachable!()
            }
        }
        let snapshot = gate.snapshot();
        assert_eq!((snapshot.observation, snapshot.plan), (None, None));
    }

    let gate = DecisionGate::new(1);
    observe(&gate, "capture-before", "sha256:before")?;
    gate.record_plan(plan_draft("sha256:before".to_string(), click(180, 640)))?;
    let before = gate.snapshot();
    gate.before_wait(Some(&json!({"seconds": 0})));
    assert_eq!(gate.snapshot(), before);
    Ok(())
}

#[test]
fn stale_plan_and_mismatched_mutation_are_rejected_and_consumed() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    observe(&gate, "capture-before", "sha256:before")?;
    assert!(matches!(
        gate.record_plan(plan_draft("sha256:stale".to_string(), click(180, 640))),
        Err(DecisionError::StaleObservation { .. })
    ));
    gate.record_plan(plan_draft("sha256:before".to_string(), click(180, 640)))?;
    assert!(matches!(
        gate.prepare_mutation("click", &json!({"x": 181, "y": 640}), "mutation-1"),
        Err(DecisionError::ActionMismatch)
    ));
    let snapshot = gate.snapshot();
    assert_eq!(
        (snapshot.observation, snapshot.plan, snapshot.requires_post_mutation_observation),
        (None, None, true)
    );
    assert_eq!(snapshot.audit.mutation_denials, 1);
    Ok(())
}

#[test]
fn authorized_mutation_exhausts_the_stage_budget() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    observe(&gate, "capture-before", "sha256:before")?;
    gate.record_plan(plan_draft("sha256:before".to_string(), click(180, 640)))?;
    gate.prepare_mutation("click", &json!({"x": 180, "y": 640}), "mutation-1")?;
    gate.record_mutation_result("mutation-1", MutationResult::Success)?;
    observe(&gate, "capture-after", "sha256:after")?;
    gate.record_plan(plan_draft("sha256:after".to_string(), click(10, 10)))?;

    assert_eq!(
        gate.prepare_mutation("click", &json!({"x": 10, "y": 10}), "mutation-2"),
        Err(DecisionError::MutationBudgetExhausted)
    );
    assert_eq!(gate.snapshot().plan, None);
    Ok(())
}

#[test]
fn interruption_and_owner_replacement_invalidate_plans() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    observe(&gate, "capture-before", "sha256:before")?;
    gate.record_plan(plan_draft("sha256:before".to_string(), click(180, 640)))?;
    gate.invalidate(InvalidationReason::TurnAborted);
    assert_eq!((gate.snapshot().observation, gate.snapshot().plan), (None, None));

    observe(&gate, "capture-again", "sha256:again")?;
    gate.record_plan(plan_draft("sha256:again".to_string(), click(180, 640)))?;
    gate.invalidate(InvalidationReason::OwnerGenerationReplaced {
        owner_generation: 2,
    });
    let snapshot = gate.snapshot();
    assert_eq!((snapshot.owner_generation, snapshot.observation, snapshot.plan), (2, None, None));
    Ok(())
}

#[test]
fn outcome_requires_the_newest_post_mutation_observation() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    observe(&gate, "capture-before", "sha256:before")?;
    let outcome = |reference: &str| OutcomeDraft {
        outcome: OutcomeKind::Win,
        observation_reference: reference.to_string(),
        visible_evidence_summary: "The victory screen is visible".to_string(),
        lesson: "The selected action won".to_string(),
    };
    assert_eq!(
        gate.report_outcome(outcome("sha256:before")),
        Err(DecisionError::OutcomeBeforeMutation)
    );
    gate.record_plan(plan_draft("sha256:before".to_string(), click(180, 640)))?;
    gate.prepare_mutation("click", &json!({"x": 180, "y": 640}), "mutation-1")?;
    gate.record_mutation_result("mutation-1", MutationResult::Success)?;
    assert_eq!(
        gate.report_outcome(outcome("sha256:before")),
        Err(DecisionError::MissingPostMutationObservation)
    );
    observe(&gate, "capture-after", "sha256:after")?;
    assert!(matches!(
        gate.report_outcome(outcome("sha256:before")),
        Err(DecisionError::StaleObservation { .. })
    ));
    let reported = gate.report_outcome(outcome("sha256:after"))?;
    assert_eq!(gate.snapshot().outcome, Some(reported));
    Ok(())
}
