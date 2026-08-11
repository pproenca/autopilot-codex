use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use pretty_assertions::assert_eq;

use crate::CHECKPOINT_VERSION;
use crate::CampaignCheckpoint;
use crate::CampaignSummary;
use crate::CheckpointDeployment;
use crate::DecisionAudit;
use crate::DurableCampaignState;
use crate::DurableMutation;
use crate::DurableMutationResult;
use crate::DurableObservation;
use crate::MAX_CHECKPOINT_BYTES;
use crate::PolicyAudit;
use crate::RunnerDeployment;
use crate::StrategyRecord;

use super::CampaignCheckpointStore;
use super::CheckpointStoreError;
use super::DurableCheckpointFs;
use super::DurableCheckpointTemp;

#[derive(Debug, Clone, PartialEq, Eq)]
enum FsOperation {
    CreateTemp,
    Write,
    SyncFile,
    Rename,
    SyncDirectory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailPoint {
    Rename,
    SyncDirectory,
}

#[derive(Default)]
struct RecordingState {
    operations: Vec<FsOperation>,
    temporary_bytes: Vec<u8>,
    checkpoint_bytes: Option<Vec<u8>>,
    fail_point: Option<FailPoint>,
}

#[derive(Clone, Default)]
struct RecordingFs {
    state: Arc<Mutex<RecordingState>>,
}

impl RecordingFs {
    fn with_checkpoint(checkpoint: &CampaignCheckpoint) -> anyhow::Result<Self> {
        Ok(Self {
            state: Arc::new(Mutex::new(RecordingState {
                checkpoint_bytes: Some(checkpoint.encode()?),
                ..RecordingState::default()
            })),
        })
    }

    fn fail_at(&self, fail_point: FailPoint) {
        self.state.lock().expect("recording state").fail_point = Some(fail_point);
    }

    fn operations(&self) -> Vec<FsOperation> {
        self.state
            .lock()
            .expect("recording state")
            .operations
            .clone()
    }

    fn checkpoint_bytes(&self) -> Option<Vec<u8>> {
        self.state
            .lock()
            .expect("recording state")
            .checkpoint_bytes
            .clone()
    }
}

struct RecordingTemp {
    state: Arc<Mutex<RecordingState>>,
}

impl Write for RecordingTemp {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.state.lock().expect("recording state");
        state.operations.push(FsOperation::Write);
        state.temporary_bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl DurableCheckpointTemp for RecordingTemp {
    fn sync_all(&self) -> io::Result<()> {
        self.state
            .lock()
            .expect("recording state")
            .operations
            .push(FsOperation::SyncFile);
        Ok(())
    }
}

impl DurableCheckpointFs for RecordingFs {
    fn acquire_lock(&self, _path: &Path) -> io::Result<Box<dyn Send>> {
        Ok(Box::new(()))
    }

    fn read_limited(&self, _path: &Path, _max_bytes: usize) -> io::Result<Option<Vec<u8>>> {
        Ok(self.checkpoint_bytes())
    }

    fn reject_symlink(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn create_temp(&self, _path: &Path) -> io::Result<Box<dyn DurableCheckpointTemp>> {
        let mut state = self.state.lock().expect("recording state");
        state.operations.push(FsOperation::CreateTemp);
        state.temporary_bytes.clear();
        drop(state);
        Ok(Box::new(RecordingTemp {
            state: Arc::clone(&self.state),
        }))
    }

    fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        let mut state = self.state.lock().expect("recording state");
        state.operations.push(FsOperation::Rename);
        if state.fail_point == Some(FailPoint::Rename) {
            return Err(io::Error::other("injected rename failure"));
        }
        state.checkpoint_bytes = Some(std::mem::take(&mut state.temporary_bytes));
        Ok(())
    }

    fn sync_directory(&self, _path: &Path) -> io::Result<()> {
        let mut state = self.state.lock().expect("recording state");
        state.operations.push(FsOperation::SyncDirectory);
        if state.fail_point == Some(FailPoint::SyncDirectory) {
            return Err(io::Error::other("injected directory sync failure"));
        }
        Ok(())
    }

