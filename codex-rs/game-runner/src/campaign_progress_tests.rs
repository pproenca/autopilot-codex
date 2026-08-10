use std::time::Duration;
use std::time::Instant;

use pretty_assertions::assert_eq;
use serde_json::json;

use super::CampaignDirective;
use super::CampaignProgress;
use super::CampaignProgressError;
use super::CampaignSummary;
use super::ContinuationReason;
use super::checked_increment;
use crate::CampaignLimits;
use crate::CampaignTerminalState;
use crate::ClickArguments;
use crate::DecisionAudit;
use crate::DecisionGate;
use crate::DecisionSnapshot;
use crate::MutationResult;
use crate::ObservationEvidence;
use crate::OutcomeDraft;
use crate::PlanCandidate;
use crate::PlanDraft;
use crate::PlannedAction;
use crate::ReportedOutcome;
use crate::StrategyRecord;

fn limits() -> CampaignLimits {
    CampaignLimits {
        turn_timeout: Duration::from_secs(15 * 60),
        post_mutation_timeout: Duration::from_secs(5 * 60),
        interrupt_timeout: Duration::from_secs(30),
    }
}

fn strategy(summary: &str) -> StrategyRecord {
    StrategyRecord {
        summary: summary.to_string(),
        confirmed_mechanics: vec!["Bosses follow shops".to_string()],
        failed_approaches: Vec::new(),
        shop_and_boss_notes: Vec::new(),
        next_attempt_priorities: vec!["Preserve mobility".to_string()],
    }
}

fn outcome(draft: OutcomeDraft) -> ReportedOutcome {
    ReportedOutcome {
        observation: ObservationEvidence {
            generation: 2,
            call_id: "capture-after".to_string(),
            reference: draft.observation_reference().to_string(),
            width: 1051,
            height: 820,
        },
        draft,
    }
}

fn loss_outcome(reference: &str, strategy: StrategyRecord) -> ReportedOutcome {
    outcome(OutcomeDraft::Loss {
        observation_reference: reference.to_string(),
        visible_evidence_summary: "The loss screen is visible".to_string(),
        lesson: "The previous build could not survive the boss".to_string(),
        strategy,
    })
}

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

