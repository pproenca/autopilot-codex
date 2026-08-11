use std::sync::Arc;

use codex_core_api::CodexThread;
use codex_core_api::Op;
use codex_core_api::SessionConfiguredEvent;
use codex_core_api::UserInput;

use super::CampaignExecutionContext;
use super::CampaignExit;
use super::CampaignProgress;
use super::CampaignRun;
use super::CampaignStart;
use super::ContinuationReason;
use super::WorkerCommand;
use crate::CampaignReport;
use crate::DecisionGate;
use crate::GameCallPolicy;
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
}

pub(super) async fn submit_worker_interrupt(thread: &CodexThread) -> Result<(), RunnerError> {
    thread
        .submit(Op::Interrupt)
        .await
        .map(|_| ())
        .map_err(campaign_submit_error)
}

pub(super) fn worker_command_exit(command: WorkerCommand) -> CampaignExit {
    match command {
        WorkerCommand::Pause => CampaignExit::Paused,
        WorkerCommand::Stop => CampaignExit::Stopped,
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

pub(super) async fn submit_turn(
    thread: &CodexThread,
    gate: &DecisionGate,
    prompt: &str,
) -> Result<(), RunnerError> {
    gate.begin_turn();
    submit_prompt(thread, prompt).await
}

pub(super) async fn submit_continuation(
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

pub(super) fn campaign_submit_error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::CampaignFailed {
        message: error.to_string(),
    }
}
