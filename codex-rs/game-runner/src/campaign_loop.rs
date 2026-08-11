use std::sync::Arc;
use std::time::Instant;

use codex_core_api::CodexThread;
use codex_core_api::EventMsg;
use codex_core_api::Op;
use codex_core_api::SessionConfiguredEvent;
use codex_core_api::UserInput;

use super::CampaignDirective;
use super::CampaignExecutionContext;
use super::CampaignExit;
use super::CampaignProgress;
use super::CampaignRun;
use super::CampaignStart;
use super::CampaignTerminalState;
use super::ContinuationReason;
use super::campaign_event::begin_safe_interrupt;
use super::campaign_event::block_exit as block_report;
use super::campaign_event::build_exit as build_report;
use super::campaign_event::initialize_campaign_start;
use super::campaign_event::observe_game_call_end;
use super::campaign_event::reduce_accepted_outcome;
use super::campaign_event::reduce_turn_aborted;
use super::campaign_event::reduce_turn_complete;
use crate::CampaignReport;
use crate::CampaignTools;
use crate::DecisionGate;
use crate::GAME_SERVER_NAME;
use crate::GameCallPolicy;
use crate::InvalidationReason;
use crate::RunnerError;
use crate::campaign_prompt::continuation_prompt;
use crate::campaign_prompt::new_attempt_prompt;

impl CampaignRun {
    pub async fn execute(
        &self,
        thread: &CodexThread,
        session: &SessionConfiguredEvent,
        policy: &GameCallPolicy,
        gate: Arc<DecisionGate>,
        target_app: &str,
    ) -> Result<CampaignReport, RunnerError> {
        match self
            .execute_controlled(
                thread,
                session,
                policy,
                gate,
                CampaignExecutionContext::Ephemeral {
                    start: CampaignStart::Fresh {
                        target_app: target_app.to_string(),
                    },
                },
            )
            .await?
        {
            CampaignExit::VerifiedWin(report) | CampaignExit::Blocked(report) => Ok(report),
            CampaignExit::Paused => Err(campaign_submit_error("campaign paused")),
            CampaignExit::Stopped => Err(campaign_submit_error("campaign stopped")),
        }
    }

