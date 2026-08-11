use pretty_assertions::assert_eq;

use super::CampaignCommand;
use super::CampaignFailure;
use super::CampaignFailureError;
use super::CampaignFailureKind;
use super::CampaignStatus;
use super::ControllerDirective;
use super::ControllerStatusEvent;
use super::StatusTransitionError;
use super::reduce_command;
use super::reduce_status;
use crate::CampaignSummary;
use crate::PauseReason;

#[test]
fn failure_summary_constructor_enforces_the_two_kibibyte_limit() {
    assert_eq!(
        CampaignFailure::new(CampaignFailureKind::Runtime, "x".repeat(2 * 1024)),
        Ok(CampaignFailure {
            kind: CampaignFailureKind::Runtime,
            summary: "x".repeat(2 * 1024),
        })
    );
    assert_eq!(
        CampaignFailure::new(CampaignFailureKind::Runtime, "x".repeat(2 * 1024 + 1)),
        Err(CampaignFailureError::SummaryTooLarge {
            actual_bytes: 2 * 1024 + 1,
            max_bytes: 2 * 1024,
        })
    );
}

#[test]
fn command_reducer_covers_every_status_and_command_pair() {
    let statuses = [
        CampaignStatus::Idle,
        CampaignStatus::Running { attempt_number: 2 },
        CampaignStatus::Pausing,
        CampaignStatus::Paused {
            reason: PauseReason::Operator,
        },
        CampaignStatus::Recovering { cycle: 1 },
        CampaignStatus::Stopping,
        CampaignStatus::Won {
            summary: summary(2),
        },
        CampaignStatus::Blocked { failure: failure() },
    ];
    let commands = [
        CampaignCommand::Start,
        CampaignCommand::Pause,
        CampaignCommand::Resume,
        CampaignCommand::Stop,
    ];

    for status in statuses {
        for command in &commands {
            let expected = expected_command_transition(&status, command);
            assert_eq!(reduce_command(&status, command.clone()), expected);
        }
    }
}

#[test]
fn status_reducer_covers_every_legal_actor_transition() {
    let cases = [
        (
            CampaignStatus::Idle,
            ControllerStatusEvent::StartCommitted { attempt_number: 1 },
            CampaignStatus::Running { attempt_number: 1 },
        ),
        (
            CampaignStatus::Won {
                summary: summary(3),
            },
            ControllerStatusEvent::StartCommitted { attempt_number: 1 },
            CampaignStatus::Running { attempt_number: 1 },
        ),
        (
            CampaignStatus::Running { attempt_number: 2 },
            ControllerStatusEvent::PauseStarted,
            CampaignStatus::Pausing,
        ),
        (
            CampaignStatus::Pausing,
            ControllerStatusEvent::PauseCommitted {
                reason: PauseReason::Operator,
            },
            CampaignStatus::Paused {
                reason: PauseReason::Operator,
            },
        ),
        (
            CampaignStatus::Paused {
                reason: PauseReason::Operator,
            },
            ControllerStatusEvent::ResumeStarted,
            CampaignStatus::Recovering { cycle: 0 },
        ),
        (
            CampaignStatus::Running { attempt_number: 2 },
            ControllerStatusEvent::ResumeStarted,
            CampaignStatus::Recovering { cycle: 0 },
        ),
        (
            CampaignStatus::Recovering { cycle: 0 },
            ControllerStatusEvent::RecoveryCycle,
            CampaignStatus::Recovering { cycle: 1 },
        ),
        (
            CampaignStatus::Recovering { cycle: 1 },
            ControllerStatusEvent::RunningCommitted { attempt_number: 2 },
            CampaignStatus::Running { attempt_number: 2 },
        ),
        (
            CampaignStatus::Recovering { cycle: 3 },
            ControllerStatusEvent::PauseCommitted {
                reason: PauseReason::HelperUnavailable {
                    summary: "helper unavailable".to_string(),
                },
            },
            CampaignStatus::Paused {
                reason: PauseReason::HelperUnavailable {
                    summary: "helper unavailable".to_string(),
                },
            },
        ),
        (
            CampaignStatus::Running { attempt_number: 2 },
            ControllerStatusEvent::StopStarted,
            CampaignStatus::Stopping,
        ),
        (
            CampaignStatus::Paused {
                reason: PauseReason::Operator,
            },
            ControllerStatusEvent::StopStarted,
            CampaignStatus::Stopping,
        ),
        (
            CampaignStatus::Stopping,
            ControllerStatusEvent::StopCommitted,
            CampaignStatus::Idle,
        ),
        (
            CampaignStatus::Running { attempt_number: 2 },
            ControllerStatusEvent::VictoryCommitted {
                summary: summary(2),
            },
            CampaignStatus::Won {
                summary: summary(2),
            },
        ),
    ];

    for (status, event, expected) in cases {
        assert_eq!(reduce_status(&status, event), Ok(expected));
    }
}

