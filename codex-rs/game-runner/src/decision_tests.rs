use pretty_assertions::assert_eq;
use serde_json::json;

use super::DecisionAudit;
use super::DecisionError;
use super::DecisionGate;
use super::DecisionSnapshot;
use super::InvalidationReason;
use super::MutationEvidence;
use super::MutationResult;
use super::PlanCandidate;
use super::PlanDraft;
use super::PlannedAction;
use crate::ClickArguments;
use crate::DragArguments;
use crate::FocusClickArguments;
use crate::MouseButton;
use crate::OutcomeDraft;
use crate::MAX_ACTIONS_PER_TURN;

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
            "bd1c262b95a3f95eaf81bc17481f5dcc19a66895cd96af45145e6fcd6363f01e".to_string(),
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
        assert_eq!(
            action.validate(/*width*/ 10, /*height*/ 10),
            Err(DecisionError::InvalidClickCount)
        );
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
    let plan = gate.record_plan(plan_draft(before.reference, click(180, 640)))?;
    let authorized = gate.prepare_mutation("click", &json!({"x": 180, "y": 640}), "mutation-1")?;
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
            batch_actions: 1,
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
fn decision_counters_reject_overflow() {
    assert_eq!(
        super::checked_increment(u64::MAX, "plans_accepted"),
        Err(DecisionError::CounterOverflow {
            counter: "plans_accepted".to_string(),
        })
    );
}

#[test]
fn capture_attempt_and_positive_wait_invalidate_authority() -> anyhow::Result<()> {
    for invalidate in [
        InvalidationReason::CaptureStarted,
        InvalidationReason::PositiveWait,
    ] {
        let gate = DecisionGate::new(1);
        observe(&gate, "capture-before", "sha256:before")?;
        gate.record_plan(plan_draft("sha256:before".to_string(), click(180, 640)))?;
        match invalidate {
            InvalidationReason::CaptureStarted => gate.begin_full_observation(),
            InvalidationReason::PositiveWait => gate.before_wait(Some(&json!({"seconds": 1}))),
            InvalidationReason::TurnAborted
            | InvalidationReason::OwnerGenerationReplaced { .. } => {
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
        (
            snapshot.observation,
            snapshot.plan,
            snapshot.requires_post_mutation_observation
        ),
        (None, None, true)
    );
    assert_eq!(snapshot.audit.mutation_denials, 1);
    Ok(())
}

#[test]
fn repeatable_action_batch_allows_eight_verified_cycles_and_resets() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    let mut reference = "sha256:before".to_string();
    observe(&gate, "capture-before", &reference)?;
    let mut latest_plan = None;
    let mut latest_authorization = None;
    let mut latest_observation = None;
    for action_number in 1..=MAX_ACTIONS_PER_TURN {
        let plan = gate.record_plan(plan_draft(reference, click(180, 640)))?;
        let call_id = format!("mutation-{action_number}");
        let authorization =
            gate.prepare_mutation("click", &json!({"x": 180, "y": 640}), &call_id)?;
        gate.record_mutation_result(&call_id, MutationResult::Success)?;
        reference = format!("sha256:after-{action_number}");
        observe(&gate, &format!("capture-after-{action_number}"), &reference)?;
        latest_plan = Some(plan);
        latest_authorization = Some(authorization);
        latest_observation = gate.snapshot().observation;
    }

    gate.record_plan(plan_draft(reference, click(10, 10)))?;

    assert_eq!(
        gate.prepare_mutation("click", &json!({"x": 10, "y": 10}), "mutation-9"),
        Err(DecisionError::ActionBatchExhausted)
    );
    assert_eq!(
        gate.snapshot(),
        DecisionSnapshot {
            owner_generation: 1,
            next_observation_generation: 10,
            observation: latest_observation,
            plan: None,
            mutation: Some(MutationEvidence {
                plan: latest_plan.expect("eighth plan"),
                authorization: latest_authorization.expect("eighth authorization"),
                result: Some(MutationResult::Success),
            }),
            outcome: None,
            requires_post_mutation_observation: false,
            batch_actions: MAX_ACTIONS_PER_TURN,
            audit: DecisionAudit {
                plans_accepted: 9,
                plan_rejections: 0,
                mutation_attempts: 9,
                mutation_authorizations: 8,
                mutation_denials: 1,
            },
        }
    );

    gate.begin_turn();
    let reset = gate.snapshot();
    assert_eq!(
        (
            reset.owner_generation,
            reset.next_observation_generation,
            reset.observation,
            reset.plan,
            reset.mutation,
            reset.outcome,
            reset.batch_actions,
            reset.audit,
        ),
        (
            1,
            10,
            None,
            None,
            None,
            None,
            0,
            DecisionAudit {
                plans_accepted: 9,
                plan_rejections: 0,
                mutation_attempts: 9,
                mutation_authorizations: 8,
                mutation_denials: 1,
            },
        )
    );
    assert_eq!(
        gate.record_plan(plan_draft("sha256:turn-2".to_string(), click(10, 10))),
        Err(DecisionError::MissingObservation)
    );
    observe(&gate, "capture-turn-2", "sha256:turn-2")?;
    let plan = gate.record_plan(plan_draft("sha256:turn-2".to_string(), click(10, 10)))?;
    assert_eq!(plan.id, "plan-10-10");
    gate.prepare_mutation("click", &json!({"x": 10, "y": 10}), "mutation-10")?;
    Ok(())
}

#[test]
fn interruption_and_owner_replacement_invalidate_plans() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    observe(&gate, "capture-before", "sha256:before")?;
    gate.record_plan(plan_draft("sha256:before".to_string(), click(180, 640)))?;
    gate.invalidate(InvalidationReason::TurnAborted);
    assert_eq!(
        (gate.snapshot().observation, gate.snapshot().plan),
        (None, None)
    );

    observe(&gate, "capture-again", "sha256:again")?;
    gate.record_plan(plan_draft("sha256:again".to_string(), click(180, 640)))?;
    gate.invalidate(InvalidationReason::OwnerGenerationReplaced {
        owner_generation: 2,
    });
    let snapshot = gate.snapshot();
    assert_eq!(
        (
            snapshot.owner_generation,
            snapshot.observation,
            snapshot.plan
        ),
        (2, None, None)
    );
    Ok(())
}

#[test]
fn outcome_requires_the_newest_post_mutation_observation() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    observe(&gate, "capture-before", "sha256:before")?;
    let outcome = |reference: &str| OutcomeDraft::Win {
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
