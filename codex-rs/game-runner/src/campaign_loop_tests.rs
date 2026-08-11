use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use codex_core_api::McpInvocation;
use codex_core_api::McpToolCallEndEvent;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::broadcast::error::TryRecvError;

use super::CampaignExecutionContext;
use super::SafeBoundary;
use super::SafeBoundaryDirective;
use super::initialize_campaign_start;
use crate::AcceptedPlan;
use crate::CampaignEvent;
use crate::CampaignLimits;
use crate::CampaignPersistence;
use crate::CampaignStart;
use crate::CampaignSummary;
use crate::CampaignTerminalState;
use crate::ClickArguments;
use crate::DecisionGate;
use crate::GameCallPolicy;
use crate::MutationCheckpointUpdate;
use crate::MutationResult;
use crate::OutcomeDraft;
use crate::PlanCandidate;
use crate::PlanDraft;
use crate::PlannedAction;
use crate::ReportedOutcome;
use crate::StrategyRecord;
use crate::WorkerCommand;
use crate::WorkerDirective;
use crate::campaign::CampaignDirective;
use crate::campaign::ContinuationReason;
use crate::campaign_persistence::tests::checkpoint;
use crate::campaign_persistence::tests::store;
use crate::campaign_prompt::ResumePromptContext;
use crate::campaign_prompt::initial_prompt;
use crate::campaign_prompt::resume_prompt;

#[derive(Debug, PartialEq, Eq)]
enum DurableOperation {
    Persist,
    Publish(Box<CampaignEvent>),
}

fn published(event: CampaignEvent) -> DurableOperation {
    DurableOperation::Publish(Box::new(event))
}

#[test]
fn pause_and_stop_wait_for_the_exact_active_game_call_boundary() -> anyhow::Result<()> {
    for (tool, command) in [
        ("get_app_state", WorkerCommand::Pause),
        ("click", WorkerCommand::Stop),
        ("drag", WorkerCommand::Pause),
    ] {
        let mut boundary = SafeBoundary::default();
        boundary.begin_game_call(format!("{tool}-active"))?;
        assert_eq!(
            boundary.request(command),
            Ok(SafeBoundaryDirective::WaitForActiveCall)
        );
        assert_eq!(
            boundary.finish_game_call("unrelated-call"),
            Ok(SafeBoundaryDirective::None)
        );
        assert_eq!(
            boundary.finish_game_call(&format!("{tool}-active")),
            Ok(SafeBoundaryDirective::Interrupt)
        );
        assert_eq!(boundary.finish_turn(), Some(command));
    }

    let mut boundary = SafeBoundary::default();
    assert_eq!(
        boundary.request(WorkerCommand::Stop),
        Ok(SafeBoundaryDirective::Interrupt)
    );
    assert_eq!(boundary.finish_turn(), Some(WorkerCommand::Stop));
    Ok(())
}

#[tokio::test]
async fn failed_game_tool_waits_for_the_controller_directive() -> anyhow::Result<()> {
    let (_codex_home, store, _guard) = store()?;
    let (events, _) = tokio::sync::broadcast::channel(1);
    let (failure_tx, mut failure_rx) = tokio::sync::mpsc::channel(1);
    let context = CampaignExecutionContext::Durable {
        persistence: Arc::new(CampaignPersistence::empty(store)),
        events,
        commands: None,
        failures: Some(failure_tx),
        start: CampaignStart::Fresh {
            target_app: "Difficult Game".to_string(),
        },
    };
    let controller = tokio::spawn(async move {
        for directive in [WorkerDirective::Continue, WorkerDirective::PauseForRecovery] {
            let signal = failure_rx.recv().await.expect("failure signal");
            assert_eq!(signal.tool, "click");
            assert_eq!(signal.summary, "socket disconnected");
            signal
                .response
                .send(directive)
                .expect("worker should await the directive");
        }
    });
    let failure = failed_tool_end();

    assert_eq!(
        context.game_tool_failure_directive(&failure).await?,
        Some(WorkerDirective::Continue)
    );
    assert_eq!(
        context.game_tool_failure_directive(&failure).await?,
        Some(WorkerDirective::PauseForRecovery)
    );
    controller.await?;
    Ok(())
}

