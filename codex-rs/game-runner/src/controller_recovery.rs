use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::ActiveCampaign;
use super::ControllerConfig;
use super::controller_runtime::resume_ready_campaign;
use super::controller_runtime::shutdown_runtime;
use super::update_status;
use crate::CampaignCheckpoint;
use crate::CampaignCheckpointStore;
use crate::CampaignEvent;
use crate::CampaignFailureKind;
use crate::CampaignPersistence;
use crate::CampaignReport;
use crate::CampaignStatus;
use crate::ControllerError;
use crate::DurableCampaignState;
use crate::GameToolFailureSignal;
use crate::HelperLauncher;
use crate::HelperReadiness;
use crate::HelperRecovery;
use crate::PauseReason;
use crate::ReadinessLimits;
use crate::RecoveryLimits;
use crate::RecoveryOutcome;
use crate::RunnerError;
use crate::RunnerRuntime;
use crate::ShutdownMode;
use crate::WorkerDirective;
use crate::controller_types::ControllerStatusEvent;
use crate::controller_types::bounded_failure;

pub(super) async fn classify_game_tool_failure<R: HelperReadiness>(
    recovery: &HelperRecovery<R>,
    socket_path: &Path,
) -> WorkerDirective {
    if recovery.socket_is_ready(socket_path).await {
        WorkerDirective::Continue
    } else {
        WorkerDirective::PauseForRecovery
    }
}

pub(super) async fn handle_game_tool_failure(
    config: &ControllerConfig,
    active: &mut ActiveCampaign,
    status: &mut CampaignStatus,
    status_tx: &tokio::sync::watch::Sender<CampaignStatus>,
    events_tx: &tokio::sync::broadcast::Sender<CampaignEvent>,
    signal: GameToolFailureSignal,
) -> Result<(), ControllerError> {
    if active.pending.is_some() || !matches!(status, CampaignStatus::Running { .. }) {
        let _ = signal.response.send(WorkerDirective::Continue);
        return Ok(());
    }
    let recovery = HelperRecovery::new(
        HelperLauncher::new(ReadinessLimits {
            timeout: Duration::from_secs(15),
            poll_interval: Duration::from_millis(100),
        }),
        RecoveryLimits::stage_4b2(),
    );
    if classify_game_tool_failure(&recovery, &config.deployment.socket_path).await
        == WorkerDirective::Continue
    {
        let _ = signal.response.send(WorkerDirective::Continue);
        return Ok(());
    }

    active.policy.close_mutation_lane();
    let failure = bounded_failure(
        CampaignFailureKind::Helper,
        &format!("{} failed: {}", signal.tool, signal.summary),
    );
    let _ = events_tx.send(CampaignEvent::Failure(failure));
    if let Err(error) = active.persistence.mark_unresolved_indeterminate().await {
        let failure = bounded_failure(CampaignFailureKind::Checkpoint, &error);
        let _ = events_tx.send(CampaignEvent::Failure(failure.clone()));
        update_status(
            status,
            ControllerStatusEvent::Blocked { failure },
            status_tx,
            events_tx,
        )?;
        let _ = signal.response.send(WorkerDirective::PauseForRecovery);
        return Ok(());
    }
    update_status(
        status,
        ControllerStatusEvent::ResumeStarted,
        status_tx,
        events_tx,
    )?;
    let _ = signal.response.send(WorkerDirective::PauseForRecovery);
    Ok(())
}

pub(super) async fn pause_exhausted_recovery(
    persistence: &CampaignPersistence,
    reason: PauseReason,
    status: &mut CampaignStatus,
    status_tx: &tokio::sync::watch::Sender<CampaignStatus>,
    events_tx: &tokio::sync::broadcast::Sender<CampaignEvent>,
) -> Result<CampaignCheckpoint, ControllerError> {
    let owner_generation = persistence
        .snapshot()
        .await
        .map_err(|source| ControllerError::Persistence { source })?
        .owner_generation;
    persistence
        .set_state(
            DurableCampaignState::Paused {
                reason: reason.clone(),
            },
            owner_generation,
        )
        .await
        .map_err(|source| ControllerError::Persistence { source })?;
    update_status(
        status,
        ControllerStatusEvent::PauseCommitted { reason },
        status_tx,
        events_tx,
    )?;
    persistence
        .snapshot()
        .await
        .map_err(|source| ControllerError::Persistence { source })
}