#[test]
fn blocked_and_crash_normalization_transitions_are_exhaustive() {
    for status in [
        CampaignStatus::Idle,
        CampaignStatus::Running { attempt_number: 2 },
        CampaignStatus::Pausing,
        CampaignStatus::Paused {
            reason: PauseReason::Operator,
        },
        CampaignStatus::Recovering { cycle: 1 },
        CampaignStatus::Stopping,
        CampaignStatus::Won {
            summary: summary(2),
        },
    ] {
        assert_eq!(
            reduce_status(
                &status,
                ControllerStatusEvent::Blocked {
                    failure: failure(),
                },
            ),
            Ok(CampaignStatus::Blocked { failure: failure() })
        );
    }

    for status in [
        CampaignStatus::Running { attempt_number: 2 },
        CampaignStatus::Pausing,
        CampaignStatus::Recovering { cycle: 1 },
        CampaignStatus::Stopping,
        CampaignStatus::Blocked { failure: failure() },
    ] {
        assert_eq!(
            reduce_status(&status, ControllerStatusEvent::CrashNormalized),
            Ok(CampaignStatus::Paused {
                reason: PauseReason::UnexpectedExit,
            })
        );
    }
}

#[test]
fn invalid_actor_transitions_and_recovery_overflow_are_typed() {
    assert_eq!(
        reduce_status(&CampaignStatus::Idle, ControllerStatusEvent::PauseStarted),
        Err(StatusTransitionError {
            status: CampaignStatus::Idle,
            event: ControllerStatusEvent::PauseStarted,
        })
    );
    assert_eq!(
        reduce_status(
            &CampaignStatus::Recovering { cycle: u8::MAX },
            ControllerStatusEvent::RecoveryCycle,
        ),
        Err(StatusTransitionError {
            status: CampaignStatus::Recovering { cycle: u8::MAX },
            event: ControllerStatusEvent::RecoveryCycle,
        })
    );
}

fn expected_command_transition(
    status: &CampaignStatus,
    command: &CampaignCommand,
) -> Result<ControllerDirective, super::CommandTransitionError> {
    let directive = match (status, command) {
        (CampaignStatus::Idle, CampaignCommand::Start)
        | (CampaignStatus::Won { .. }, CampaignCommand::Start) => {
            Some(ControllerDirective::BeginStart)
        }
        (CampaignStatus::Running { .. }, CampaignCommand::Pause) => {
            Some(ControllerDirective::BeginPause)
        }
        (CampaignStatus::Paused { .. }, CampaignCommand::Resume) => {
            Some(ControllerDirective::BeginResume)
        }
        (CampaignStatus::Running { .. }, CampaignCommand::Stop)
        | (CampaignStatus::Paused { .. }, CampaignCommand::Stop) => {
            Some(ControllerDirective::BeginStop)
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
        | (CampaignStatus::Blocked { .. }, CampaignCommand::Stop) => None,
    };
    directive.ok_or_else(|| super::CommandTransitionError {
        status: status.clone(),
        command: command.clone(),
    })
}

fn summary(attempt_number: u64) -> CampaignSummary {
    CampaignSummary {
        attempt_number,
        total_turns: 4,
        total_actions: 3,
        losses: attempt_number - 1,
        strategy: None,
        recent_turn_ids: vec!["turn-4".to_string()],
    }
}

fn failure() -> CampaignFailure {
    CampaignFailure::new(CampaignFailureKind::Helper, "helper unavailable")
        .expect("bounded failure")
}
