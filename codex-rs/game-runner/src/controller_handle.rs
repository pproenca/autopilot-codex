use std::sync::Arc;

use super::run_controller_actor;
use crate::CampaignCheckpointStore;
use crate::CampaignCommand;
use crate::CampaignEvent;
use crate::CampaignFailureKind;
use crate::CampaignReport;
use crate::CampaignStatus;
use crate::controller_types::ControllerConfig;
use crate::controller_types::ControllerError;
use crate::controller_types::ControllerRequest;
use crate::controller_types::EVENT_CAPACITY;
use crate::controller_types::REQUEST_CAPACITY;
use crate::controller_types::bounded_failure;
use crate::controller_types::status_from_checkpoint;

pub struct CampaignController {
    request_tx: tokio::sync::mpsc::Sender<ControllerRequest>,
    status_rx: tokio::sync::watch::Receiver<CampaignStatus>,
    events_tx: tokio::sync::broadcast::Sender<CampaignEvent>,
    report_rx: tokio::sync::mpsc::Receiver<Result<CampaignReport, ControllerError>>,
    actor: Option<tokio::task::JoinHandle<Result<(), ControllerError>>>,
}

impl CampaignController {
    pub async fn open(config: ControllerConfig) -> Result<Self, ControllerError> {
        let (store, guard) = CampaignCheckpointStore::open(&config.deployment.codex_home)
            .map_err(|source| ControllerError::Checkpoint { source })?;
        let store = Arc::new(store);
        let (status, checkpoint, initial_failure) = match store.load_and_normalize(&config.deployment) {
            Ok(None) => (CampaignStatus::Idle, None, None),
            Ok(Some(checkpoint)) => (
                status_from_checkpoint(&checkpoint),
                Some(checkpoint),
                None,
            ),
            Err(error) => {
                let failure = bounded_failure(CampaignFailureKind::Checkpoint, &error);
                (
                    CampaignStatus::Blocked {
                        failure: failure.clone(),
                    },
                    None,
                    Some(failure),
                )
            }
        };
        let (request_tx, request_rx) = tokio::sync::mpsc::channel(REQUEST_CAPACITY);
        let (status_tx, status_rx) = tokio::sync::watch::channel(status.clone());
        let (events_tx, _) = tokio::sync::broadcast::channel(EVENT_CAPACITY);
        let (report_tx, report_rx) = tokio::sync::mpsc::channel(1);
        let actor_events = events_tx.clone();
        let actor = tokio::spawn(run_controller_actor(
            config,
            store,
            guard,
            status,
            checkpoint,
            initial_failure,
            status_tx,
            actor_events,
            report_tx,
            request_rx,
        ));
        Ok(Self {
            request_tx,
            status_rx,
            events_tx,
            report_rx,
            actor: Some(actor),
        })
    }

    pub async fn command(
        &self,
        command: CampaignCommand,
    ) -> Result<CampaignStatus, ControllerError> {
        let (response, receiver) = tokio::sync::oneshot::channel();
        self.request_tx
            .send(ControllerRequest::Command { command, response })
            .await
            .map_err(|_| ControllerError::ActorClosed)?;
        receiver.await.map_err(|_| ControllerError::ActorClosed)?
    }

    pub fn status(&self) -> CampaignStatus {
        self.status_rx.borrow().clone()
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<CampaignEvent> {
        self.events_tx.subscribe()
    }

    pub async fn compact(&self) -> Result<(), ControllerError> {
        let (response, receiver) = tokio::sync::oneshot::channel();
        self.request_tx
            .send(ControllerRequest::Compact { response })
            .await
            .map_err(|_| ControllerError::ActorClosed)?;
        receiver.await.map_err(|_| ControllerError::ActorClosed)?
    }

    pub async fn wait_for_report(&mut self) -> Result<CampaignReport, ControllerError> {
        match self.status() {
            CampaignStatus::Paused { reason } => {
                return Err(ControllerError::CampaignPaused { reason });
            }
            CampaignStatus::Blocked { failure } => {
                return Err(ControllerError::CampaignBlocked { failure });
            }
            CampaignStatus::Idle => return Err(ControllerError::CampaignStopped),
            CampaignStatus::Running { .. }
            | CampaignStatus::Pausing
            | CampaignStatus::Recovering { .. }
            | CampaignStatus::Stopping
            | CampaignStatus::Won { .. } => {}
        }
        let actor = self.actor.as_mut().ok_or(ControllerError::ActorClosed)?;
        tokio::select! {
            report = self.report_rx.recv() => {
                report.ok_or(ControllerError::ActorClosed)?
            }
            actor_result = actor => {
                actor_result.map_err(|_| ControllerError::ActorClosed)??;
                Err(ControllerError::ActorClosed)
            }
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), ControllerError> {
        if matches!(self.status(), CampaignStatus::Running { .. }) {
            self.command(CampaignCommand::Stop).await?;
        }
        let (response, receiver) = tokio::sync::oneshot::channel();
        self.request_tx
            .send(ControllerRequest::Shutdown { response })
            .await
            .map_err(|_| ControllerError::ActorClosed)?;
        receiver.await.map_err(|_| ControllerError::ActorClosed)?;
        let actor = self.actor.take().ok_or(ControllerError::ActorClosed)?;
        actor.await.map_err(|_| ControllerError::ActorClosed)??;
        Ok(())
    }
}