fn gate_with_unresolved_mutation() -> anyhow::Result<DecisionGate> {
    let gate = DecisionGate::new(1);
    gate.complete_full_observation(
        "capture-before".to_string(),
        "sha256:before".to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;
    gate.record_plan(PlanDraft {
        observation_reference: "sha256:before".to_string(),
        objective: "Advance the game".to_string(),
        visible_state_summary: "A playable board is visible".to_string(),
        candidates: vec![
            PlanCandidate {
                action: "Advance".to_string(),
                predicted_visible_consequence: "The board advances".to_string(),
            },
            PlanCandidate {
                action: "Wait".to_string(),
                predicted_visible_consequence: "The board remains".to_string(),
            },
        ],
        chosen_action: PlannedAction::Click(ClickArguments {
            x: 180,
            y: 640,
            button: None,
            count: None,
        }),
        reason: "Advance is the best visible move".to_string(),
        expected_visible_result: "The next board state".to_string(),
        invalidation_condition: "The board changes before the click".to_string(),
    })?;
    gate.prepare_mutation("click", &json!({"x": 180, "y": 640}), "mutation-1")?;
    Ok(gate)
}

#[test]
fn two_losses_replace_strategy_and_never_complete_the_campaign() -> anyhow::Result<()> {
    let mut progress = CampaignProgress::new(limits());
    progress.on_turn_started("turn-1".to_string())?;
    assert_eq!(
        progress.accept_outcome(&loss_outcome("sha256:loss-1", strategy("economy")))?,
        CampaignDirective::InterruptThenContinue(ContinuationReason::NewAttempt)
    );
    progress.on_turn_started("turn-2".to_string())?;
    assert_eq!(
        progress.accept_outcome(&loss_outcome("sha256:loss-2", strategy("mobility")))?,
        CampaignDirective::InterruptThenContinue(ContinuationReason::NewAttempt)
    );
    assert_eq!(
        progress.summary(),
        CampaignSummary {
            attempt_number: 3,
            total_turns: 2,
            total_actions: 0,
            losses: 2,
            strategy: Some(strategy("mobility")),
            recent_turn_ids: vec!["turn-1".to_string(), "turn-2".to_string()],
        }
    );
    Ok(())
}

#[test]
fn recent_turn_ids_are_bounded_and_validated_before_retention() -> anyhow::Result<()> {
    let mut progress = CampaignProgress::new(limits());
    for turn in 1..=65 {
        progress.on_turn_started(format!("turn-{turn}"))?;
    }
    assert_eq!(progress.summary().recent_turn_ids.len(), 64);
    assert_eq!(progress.summary().recent_turn_ids[0], "turn-2");
    assert_eq!(
        progress.on_turn_started("x".repeat(2049)),
        Err(CampaignProgressError::TurnIdTooLarge)
    );
    assert_eq!(progress.summary().total_turns, 65);
    Ok(())
}

#[test]
fn win_completes_and_terminal_block_blocks() -> anyhow::Result<()> {
    let mut won = CampaignProgress::new(limits());
    let win = outcome(OutcomeDraft::Win {
        observation_reference: "sha256:win".to_string(),
        visible_evidence_summary: "The full victory screen is visible".to_string(),
        lesson: "The final boss was defeated".to_string(),
    });
    assert_eq!(
        won.accept_outcome(&win)?,
        CampaignDirective::Complete(CampaignTerminalState::Won)
    );

    let mut blocked = CampaignProgress::new(limits());
    let terminal_block = outcome(OutcomeDraft::TerminalBlock {
        observation_reference: "sha256:block".to_string(),
        visible_evidence_summary: "The helper is unavailable".to_string(),
        lesson: "The game cannot be controlled".to_string(),
    });
    assert_eq!(
        blocked.accept_outcome(&terminal_block)?,
        CampaignDirective::Block("The helper is unavailable".to_string())
    );
    Ok(())
}

#[test]
fn action_audits_are_monotonic_and_checked() -> anyhow::Result<()> {
    let mut progress = CampaignProgress::new(limits());
    let mut snapshot = empty_snapshot();
    snapshot.audit.mutation_authorizations = 3;
    progress.observe_snapshot(&snapshot, Instant::now())?;
    assert_eq!(progress.summary().total_actions, 3);

    snapshot.audit.mutation_authorizations = 2;
    assert_eq!(
        progress.observe_snapshot(&snapshot, Instant::now()),
        Err(CampaignProgressError::ActionAuditRegressed {
            previous: 3,
            actual: 2,
        })
    );
    assert_eq!(
        checked_increment(u64::MAX, "losses"),
        Err(CampaignProgressError::CounterOverflow { counter: "losses" })
    );
    Ok(())
}

#[test]
fn deadlines_distinguish_post_mutation_turn_and_expected_interrupts() -> anyhow::Result<()> {
    let now = Instant::now();
    let mut progress = CampaignProgress::new(limits());
    progress.on_turn_started("turn-1".to_string())?;
    let gate = gate_with_unresolved_mutation()?;
    progress.observe_snapshot(&gate.snapshot(), now)?;
    assert!(matches!(
        progress.deadline_directive(
            &gate.snapshot(),
            now + Duration::from_secs(5 * 60),
        ),
        Some(CampaignDirective::Block(_))
    ));

    gate.record_mutation_result("mutation-1", MutationResult::Success)?;
    gate.complete_full_observation(
        "capture-after".to_string(),
        "sha256:after".to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;
    progress.observe_snapshot(&gate.snapshot(), now + Duration::from_secs(1))?;
    assert_eq!(
        progress.deadline_directive(
            &gate.snapshot(),
            now + Duration::from_secs(5 * 60),
        ),
        None
    );

    let turn_deadline = progress.next_deadline();
    assert_eq!(
        progress.deadline_directive(&gate.snapshot(), turn_deadline),
        Some(CampaignDirective::InterruptThenContinue(
            ContinuationReason::TurnTimeout,
        ))
    );
    progress.begin_interrupt(ContinuationReason::TurnTimeout, turn_deadline)?;
    assert!(matches!(
        progress.deadline_directive(
            &gate.snapshot(),
            turn_deadline + Duration::from_secs(30),
        ),
        Some(CampaignDirective::Block(_))
    ));
    assert_eq!(
        progress.complete_expected_interrupt()?,
        CampaignDirective::SubmitContinuation(ContinuationReason::TurnTimeout)
    );
    Ok(())
}
