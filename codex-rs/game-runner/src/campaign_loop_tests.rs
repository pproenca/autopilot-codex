use std::time::Duration;
use std::time::Instant;

use codex_core_api::CallToolResult;
use codex_core_api::McpInvocation;
use codex_core_api::McpToolCallEndEvent;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::ClickArguments;
use crate::CampaignLimits;
use crate::CampaignTerminalState;
use crate::DecisionGate;
use crate::MutationResult;
use crate::ObservationEvidence;
use crate::PlanCandidate;
use crate::PlanDraft;
use crate::PlannedAction;
use crate::ReportedOutcome;
use crate::StrategyRecord;
use crate::OutcomeDraft;
use crate::campaign_progress::CampaignDirective;
use crate::campaign_progress::CampaignProgress;
use crate::campaign_progress::ContinuationReason;

use super::reduce_accepted_outcome;
use super::reduce_turn_aborted;
use super::reduce_turn_complete;
use super::observe_game_call_end;

fn limits() -> CampaignLimits {
    CampaignLimits {
        turn_timeout: Duration::from_secs(15 * 60),
        post_mutation_timeout: Duration::from_secs(5 * 60),
        interrupt_timeout: Duration::from_secs(30),
    }
}

fn reported_outcome(draft: OutcomeDraft) -> ReportedOutcome {
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

fn tool_end(tool: &str, result: Result<CallToolResult, String>) -> McpToolCallEndEvent {
    McpToolCallEndEvent {
        call_id: if tool == "get_app_state" {
            "capture-1".to_string()
        } else {
            "mutation-1".to_string()
        },
        invocation: McpInvocation {
            server: "game".to_string(),
            tool: tool.to_string(),
            arguments: Some(json!({})),
        },
        connector_id: None,
        mcp_app_resource_uri: None,
        link_id: None,
        app_name: None,
        action_name: None,
        plugin_id: None,
        read_only_hint: None,
        duration: Duration::from_millis(5),
        result,
    }
}

fn call_result(content: serde_json::Value, is_error: bool) -> CallToolResult {
    CallToolResult {
        content: Vec::new(),
        structured_content: Some(content),
        is_error: Some(is_error),
        meta: None,
    }
}

fn authorized_gate() -> anyhow::Result<DecisionGate> {
    let gate = DecisionGate::new(1);
    gate.begin_full_observation();
    gate.complete_full_observation(
        "capture-before".to_string(),
        "sha256:before".to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;
    gate.record_plan(PlanDraft {
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
        chosen_action: PlannedAction::Click(ClickArguments {
            x: 180,
            y: 640,
            button: None,
            count: None,
        }),
        reason: "Reversible".to_string(),
        expected_visible_result: "A safe screen".to_string(),
        invalidation_condition: "The menu changes".to_string(),
    })?;
    gate.prepare_mutation("click", &json!({"x": 180, "y": 640}), "mutation-1")?;
    Ok(gate)
}

#[test]
fn game_call_reducer_installs_only_complete_full_frame_evidence() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    gate.begin_full_observation();
    observe_game_call_end(
        &gate,
        &tool_end(
            "get_app_state",
            Ok(call_result(
                json!({"artifact_uri": "sha256:after", "width": 1051, "height": 820}),
                false,
            )),
        ),
    )?;
    assert_eq!(
        gate.snapshot().observation,
        Some(ObservationEvidence {
            generation: 1,
            call_id: "capture-1".to_string(),
            reference: "sha256:after".to_string(),
            width: 1051,
            height: 820,
        })
    );

    gate.begin_full_observation();
    observe_game_call_end(
        &gate,
        &tool_end(
            "get_app_state",
            Ok(call_result(
                json!({"artifact_uri": "sha256:incomplete", "width": 1051}),
                false,
            )),
        ),
    )?;
    assert_eq!(gate.snapshot().observation, None);
    Ok(())
}

#[test]
fn game_call_reducer_classifies_authorized_mutation_results() -> anyhow::Result<()> {
    for (result, expected) in [
        (
            Ok(call_result(json!({"clicked": true}), false)),
            MutationResult::Success,
        ),
        (
            Ok(call_result(json!({"clicked": false}), true)),
            MutationResult::CleanFailure,
        ),
        (
            Err("connection closed".to_string()),
            MutationResult::Indeterminate,
        ),
    ] {
        let gate = authorized_gate()?;
        observe_game_call_end(&gate, &tool_end("click", result))?;
        assert_eq!(
            gate.snapshot()
                .mutation
                .and_then(|mutation| mutation.result),
            Some(expected)
        );
    }
    Ok(())
}

#[test]
fn turn_reducers_continue_normally_and_only_accept_expected_aborts() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    let mut progress = CampaignProgress::new(limits());
    assert_eq!(
        reduce_turn_complete(&mut progress, &gate.snapshot())?,
        CampaignDirective::SubmitContinuation(ContinuationReason::Ordinary)
    );
    assert!(matches!(
        reduce_turn_aborted(&mut progress)?,
        CampaignDirective::Block(_)
    ));

    progress.begin_interrupt(ContinuationReason::NewAttempt, Instant::now())?;
    assert_eq!(
        reduce_turn_aborted(&mut progress)?,
        CampaignDirective::SubmitContinuation(ContinuationReason::NewAttempt)
    );
    Ok(())
}

#[test]
fn accepted_outcomes_continue_losses_but_finish_campaign_terminals() -> anyhow::Result<()> {
    let loss = reported_outcome(OutcomeDraft::Loss {
        observation_reference: "sha256:loss".to_string(),
        visible_evidence_summary: "The loss screen is visible".to_string(),
        lesson: "The build lacked mobility".to_string(),
        strategy: StrategyRecord {
            summary: "Prioritize mobility".to_string(),
            confirmed_mechanics: Vec::new(),
            failed_approaches: vec!["Static defense".to_string()],
            shop_and_boss_notes: Vec::new(),
            next_attempt_priorities: vec!["Buy movement".to_string()],
        },
    });
    assert_eq!(
        reduce_accepted_outcome(&mut CampaignProgress::new(limits()), &loss)?,
        CampaignDirective::InterruptThenContinue(ContinuationReason::NewAttempt)
    );

    let win = reported_outcome(OutcomeDraft::Win {
        observation_reference: "sha256:win".to_string(),
        visible_evidence_summary: "The victory screen is visible".to_string(),
        lesson: "The boss is defeated".to_string(),
    });
    assert_eq!(
        reduce_accepted_outcome(&mut CampaignProgress::new(limits()), &win)?,
        CampaignDirective::Complete(CampaignTerminalState::Won)
    );

    let terminal_block = reported_outcome(OutcomeDraft::TerminalBlock {
        observation_reference: "sha256:block".to_string(),
        visible_evidence_summary: "The helper disconnected".to_string(),
        lesson: "Physical state is unresolved".to_string(),
    });
    assert_eq!(
        reduce_accepted_outcome(
            &mut CampaignProgress::new(limits()),
            &terminal_block,
        )?,
        CampaignDirective::Block("The helper disconnected".to_string())
    );
    Ok(())
}