pub(super) struct RecoveryCompletion {
    pub(super) active: Option<ActiveCampaign>,
    pub(super) checkpoint: CampaignCheckpoint,
}

pub(super) struct RecoveryExitContext<'a> {
    pub(super) config: &'a ControllerConfig,
    pub(super) store: Arc<CampaignCheckpointStore>,
    pub(super) persistence: Arc<CampaignPersistence>,
    pub(super) status: &'a mut CampaignStatus,
    pub(super) status_tx: &'a tokio::sync::watch::Sender<CampaignStatus>,
    pub(super) events_tx: &'a tokio::sync::broadcast::Sender<CampaignEvent>,
    pub(super) report_tx: &'a tokio::sync::mpsc::Sender<Result<CampaignReport, ControllerError>>,
}

pub(super) async fn finish_recovery_exit(
    runtime: RunnerRuntime,
    context: RecoveryExitContext<'_>,
) -> Result<RecoveryCompletion, ControllerError> {
    let RecoveryExitContext {
        config,
        store,
        persistence,
        status,
        status_tx,
        events_tx,
        report_tx,
    } = context;
    let flush_result = runtime.thread.flush_rollout().await.map_err(|error| {
        ControllerError::Runner(RunnerError::CampaignFailed {
            message: format!("failed to flush damaged campaign rollout: {error}"),
        })
    });
    let shutdown_result = shutdown_runtime(runtime, ShutdownMode::Completed).await;
    flush_result?;
    shutdown_result?;

    if let CampaignStatus::Blocked { failure } = status {
        let failure = failure.clone();
        let _ = report_tx
            .send(Err(ControllerError::CampaignBlocked { failure }))
            .await;
        return Ok(RecoveryCompletion {
            active: None,
            checkpoint: persistence
                .snapshot()
                .await
                .map_err(|source| ControllerError::Persistence { source })?,
        });
    }

    let recovery = HelperRecovery::new(
        HelperLauncher::new(ReadinessLimits {
            timeout: Duration::from_secs(15),
            poll_interval: Duration::from_millis(100),
        }),
        RecoveryLimits::stage_4b2(),
    );
    let outcome = recovery.recover(&config.deployment).await?;
    let attempts = match &outcome {
        RecoveryOutcome::Recovered { attempts } | RecoveryOutcome::Exhausted { attempts, .. } => {
            *attempts
        }
    };
    for _ in 0..attempts {
        update_status(
            status,
            ControllerStatusEvent::RecoveryCycle,
            status_tx,
            events_tx,
        )?;
    }
    match outcome {
        RecoveryOutcome::Recovered { attempts: _ } => {
            let checkpoint = persistence
                .snapshot()
                .await
                .map_err(|source| ControllerError::Persistence { source })?;
            let (active, checkpoint) =
                resume_ready_campaign(config, store, events_tx.clone(), checkpoint).await?;
            update_status(
                status,
                ControllerStatusEvent::RunningCommitted {
                    attempt_number: checkpoint.summary.attempt_number,
                },
                status_tx,
                events_tx,
            )?;
            Ok(RecoveryCompletion {
                active: Some(active),
                checkpoint,
            })
        }
        RecoveryOutcome::Exhausted {
            attempts: _,
            reason,
        } => {
            let checkpoint = pause_exhausted_recovery(
                persistence.as_ref(),
                reason.clone(),
                status,
                status_tx,
                events_tx,
            )
            .await?;
            let _ = report_tx
                .send(Err(ControllerError::CampaignPaused { reason }))
                .await;
            Ok(RecoveryCompletion {
                active: None,
                checkpoint,
            })
        }
    }
}

#[cfg(test)]
#[path = "controller_recovery_tests.rs"]
mod tests;
