use std::sync::Arc;

use serde::Serialize;

use crate::CampaignCheckpoint;
use crate::CampaignEvent;
use crate::CampaignPersistence;
use crate::CampaignReport;

pub(crate) use crate::campaign_progress::CampaignDirective;
pub(crate) use crate::campaign_progress::CampaignProgress;
pub(crate) use crate::campaign_progress::ContinuationReason;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CampaignTerminalState {
    Won,
    TerminalBlock,
}

impl CampaignTerminalState {
    pub fn is_success(self) -> bool {
        match self {
            Self::Won => true,
            Self::TerminalBlock => false,
        }
    }
}

pub struct CampaignRun {
    limits: crate::campaign_progress::CampaignLimits,
}

pub(crate) enum CampaignStart {
    Fresh { target_app: String },
    Resumed { checkpoint: CampaignCheckpoint },
}

pub(crate) enum CampaignExecutionContext {
    Ephemeral {
        start: CampaignStart,
    },
    Durable {
        persistence: Arc<CampaignPersistence>,
        events: tokio::sync::broadcast::Sender<CampaignEvent>,
        commands: Option<tokio::sync::mpsc::Receiver<WorkerCommand>>,
        failures: Option<tokio::sync::mpsc::Sender<crate::GameToolFailureSignal>>,
        start: CampaignStart,
    },
}

pub(crate) enum CampaignExit {
    VerifiedWin(CampaignReport),
    Paused,
    Stopped,
    RecoveryRequired,
    Blocked(CampaignReport),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkerCommand {
    Pause,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SafeBoundaryDirective {
    None,
    WaitForActiveCall,
    Interrupt,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub(crate) enum SafeBoundaryError {
    #[error("a game call is already active")]
    ActiveCallAlreadyTracked,
    #[error("a worker command is already pending")]
    CommandAlreadyPending,
}

#[derive(Default)]
pub(crate) struct SafeBoundary {
    active_call_id: Option<String>,
    pending_command: Option<WorkerCommand>,
    interrupt_requested: bool,
}

impl SafeBoundary {
    pub(crate) fn begin_game_call(
        &mut self,
        call_id: String,
    ) -> Result<(), SafeBoundaryError> {
        if self.active_call_id.is_some() {
            return Err(SafeBoundaryError::ActiveCallAlreadyTracked);
        }
        self.active_call_id = Some(call_id);
        Ok(())
    }

    pub(crate) fn request(
        &mut self,
        command: WorkerCommand,
    ) -> Result<SafeBoundaryDirective, SafeBoundaryError> {
        if self.pending_command.is_some() {
            return Err(SafeBoundaryError::CommandAlreadyPending);
        }
        self.pending_command = Some(command);
        if self.active_call_id.is_some() {
            Ok(SafeBoundaryDirective::WaitForActiveCall)
        } else {
            self.interrupt_requested = true;
            Ok(SafeBoundaryDirective::Interrupt)
        }
    }

    pub(crate) fn finish_game_call(
        &mut self,
        call_id: &str,
    ) -> Result<SafeBoundaryDirective, SafeBoundaryError> {
        if self.active_call_id.as_deref() != Some(call_id) {
            return Ok(SafeBoundaryDirective::None);
        }
        self.active_call_id = None;
        if self.pending_command.is_some() {
            self.interrupt_requested = true;
            Ok(SafeBoundaryDirective::Interrupt)
        } else {
            Ok(SafeBoundaryDirective::None)
        }
    }

    pub(crate) fn finish_turn(&mut self) -> Option<WorkerCommand> {
        if !self.interrupt_requested {
            return None;
        }
        self.interrupt_requested = false;
        self.pending_command.take()
    }
}

impl CampaignRun {
    pub fn new(limits: crate::campaign_progress::CampaignLimits) -> Self {
        Self { limits }
    }
}

#[path = "campaign_loop.rs"]
mod campaign_loop;

#[path = "campaign_coordination.rs"]
mod campaign_coordination;

#[path = "campaign_game_call.rs"]
mod campaign_game_call;

#[path = "campaign_submit.rs"]
mod campaign_submit;

#[path = "campaign_event.rs"]
mod campaign_event;

#[cfg(test)]
#[path = "campaign_tests.rs"]
mod tests;