    pub(crate) async fn execute_controlled(
        &self,
        thread: &CodexThread,
        session: &SessionConfiguredEvent,
        policy: &GameCallPolicy,
        gate: Arc<DecisionGate>,
        context: CampaignExecutionContext,
    ) -> Result<CampaignExit, RunnerError> {
        let (mut progress, prompt) = initialize_campaign_start(context.start(), self.limits)?;
        submit_turn(thread, &gate, &prompt).await?;
        let tools = CampaignTools::new(Arc::clone(&gate));

        loop {
            let deadline = tokio::time::Instant::from_std(progress.next_deadline());
            let event = match tokio::time::timeout_at(deadline, thread.next_event()).await {
                Ok(Ok(event)) => event,
                Ok(Err(error)) => {
                    return block_report(
                        session,
                        &progress,
                        policy,
                        &gate,
                        format!("failed to read campaign event: {error}"),
                    );
                }
                Err(_) => {
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
                    if event.invocation.server == GAME_SERVER_NAME
                        && matches!(
                            event.invocation.tool.as_str(),
                            "click" | "drag" | "focus_click"
                        ) =>
                {
                    let snapshot = gate.snapshot();
                    if let Some(authorization) = snapshot
                        .mutation
                        .as_ref()
                        .map(|mutation| &mutation.authorization)
                        .filter(|authorization| authorization.call_id == event.call_id)
                        && let Err(error) = context.record_mutation(authorization, policy).await
                    {
                        return block_report(session, &progress, policy, &gate, error.to_string());
                    }
                }
                EventMsg::McpToolCallEnd(event) => {
                    if let Err(error) = observe_game_call_end(&gate, &event) {
                        return block_report(session, &progress, policy, &gate, error.to_string());
                    }
                    let snapshot = gate.snapshot();
                    let durable_result = if event.invocation.server != GAME_SERVER_NAME {
                        Ok(())
                    } else {
                        match event.invocation.tool.as_str() {
                            "get_app_state" => match snapshot
                                .observation
                                .as_ref()
                                .filter(|observation| observation.call_id == event.call_id)
                            {
                                Some(observation) => {
                                    context.record_observation(observation, policy).await
                                }
                                None => Ok(()),
                            },
                            "click" | "drag" | "focus_click" => match snapshot
                                .mutation
                                .as_ref()
                                .filter(|mutation| mutation.authorization.call_id == event.call_id)
                                .and_then(|mutation| mutation.result)
                            {
                                Some(result) => {
                                    context
                                        .record_mutation_finished(&event.call_id, result, policy)
                                        .await
                                }
                                None => Ok(()),
                            },
                            "wait" | "zoom" => Ok(()),
                            _ => Ok(()),
                        }
                    };
                    if let Err(error) = durable_result {
                        return block_report(session, &progress, policy, &gate, error.to_string());
                    }
                    if let Err(error) = progress.observe_snapshot(&snapshot, Instant::now()) {
                        return block_report(session, &progress, policy, &gate, error.to_string());
                    }
                }
                EventMsg::DynamicToolCallRequest(request) => {
                    let response = match tools.handle(&request) {
                        Ok(response) => response,
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
                    let accepted_plan = response.success && request.tool == "record_plan";
                    let accepted_outcome = response.success && request.tool == "report_outcome";
                    if accepted_plan {
                        let snapshot = gate.snapshot();
                        let Some(plan) = snapshot.plan.as_ref() else {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                "accepted plan response did not retain its plan".to_string(),
                            );
                        };
                        if let Err(error) = context
                            .record_plan(&progress.summary(), plan, &gate, policy)
                            .await
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
                    let outcome_directive = if accepted_outcome {
                        let snapshot = gate.snapshot();
                        let Some(outcome) = snapshot.outcome else {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                "accepted outcome response did not retain evidence".to_string(),
                            );
                        };
                        let directive = match reduce_accepted_outcome(&mut progress, &outcome) {
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
                        if let Err(error) = context
                            .record_outcome(
                                &progress.summary(),
                                &outcome,
                                &directive,
                                &gate,
                                policy,
                            )
                            .await
                        {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                error.to_string(),
                            );
                        }
                        if matches!(
                            directive,
                            CampaignDirective::Complete(CampaignTerminalState::Won)
                        ) {
                            policy.close_mutation_lane();
                        }
                        Some(directive)
                    } else {
                        None
                    };
                    thread
                        .submit(Op::DynamicToolResponse {
                            id: request.call_id,
                            response,
                        })
                        .await
                        .map_err(campaign_submit_error)?;
                    if let Some(directive) = outcome_directive {
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
                            submit_continuation(thread, &gate, &progress, reason).await?;
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
                            submit_continuation(thread, &gate, &progress, reason).await?;
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

async fn submit_prompt(thread: &CodexThread, prompt: &str) -> Result<(), RunnerError> {
    thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: prompt.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await
        .map(|_| ())
        .map_err(campaign_submit_error)
}

async fn submit_turn(
    thread: &CodexThread,
    gate: &DecisionGate,
    prompt: &str,
) -> Result<(), RunnerError> {
    gate.begin_turn();
    submit_prompt(thread, prompt).await
}

async fn submit_continuation(
    thread: &CodexThread,
    gate: &DecisionGate,
    progress: &CampaignProgress,
    reason: ContinuationReason,
) -> Result<(), RunnerError> {
    let attempt_number = progress.summary().attempt_number;
    let prompt = match reason {
        ContinuationReason::Ordinary | ContinuationReason::TurnTimeout => {
            continuation_prompt(attempt_number)
        }
        ContinuationReason::NewAttempt => new_attempt_prompt(attempt_number),
    };
    submit_turn(thread, gate, &prompt).await
}

fn campaign_submit_error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::CampaignFailed {
        message: error.to_string(),
    }
}

#[cfg(test)]
#[path = "campaign_loop_tests.rs"]
mod tests;
