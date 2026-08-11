use std::sync::Arc;

use codex_core_api::ThreadId;
use uuid::Uuid;

use crate::CHECKPOINT_VERSION;
use crate::CampaignCheckpoint;
use crate::CampaignCheckpointStore;
use crate::CampaignCommand;
use crate::CampaignExecutionContext;
use crate::CampaignEvent;
use crate::CampaignExit;
use crate::CampaignFailure;
use crate::CampaignFailureKind;
use crate::CampaignPersistence;
use crate::CampaignReport;
use crate::CampaignRun;
use crate::CampaignStart;
use crate::CampaignStatus;
use crate::CampaignStoreGuard;
use crate::CampaignSummary;
use crate::CampaignTools;
use crate::CheckpointDeployment;
use crate::DurableCampaignState;
use crate::PauseReason;
use crate::DecisionGate;
use crate::DecisionAudit;
use crate::GameCallPolicy;
use crate::GameToolFailureSignal;
use crate::HelperLauncher;
use crate::OwnerLeaseState;
use crate::PolicyAudit;
use crate::ReadinessLimits;
use crate::RunnerError;
use crate::RunnerRuntime;
use crate::ShutdownMode;
use crate::WorkerCommand;
use crate::controller_types::ControllerDirective;
use crate::controller_types::ControllerConfig;
use crate::controller_types::ControllerError;
use crate::controller_types::ControllerRequest;
use crate::controller_types::ControllerStatusEvent;
use crate::controller_types::PendingCommand;
use crate::controller_types::WorkerCompletion;
use crate::controller_types::bounded_failure;
use crate::controller_types::reduce_command;
use crate::controller_types::reduce_status;

#[path = "controller_handle.rs"]
mod controller_handle;

pub use controller_handle::CampaignController;

#[path = "controller_runtime.rs"]
mod controller_runtime;

#[path = "controller_recovery.rs"]
mod controller_recovery;

use controller_runtime::finish_worker;
use controller_runtime::resume_campaign;
use controller_runtime::start_fresh_campaign;

struct ActiveCampaign {
    command_tx: tokio::sync::mpsc::Sender<WorkerCommand>,
    failure_rx: tokio::sync::mpsc::Receiver<GameToolFailureSignal>,
    failures_closed: bool,
    worker: tokio::task::JoinHandle<WorkerCompletion>,
    policy: Arc<GameCallPolicy>,
    persistence: Arc<CampaignPersistence>,
    pending: Option<PendingCommand>,
}

enum ActorInput {
    Request(Option<ControllerRequest>),
    Failure(Option<GameToolFailureSignal>),
    Worker(Result<WorkerCompletion, tokio::task::JoinError>),
}