#[test]
fn fresh_and_resumed_starts_choose_the_correct_progress_and_prompt() -> anyhow::Result<()> {
    let limits = CampaignLimits::stage_4b1();
    let fresh = CampaignStart::Fresh {
        target_app: "Difficult Game".to_string(),
    };
    let (fresh_progress, fresh_prompt) = initialize_campaign_start(&fresh, limits)?;
    assert_eq!(
        fresh_progress.summary(),
        CampaignSummary {
            attempt_number: 1,
            total_turns: 0,
            total_actions: 0,
            losses: 0,
            strategy: None,
            recent_turn_ids: Vec::new(),
        }
    );
    assert_eq!(fresh_prompt, initial_prompt("Difficult Game"));

    let mut restored_checkpoint = checkpoint();
    restored_checkpoint.summary.strategy = Some(strategy());
    let resumed = CampaignStart::Resumed {
        checkpoint: Box::new(restored_checkpoint.clone()),
    };
    let (restored_progress, restored_prompt) = initialize_campaign_start(&resumed, limits)?;
    assert_eq!(restored_progress.summary(), restored_checkpoint.summary);
    assert_eq!(
        restored_prompt,
        resume_prompt(ResumePromptContext {
            attempt_number: restored_checkpoint.summary.attempt_number,
            strategy: restored_checkpoint.summary.strategy.as_ref(),
            unresolved_mutation: restored_checkpoint.unresolved_mutation.as_ref(),
        })?
    );
    Ok(())
}

