use std::path::PathBuf;
use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::AcceptedPlan;
use crate::AuthorizedMutation;
use crate::CampaignReport;
use crate::ClickArguments;
use crate::DecisionAudit;
use crate::DecisionSnapshot;
use crate::MutationEvidence;
use crate::MutationResult;
use crate::ObservationEvidence;
use crate::OutcomeDraft;
use crate::OutcomeKind;
use crate::OwnerLease;
use crate::PlanCandidate;
use crate::PlanDraft;
use crate::PlannedAction;
use crate::PolicyAudit;
use crate::ReportedOutcome;
use crate::campaign_report::CampaignReportContext;

use super::CampaignDirective;
use super::CampaignLimits;
use super::CampaignProgress;
use super::CampaignTerminalState;

fn empty_snapshot() -> DecisionSnapshot {
    DecisionSnapshot {
        owner_generation: 1,
        next_observation_generation: 1,
        observation: None,
        plan: None,
        mutation: None,
        outcome: None,
        requires_post_mutation_observation: false,
        audit: DecisionAudit::default(),
    }
}

fn limits() -> CampaignLimits {
    CampaignLimits {
        max_turns: 6,
        total_timeout: Duration::from_secs(900),
        post_mutation_timeout: Duration::from_secs(300),
    }
}

fn mutation_snapshot(after: bool, outcome: Option<OutcomeKind>) -> DecisionSnapshot {
    let before = ObservationEvidence {
        generation: 1,
        call_id: "capture-before".to_string(),
        reference: "sha256:before".to_string(),
        width: 1051,
        height: 820,
    };
    let action = PlannedAction::Click(ClickArguments {
        x: 180,
        y: 640,
        button: None,
        count: None,
    });
    let plan = AcceptedPlan {
        id: "plan-1-1".to_string(),
        observation: before,
        draft: PlanDraft {
            observation_reference: "sha256:before".to_string(),
            objective: "Open a safe menu".to_string(),
            visible_state_summary: "The menu is visible".to_string(),
            candidates: vec![
                PlanCandidate {
                    action: "Settings".to_string(),
                    predicted_visible_consequence: "Settings appears".to_string(),
                },
                PlanCandidate {
                    action: "Credits".to_string(),
                    predicted_visible_consequence: "Credits appears".to_string(),
                },
            ],
            chosen_action: action,
            reason: "Reversible".to_string(),
            expected_visible_result: "A safe screen".to_string(),
            invalidation_condition: "The menu changes".to_string(),
        },
        action_sha256: "f709b60a7bbf91028aa10498db469d2a47fd96669d5167f6163200718809b3e1"
            .to_string(),
    };
    let authorization = AuthorizedMutation {
        call_id: "mutation-1".to_string(),
        operation_id: "mutation-1".to_string(),
        action_sha256: plan.action_sha256.clone(),
        tool: "click".to_string(),
        arguments: json!({"x": 180, "y": 640}),
    };
    let after_observation = after.then(|| ObservationEvidence {
        generation: 2,
        call_id: "capture-after".to_string(),
        reference: "sha256:after".to_string(),
        width: 1051,
        height: 820,
    });
    let reported = outcome.map(|outcome| ReportedOutcome {
        observation: after_observation
            .clone()
            .expect("outcome needs after evidence"),
        draft: OutcomeDraft {
            outcome,
            observation_reference: "sha256:after".to_string(),
            visible_evidence_summary: "The terminal screen is visible".to_string(),
            lesson: "Use the planned action".to_string(),
        },
    });
    DecisionSnapshot {
        owner_generation: 1,
        next_observation_generation: if after { 3 } else { 2 },
        observation: after_observation,
        plan: None,
        mutation: Some(MutationEvidence {
            plan,
            authorization,
            result: Some(MutationResult::Success),
        }),
        outcome: reported,
        requires_post_mutation_observation: !after,
        audit: DecisionAudit {
            plans_accepted: 1,
            plan_rejections: 0,
            mutation_attempts: 1,
            mutation_authorizations: 1,
            mutation_denials: 0,
        },
    }
}

#[test]
fn early_turn_completion_continues_until_after_evidence_exists() {
    let mut progress = CampaignProgress::new(limits());
    assert_eq!(
        progress.on_turn_complete(&empty_snapshot()),
        CampaignDirective::Continue
    );
    progress.on_turn_started("turn-2".to_string());
    assert_eq!(progress.turn_ids(), &["turn-2".to_string()]);
}

#[test]
fn sixth_turn_is_the_last_allowed_turn() {
    let mut progress = CampaignProgress::new(limits());
    for turn in 1..6 {
        progress.on_turn_started(format!("turn-{turn}"));
        assert_eq!(
            progress.on_turn_complete(&empty_snapshot()),
            CampaignDirective::Continue
        );
    }
    progress.on_turn_started("turn-6".to_string());
    assert!(matches!(
        progress.on_turn_complete(&empty_snapshot()),
        CampaignDirective::Block(_)
    ));
}

#[test]
fn fresh_after_evidence_without_model_confirmation_blocks_canary() {
    let mut progress = CampaignProgress::new(limits());
    progress.on_turn_started("turn-1".to_string());
    assert_eq!(
        progress.on_turn_complete(&mutation_snapshot(false, None)),
        CampaignDirective::Continue
    );
    let deadline = progress.next_deadline();
    assert!(matches!(
        progress.deadline_directive(&mutation_snapshot(false, None), deadline),
        Some(CampaignDirective::Block(_))
    ));
    assert_eq!(
        progress.on_turn_complete(&mutation_snapshot(true, None)),
        CampaignDirective::Block(
            "fresh post-mutation evidence was not classified by the model".to_string()
        )
    );
}

#[test]
fn reported_outcomes_map_to_terminal_states() {
    for (outcome, state) in [
        (
            OutcomeKind::CanaryComplete,
            CampaignTerminalState::CanaryComplete,
        ),
        (OutcomeKind::Win, CampaignTerminalState::Won),
        (OutcomeKind::Loss, CampaignTerminalState::LossObserved),
        (
            OutcomeKind::TerminalBlock,
            CampaignTerminalState::TerminalBlock,
        ),
    ] {
        assert_eq!(
            CampaignProgress::new(limits())
                .on_turn_complete(&mutation_snapshot(true, Some(outcome))),
            CampaignDirective::Complete(state)
        );
    }
}

#[test]
fn report_projects_correlated_evidence_without_image_bytes() {
    let report = CampaignReport::from_snapshot(
        CampaignReportContext {
            terminal_state: CampaignTerminalState::Won,
            thread_id: "thread-1".to_string(),
            turn_ids: vec!["turn-1".to_string()],
            rollout_path: PathBuf::from("/rollouts/thread-1.jsonl"),
            owner_lease: OwnerLease {
                epoch: "epoch-1".to_string(),
                generation: 1,
            },
            policy_audit: PolicyAudit {
                mutation_attempts: 1,
                unknown_tool_attempts: 0,
                mutation_authorizations: 1,
            },
            terminal_failure: None,
        },
        mutation_snapshot(true, Some(OutcomeKind::Win)),
    );
    let encoded = serde_json::to_string(&report).expect("campaign report serializes");
    assert_eq!(
        report.before.as_ref().map(|value| value.reference.as_str()),
        Some("sha256:before")
    );
    assert_eq!(
        report.after.as_ref().map(|value| value.reference.as_str()),
        Some("sha256:after")
    );
    assert!(!encoded.contains("base64"));
    assert!(!encoded.contains("image_url"));
}
