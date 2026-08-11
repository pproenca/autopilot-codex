use std::time::Instant;

use codex_core_api::McpToolCallEndEvent;

use super::CampaignExecutionContext;
use super::CampaignProgress;
use super::SafeBoundary;
use super::SafeBoundaryDirective;
use super::campaign_event::observe_game_call_end;
use crate::DecisionGate;
use crate::GAME_SERVER_NAME;
use crate::GameCallPolicy;
use crate::RunnerError;
use crate::WorkerDirective;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GameCallEndDirective {
    Continue,
    InterruptForCommand,
    PauseForRecovery,
}

pub(super) async fn finish_game_call_event(
    event: &McpToolCallEndEvent,
    context: &CampaignExecutionContext,
    progress: &mut CampaignProgress,
    safe_boundary: &mut SafeBoundary,
    policy: &GameCallPolicy,
    gate: &DecisionGate,
) -> Result<GameCallEndDirective, RunnerError> {
    observe_game_call_end(gate, event)?;
    let snapshot = gate.snapshot();
    if event.invocation.server == GAME_SERVER_NAME {
        match event.invocation.tool.as_str() {
            "get_app_state" => {
                if let Some(observation) = snapshot
                    .observation
                    .as_ref()
                    .filter(|observation| observation.call_id == event.call_id)
                {
                    context.record_observation(observation, policy).await?;
                }
            }
            "click" | "drag" | "focus_click" => {
                if let Some(result) = snapshot
                    .mutation
                    .as_ref()
                    .filter(|mutation| mutation.authorization.call_id == event.call_id)
                    .and_then(|mutation| mutation.result)
                {
                    context
                        .record_mutation_finished(&event.call_id, result, policy)
                        .await?;
                }
            }
            "wait" | "zoom" => {}
            _ => {}
        }
    }
    progress
        .observe_snapshot(&snapshot, Instant::now())
        .map_err(game_call_error)?;
    match safe_boundary
        .finish_game_call(&event.call_id)
        .map_err(game_call_error)?
    {
        SafeBoundaryDirective::Interrupt => {
            return Ok(GameCallEndDirective::InterruptForCommand);
        }
        SafeBoundaryDirective::None | SafeBoundaryDirective::WaitForActiveCall => {}
    }
    match context.game_tool_failure_directive(event).await? {
        Some(WorkerDirective::PauseForRecovery) => {
            Ok(GameCallEndDirective::PauseForRecovery)
        }
        Some(WorkerDirective::Continue) | None => Ok(GameCallEndDirective::Continue),
    }
}

fn game_call_error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::CampaignFailed {
        message: error.to_string(),
    }
}
