use std::sync::Arc;
use std::time::Instant;

use codex_core_api::CodexThread;
use codex_core_api::EventMsg;
use codex_core_api::Op;
use codex_core_api::SessionConfiguredEvent;

use super::CampaignDirective;
use super::CampaignExecutionContext;
use super::CampaignExit;
use super::CampaignRun;
use super::SafeBoundary;
use super::SafeBoundaryDirective;
use super::WorkerCommand;
use super::campaign_compaction::CampaignCompaction;
use super::campaign_event::begin_safe_interrupt;
use super::campaign_event::block_exit as block_report;
use super::campaign_event::build_exit as build_report;
use super::campaign_event::initialize_campaign_start;
use super::campaign_event::reduce_turn_aborted;
use super::campaign_event::reduce_turn_complete;
use super::campaign_dynamic_tool::prepare_dynamic_tool_response;
use super::campaign_game_call::GameCallEndDirective;
use super::campaign_game_call::finish_game_call_event;
use super::campaign_submit::campaign_submit_error;
use super::campaign_submit::submit_continuation;
use super::campaign_submit::submit_turn;
use super::campaign_submit::submit_worker_interrupt;
use super::campaign_submit::worker_command_exit;
use crate::CampaignTools;
use crate::DecisionGate;
use crate::GAME_SERVER_NAME;
use crate::GameCallPolicy;
use crate::InvalidationReason;
use crate::RunnerError;

