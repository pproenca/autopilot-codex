use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::AcceptedPlan;
use crate::AuthorizedMutation;
use crate::CampaignLimits;
use crate::CampaignReport;
use crate::CampaignSummary;
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
use crate::StrategyRecord;
use crate::campaign_report::CampaignReportContext;

use super::CampaignDirective;
use super::CampaignProgress;
use super::CampaignTerminalState;
use super::ContinuationReason;

fn empty_snapshot() -> DecisionSnapshot {
    DecisionSnapshot {
        owner_generation: 1,
        next_observation_generation: 1,
        observation: None,
        plan: None,
        mutation: None,
        outcome: None,
        requires_post_mutation_observation: false,
        batch_actions: 0,
        audit: DecisionAudit::default(),
    }
}

fn limits() -> CampaignLimits {
    CampaignLimits {
        turn_timeout: Duration::from_secs(900),
        post_mutation_timeout: Duration::from_secs(300),
        interrupt_timeout: Duration::from_secs(30),
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
        draft: outcome_draft(outcome),
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
        batch_actions: 1,
        audit: DecisionAudit {
            plans_accepted: 1,
            plan_rejections: 0,
            mutation_attempts: 1,
            mutation_authorizations: 1,
            mutation_denials: 0,
        },
    }
}

fn outcome_draft(outcome: OutcomeKind) -> OutcomeDraft {
    let observation_reference = "sha256:after".to_string();
    let visible_evidence_summary = "The terminal screen is visible".to_string();
    let lesson = "Use the planned action".to_string();
    match outcome {
        OutcomeKind::Loss => OutcomeDraft::Loss {
            observation_reference,
            visible_evidence_summary,
            lesson,
            strategy: StrategyRecord {
                summary: "Change the next attempt".to_string(),
                confirmed_mechanics: Vec::new(),
                failed_approaches: vec!["The previous action lost".to_string()],
                shop_and_boss_notes: Vec::new(),
                next_attempt_priorities: vec!["Try a safer action".to_string()],
            },
        },
        OutcomeKind::Win => OutcomeDraft::Win {
            observation_reference,
            visible_evidence_summary,
            lesson,
        },
        OutcomeKind::TerminalBlock => OutcomeDraft::TerminalBlock {
            observation_reference,
            visible_evidence_summary,
            lesson,
        },
    }
}

#[test]
fn ordinary_turn_completion_requests_another_bounded_turn() -> anyhow::Result<()> {
    let mut progress = CampaignProgress::new(limits());
    assert_eq!(
        progress.on_turn_complete(&empty_snapshot())?,
        CampaignDirective::SubmitContinuation(ContinuationReason::Ordinary)
    );
    progress.on_turn_started("turn-2".to_string())?;
    assert_eq!(
        progress.summary().recent_turn_ids,
        vec!["turn-2".to_string()]
    );
    Ok(())
}

#[test]
fn campaign_has_no_total_turn_limit() -> anyhow::Result<()> {
    let mut progress = CampaignProgress::new(limits());
    for turn in 1..=65 {
        progress.on_turn_started(format!("turn-{turn}"))?;
        assert_eq!(
            progress.on_turn_complete(&empty_snapshot())?,
            CampaignDirective::SubmitContinuation(ContinuationReason::Ordinary)
        );
    }
    assert_eq!(progress.summary().total_turns, 65);
    Ok(())
}

#[test]
fn post_mutation_deadline_clears_after_fresh_evidence() -> anyhow::Result<()> {
    let mut progress = CampaignProgress::new(limits());
    progress.on_turn_started("turn-1".to_string())?;
    let now = Instant::now();
    progress.observe_snapshot(&mutation_snapshot(false, None), now)?;
    assert!(matches!(
        progress.deadline_directive(
            &mutation_snapshot(false, None),
            now + Duration::from_secs(300),
        ),
        Some(CampaignDirective::Block(_))
    ));
    progress.observe_snapshot(&mutation_snapshot(true, None), now + Duration::from_secs(1))?;
    assert_eq!(
        progress.deadline_directive(
            &mutation_snapshot(true, None),
            now + Duration::from_secs(300),
        ),
        None
    );
    Ok(())
}

#[test]
fn reported_outcomes_distinguish_attempt_and_campaign_terminal_states() -> anyhow::Result<()> {
    assert_eq!(
        CampaignProgress::new(limits())
            .on_turn_complete(&mutation_snapshot(true, Some(OutcomeKind::Win)))?,
        CampaignDirective::Complete(CampaignTerminalState::Won)
    );
    assert_eq!(
        CampaignProgress::new(limits())
            .on_turn_complete(&mutation_snapshot(true, Some(OutcomeKind::Loss)))?,
        CampaignDirective::InterruptThenContinue(ContinuationReason::NewAttempt)
    );
    assert!(matches!(
        CampaignProgress::new(limits())
            .on_turn_complete(&mutation_snapshot(true, Some(OutcomeKind::TerminalBlock),))?,
        CampaignDirective::Block(_)
    ));
    Ok(())
}

#[test]
fn report_projects_correlated_evidence_without_image_bytes() {
    let report = CampaignReport::from_snapshot(
        CampaignReportContext {
            terminal_state: CampaignTerminalState::Won,
            thread_id: "thread-1".to_string(),
            summary: CampaignSummary {
                attempt_number: 1,
                total_turns: 1,
                total_actions: 1,
                losses: 0,
                strategy: None,
                recent_turn_ids: vec!["turn-1".to_string()],
            },
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
