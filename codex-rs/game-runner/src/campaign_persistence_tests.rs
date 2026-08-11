use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::CHECKPOINT_VERSION;
use crate::CampaignCheckpoint;
use crate::CampaignCheckpointStore;
use crate::CampaignStoreGuard;
use crate::CampaignSummary;
use crate::CheckpointDeployment;
use crate::DecisionAudit;
use crate::DurableCampaignState;
use crate::DurableMutation;
use crate::DurableMutationResult;
use crate::DurableObservation;
use crate::MutationResult;
use crate::ObservationEvidence;
use crate::PolicyAudit;
use crate::AuthorizedMutation;

use super::CampaignPersistence;
use super::MutationCheckpointUpdate;
use super::PersistenceError;

pub(crate) fn checkpoint() -> CampaignCheckpoint {
    let root = std::env::temp_dir().join("codex-game-runner-persistence-tests");
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
            total_actions: 0,
            losses: 0,
            strategy: None,
            recent_turn_ids: vec!["turn-1".to_string()],
        },
        owner_generation: 1,
        decision_audit: DecisionAudit::default(),
        policy_audit: PolicyAudit {
            mutation_attempts: 0,
            unknown_tool_attempts: 0,
            mutation_authorizations: 0,
        },
        latest_observation: Some(DurableObservation {
            observation_sequence: 1,
            confirms_action_sequence: None,
            reference: "sha256:initial".to_string(),
        }),
        unresolved_mutation: None,
    }
}

pub(crate) fn store() -> anyhow::Result<(
    tempfile::TempDir,
    Arc<CampaignCheckpointStore>,
    CampaignStoreGuard,
)> {
    let codex_home = tempfile::tempdir()?;
    let (store, guard) = CampaignCheckpointStore::open(codex_home.path())?;
    Ok((codex_home, Arc::new(store), guard))
}

#[tokio::test]
async fn install_and_summary_update_replace_the_complete_checkpoint() -> anyhow::Result<()> {
    let (_codex_home, store, _guard) = store()?;
    let persistence = CampaignPersistence::empty(Arc::clone(&store));
    let initial = checkpoint();

    persistence.install(initial.clone()).await?;
    assert_eq!(persistence.snapshot().await?, initial);

    let mut expected = checkpoint();
    expected.summary.total_turns = 2;
    expected.summary.recent_turn_ids.push("turn-2".to_string());
    persistence
        .persist_summary(
            expected.summary.clone(),
            expected.decision_audit,
            expected.policy_audit,
        )
        .await?;

    assert_eq!(persistence.snapshot().await?, expected.clone());
    assert_eq!(
        CampaignCheckpoint::decode(&std::fs::read(store.path())?)?,
        expected
    );
    Ok(())
}

#[tokio::test]
async fn mutation_protocol_persists_pending_result_and_confirming_observation() -> anyhow::Result<()>
{
    let (_codex_home, store, _guard) = store()?;
    let persistence = CampaignPersistence::empty(store);
    persistence.install(checkpoint()).await?;
    let decision_audit = DecisionAudit {
        plans_accepted: 1,
        plan_rejections: 0,
        mutation_attempts: 1,
        mutation_authorizations: 1,
        mutation_denials: 0,
    };
    let policy_audit = PolicyAudit {
        mutation_attempts: 1,
        unknown_tool_attempts: 0,
        mutation_authorizations: 1,
    };
    let expected_pending = DurableMutation {
        action_sequence: 1,
        operation_id: "operation-1".to_string(),
        action_sha256: "a".repeat(64),
        tool: "click".to_string(),
        result: DurableMutationResult::Pending,
    };

    assert_eq!(
        persistence
            .begin_mutation(&MutationCheckpointUpdate {
                authorization: AuthorizedMutation {
                    call_id: "call-1".to_string(),
                    operation_id: "operation-1".to_string(),
                    action_sha256: "a".repeat(64),
                    tool: "click".to_string(),
                    arguments: json!({"x": 40, "y": 50}),
                },
                decision_audit,
                policy_audit,
            })
            .await?,
        expected_pending
    );
    let mut expected = checkpoint();
    expected.summary.total_actions = 1;
    expected.decision_audit = decision_audit;
    expected.policy_audit = policy_audit;
    expected.unresolved_mutation = Some(expected_pending.clone());
    assert_eq!(persistence.snapshot().await?, expected);

    persistence
        .finish_mutation("call-1", MutationResult::Success)
        .await?;
    expected.unresolved_mutation.as_mut().expect("mutation").result =
        DurableMutationResult::Success;
    assert_eq!(persistence.snapshot().await?, expected);

    persistence.mark_unresolved_indeterminate().await?;
    expected.unresolved_mutation.as_mut().expect("mutation").result =
        DurableMutationResult::Indeterminate;
    assert_eq!(persistence.snapshot().await?, expected);

    persistence
        .confirm_observation(&ObservationEvidence {
            generation: 2,
            call_id: "capture-2".to_string(),
            reference: "sha256:after-action".to_string(),
            width: 1051,
            height: 820,
        })
        .await?;
    expected.latest_observation = Some(DurableObservation {
        observation_sequence: 2,
        confirms_action_sequence: Some(1),
        reference: "sha256:after-action".to_string(),
    });
    expected.unresolved_mutation = None;
    assert_eq!(persistence.snapshot().await?, expected);

    persistence
        .confirm_observation(&ObservationEvidence {
            generation: 3,
            call_id: "capture-3".to_string(),
            reference: "sha256:read-only-follow-up".to_string(),
            width: 1051,
            height: 820,
        })
        .await?;
    expected.latest_observation = Some(DurableObservation {
        observation_sequence: 3,
        confirms_action_sequence: Some(1),
        reference: "sha256:read-only-follow-up".to_string(),
    });
    assert_eq!(persistence.snapshot().await?, expected);
    Ok(())
}

#[tokio::test]
async fn durable_state_update_and_removal_replace_the_active_checkpoint() -> anyhow::Result<()> {
    let (_codex_home, store, _guard) = store()?;
    let persistence = CampaignPersistence::empty(Arc::clone(&store));
    persistence.install(checkpoint()).await?;
    let paused = DurableCampaignState::Paused {
        reason: crate::PauseReason::Operator,
    };

    persistence.set_state(paused.clone(), 2).await?;
    let mut expected = checkpoint();
    expected.state = paused;
    expected.owner_generation = 2;
    assert_eq!(persistence.snapshot().await?, expected);

    persistence.remove().await?;
    assert!(matches!(
        persistence.snapshot().await,
        Err(PersistenceError::MissingCheckpoint)
    ));
    assert_eq!(std::fs::exists(store.path())?, false);
    Ok(())
}