async fn run_controller_actor(
    config: ControllerConfig,
    store: Arc<CampaignCheckpointStore>,
    _guard: CampaignStoreGuard,
    mut status: CampaignStatus,
    mut checkpoint: Option<CampaignCheckpoint>,
    initial_failure: Option<CampaignFailure>,
    status_tx: tokio::sync::watch::Sender<CampaignStatus>,
    events_tx: tokio::sync::broadcast::Sender<CampaignEvent>,
    report_tx: tokio::sync::mpsc::Sender<Result<CampaignReport, ControllerError>>,
    mut request_rx: tokio::sync::mpsc::Receiver<ControllerRequest>,
) -> Result<(), ControllerError> {
    let _ = events_tx.send(CampaignEvent::StatusChanged(status.clone()));
    if let Some(failure) = initial_failure {
        let _ = events_tx.send(CampaignEvent::Failure(failure));
    }
    let mut active: Option<ActiveCampaign> = None;
    loop {
        let input = match active.as_mut() {
            Some(active) => tokio::select! {
                request = request_rx.recv() => ActorInput::Request(request),
                failure = active.failure_rx.recv(), if !active.failures_closed => {
                    ActorInput::Failure(failure)
                }
                completion = &mut active.worker => ActorInput::Worker(completion),
            },
            None => ActorInput::Request(request_rx.recv().await),
        };
        match input {
            ActorInput::Request(None) => break,
            ActorInput::Request(Some(ControllerRequest::Compact { response })) => {
                let result = match (&status, active.as_mut()) {
                    (CampaignStatus::Running { .. }, Some(active_campaign)) => active_campaign
                        .command_tx
                        .send(WorkerCommand::Compact)
                        .await
                        .map_err(|_| ControllerError::ActorClosed),
                    _ => Err(ControllerError::CampaignNotRunning),
                };
                let _ = response.send(result);
            }
            ActorInput::Request(Some(ControllerRequest::Command { command, response })) => {
                let directive = match reduce_command(&status, command.clone()) {
                    Ok(directive) => directive,
                    Err(error) => {
                        let controller_error = if matches!(
                            (&status, &command),
                            (CampaignStatus::Paused { .. }, CampaignCommand::Start)
                        ) {
                            ControllerError::CampaignRequiresResume {
                                path: store.path().to_path_buf(),
                            }
                        } else {
                            ControllerError::InvalidCommand(error)
                        };
                        let _ = response.send(Err(controller_error));
                        continue;
                    }
                };
                match directive {
                    ControllerDirective::BeginStart => {
                        match start_fresh_campaign(
                            &config,
                            Arc::clone(&store),
                            events_tx.clone(),
                        )
                        .await
                        {
                            Ok((started, installed)) => {
                                checkpoint = Some(installed);
                                active = Some(started);
                                update_status(
                                    &mut status,
                                    ControllerStatusEvent::StartCommitted {
                                        attempt_number: 1,
                                    },
                                    &status_tx,
                                    &events_tx,
                                )?;
                                let _ = response.send(Ok(status.clone()));
                            }
                            Err(error) => {
                                let _ = response.send(Err(error));
                            }
                        }
                    }
                    ControllerDirective::BeginPause | ControllerDirective::BeginStop => {
                        let Some(active_campaign) = active.as_mut() else {
                            if directive == ControllerDirective::BeginStop {
                                update_status(
                                    &mut status,
                                    ControllerStatusEvent::StopStarted,
                                    &status_tx,
                                    &events_tx,
                                )?;
                                let persistence = CampaignPersistence::empty(Arc::clone(&store));
                                if let Some(existing) = checkpoint.take() {
                                    persistence.install(existing).await.map_err(|source| {
                                        ControllerError::Persistence { source }
                                    })?;
                                    persistence.remove().await.map_err(|source| {
                                        ControllerError::Persistence { source }
                                    })?;
                                }
                                update_status(
                                    &mut status,
                                    ControllerStatusEvent::StopCommitted,
                                    &status_tx,
                                    &events_tx,
                                )?;
                                let _ = response.send(Ok(status.clone()));
                            } else {
                                let _ = response.send(Err(ControllerError::ActorClosed));
                            }
                            continue;
                        };
                        let (worker_command, status_event) = match directive {
                            ControllerDirective::BeginPause => {
                                (WorkerCommand::Pause, ControllerStatusEvent::PauseStarted)
                            }
                            ControllerDirective::BeginStop => {
                                (WorkerCommand::Stop, ControllerStatusEvent::StopStarted)
                            }
                            ControllerDirective::BeginStart | ControllerDirective::BeginResume => {
                                unreachable!("directive arm is restricted to pause or stop")
                            }
                        };
                        active_campaign.policy.close_mutation_lane();
                        active_campaign
                            .command_tx
                            .send(worker_command)
                            .await
                            .map_err(|_| ControllerError::ActorClosed)?;
                        active_campaign.pending = Some(PendingCommand { response });
                        update_status(
                            &mut status,
                            status_event,
                            &status_tx,
                            &events_tx,
                        )?;
                    }
                    ControllerDirective::BeginResume => {
                        let Some(existing) = checkpoint.clone() else {
                            let _ = response.send(Err(ControllerError::ActorClosed));
                            continue;
                        };
                        update_status(
                            &mut status,
                            ControllerStatusEvent::ResumeStarted,
                            &status_tx,
                            &events_tx,
                        )?;
                        match resume_campaign(
                            &config,
                            Arc::clone(&store),
                            events_tx.clone(),
                            existing,
                        )
                        .await
                        {
                            Ok((resumed, installed)) => {
                                let attempt_number = installed.summary.attempt_number;
                                checkpoint = Some(installed);
                                active = Some(resumed);
                                update_status(
                                    &mut status,
                                    ControllerStatusEvent::RunningCommitted { attempt_number },
                                    &status_tx,
                                    &events_tx,
                                )?;
                                let _ = response.send(Ok(status.clone()));
                            }
                            Err(error) => {
                                let failure =
                                    bounded_failure(CampaignFailureKind::Runtime, &error);
                                let _ = events_tx.send(CampaignEvent::Failure(failure.clone()));
                                update_status(
                                    &mut status,
                                    ControllerStatusEvent::Blocked {
                                        failure: failure.clone(),
                                    },
                                    &status_tx,
                                    &events_tx,
                                )?;
                                let _ = report_tx
                                    .send(Err(ControllerError::CampaignBlocked {
                                        failure: failure.clone(),
                                    }))
                                    .await;
                                let _ = response
                                    .send(Err(ControllerError::CampaignBlocked { failure }));
                            }
                        }
                    }
                }
            }
            ActorInput::Request(Some(ControllerRequest::Shutdown { response })) => {
                if let Some(active_campaign) = &active {
                    active_campaign.policy.close_mutation_lane();
                    let _ = active_campaign.command_tx.send(WorkerCommand::Stop).await;
                }
                let _ = response.send(());
                break;
            }
            ActorInput::Failure(None) => {
                let active_campaign = active.as_mut().ok_or(ControllerError::ActorClosed)?;
                active_campaign.failures_closed = true;
            }
            ActorInput::Failure(Some(signal)) => {
                controller_recovery::handle_game_tool_failure(
                    &config,
                    active.as_mut().ok_or(ControllerError::ActorClosed)?,
                    &mut status,
                    &status_tx,
                    &events_tx,
                    signal,
                )
                .await?;
            }
            ActorInput::Worker(completion) => {
                let mut finished = active.take().ok_or(ControllerError::ActorClosed)?;
                let completion = completion.map_err(|_| ControllerError::ActorClosed)?;
                let pending = finished.pending.take();
                let mut recovery_checkpoint_installed = false;
                let result = match completion {
                    WorkerCompletion {
                        exit: Ok(CampaignExit::RecoveryRequired),
                        runtime,
                    } => {
                        match controller_recovery::finish_recovery_exit(
                            &config,
                            Arc::clone(&store),
                            runtime,
                            Arc::clone(&finished.persistence),
                            &mut status,
                            &status_tx,
                            &events_tx,
                            &report_tx,
                        )
                        .await
                        {
                            Ok(recovery) => {
                                active = recovery.active;
                                checkpoint = Some(recovery.checkpoint);
                                recovery_checkpoint_installed = true;
                                Ok(())
                            }
                            Err(error) => Err(error),
                        }
                    }
                    completion => {
                        finish_worker(
                            completion,
                            &finished.persistence,
                            &events_tx,
                            &report_tx,
                            &mut status,
                            &status_tx,
                        )
                        .await
                    }
                };
                if let Err(error) = &result {
                    let failure = bounded_failure(CampaignFailureKind::Runtime, error);
                    let _ = events_tx.send(CampaignEvent::Failure(failure.clone()));
                    update_status(
                        &mut status,
                        ControllerStatusEvent::Blocked {
                            failure: failure.clone(),
                        },
                        &status_tx,
                        &events_tx,
                    )?;
                    let _ = report_tx
                        .send(Err(ControllerError::CampaignBlocked { failure }))
                        .await;
                }
                if !recovery_checkpoint_installed {
                    if let Ok(updated_checkpoint) = finished.persistence.snapshot().await {
                        checkpoint = Some(updated_checkpoint);
                    } else if status == CampaignStatus::Idle {
                        checkpoint = None;
                    }
                }
                if let Some(pending) = pending {
                    let response_result = match result {
                        Ok(()) => Ok(status.clone()),
                        Err(error) => Err(error),
                    };
                    let _ = pending.response.send(response_result);
                }
            }
        }
    }
    Ok(())
}

fn update_status(
    status: &mut CampaignStatus,
    event: ControllerStatusEvent,
    status_tx: &tokio::sync::watch::Sender<CampaignStatus>,
    events_tx: &tokio::sync::broadcast::Sender<CampaignEvent>,
) -> Result<(), ControllerError> {
    *status = reduce_status(status, event).map_err(|error| {
        ControllerError::Runner(RunnerError::CampaignFailed {
            message: error.to_string(),
        })
    })?;
    status_tx.send_replace(status.clone());
    let _ = events_tx.send(CampaignEvent::StatusChanged(status.clone()));
    Ok(())
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
