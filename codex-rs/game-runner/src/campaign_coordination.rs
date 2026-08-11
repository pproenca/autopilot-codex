use codex_core_api::McpToolCallEndEvent;

use super::CampaignExecutionContext;
use super::WorkerCommand;
use crate::RunnerError;
use crate::WorkerDirective;
use crate::worker_coordination::game_tool_failure_signal;

impl CampaignExecutionContext {
    pub(super) fn has_worker_commands(&self) -> bool {
        matches!(
            self,
            Self::Durable {
                commands: Some(_),
                ..
            }
        )
    }

    pub(super) async fn next_worker_command(&mut self) -> Option<WorkerCommand> {
        let Self::Durable { commands, .. } = self else {
            return std::future::pending().await;
        };
        let command = match commands {
            Some(commands) => commands.recv().await,
            None => return std::future::pending().await,
        };
        if command.is_none() {
            *commands = None;
        }
        command
    }

    pub(super) async fn game_tool_failure_directive(
        &self,
        event: &McpToolCallEndEvent,
    ) -> Result<Option<WorkerDirective>, RunnerError> {
        let Self::Durable {
            failures: Some(failures),
            ..
        } = self
        else {
            return Ok(None);
        };
        let Some((signal, response)) =
            game_tool_failure_signal(event).map_err(coordination_error)?
        else {
            return Ok(None);
        };
        failures.send(signal).await.map_err(coordination_error)?;
        response.await.map(Some).map_err(coordination_error)
    }
}

fn coordination_error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::CampaignFailed {
        message: error.to_string(),
    }
}
