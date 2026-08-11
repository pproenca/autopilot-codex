use std::path::PathBuf;

use pretty_assertions::assert_eq;
use serde_json::Value;

use crate::CampaignSummary;
use crate::DecisionAudit;
use crate::PolicyAudit;
use crate::StrategyRecord;

use super::CHECKPOINT_VERSION;
use super::CampaignCheckpoint;
use super::CheckpointDeployment;
use super::CheckpointValidationError;
use super::DurableCampaignState;
use super::DurableMutation;
use super::DurableMutationResult;
use super::DurableObservation;
use super::MAX_CHECKPOINT_BYTES;
use super::PauseReason;

fn strategy() -> StrategyRecord {
    StrategyRecord {
        summary: "Build mobility before committing to the boss".to_string(),
        confirmed_mechanics: vec!["The shop accepts drag-to-buy".to_string()],
        failed_approaches: vec!["An early all-in stalled at the boss".to_string()],
        shop_and_boss_notes: vec!["Keep one reroll for the boss shop".to_string()],
        next_attempt_priorities: vec!["Buy mobility".to_string()],
    }
}

fn valid_checkpoint() -> CampaignCheckpoint {
    let root = std::env::temp_dir().join("codex-game-runner-checkpoint-tests");
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
            strategy: Some(strategy()),
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

#[test]
fn version_one_checkpoint_round_trips_as_one_value() -> anyhow::Result<()> {
    let checkpoint = valid_checkpoint();
    let encoded = checkpoint.encode()?;
    assert_eq!(CampaignCheckpoint::decode(&encoded)?, checkpoint);
    Ok(())
}

#[test]
fn checkpoint_rejects_invalid_identity_paths_and_bounds() {
    let cases = [
        (
            {
                let mut value = valid_checkpoint();
                value.schema_version = 2;
                value
            },
            CheckpointValidationError::UnsupportedVersion { actual: 2 },
        ),
        (
            {
                let mut value = valid_checkpoint();
                value.epoch.clear();
                value
            },
            CheckpointValidationError::EmptyString { field: "epoch" },
        ),
        (
            {
                let mut value = valid_checkpoint();
                value.thread_id = "not-a-thread-id".to_string();
                value
            },
            CheckpointValidationError::InvalidThreadId,
        ),
        (
            {
                let mut value = valid_checkpoint();
                value.rollout_path = PathBuf::from("relative-rollout.jsonl");
                value
            },
            CheckpointValidationError::PathNotAbsolute {
                field: "rollout_path",
            },
        ),
        (
            {
                let mut value = valid_checkpoint();
                value.deployment.target_app.clear();
                value
            },
            CheckpointValidationError::EmptyString {
                field: "target_app",
            },
        ),
        (
            {
                let mut value = valid_checkpoint();
                value.state = DurableCampaignState::Paused {
                    reason: PauseReason::HelperUnavailable {
                        summary: "x".repeat(2049),
                    },
                };
                value
            },
            CheckpointValidationError::StringTooLarge {
                field: "pause_reason.summary",
                max_bytes: 2048,
            },
        ),
    ];

    assert_eq!(
        cases
            .iter()
            .map(|(value, _)| value.validate())
            .collect::<Vec<_>>(),
        cases
            .iter()
            .map(|(_, error)| Err(error.clone()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn checkpoint_rejects_inconsistent_campaign_and_mutation_state() {
    let cases = [
        (
            {
                let mut value = valid_checkpoint();
                value.summary.attempt_number = 0;
                value
            },
            CheckpointValidationError::InvalidCampaignCounters,
        ),
        (
            {
                let mut value = valid_checkpoint();
                value.summary.losses = 1;
                value
            },
            CheckpointValidationError::InvalidCampaignCounters,
        ),
        (
            {
                let mut value = valid_checkpoint();
                value.summary.recent_turn_ids = vec!["turn".to_string(); 65];
                value
            },
            CheckpointValidationError::TooManyRecentTurnIds {
                actual: 65,
                max: 64,
            },
        ),
        (
            {
                let mut value = valid_checkpoint();
                value.summary.strategy = Some(StrategyRecord {
                    next_attempt_priorities: Vec::new(),
                    ..strategy()
                });
                value
            },
            CheckpointValidationError::InvalidStrategy,
        ),
        (
            {
                let mut value = valid_checkpoint();
                value.unresolved_mutation.as_mut().expect("mutation").tool = "zoom".to_string();
                value
            },
            CheckpointValidationError::UnknownMutationTool {
                tool: "zoom".to_string(),
            },
        ),
        (
            {
                let mut value = valid_checkpoint();
                value
                    .unresolved_mutation
                    .as_mut()
                    .expect("mutation")
                    .action_sha256 = "A".repeat(64);
                value
            },
            CheckpointValidationError::InvalidActionHash,
        ),
        (
            {
                let mut value = valid_checkpoint();
                value
                    .latest_observation
                    .as_mut()
                    .expect("observation")
                    .confirms_action_sequence = Some(1);
                value
            },
            CheckpointValidationError::InvalidSequence {
                field: "latest_observation.confirms_action_sequence",
            },
        ),
    ];

    assert_eq!(
        cases
            .iter()
            .map(|(value, _)| value.validate())
            .collect::<Vec<_>>(),
        cases
            .iter()
            .map(|(_, error)| Err(error.clone()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn checkpoint_decode_rejects_unknown_fields_and_oversized_input() -> anyhow::Result<()> {
    let mut encoded = serde_json::to_value(valid_checkpoint())?;
    let Value::Object(root) = &mut encoded else {
        unreachable!("checkpoint encodes as an object")
    };
    root.insert("futureField".to_string(), Value::Bool(true));
    assert!(matches!(
        CampaignCheckpoint::decode(&serde_json::to_vec(&encoded)?),
        Err(CheckpointValidationError::Json { .. })
    ));

    let oversized = vec![b' '; MAX_CHECKPOINT_BYTES + 1];
    assert_eq!(
        CampaignCheckpoint::decode(&oversized),
        Err(CheckpointValidationError::CheckpointTooLarge {
            actual: MAX_CHECKPOINT_BYTES + 1,
            max: MAX_CHECKPOINT_BYTES,
        })
    );
    Ok(())
}