impl CampaignRun {
    pub(crate) async fn execute_controlled(
        &self,
        thread: &CodexThread,
        session: &SessionConfiguredEvent,
        policy: &GameCallPolicy,
        gate: Arc<DecisionGate>,
        mut context: CampaignExecutionContext,
    ) -> Result<CampaignExit, RunnerError> {
        let (mut progress, prompt) = initialize_campaign_start(context.start(), self.limits)?;
        submit_turn(thread, &gate, &prompt).await?;
        let tools = CampaignTools::new(Arc::clone(&gate));
        let mut safe_boundary = SafeBoundary::default();
        let mut recovery_pending = false;
        let mut compaction = CampaignCompaction::default();
        let mut worker_exit_deadline: Option<tokio::time::Instant> = None;

        loop {
            let progress_deadline = tokio::time::Instant::from_std(progress.next_deadline());
            let deadline = worker_exit_deadline
                .map_or(progress_deadline, |deadline| deadline.min(progress_deadline));
            let event_result = tokio::select! {
                event = thread.next_event() => Some(event),
                _ = tokio::time::sleep_until(deadline) => None,
                command = context.next_worker_command(), if context.has_worker_commands() => {
                    let Some(command) = command else {
                        continue;
                    };
                    if command == WorkerCommand::Compact {
                        compaction.request();
                        continue;
                    }
                    policy.close_mutation_lane();
                    match safe_boundary.request(command) {
                        Ok(SafeBoundaryDirective::Interrupt) => {
                            if let Err(error) = submit_worker_interrupt(thread).await {
                                return block_report(
                                    session,
                                    &progress,
                                    policy,
                                    &gate,
                                    error.to_string(),
                                );
                            }
                            worker_exit_deadline =
                                Some(tokio::time::Instant::now() + self.limits.interrupt_timeout);
                        }
                        Ok(
                            SafeBoundaryDirective::None
                            | SafeBoundaryDirective::WaitForActiveCall,
                        ) => {}
                        Err(error) => {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                error.to_string(),
                            );
                        }
                    }
                    continue;
                }
            };
            let event = match event_result {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    return block_report(
                        session,
                        &progress,
                        policy,
                        &gate,
                        format!("failed to read campaign event: {error}"),
                    );
                }
                None => {
                    if recovery_pending {
                        return Ok(CampaignExit::RecoveryRequired);
                    }
                    if let Some(command) = safe_boundary.finish_turn() {
                        gate.invalidate(InvalidationReason::TurnAborted);
                        return Ok(worker_command_exit(command));
                    }
                    let directive = progress.deadline_directive(&gate.snapshot(), Instant::now());
                    match directive {
                        Some(CampaignDirective::InterruptThenContinue(reason)) => {
                            if let Err(error) =
                                begin_safe_interrupt(thread, &mut progress, reason).await
                            {
                                return block_report(
                                    session,
                                    &progress,
                                    policy,
                                    &gate,
                                    error.to_string(),
                                );
                            }
                            continue;
                        }
                        Some(CampaignDirective::Block(reason)) => {
                            return block_report(session, &progress, policy, &gate, reason);
                        }
                        Some(
                            CampaignDirective::SubmitContinuation(_)
                            | CampaignDirective::Complete(_),
                        )
                        | None => {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                "campaign deadline elapsed without a valid transition".to_string(),
                            );
                        }
                    }
                }
            };

            match event.msg {
                EventMsg::TurnStarted(event) => {
                    if compaction.is_active() {
                        continue;
                    }
                    if let Err(error) = progress.on_turn_started(event.turn_id) {
                        return block_report(session, &progress, policy, &gate, error.to_string());
                    }
                    if let Err(error) = context
                        .record_progress(&progress.summary(), &gate, policy)
                        .await
                    {
                        return block_report(session, &progress, policy, &gate, error.to_string());
                    }
                }
                EventMsg::McpToolCallBegin(event)
                    if event.invocation.server == GAME_SERVER_NAME =>
                {
                    if let Err(error) = safe_boundary.begin_game_call(event.call_id.clone()) {
                        return block_report(session, &progress, policy, &gate, error.to_string());
                    }
                    if matches!(
                        event.invocation.tool.as_str(),
                        "click" | "drag" | "focus_click"
                    ) {
                        let snapshot = gate.snapshot();
                        if let Some(authorization) = snapshot
                            .mutation
                            .as_ref()
                            .map(|mutation| &mutation.authorization)
                            .filter(|authorization| authorization.call_id == event.call_id)
                            && let Err(error) =
                                context.record_mutation(authorization, policy).await
                        {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                error.to_string(),
                            );
                        }
                    }
                }
                EventMsg::McpToolCallEnd(event) => {
                    match finish_game_call_event(
                        &event,
                        &context,
                        &mut progress,
                        &mut safe_boundary,
                        policy,
                        &gate,
                    )
                    .await
                    {
                        Ok(GameCallEndDirective::InterruptForCommand) => {
                            if let Err(error) = submit_worker_interrupt(thread).await {
                                return block_report(
                                    session,
                                    &progress,
                                    policy,
                                    &gate,
                                    error.to_string(),
                                );
                            }
                            worker_exit_deadline =
                                Some(tokio::time::Instant::now() + self.limits.interrupt_timeout);
                        }
                        Ok(GameCallEndDirective::PauseForRecovery) => {
                            policy.close_mutation_lane();
                            if let Err(error) = submit_worker_interrupt(thread).await {
                                return block_report(
                                    session,
                                    &progress,
                                    policy,
                                    &gate,
                                    error.to_string(),
                                );
                            }
                            recovery_pending = true;
                            worker_exit_deadline =
                                Some(tokio::time::Instant::now() + self.limits.interrupt_timeout);
                        }
                        Ok(GameCallEndDirective::Continue) => {}
                        Err(error) => {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                error.to_string(),
                            );
                        }
                    }
                }
                EventMsg::DynamicToolCallRequest(request) => {
                    let prepared = match prepare_dynamic_tool_response(
                        request,
                        &tools,
                        &context,
                        &mut progress,
                        policy,
                        &gate,
                    )
                    .await
                    {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                error.to_string(),
                            );
                        }
                    };
                    thread
                        .submit(Op::DynamicToolResponse {
                            id: prepared.call_id,
                            response: prepared.response,
                        })
                        .await
                        .map_err(campaign_submit_error)?;
                    if let Some(directive) = prepared.outcome_directive {
                        match directive {
                            CampaignDirective::InterruptThenContinue(reason) => {
                                if let Err(error) =
                                    begin_safe_interrupt(thread, &mut progress, reason).await
                                {
                                    return block_report(
                                        session,
                                        &progress,
                                        policy,
                                        &gate,
                                        error.to_string(),
                                    );
                                }
                            }
                            CampaignDirective::Complete(state) => {
                                return build_report(
                                    session, &progress, policy, &gate, state, None,
                                );
                            }
                            CampaignDirective::Block(reason) => {
                                return block_report(session, &progress, policy, &gate, reason);
                            }
                            CampaignDirective::SubmitContinuation(_) => {
                                return block_report(
                                    session,
                                    &progress,
                                    policy,
                                    &gate,
                                    "accepted outcome requested an invalid continuation"
                                        .to_string(),
                                );
                            }
                        }
                    }
                }
                EventMsg::TurnComplete(event) => {
                    match compaction.finish(
                        event.error.as_ref().map(|error| error.message.clone()),
                    ) {
                        Ok(Some(reason)) => {
                            context.record_context_compacted();
                            submit_continuation(thread, &gate, &progress, reason).await?;
                            continue;
                        }
                        Err(error) => {
                            return block_report(session, &progress, policy, &gate, error);
                        }
                        Ok(None) => {}
                    }
                    if recovery_pending {
                        gate.invalidate(InvalidationReason::TurnAborted);
                        return Ok(CampaignExit::RecoveryRequired);
                    }
                    if let Some(command) = safe_boundary.finish_turn() {
                        gate.invalidate(InvalidationReason::TurnAborted);
                        return Ok(worker_command_exit(command));
                    }
                    if let Some(error) = event.error {
                        return block_report(session, &progress, policy, &gate, error.message);
                    }
                    if gate.snapshot().requires_post_mutation_observation {
                        continue;
                    }
                    let directive = match reduce_turn_complete(&mut progress, &gate.snapshot()) {
                        Ok(directive) => directive,
                        Err(error) => {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                error.to_string(),
                            );
                        }
                    };
                    match directive {
                        CampaignDirective::SubmitContinuation(reason) => {
                            compaction
                                .submit_at_boundary(thread, &gate, &progress, reason)
                                .await?;
                        }
                        CampaignDirective::InterruptThenContinue(reason) => {
                            if let Err(error) =
                                begin_safe_interrupt(thread, &mut progress, reason).await
                            {
                                return block_report(
                                    session,
                                    &progress,
                                    policy,
                                    &gate,
                                    error.to_string(),
                                );
                            }
                        }
                        CampaignDirective::Complete(state) => {
                            return build_report(session, &progress, policy, &gate, state, None);
                        }
                        CampaignDirective::Block(reason) => {
                            return block_report(session, &progress, policy, &gate, reason);
                        }
                    }
                }
                EventMsg::Error(event) => {
                    return block_report(session, &progress, policy, &gate, event.message);
                }
                EventMsg::TurnAborted(event) => {
                    gate.invalidate(InvalidationReason::TurnAborted);
                    if recovery_pending {
                        return Ok(CampaignExit::RecoveryRequired);
                    }
                    if let Some(command) = safe_boundary.finish_turn() {
                        return Ok(worker_command_exit(command));
                    }
                    let directive = match reduce_turn_aborted(&mut progress) {
                        Ok(directive) => directive,
                        Err(error) => {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                error.to_string(),
                            );
                        }
                    };
                    match directive {
                        CampaignDirective::SubmitContinuation(reason) => {
                            compaction
                                .submit_at_boundary(thread, &gate, &progress, reason)
                                .await?;
                        }
                        CampaignDirective::Block(reason) => {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                format!("{reason}: {:?}", event.reason),
                            );
                        }
                        CampaignDirective::InterruptThenContinue(_)
                        | CampaignDirective::Complete(_) => {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                "turn abort produced an invalid transition".to_string(),
                            );
                        }
                    }
                }
                EventMsg::ContextCompacted(_) => {
                    compaction.record_applied();
                }
                EventMsg::ExecApprovalRequest(_)
                | EventMsg::ApplyPatchApprovalRequest(_)
                | EventMsg::RequestPermissions(_)
                | EventMsg::RequestUserInput(_) => {
                    return block_report(
                        session,
                        &progress,
                        policy,
                        &gate,
                        "campaign requested a forbidden interactive operation".to_string(),
                    );
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
#[path = "campaign_loop_tests.rs"]
mod tests;