    fn remove_file(&self, _path: &Path) -> io::Result<bool> {
        Ok(false)
    }
}

fn checkpoint() -> CampaignCheckpoint {
    let root = std::env::temp_dir().join("codex-game-runner-store-tests");
    CampaignCheckpoint {
        schema_version: CHECKPOINT_VERSION,
        epoch: "11111111-1111-4111-8111-111111111111".to_string(),
        thread_id: "22222222-2222-4222-8222-222222222222".to_string(),
        rollout_path: root.join("rollout.jsonl"),
        deployment: CheckpointDeployment {
            helper_app: root.join("GameHelper.app"),
            socket_path: root.join("game.sock"),
            target_app: "Difficult Game".to_string(),
        },
        state: DurableCampaignState::Running,
        summary: CampaignSummary {
            attempt_number: 1,
            total_turns: 1,
            total_actions: 1,
            losses: 0,
            strategy: Some(StrategyRecord {
                summary: "Build mobility".to_string(),
                confirmed_mechanics: Vec::new(),
                failed_approaches: Vec::new(),
                shop_and_boss_notes: Vec::new(),
                next_attempt_priorities: vec!["Buy mobility".to_string()],
            }),
            recent_turn_ids: vec!["turn-1".to_string()],
        },
        owner_generation: 1,
        decision_audit: DecisionAudit {
            plans_accepted: 1,
            plan_rejections: 0,
            mutation_attempts: 1,
            mutation_authorizations: 1,
            mutation_denials: 0,
        },
        policy_audit: PolicyAudit {
            mutation_attempts: 1,
            unknown_tool_attempts: 0,
            mutation_authorizations: 1,
        },
        latest_observation: Some(DurableObservation {
            observation_sequence: 1,
            confirms_action_sequence: None,
            reference: "sha256:before-action".to_string(),
        }),
        unresolved_mutation: Some(DurableMutation {
            action_sequence: 1,
            operation_id: "operation-1".to_string(),
            action_sha256: "a".repeat(64),
            tool: "click".to_string(),
            result: DurableMutationResult::Pending,
        }),
    }
}

fn deployment(checkpoint: &CampaignCheckpoint) -> RunnerDeployment {
    RunnerDeployment {
        helper_app: checkpoint.deployment.helper_app.clone(),
        socket_path: checkpoint.deployment.socket_path.clone(),
        target_app: checkpoint.deployment.target_app.clone(),
        codex_home: std::env::temp_dir().join("codex-home"),
    }
}

#[test]
fn replacement_orders_file_and_directory_durability_barriers() -> anyhow::Result<()> {
    let filesystem = RecordingFs::default();
    let root = PathBuf::from("/virtual/game-runner");
    let store = CampaignCheckpointStore::from_parts(root, Arc::new(filesystem.clone()));

    store.replace(&checkpoint())?;

    assert_eq!(
        filesystem.operations(),
        vec![
            FsOperation::CreateTemp,
            FsOperation::Write,
            FsOperation::SyncFile,
            FsOperation::Rename,
            FsOperation::SyncDirectory,
        ]
    );
    assert_eq!(
        CampaignCheckpoint::decode(
            &filesystem
                .checkpoint_bytes()
                .expect("checkpoint must be committed")
        )?,
        checkpoint()
    );
    Ok(())
}

#[test]
fn rename_failure_preserves_the_previous_checkpoint() -> anyhow::Result<()> {
    let previous = checkpoint();
    let mut candidate = checkpoint();
    candidate.owner_generation = 2;
    let filesystem = RecordingFs::with_checkpoint(&previous)?;
    filesystem.fail_at(FailPoint::Rename);
    let store = CampaignCheckpointStore::from_parts(
        PathBuf::from("/virtual/game-runner"),
        Arc::new(filesystem.clone()),
    );

    assert!(matches!(
        store.replace(&candidate),
        Err(CheckpointStoreError::Io {
            operation: "replace",
            ..
        })
    ));
    assert_eq!(
        CampaignCheckpoint::decode(
            &filesystem
                .checkpoint_bytes()
                .expect("previous checkpoint remains")
        )?,
        previous
    );
    Ok(())
}

#[test]
fn directory_sync_failure_reports_uncertain_committed_state() -> anyhow::Result<()> {
    let previous = checkpoint();
    let mut candidate = checkpoint();
    candidate.owner_generation = 2;
    let filesystem = RecordingFs::with_checkpoint(&previous)?;
    filesystem.fail_at(FailPoint::SyncDirectory);
    let store = CampaignCheckpointStore::from_parts(
        PathBuf::from("/virtual/game-runner"),
        Arc::new(filesystem.clone()),
    );

    assert!(matches!(
        store.replace(&candidate),
        Err(CheckpointStoreError::DurabilityUncertain {
            operation: "directory sync",
            ..
        })
    ));
    assert_eq!(
        CampaignCheckpoint::decode(
            &filesystem
                .checkpoint_bytes()
                .expect("candidate reached rename")
        )?,
        candidate
    );
    Ok(())
}

#[test]
fn load_durably_normalizes_crashed_running_mutation() -> anyhow::Result<()> {
    let running = checkpoint();
    let filesystem = RecordingFs::with_checkpoint(&running)?;
    let store = CampaignCheckpointStore::from_parts(
        PathBuf::from("/virtual/game-runner"),
        Arc::new(filesystem.clone()),
    );
    let mut expected = running.clone();
    expected.state = DurableCampaignState::Paused {
        reason: crate::PauseReason::UnexpectedExit,
    };
    expected
        .unresolved_mutation
        .as_mut()
        .expect("mutation")
        .result = DurableMutationResult::Indeterminate;

    assert_eq!(
        store.load_and_normalize(&deployment(&running))?,
        Some(expected.clone())
    );
    assert_eq!(
        CampaignCheckpoint::decode(
            &filesystem
                .checkpoint_bytes()
                .expect("normalized checkpoint")
        )?,
        expected
    );
    assert_eq!(
        filesystem.operations(),
        vec![
            FsOperation::CreateTemp,
            FsOperation::Write,
            FsOperation::SyncFile,
            FsOperation::Rename,
            FsOperation::SyncDirectory,
        ]
    );
    Ok(())
}

#[test]
fn deployment_mismatch_preserves_the_checkpoint() -> anyhow::Result<()> {
    let checkpoint = checkpoint();
    let original_bytes = checkpoint.encode()?;
    let filesystem = RecordingFs::with_checkpoint(&checkpoint)?;
    let store = CampaignCheckpointStore::from_parts(
        PathBuf::from("/virtual/game-runner"),
        Arc::new(filesystem.clone()),
    );
    let mut wrong_deployment = deployment(&checkpoint);
    wrong_deployment.target_app = "Other Game".to_string();

    assert!(matches!(
        store.load_and_normalize(&wrong_deployment),
        Err(CheckpointStoreError::DeploymentMismatch)
    ));
    assert_eq!(filesystem.checkpoint_bytes(), Some(original_bytes));
    assert_eq!(filesystem.operations(), Vec::<FsOperation>::new());
    Ok(())
}

#[test]
fn open_holds_an_exclusive_lock_for_the_guard_lifetime() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let (_store, guard) = CampaignCheckpointStore::open(codex_home.path())?;

