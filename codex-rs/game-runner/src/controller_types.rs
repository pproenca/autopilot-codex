use crate::AcceptedPlan;
use crate::AuthorizedMutation;
use crate::CampaignSummary;
use crate::MutationResult;
use crate::ObservationEvidence;
use crate::PauseReason;
use crate::ReportedOutcome;

const MAX_FAILURE_SUMMARY_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampaignCommand {
    Start,
    Pause,
    Resume,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampaignStatus {
    Idle,
    Running { attempt_number: u64 },
    Pausing,
    Paused { reason: PauseReason },
    Recovering { cycle: u8 },
    Stopping,
    Won { summary: CampaignSummary },
    Blocked { failure: CampaignFailure },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampaignFailureKind {
    Checkpoint,
    Rollout,
    Helper,
    Runtime,
    Command,
    Campaign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CampaignFailure {
    pub kind: CampaignFailureKind,
    pub summary: String,
}

impl CampaignFailure {
    pub fn new(
        kind: CampaignFailureKind,
        summary: impl Into<String>,
    ) -> Result<Self, CampaignFailureError> {
        let summary = summary.into();
        let actual_bytes = summary.len();
        if actual_bytes > MAX_FAILURE_SUMMARY_BYTES {
            return Err(CampaignFailureError::SummaryTooLarge {
                actual_bytes,
                max_bytes: MAX_FAILURE_SUMMARY_BYTES,
            });
        }
        Ok(Self { kind, summary })
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum CampaignFailureError {
    #[error("campaign failure summary is {actual_bytes} bytes; maximum is {max_bytes}")]
    SummaryTooLarge {
        actual_bytes: usize,
        max_bytes: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampaignEvent {
    StatusChanged(CampaignStatus),
    Progress(CampaignSummary),
    Observation(ObservationEvidence),
    Plan(AcceptedPlan),
    Mutation(AuthorizedMutation),
    MutationFinished(MutationResult),
    Outcome(ReportedOutcome),
    Failure(CampaignFailure),
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("command {command:?} is invalid while campaign status is {status:?}")]
pub struct CommandTransitionError {
    pub status: CampaignStatus,
    pub command: CampaignCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerDirective {
    BeginStart,
    BeginPause,
    BeginResume,
    BeginStop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerStatusEvent {
    StartCommitted { attempt_number: u64 },
    PauseStarted,
    PauseCommitted { reason: PauseReason },
    ResumeStarted,
    RecoveryCycle,
    RunningCommitted { attempt_number: u64 },
    StopStarted,
    StopCommitted,
    VictoryCommitted { summary: CampaignSummary },
    Blocked { failure: CampaignFailure },
    CrashNormalized,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("event {event:?} is invalid while campaign status is {status:?}")]
pub(crate) struct StatusTransitionError {
    pub(crate) status: CampaignStatus,
    pub(crate) event: ControllerStatusEvent,
}

pub(crate) fn reduce_command(
    status: &CampaignStatus,
    command: CampaignCommand,
) -> Result<ControllerDirective, CommandTransitionError> {
    match (status, &command) {
        (CampaignStatus::Idle, CampaignCommand::Start)
        | (CampaignStatus::Won { .. }, CampaignCommand::Start) => {
            Ok(ControllerDirective::BeginStart)
        }
        (CampaignStatus::Running { .. }, CampaignCommand::Pause) => {
            Ok(ControllerDirective::BeginPause)
        }
        (CampaignStatus::Paused { .. }, CampaignCommand::Resume) => {
            Ok(ControllerDirective::BeginResume)
        }
        (CampaignStatus::Running { .. }, CampaignCommand::Stop)
        | (CampaignStatus::Paused { .. }, CampaignCommand::Stop) => {
            Ok(ControllerDirective::BeginStop)
        }
        (CampaignStatus::Idle, CampaignCommand::Pause)
        | (CampaignStatus::Idle, CampaignCommand::Resume)
        | (CampaignStatus::Idle, CampaignCommand::Stop)
        | (CampaignStatus::Running { .. }, CampaignCommand::Start)
        | (CampaignStatus::Running { .. }, CampaignCommand::Resume)
        | (CampaignStatus::Pausing, CampaignCommand::Start)
        | (CampaignStatus::Pausing, CampaignCommand::Pause)
        | (CampaignStatus::Pausing, CampaignCommand::Resume)
        | (CampaignStatus::Pausing, CampaignCommand::Stop)
        | (CampaignStatus::Paused { .. }, CampaignCommand::Start)
        | (CampaignStatus::Paused { .. }, CampaignCommand::Pause)
        | (CampaignStatus::Recovering { .. }, CampaignCommand::Start)
        | (CampaignStatus::Recovering { .. }, CampaignCommand::Pause)
        | (CampaignStatus::Recovering { .. }, CampaignCommand::Resume)
        | (CampaignStatus::Recovering { .. }, CampaignCommand::Stop)
        | (CampaignStatus::Stopping, CampaignCommand::Start)
        | (CampaignStatus::Stopping, CampaignCommand::Pause)
        | (CampaignStatus::Stopping, CampaignCommand::Resume)
        | (CampaignStatus::Stopping, CampaignCommand::Stop)
        | (CampaignStatus::Won { .. }, CampaignCommand::Pause)
        | (CampaignStatus::Won { .. }, CampaignCommand::Resume)
        | (CampaignStatus::Won { .. }, CampaignCommand::Stop)
        | (CampaignStatus::Blocked { .. }, CampaignCommand::Start)
        | (CampaignStatus::Blocked { .. }, CampaignCommand::Pause)
        | (CampaignStatus::Blocked { .. }, CampaignCommand::Resume)
        | (CampaignStatus::Blocked { .. }, CampaignCommand::Stop) => {
            Err(CommandTransitionError {
                status: status.clone(),
                command,
            })
        }
    }
}

pub(crate) fn reduce_status(
    status: &CampaignStatus,
    event: ControllerStatusEvent,
) -> Result<CampaignStatus, StatusTransitionError> {
    match (status, &event) {
        (
            CampaignStatus::Idle | CampaignStatus::Won { .. },
            ControllerStatusEvent::StartCommitted {
                attempt_number: attempt_number @ 1..,
            },
        ) => Ok(CampaignStatus::Running {
            attempt_number: *attempt_number,
        }),
        (CampaignStatus::Running { .. }, ControllerStatusEvent::PauseStarted) => {
            Ok(CampaignStatus::Pausing)
        }
        (
            CampaignStatus::Pausing | CampaignStatus::Recovering { .. },
            ControllerStatusEvent::PauseCommitted { reason },
        ) => Ok(CampaignStatus::Paused {
            reason: reason.clone(),
        }),
        (
            CampaignStatus::Running { .. } | CampaignStatus::Paused { .. },
            ControllerStatusEvent::ResumeStarted,
        ) => Ok(CampaignStatus::Recovering { cycle: 0 }),
        (
            CampaignStatus::Recovering { cycle },
            ControllerStatusEvent::RecoveryCycle,
        ) => cycle
            .checked_add(1)
            .map(|cycle| CampaignStatus::Recovering { cycle })
            .ok_or_else(|| StatusTransitionError {
                status: status.clone(),
                event: event.clone(),
            }),
        (
            CampaignStatus::Recovering { .. },
            ControllerStatusEvent::RunningCommitted {
                attempt_number: attempt_number @ 1..,
            },
        ) => Ok(CampaignStatus::Running {
            attempt_number: *attempt_number,
        }),
        (
            CampaignStatus::Running { .. } | CampaignStatus::Paused { .. },
            ControllerStatusEvent::StopStarted,
        ) => Ok(CampaignStatus::Stopping),
        (CampaignStatus::Stopping, ControllerStatusEvent::StopCommitted) => {
            Ok(CampaignStatus::Idle)
        }
        (
            CampaignStatus::Running { .. },
            ControllerStatusEvent::VictoryCommitted { summary },
        ) => Ok(CampaignStatus::Won {
            summary: summary.clone(),
        }),
        (
            CampaignStatus::Idle
            | CampaignStatus::Running { .. }
            | CampaignStatus::Pausing
            | CampaignStatus::Paused { .. }
            | CampaignStatus::Recovering { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Won { .. },
            ControllerStatusEvent::Blocked { failure },
        ) => Ok(CampaignStatus::Blocked {
            failure: failure.clone(),
        }),
        (
            CampaignStatus::Running { .. }
            | CampaignStatus::Pausing
            | CampaignStatus::Recovering { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Blocked { .. },
            ControllerStatusEvent::CrashNormalized,
        ) => Ok(CampaignStatus::Paused {
            reason: PauseReason::UnexpectedExit,
        }),
        (
            CampaignStatus::Idle | CampaignStatus::Won { .. },
            ControllerStatusEvent::StartCommitted {
                attempt_number: 0,
            },
        )
        | (
            CampaignStatus::Running { .. }
            | CampaignStatus::Pausing
            | CampaignStatus::Paused { .. }
            | CampaignStatus::Recovering { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Blocked { .. },
            ControllerStatusEvent::StartCommitted { .. },
        )
        | (
            CampaignStatus::Idle
            | CampaignStatus::Pausing
            | CampaignStatus::Paused { .. }
            | CampaignStatus::Recovering { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Won { .. }
            | CampaignStatus::Blocked { .. },
            ControllerStatusEvent::PauseStarted,
        )
        | (
            CampaignStatus::Idle
            | CampaignStatus::Running { .. }
            | CampaignStatus::Paused { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Won { .. }
            | CampaignStatus::Blocked { .. },
            ControllerStatusEvent::PauseCommitted { .. },
        )
        | (
            CampaignStatus::Idle
            | CampaignStatus::Pausing
            | CampaignStatus::Recovering { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Won { .. }
            | CampaignStatus::Blocked { .. },
            ControllerStatusEvent::ResumeStarted,
        )
        | (
            CampaignStatus::Idle
            | CampaignStatus::Running { .. }
            | CampaignStatus::Pausing
            | CampaignStatus::Paused { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Won { .. }
            | CampaignStatus::Blocked { .. },
            ControllerStatusEvent::RecoveryCycle,
        )
        | (
            CampaignStatus::Recovering { .. },
            ControllerStatusEvent::RunningCommitted {
                attempt_number: 0,
            },
        )
        | (
            CampaignStatus::Idle
            | CampaignStatus::Running { .. }
            | CampaignStatus::Pausing
            | CampaignStatus::Paused { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Won { .. }
            | CampaignStatus::Blocked { .. },
            ControllerStatusEvent::RunningCommitted { .. },
        )
        | (
            CampaignStatus::Idle
            | CampaignStatus::Pausing
            | CampaignStatus::Recovering { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Won { .. }
            | CampaignStatus::Blocked { .. },
            ControllerStatusEvent::StopStarted,
        )
        | (
            CampaignStatus::Idle
            | CampaignStatus::Running { .. }
            | CampaignStatus::Pausing
            | CampaignStatus::Paused { .. }
            | CampaignStatus::Recovering { .. }
            | CampaignStatus::Won { .. }
            | CampaignStatus::Blocked { .. },
            ControllerStatusEvent::StopCommitted,
        )
        | (
            CampaignStatus::Idle
            | CampaignStatus::Pausing
            | CampaignStatus::Paused { .. }
            | CampaignStatus::Recovering { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Won { .. }
            | CampaignStatus::Blocked { .. },
            ControllerStatusEvent::VictoryCommitted { .. },
        )
        | (
            CampaignStatus::Blocked { .. },
            ControllerStatusEvent::Blocked { .. },
        )
        | (
            CampaignStatus::Idle
            | CampaignStatus::Paused { .. }
            | CampaignStatus::Won { .. },
            ControllerStatusEvent::CrashNormalized,
        ) => Err(StatusTransitionError {
            status: status.clone(),
            event,
        }),
    }
}

#[cfg(test)]
#[path = "controller_types_tests.rs"]
mod tests;
