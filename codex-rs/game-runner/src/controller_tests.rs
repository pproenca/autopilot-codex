use std::path::PathBuf;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::CampaignController;
use super::ControllerConfig;
use super::ControllerError;
use crate::CampaignCheckpointStore;
use crate::CampaignCommand;
use crate::CampaignEvent;
use crate::CampaignFailureKind;
use crate::CampaignLimits;
use crate::CampaignStatus;
use crate::CheckpointStoreError;
use crate::DurableCampaignState;
use crate::PauseReason;
use crate::RunnerDeployment;
use crate::campaign_persistence::tests::checkpoint;

#[tokio::test]
async fn open_idle_controller_rejects_invalid_commands_and_retains_the_lock() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let mut controller = CampaignController::open(config(codex_home.path().to_path_buf())).await?;

    assert_eq!(controller.status(), CampaignStatus::Idle);
    assert!(matches!(
        controller.command(CampaignCommand::Pause).await,
        Err(ControllerError::InvalidCommand(_))
    ));
    assert!(matches!(
        controller.command(CampaignCommand::Start).await,
        Err(ControllerError::Runner(_))
    ));
    assert_eq!(controller.status(), CampaignStatus::Idle);
    assert!(matches!(
        CampaignCheckpointStore::open(codex_home.path()),
        Err(CheckpointStoreError::AlreadyLocked { .. })
    ));

    controller.shutdown().await?;
    assert!(CampaignCheckpointStore::open(codex_home.path()).is_ok());
    Ok(())
}

#[tokio::test]
async fn corrupt_checkpoint_is_preserved_as_bounded_blocked_status() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let state_root = codex_home.path().join("game-runner");
    std::fs::create_dir(&state_root)?;
    let checkpoint_path = state_root.join("campaign.json");
    std::fs::write(&checkpoint_path, br#"{"schema_version":1,"corrupt":true}"#)?;

    let mut controller = CampaignController::open(config(codex_home.path().to_path_buf())).await?;
    let CampaignStatus::Blocked { failure } = controller.status() else {
        panic!("corrupt checkpoint should block the controller");
    };
    assert_eq!(failure.kind, CampaignFailureKind::Checkpoint);
    assert!(failure.summary.len() <= 2 * 1024);
    assert_eq!(
        std::fs::read(&checkpoint_path)?,
        br#"{"schema_version":1,"corrupt":true}"#
    );

    controller.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn paused_campaign_requires_resume_but_can_be_stopped_durably() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let controller_config = config(codex_home.path().to_path_buf());
    let (store, guard) = CampaignCheckpointStore::open(codex_home.path())?;
    let mut paused = checkpoint();
    paused.rollout_path = codex_home.path().join("rollout.jsonl");
    paused.deployment.helper_app = controller_config.deployment.helper_app.clone();
    paused.deployment.socket_path = controller_config.deployment.socket_path.clone();
    paused.deployment.target_app = controller_config.deployment.target_app.clone();
    paused.state = DurableCampaignState::Paused {
        reason: PauseReason::Operator,
    };
    store.replace(&paused)?;
    drop(guard);

    let mut controller = CampaignController::open(controller_config).await?;
    assert_eq!(
        controller.status(),
        CampaignStatus::Paused {
            reason: PauseReason::Operator,
        }
    );
    assert!(matches!(
        controller.command(CampaignCommand::Start).await,
        Err(ControllerError::InvalidCommand(_))
    ));
    let mut events = controller.subscribe();
    assert_eq!(
        controller.command(CampaignCommand::Stop).await?,
        CampaignStatus::Idle
    );
    assert_eq!(
        events.recv().await?,
        CampaignEvent::StatusChanged(CampaignStatus::Stopping)
    );
    assert_eq!(
        events.recv().await?,
        CampaignEvent::StatusChanged(CampaignStatus::Idle)
    );
    assert!(!store.path().exists());
    controller.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn failed_resume_blocks_the_campaign_with_bounded_failure() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let controller_config = config(codex_home.path().to_path_buf());
    let (store, guard) = CampaignCheckpointStore::open(codex_home.path())?;
    let mut paused = checkpoint();
    paused.rollout_path = codex_home.path().join("rollout.jsonl");
    paused.deployment.helper_app = controller_config.deployment.helper_app.clone();
    paused.deployment.socket_path = controller_config.deployment.socket_path.clone();
    paused.deployment.target_app = controller_config.deployment.target_app.clone();
    paused.state = DurableCampaignState::Paused {
        reason: PauseReason::Operator,
    };
    store.replace(&paused)?;
    drop(guard);

    let mut controller = CampaignController::open(controller_config).await?;
    let mut events = controller.subscribe();
    let Err(ControllerError::CampaignBlocked { failure }) =
        controller.command(CampaignCommand::Resume).await
    else {
        panic!("failed resume should block the campaign");
    };

    assert_eq!(failure.kind, CampaignFailureKind::Runtime);
    assert!(failure.summary.len() <= 2 * 1024);
    assert_eq!(
        controller.status(),
        CampaignStatus::Blocked {
            failure: failure.clone(),
        }
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if events.recv().await?
                == CampaignEvent::StatusChanged(CampaignStatus::Recovering { cycle: 0 })
            {
                return Ok::<_, tokio::sync::broadcast::error::RecvError>(());
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for recovering status"))??;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for failure event"))??,
        CampaignEvent::Failure(failure.clone())
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for blocked status"))??,
        CampaignEvent::StatusChanged(CampaignStatus::Blocked { failure })
    );

    tokio::time::timeout(Duration::from_secs(1), controller.shutdown())
        .await
        .map_err(|_| anyhow::anyhow!("timed out shutting down controller"))??;
    Ok(())
}

fn config(codex_home: PathBuf) -> ControllerConfig {
    ControllerConfig {
        deployment: RunnerDeployment {
            helper_app: codex_home.join("GameHelper.app"),
            socket_path: codex_home.join("game.sock"),
            target_app: "Difficult Game".to_string(),
            codex_home,
        },
        runner_executable: PathBuf::from("/not-used-before-start"),
        limits: CampaignLimits::stage_4b1(),
    }
}
