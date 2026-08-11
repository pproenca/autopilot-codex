use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use super::classify_game_tool_failure;
use super::pause_exhausted_recovery;
use crate::CampaignEvent;
use crate::CampaignPersistence;
use crate::CampaignStatus;
use crate::HelperReadiness;
use crate::HelperRecovery;
use crate::RecoveryLimits;
use crate::RunnerDeployment;
use crate::RunnerError;
use crate::WorkerDirective;
use crate::campaign_persistence::tests::checkpoint;
use crate::campaign_persistence::tests::store;

struct SocketReadiness(bool);

impl HelperReadiness for SocketReadiness {
    fn socket_is_ready(
        &self,
        _socket_path: &Path,
    ) -> impl std::future::Future<Output = bool> + Send {
        std::future::ready(self.0)
    }

    fn ensure_serving(
        &self,
        _deployment: &RunnerDeployment,
    ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send {
        std::future::ready(Err(RunnerError::CampaignFailed {
            message: "recovery should not start during classification".to_string(),
        }))
    }
}

#[tokio::test]
async fn healthy_socket_continues_and_unavailable_socket_pauses_for_recovery() {
    for (ready, expected) in [
        (true, WorkerDirective::Continue),
        (false, WorkerDirective::PauseForRecovery),
    ] {
        let recovery = HelperRecovery::new(SocketReadiness(ready), RecoveryLimits::stage_4b2());
        assert_eq!(
            classify_game_tool_failure(&recovery, Path::new("/tmp/game.sock")).await,
            expected
        );
    }
}

#[tokio::test]
async fn exhausted_recovery_pauses_without_changing_losses_or_generation() -> anyhow::Result<()> {
    let (_codex_home, store, _guard) = store()?;
    let persistence = Arc::new(CampaignPersistence::empty(store));
    let mut initial = checkpoint();
    initial.summary.attempt_number = 3;
    initial.summary.total_turns = 3;
    initial.summary.losses = 2;
    initial.owner_generation = 7;
    persistence.install(initial.clone()).await?;
    let reason = crate::PauseReason::HelperUnavailable {
        summary: "helper unavailable after 3 recovery cycles".to_string(),
    };
    let mut status = CampaignStatus::Recovering { cycle: 3 };
    let (status_tx, status_rx) = tokio::sync::watch::channel(status.clone());
    let (events_tx, _) = tokio::sync::broadcast::channel::<CampaignEvent>(1);

    let paused = pause_exhausted_recovery(
        persistence.as_ref(),
        reason.clone(),
        &mut status,
        &status_tx,
        &events_tx,
    )
    .await?;

    let mut expected = initial;
    expected.state = crate::DurableCampaignState::Paused {
        reason: reason.clone(),
    };
    assert_eq!(paused, expected);
    assert_eq!(
        status,
        CampaignStatus::Paused {
            reason: reason.clone(),
        }
    );
    assert_eq!(
        status_rx.borrow().clone(),
        CampaignStatus::Paused { reason }
    );
    Ok(())
}
