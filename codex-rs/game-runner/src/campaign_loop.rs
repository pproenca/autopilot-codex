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
use crate::CampaignReport;
use crate::CampaignTools;
use crate::DecisionGate;
use crate::GAME_SERVER_NAME;
use crate::GameCallPolicy;
use crate::InvalidationReason;
use crate::MutationResult;
use crate::RunnerError;
use crate::campaign_report::CampaignReportContext;

const CONTINUATION_PROMPT: &str = "Continue the same game campaign. Re-observe whenever authority was invalidated. Before any mutation, call game_runner.record_plan with two to four candidates and an exact chosen action. After the one allowed mutation, capture a fresh full-frame observation and stop.";

impl CampaignRun {
    pub async fn execute(
        &self,
        thread: &CodexThread,
        session: &SessionConfiguredEvent,
        policy: &GameCallPolicy,
        gate: Arc<DecisionGate>,
        target_app: &str,
    ) -> Result<CampaignReport, RunnerError> {
        submit_prompt(thread, &initial_prompt(target_app)).await?;
        let tools = CampaignTools::new(Arc::clone(&gate));
        let mut progress = CampaignProgress::new(self.limits);

        loop {
            let deadline = tokio::time::Instant::from_std(progress.next_deadline());
            let event = match tokio::time::timeout_at(deadline, thread.next_event()).await {
                Ok(Ok(event)) => event,
                Ok(Err(error)) => {
                    return build_report(
                        session,
                        &progress,
                        policy,
                        &gate,
                        CampaignTerminalState::TerminalBlock,
                        Some(format!("failed to read campaign event: {error}")),
                    );
                }
                Err(_) => {
                    let directive = progress
                        .deadline_directive(&gate.snapshot(), Instant::now())
                        .unwrap_or_else(|| {
                            CampaignDirective::Block("campaign deadline elapsed".to_string())
                        });
                    let CampaignDirective::Block(reason) = directive else {
                        unreachable!("elapsed campaign deadline must block")
                    };
                    return build_report(
                        session,
                        &progress,
                        policy,
                        &gate,
                        CampaignTerminalState::TerminalBlock,
                        Some(reason),
                    );
                }
            };

            match event.msg {
                EventMsg::TurnStarted(event) => progress.on_turn_started(event.turn_id),
                EventMsg::McpToolCallEnd(event) => observe_game_call_end(&gate, &event)?,
                EventMsg::DynamicToolCallRequest(request) => {
                    let response = match tools.handle(&request) {
                        Ok(response) => response,
                        Err(error) => {
                            return build_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                CampaignTerminalState::TerminalBlock,
                                Some(error.to_string()),
                            );
                        }
                    };
                    thread
                        .submit(Op::DynamicToolResponse {
                            id: request.call_id,
                            response,
                        })
                        .await
                        .map_err(campaign_submit_error)?;
                }
                EventMsg::TurnComplete(event) => {
                    if let Some(error) = event.error {
                        return build_report(
                            session,
                            &progress,
                            policy,
                            &gate,
                            CampaignTerminalState::TerminalBlock,
                            Some(error.message),
                        );
                    }
                    match progress.on_turn_complete(&gate.snapshot()) {
                        CampaignDirective::Continue => {
                            submit_prompt(thread, CONTINUATION_PROMPT).await?;
                        }
                        CampaignDirective::Complete(state) => {
                            return build_report(session, &progress, policy, &gate, state, None);
                        }
                        CampaignDirective::Block(reason) => {
                            return build_report(
                                session,
                                &progress,
                                policy,
                                &gate,
                                CampaignTerminalState::TerminalBlock,
                                Some(reason),
                            );
                        }
                    }
                }
                EventMsg::Error(event) => {
                    return build_report(
                        session,
                        &progress,
                        policy,
                        &gate,
                        CampaignTerminalState::TerminalBlock,
                        Some(event.message),
                    );
                }
                EventMsg::TurnAborted(event) => {
                    gate.invalidate(InvalidationReason::TurnAborted);
                    return build_report(
                        session,
                        &progress,
                        policy,
                        &gate,
                        CampaignTerminalState::TerminalBlock,
                        Some(format!("turn aborted: {:?}", event.reason)),
                    );
                }
                EventMsg::ExecApprovalRequest(_)
                | EventMsg::ApplyPatchApprovalRequest(_)
                | EventMsg::RequestPermissions(_)
                | EventMsg::RequestUserInput(_) => {
                    return build_report(
                        session,
                        &progress,
                        policy,
                        &gate,
                        CampaignTerminalState::TerminalBlock,
                        Some("campaign requested a forbidden interactive operation".to_string()),
                    );
                }
                _ => {}
            }
            progress.observe_snapshot(&gate.snapshot());
        }
    }
}

fn initial_prompt(target_app: &str) -> String {
    format!(
        "Control the currently visible {target_app} game for one safe Stage 4A canary action. First call mcp__game__get_app_state and inspect the full frame. Before any click, drag, or focus-click, call game_runner.record_plan with two to four candidates and the exact complete chosen tool arguments. Choose only reversible non-gameplay navigation such as Settings, Collection, or Credits; never choose Play or Continue. Execute exactly the accepted action once, then call mcp__game__get_app_state again. Do not attempt a second mutation. Call game_runner.report_outcome only if the fresh screen visibly proves a win, loss, or terminal block."
    )
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

fn campaign_submit_error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::CampaignFailed {
        message: error.to_string(),
    }
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
            turn_ids: progress.turn_ids().to_vec(),
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

#[cfg(test)]
#[path = "campaign_loop_tests.rs"]
mod tests;
