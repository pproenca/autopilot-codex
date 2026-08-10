use std::sync::Arc;
use std::time::Instant;

use codex_core_api::CodexThread;
use codex_core_api::EventMsg;
use codex_core_api::McpToolCallEndEvent;
use codex_core_api::Op;
use codex_core_api::SessionConfiguredEvent;
use codex_core_api::UserInput;

use super::CampaignDirective;
use super::CampaignProgress;
use super::CampaignRun;
use super::CampaignTerminalState;
use super::ContinuationReason;
use crate::CampaignReport;
use crate::CampaignTools;
use crate::DecisionGate;
use crate::GAME_SERVER_NAME;
use crate::GameCallPolicy;
use crate::InvalidationReason;
use crate::MutationResult;
use crate::ReportedOutcome;
use crate::RunnerError;
use crate::campaign_progress::CampaignProgressError;
use crate::campaign_prompt::continuation_prompt;
use crate::campaign_prompt::initial_prompt;
use crate::campaign_prompt::new_attempt_prompt;
use crate::campaign_report::CampaignReportContext;

impl CampaignRun {
    pub async fn execute(
        &self,
        thread: &CodexThread,
        session: &SessionConfiguredEvent,
        policy: &GameCallPolicy,
        gate: Arc<DecisionGate>,
        target_app: &str,
    ) -> Result<CampaignReport, RunnerError> {
        let mut progress = CampaignProgress::new(self.limits);
        submit_turn(thread, &gate, &initial_prompt(target_app)).await?;
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
                }
                EventMsg::McpToolCallEnd(event) => {
                    if let Err(error) = observe_game_call_end(&gate, &event) {
                        return block_report(session, &progress, policy, &gate, error.to_string());
                    }
                    if let Err(error) = progress.observe_snapshot(&gate.snapshot(), Instant::now())
                    {
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
                    let accepted_outcome = response.success && request.tool == "report_outcome";
                    thread
                        .submit(Op::DynamicToolResponse {
                            id: request.call_id,
                            response,
                        })
                        .await
                        .map_err(campaign_submit_error)?;
                    if accepted_outcome {
                        let snapshot = gate.snapshot();
                        let Some(outcome) = snapshot.outcome.as_ref() else {
                            return block_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                "accepted outcome response did not retain evidence".to_string(),
                            );
                        };
                        let directive = match reduce_accepted_outcome(&mut progress, outcome) {
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

async fn begin_safe_interrupt(
    thread: &CodexThread,
    progress: &mut CampaignProgress,
    reason: ContinuationReason,
) -> Result<(), RunnerError> {
    progress
        .begin_interrupt(reason, Instant::now())
        .map_err(campaign_progress_error)?;
    thread
        .submit(Op::Interrupt)
        .await
        .map(|_| ())
        .map_err(campaign_submit_error)
}

fn campaign_submit_error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::CampaignFailed {
        message: error.to_string(),
    }
}

fn campaign_progress_error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::CampaignFailed {
        message: error.to_string(),
    }
}

fn block_report(
    session: &SessionConfiguredEvent,
    progress: &CampaignProgress,
    policy: &GameCallPolicy,
    gate: &DecisionGate,
    reason: String,
) -> Result<CampaignReport, RunnerError> {
    build_report(
        session,
        progress,
        policy,
        gate,
        CampaignTerminalState::TerminalBlock,
        Some(reason),
    )
}

fn build_report(
    session: &SessionConfiguredEvent,
    progress: &CampaignProgress,
    policy: &GameCallPolicy,
    gate: &DecisionGate,
    terminal_state: CampaignTerminalState,
    terminal_failure: Option<String>,
) -> Result<CampaignReport, RunnerError> {
    let rollout_path = session
        .rollout_path
        .clone()
        .ok_or(RunnerError::MissingRolloutPath)?;
    Ok(CampaignReport::from_snapshot(
        CampaignReportContext {
            terminal_state,
            thread_id: session.thread_id.to_string(),
            summary: progress.summary(),
            rollout_path,
            owner_lease: policy.lease(),
            policy_audit: policy.audit(),
            terminal_failure,
        },
        gate.snapshot(),
    ))
}

fn observe_game_call_end(
    gate: &DecisionGate,
    event: &McpToolCallEndEvent,
) -> Result<(), RunnerError> {
    if event.invocation.server != GAME_SERVER_NAME {
        return Ok(());
    }
    match event.invocation.tool.as_str() {
        "get_app_state" => {
            let Some((reference, width, height)) = event
                .result
                .as_ref()
                .ok()
                .filter(|result| !result.is_error.unwrap_or(false))
                .and_then(|result| result.structured_content.as_ref())
                .and_then(full_frame_metadata)
            else {
                return Ok(());
            };
            gate.complete_full_observation(event.call_id.clone(), reference, width, height)
                .map_err(|error| RunnerError::CampaignFailed {
                    message: error.to_string(),
                })?;
        }
        "click" | "drag" | "focus_click" => {
            let is_authorized_call = gate
                .snapshot()
                .mutation
                .is_some_and(|mutation| mutation.authorization.call_id == event.call_id);
            if is_authorized_call {
                let result = match &event.result {
                    Ok(result) if result.is_error.unwrap_or(false) => MutationResult::CleanFailure,
                    Ok(_) => MutationResult::Success,
                    Err(_) => MutationResult::Indeterminate,
                };
                gate.record_mutation_result(&event.call_id, result)
                    .map_err(|error| RunnerError::CampaignFailed {
                        message: error.to_string(),
                    })?;
            }
        }
        "wait" | "zoom" => {}
        _ => {}
    }
    Ok(())
}

fn full_frame_metadata(content: &serde_json::Value) -> Option<(String, u32, u32)> {
    let reference = content.get("artifact_uri")?.as_str()?;
    if reference.len() > 2 * 1024 {
        return None;
    }
    let width = u32::try_from(content.get("width")?.as_u64()?).ok()?;
    let height = u32::try_from(content.get("height")?.as_u64()?).ok()?;
    (width > 0 && height > 0).then(|| (reference.to_string(), width, height))
}

fn reduce_accepted_outcome(
    progress: &mut CampaignProgress,
    outcome: &ReportedOutcome,
) -> Result<CampaignDirective, CampaignProgressError> {
    progress.accept_outcome(outcome)
}

fn reduce_turn_complete(
    progress: &mut CampaignProgress,
    snapshot: &crate::DecisionSnapshot,
) -> Result<CampaignDirective, CampaignProgressError> {
    match progress.complete_expected_interrupt() {
        Ok(directive) => Ok(directive),
        Err(CampaignProgressError::MissingPendingInterrupt) => progress.on_turn_complete(snapshot),
        Err(error) => Err(error),
    }
}

fn reduce_turn_aborted(
    progress: &mut CampaignProgress,
) -> Result<CampaignDirective, CampaignProgressError> {
    match progress.complete_expected_interrupt() {
        Ok(directive) => Ok(directive),
        Err(CampaignProgressError::MissingPendingInterrupt) => Ok(CampaignDirective::Block(
            "campaign turn aborted unexpectedly".to_string(),
        )),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "campaign_loop_tests.rs"]
mod tests;
