use super::*;

pub(super) async fn start_fresh_campaign(
    config: &ControllerConfig,
    store: Arc<CampaignCheckpointStore>,
    events: tokio::sync::broadcast::Sender<CampaignEvent>,
) -> Result<(ActiveCampaign, CampaignCheckpoint), ControllerError> {
    HelperLauncher::new(ReadinessLimits {
        timeout: std::time::Duration::from_secs(15),
        poll_interval: std::time::Duration::from_millis(100),
    })
    .ensure_serving(&config.deployment)
    .await?;
    let runtime_config = crate::load_runner_config(
        &config.deployment,
        &config.runner_executable,
    )
    .await?;
    let epoch = Uuid::new_v4().to_string();
    let gate = Arc::new(DecisionGate::new(/*owner_generation*/ 1));
    let persistence = Arc::new(CampaignPersistence::empty(store));
    let lease = Arc::new(OwnerLeaseState::new(epoch.clone(), /*generation*/ 1));
    let policy = Arc::new(GameCallPolicy::durable(
        lease,
        Arc::clone(&gate),
        Arc::clone(&persistence),
    ));
    let runtime = RunnerRuntime::start(
        runtime_config,
        Arc::clone(&policy),
        CampaignTools::specs(),
    )
    .await?;
    let rollout_path = runtime
        .session_configured
        .rollout_path
        .clone()
        .ok_or(RunnerError::MissingRolloutPath)?;
    let checkpoint = CampaignCheckpoint {
        schema_version: CHECKPOINT_VERSION,
        epoch,
        thread_id: runtime.thread_id.to_string(),
        rollout_path,
        deployment: CheckpointDeployment {
            helper_app: config.deployment.helper_app.clone(),
            socket_path: config.deployment.socket_path.clone(),
            target_app: config.deployment.target_app.clone(),
        },
        state: DurableCampaignState::Running,
        summary: CampaignSummary {
            attempt_number: 1,
            total_turns: 0,
            total_actions: 0,
            losses: 0,
            strategy: None,
            recent_turn_ids: Vec::new(),
        },
        owner_generation: 1,
        decision_audit: DecisionAudit::default(),
        policy_audit: PolicyAudit {
            mutation_attempts: 0,
            unknown_tool_attempts: 0,
            mutation_authorizations: 0,
        },
        latest_observation: None,
        unresolved_mutation: None,
    };
    persistence
        .install(checkpoint.clone())
        .await
        .map_err(|source| ControllerError::Persistence { source })?;
    let (command_tx, command_rx) = tokio::sync::mpsc::channel(1);
    let limits = config.limits;
    let target_app = config.deployment.target_app.clone();
    let worker_policy = Arc::clone(&policy);
    let worker_persistence = Arc::clone(&persistence);
    let worker = tokio::spawn(async move {
        let exit = CampaignRun::new(limits)
            .execute_controlled(
                &runtime.thread,
                &runtime.session_configured,
                worker_policy.as_ref(),
                gate,
                CampaignExecutionContext::Durable {
                    persistence: worker_persistence,
                    events,
                    commands: Some(command_rx),
                    start: CampaignStart::Fresh { target_app },
                },
            )
            .await?;
        Ok(WorkerCompletion { exit, runtime })
    });
    Ok((
        ActiveCampaign {
            command_tx,
            worker,
            policy,
            persistence,
            pending: None,
        },
        checkpoint,
    ))
}

pub(super) async fn resume_campaign(
    config: &ControllerConfig,
    store: Arc<CampaignCheckpointStore>,
    events: tokio::sync::broadcast::Sender<CampaignEvent>,
    checkpoint: CampaignCheckpoint,
) -> Result<(ActiveCampaign, CampaignCheckpoint), ControllerError> {
    HelperLauncher::new(ReadinessLimits {
        timeout: std::time::Duration::from_secs(15),
        poll_interval: std::time::Duration::from_millis(100),
    })
    .ensure_serving(&config.deployment)
    .await?;
    let runtime_config = crate::load_runner_config(
        &config.deployment,
        &config.runner_executable,
    )
    .await?;
    let persistence = Arc::new(CampaignPersistence::empty(store));
    persistence
        .install(checkpoint.clone())
        .await
        .map_err(|source| ControllerError::Persistence { source })?;
    let lease = Arc::new(OwnerLeaseState::new(
        checkpoint.epoch.clone(),
        checkpoint.owner_generation,
    ));
    let owner_generation = lease
        .increment_generation()
        .map_err(|error| RunnerError::CampaignFailed {
            message: error.to_string(),
        })?
        .generation;
    let next_observation_generation = checkpoint
        .latest_observation
        .as_ref()
        .map_or(Some(1), |observation| {
            observation.observation_sequence.checked_add(1)
        })
        .ok_or_else(|| RunnerError::CampaignFailed {
            message: "campaign observation generation overflowed".to_string(),
        })?;
    let gate = Arc::new(
        DecisionGate::restore(
            owner_generation,
            next_observation_generation,
            checkpoint.decision_audit,
        )
        .map_err(|error| RunnerError::CampaignFailed {
            message: error.to_string(),
        })?,
    );
    let policy = Arc::new(GameCallPolicy::durable(
        lease,
        Arc::clone(&gate),
        Arc::clone(&persistence),
    ));
    let expected_thread_id = ThreadId::from_string(&checkpoint.thread_id).map_err(|error| {
        RunnerError::CampaignFailed {
            message: format!("invalid checkpoint thread id: {error}"),
        }
    })?;
    let runtime = RunnerRuntime::resume(
        runtime_config,
        Arc::clone(&policy),
        checkpoint.rollout_path.clone(),
        expected_thread_id,
    )
    .await?;
    persistence
        .set_state(DurableCampaignState::Running, owner_generation)
        .await
        .map_err(|source| ControllerError::Persistence { source })?;
    let installed = persistence
        .snapshot()
        .await
        .map_err(|source| ControllerError::Persistence { source })?;
    let (command_tx, command_rx) = tokio::sync::mpsc::channel(1);
    let limits = config.limits;
    let worker_policy = Arc::clone(&policy);
    let worker_persistence = Arc::clone(&persistence);
    let worker_start = CampaignStart::Resumed {
        checkpoint: installed.clone(),
    };
    let worker = tokio::spawn(async move {
        let exit = CampaignRun::new(limits)
            .execute_controlled(
                &runtime.thread,
                &runtime.session_configured,
                worker_policy.as_ref(),
                gate,
                CampaignExecutionContext::Durable {
                    persistence: worker_persistence,
                    events,
                    commands: Some(command_rx),
                    start: worker_start,
                },
            )
            .await?;
        Ok(WorkerCompletion { exit, runtime })
    });
    Ok((
        ActiveCampaign {
            command_tx,
            worker,
            policy,
            persistence,
            pending: None,
        },
        installed,
    ))
}