#[tokio::test]
async fn durable_activity_is_persisted_before_publication_and_win_is_deferred() -> anyhow::Result<()>
{
    let (_codex_home, store, _guard) = store()?;
    let persistence = Arc::new(CampaignPersistence::empty(store));
    let initial = checkpoint();
    persistence.install(initial.clone()).await?;
    let (events, mut event_rx) = tokio::sync::broadcast::channel(16);
    let context = CampaignExecutionContext::Durable {
        persistence: Arc::clone(&persistence),
        events,
        commands: None,
        failures: None,
        start: CampaignStart::Resumed {
            checkpoint: Box::new(initial),
        },
    };
    let gate = Arc::new(DecisionGate::new(1));
    let policy = GameCallPolicy::new(
        "11111111-1111-4111-8111-111111111111".to_string(),
        1,
        Arc::clone(&gate),
    );
    let mut operations = Vec::new();

    let running_summary = CampaignSummary {
        total_turns: 2,
        recent_turn_ids: vec!["turn-1".to_string(), "turn-2".to_string()],
        ..summary(/*attempt_number*/ 1, /*losses*/ 0)
    };
    context
        .record_progress(&running_summary, gate.as_ref(), &policy)
        .await
        .context("record progress")?;
    operations.extend([DurableOperation::Persist, published(event_rx.recv().await?)]);
    assert_eq!(persistence.snapshot().await?.summary, running_summary);

    let plan = accepted_plan(gate.as_ref())?;
    context
        .record_plan(&running_summary, &plan, gate.as_ref(), &policy)
        .await
        .context("record plan")?;
    operations.extend([DurableOperation::Persist, published(event_rx.recv().await?)]);
    assert_eq!(
        persistence.snapshot().await?.decision_audit,
        gate.snapshot().audit
    );

    let authorization =
        gate.prepare_mutation("click", &json!({"x": 180, "y": 640}), "mutation-1")?;
    persistence
        .begin_mutation(&MutationCheckpointUpdate {
            authorization: authorization.clone(),
            decision_audit: gate.snapshot().audit,
            policy_audit: policy.audit(),
        })
        .await?;
    operations.push(DurableOperation::Persist);
    context
        .record_mutation(&authorization, &policy)
        .await
        .context("record mutation authorization")?;
    operations.push(published(event_rx.recv().await?));

    gate.record_mutation_result(&authorization.call_id, MutationResult::Success)?;
    context
        .record_mutation_finished(&authorization.call_id, MutationResult::Success, &policy)
        .await
        .context("record mutation result")?;
    operations.extend([DurableOperation::Persist, published(event_rx.recv().await?)]);

    gate.begin_full_observation();
    gate.complete_full_observation(
        "capture-after".to_string(),
        "sha256:after".to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;
    let observation = gate.snapshot().observation.expect("fresh observation");
    context
        .record_observation(&observation, &policy)
        .await
        .context("record observation")?;
    operations.extend([DurableOperation::Persist, published(event_rx.recv().await?)]);
    assert_eq!(persistence.snapshot().await?.unresolved_mutation, None);

    let loss = ReportedOutcome {
        observation: observation.clone(),
        draft: OutcomeDraft::Loss {
            observation_reference: observation.reference.clone(),
            visible_evidence_summary: "The loss screen is visible".to_string(),
            lesson: "The build lacked mobility".to_string(),
            strategy: strategy(),
        },
    };
    let loss_summary = CampaignSummary {
        attempt_number: 2,
        total_turns: 2,
        total_actions: 1,
        losses: 1,
        strategy: Some(strategy()),
        recent_turn_ids: running_summary.recent_turn_ids.clone(),
    };
    context
        .record_outcome(
            &loss_summary,
            &loss,
            &CampaignDirective::InterruptThenContinue(ContinuationReason::NewAttempt),
            gate.as_ref(),
            &policy,
        )
        .await
        .context("record loss")?;
    operations.extend([
        DurableOperation::Persist,
        published(event_rx.recv().await?),
        published(event_rx.recv().await?),
    ]);
    assert_eq!(persistence.snapshot().await?.summary, loss_summary);

    let win = ReportedOutcome {
        observation,
        draft: OutcomeDraft::Win {
            observation_reference: "sha256:after".to_string(),
            visible_evidence_summary: "The full victory screen is visible".to_string(),
            lesson: "The boss is defeated".to_string(),
        },
    };
    let before_win = persistence.snapshot().await?;
    context
        .record_outcome(
            &loss_summary,
            &win,
            &CampaignDirective::Complete(CampaignTerminalState::Won),
            gate.as_ref(),
            &policy,
        )
        .await
        .context("defer win")?;
    assert_eq!(persistence.snapshot().await?, before_win);
    assert_eq!(event_rx.try_recv(), Err(TryRecvError::Empty));

    assert_eq!(
        operations,
        vec![
            DurableOperation::Persist,
            published(CampaignEvent::Progress(running_summary.clone())),
            DurableOperation::Persist,
            published(CampaignEvent::Plan(plan)),
            DurableOperation::Persist,
            published(CampaignEvent::Mutation(authorization)),
            DurableOperation::Persist,
            published(CampaignEvent::MutationFinished(MutationResult::Success)),
            DurableOperation::Persist,
            published(CampaignEvent::Observation(
                gate.snapshot().observation.expect("observation")
            )),
            DurableOperation::Persist,
            published(CampaignEvent::Outcome(loss)),
            published(CampaignEvent::Progress(loss_summary)),
        ]
    );
    Ok(())
}

fn failed_tool_end() -> McpToolCallEndEvent {
    McpToolCallEndEvent {
        call_id: "mutation-1".to_string(),
        invocation: McpInvocation {
            server: "game".to_string(),
            tool: "click".to_string(),
            arguments: None,
        },
        connector_id: None,
        mcp_app_resource_uri: None,
        link_id: None,
        app_name: None,
        action_name: None,
        plugin_id: None,
        read_only_hint: None,
        duration: Duration::ZERO,
        result: Err("socket disconnected".to_string()),
    }
}

fn accepted_plan(gate: &DecisionGate) -> anyhow::Result<AcceptedPlan> {
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
    })
    .map_err(Into::into)
}

fn summary(attempt_number: u64, losses: u64) -> CampaignSummary {
    CampaignSummary {
        attempt_number,
        total_turns: 1,
        total_actions: 0,
        losses,
        strategy: None,
        recent_turn_ids: vec!["turn-1".to_string()],
    }
}

fn strategy() -> StrategyRecord {
    StrategyRecord {
        summary: "Prioritize mobility".to_string(),
        confirmed_mechanics: vec!["Shops precede bosses".to_string()],
        failed_approaches: vec!["Static defense".to_string()],
        shop_and_boss_notes: vec!["Keep one reroll".to_string()],
        next_attempt_priorities: vec!["Buy movement".to_string()],
    }
}