    assert!(matches!(
        CampaignCheckpointStore::open(codex_home.path()),
        Err(CheckpointStoreError::AlreadyLocked { .. })
    ));

    drop(guard);
    let (_store, _guard) = CampaignCheckpointStore::open(codex_home.path())?;
    Ok(())
}

#[test]
fn real_store_replaces_reads_and_removes_a_checkpoint() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let (store, _guard) = CampaignCheckpointStore::open(codex_home.path())?;
    let mut expected = checkpoint();
    expected.state = DurableCampaignState::Paused {
        reason: crate::PauseReason::Operator,
    };
    expected
        .unresolved_mutation
        .as_mut()
        .expect("mutation")
        .result = DurableMutationResult::Indeterminate;

    store.replace(&expected)?;
    assert_eq!(
        store.load_and_normalize(&deployment(&expected))?,
        Some(expected)
    );
    store.remove()?;
    assert_eq!(store.load_and_normalize(&deployment(&checkpoint()))?, None);
    Ok(())
}

#[test]
fn oversized_checkpoint_is_rejected_before_json_decode() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let (store, _guard) = CampaignCheckpointStore::open(codex_home.path())?;
    std::fs::write(store.path(), vec![b' '; MAX_CHECKPOINT_BYTES + 1])?;

    assert!(matches!(
        store.load_and_normalize(&deployment(&checkpoint())),
        Err(CheckpointStoreError::Io {
            operation: "read",
            ..
        })
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_checkpoint_is_rejected_without_following_it() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let codex_home = tempfile::tempdir()?;
    let outside = tempfile::NamedTempFile::new()?;
    let (store, _guard) = CampaignCheckpointStore::open(codex_home.path())?;
    symlink(outside.path(), store.path())?;

    assert!(matches!(
        store.load_and_normalize(&deployment(&checkpoint())),
        Err(CheckpointStoreError::Io {
            operation: "inspect",
            ..
        })
    ));
    Ok(())
}