pub(super) async fn finish_worker(
    completion: WorkerCompletion,
    persistence: &CampaignPersistence,
    events_tx: &tokio::sync::broadcast::Sender<CampaignEvent>,
    report_tx: &tokio::sync::mpsc::Sender<Result<CampaignReport, ControllerError>>,
    status: &mut CampaignStatus,
    status_tx: &tokio::sync::watch::Sender<CampaignStatus>,
) -> Result<(), ControllerError> {
    completion
        .runtime
        .thread
        .flush_rollout()
        .await
        .map_err(|error| RunnerError::CampaignFailed {
            message: format!("failed to flush campaign rollout: {error}"),
        })?;
    match completion.exit {
        CampaignExit::VerifiedWin(report) => {
            let evidence_reference = report
                .outcome
                .as_ref()
                .map(|outcome| outcome.observation.reference.clone())
                .ok_or_else(|| RunnerError::CampaignFailed {
                    message: "verified win report omitted outcome evidence".to_string(),
                })?;
            let owner_generation = persistence.snapshot().await.map_err(|source| {
                ControllerError::Persistence { source }
            })?.owner_generation;
            persistence
                .set_state(
                    DurableCampaignState::Won { evidence_reference },
                    owner_generation,
                )
                .await
                .map_err(|source| ControllerError::Persistence { source })?;
            let snapshot = persistence.snapshot().await.map_err(|source| {
                ControllerError::Persistence { source }
            })?;
            if let Some(outcome) = report.outcome.clone() {
                let _ = events_tx.send(CampaignEvent::Outcome(outcome));
            }
            update_status(
                status,
                ControllerStatusEvent::VictoryCommitted {
                    summary: snapshot.summary,
                },
                status_tx,
                events_tx,
            )?;
            shutdown_runtime(completion.runtime, ShutdownMode::Completed).await?;
            let _ = report_tx.send(Ok(report)).await;
        }
        CampaignExit::Paused => {
            let owner_generation = persistence.snapshot().await.map_err(|source| {
                ControllerError::Persistence { source }
            })?.owner_generation;
            persistence
                .set_state(
                    DurableCampaignState::Paused {
                        reason: PauseReason::Operator,
                    },
                    owner_generation,
                )
                .await
                .map_err(|source| ControllerError::Persistence { source })?;
            update_status(
                status,
                ControllerStatusEvent::PauseCommitted {
                    reason: PauseReason::Operator,
                },
                status_tx,
                events_tx,
            )?;
            shutdown_runtime(completion.runtime, ShutdownMode::Completed).await?;
            let _ = report_tx
                .send(Err(ControllerError::CampaignPaused {
                    reason: PauseReason::Operator,
                }))
                .await;
        }
        CampaignExit::Stopped => {
            shutdown_runtime(completion.runtime, ShutdownMode::Completed).await?;
            persistence
                .remove()
                .await
                .map_err(|source| ControllerError::Persistence { source })?;
            update_status(
                status,
                ControllerStatusEvent::StopCommitted,
                status_tx,
                events_tx,
            )?;
            let _ = report_tx.send(Err(ControllerError::CampaignStopped)).await;
        }
        CampaignExit::Blocked(report) => {
            shutdown_runtime(completion.runtime, ShutdownMode::Interrupt).await?;
            let failure = bounded_failure(
                CampaignFailureKind::Campaign,
                &report
                    .terminal_failure
                    .as_deref()
                    .unwrap_or("campaign blocked without a failure summary"),
            );
            let _ = events_tx.send(CampaignEvent::Failure(failure.clone()));
            update_status(
                status,
                ControllerStatusEvent::Blocked {
                    failure: failure.clone(),
                },
                status_tx,
                events_tx,
            )?;
            let _ = report_tx
                .send(Err(ControllerError::CampaignBlocked { failure }))
                .await;
        }
    }
    Ok(())
}

async fn shutdown_runtime(
    runtime: RunnerRuntime,
    mode: ShutdownMode,
) -> Result<(), ControllerError> {
    let errors = runtime.shutdown(mode).await;
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ControllerError::Runner(RunnerError::CampaignFailed {
            message: format!("campaign runtime cleanup failed: {}", errors.join("; ")),
        }))
    }
}
