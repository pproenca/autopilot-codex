use std::path::Path;
use std::path::PathBuf;

use codex_core_api::ThreadId;
use serde::Deserialize;
use serde::Serialize;

use crate::CampaignSummary;
use crate::DecisionAudit;
use crate::PolicyAudit;

pub const CHECKPOINT_VERSION: u32 = 1;
pub const MAX_CHECKPOINT_BYTES: usize = 256 * 1024;
const MAX_CONTROL_STRING_BYTES: usize = 2 * 1024;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_RECENT_TURNS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CampaignCheckpoint {
    pub schema_version: u32,
    pub epoch: String,
    pub thread_id: String,
    pub rollout_path: PathBuf,
    pub deployment: CheckpointDeployment,
    pub state: DurableCampaignState,
    pub summary: CampaignSummary,
    pub owner_generation: u64,
    pub decision_audit: DecisionAudit,
    pub policy_audit: PolicyAudit,
    pub latest_observation: Option<DurableObservation>,
    pub unresolved_mutation: Option<DurableMutation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointDeployment {
    pub helper_app: PathBuf,
    pub socket_path: PathBuf,
    pub target_app: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
pub enum DurableCampaignState {
    Running,
    Paused { reason: PauseReason },
    Won { evidence_reference: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "reason", rename_all = "camelCase", deny_unknown_fields)]
pub enum PauseReason {
    UnexpectedExit,
    Operator,
    HelperUnavailable { summary: String },
    DurabilityFailure { summary: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DurableMutation {
    pub action_sequence: u64,
    pub operation_id: String,
    pub action_sha256: String,
    pub tool: String,
    pub result: DurableMutationResult,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DurableMutationResult {
    Pending,
    Success,
    CleanFailure,
    Indeterminate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DurableObservation {
    pub observation_sequence: u64,
    pub confirms_action_sequence: Option<u64>,
    pub reference: String,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum CheckpointValidationError {
    #[error("unsupported campaign checkpoint version {actual}")]
    UnsupportedVersion { actual: u32 },
    #[error("{field} must not be empty")]
    EmptyString { field: &'static str },
    #[error("{field} exceeds the {max_bytes}-byte limit")]
    StringTooLarge {
        field: &'static str,
        max_bytes: usize,
    },
    #[error("campaign epoch is not a UUID")]
    InvalidEpoch,
    #[error("campaign thread id is not a UUID")]
    InvalidThreadId,
    #[error("{field} must be an absolute path")]
    PathNotAbsolute { field: &'static str },
    #[error("{field} exceeds the {max_bytes}-byte path limit")]
    PathTooLarge {
        field: &'static str,
        max_bytes: usize,
    },
    #[error("campaign counters or audits are inconsistent")]
    InvalidCampaignCounters,
    #[error("checkpoint has {actual} recent turn ids; maximum is {max}")]
    TooManyRecentTurnIds { actual: usize, max: usize },
    #[error("checkpoint strategy is invalid")]
    InvalidStrategy,
    #[error("unsupported mutation tool {tool}")]
    UnknownMutationTool { tool: String },
    #[error("mutation action hash must be 64 lowercase hexadecimal characters")]
    InvalidActionHash,
    #[error("checkpoint sequence {field} is inconsistent")]
    InvalidSequence { field: &'static str },
    #[error("checkpoint lifecycle state is inconsistent")]
    InvalidLifecycleState,
    #[error("checkpoint is {actual} bytes; maximum is {max}")]
    CheckpointTooLarge { actual: usize, max: usize },
    #[error("checkpoint JSON is invalid: {message}")]
    Json { message: String },
}

impl CampaignCheckpoint {
    pub fn validate(&self) -> Result<(), CheckpointValidationError> {
        if self.schema_version != CHECKPOINT_VERSION {
            return Err(CheckpointValidationError::UnsupportedVersion {
                actual: self.schema_version,
            });
        }
        validate_string("epoch", &self.epoch)?;
        if uuid::Uuid::parse_str(&self.epoch).is_err() {
            return Err(CheckpointValidationError::InvalidEpoch);
        }
        validate_string("thread_id", &self.thread_id)?;
        if ThreadId::from_string(&self.thread_id).is_err() {
            return Err(CheckpointValidationError::InvalidThreadId);
        }
        validate_path("rollout_path", &self.rollout_path)?;
        validate_path("helper_app", &self.deployment.helper_app)?;
        validate_path("socket_path", &self.deployment.socket_path)?;
        validate_string("target_app", &self.deployment.target_app)?;
        validate_state_strings(&self.state)?;
        self.validate_summary()?;
        self.validate_observation()?;
        self.validate_mutation()?;
        self.validate_lifecycle()?;
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, CheckpointValidationError> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(json_error)?;
        validate_encoded_size(encoded.len())?;
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, CheckpointValidationError> {
        validate_encoded_size(encoded.len())?;
        let checkpoint = serde_json::from_slice::<Self>(encoded).map_err(json_error)?;
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    fn validate_summary(&self) -> Result<(), CheckpointValidationError> {
        let summary = &self.summary;
        let expected_attempt = summary.losses.checked_add(1);
        let audit_total = self
            .decision_audit
            .mutation_authorizations
            .checked_add(self.decision_audit.mutation_denials);
        if summary.attempt_number == 0
            || expected_attempt != Some(summary.attempt_number)
            || summary.losses > summary.total_turns
            || summary.total_actions != self.decision_audit.mutation_authorizations
            || summary.total_actions != self.policy_audit.mutation_authorizations
            || self.decision_audit.mutation_attempts != self.policy_audit.mutation_attempts
            || audit_total != Some(self.decision_audit.mutation_attempts)
            || self.decision_audit.plans_accepted < summary.total_actions
            || self.owner_generation == 0
        {
            return Err(CheckpointValidationError::InvalidCampaignCounters);
        }
        if summary.recent_turn_ids.len() > MAX_RECENT_TURNS {
            return Err(CheckpointValidationError::TooManyRecentTurnIds {
                actual: summary.recent_turn_ids.len(),
                max: MAX_RECENT_TURNS,
            });
        }
        if summary.recent_turn_ids.len() as u64 > summary.total_turns {
            return Err(CheckpointValidationError::InvalidCampaignCounters);
        }
        for turn_id in &summary.recent_turn_ids {
            validate_bounded_string("recent_turn_ids", turn_id)?;
        }
        if summary
            .strategy
            .as_ref()
            .is_some_and(|strategy| strategy.validate().is_err())
        {
            return Err(CheckpointValidationError::InvalidStrategy);
        }
        Ok(())
    }

    fn validate_observation(&self) -> Result<(), CheckpointValidationError> {
        let Some(observation) = &self.latest_observation else {
            if self.summary.total_actions > 0 {
                return Err(CheckpointValidationError::InvalidSequence {
                    field: "latest_observation",
                });
            }
            return Ok(());
        };
        if observation.observation_sequence == 0 {
            return Err(CheckpointValidationError::InvalidSequence {
                field: "latest_observation.observation_sequence",
            });
        }
        validate_string("latest_observation.reference", &observation.reference)?;
        if observation
            .confirms_action_sequence
            .is_some_and(|sequence| sequence == 0 || sequence > self.summary.total_actions)
            || observation.confirms_action_sequence.is_some_and(|sequence| {
                self.unresolved_mutation
                    .as_ref()
                    .is_some_and(|mutation| sequence >= mutation.action_sequence)
            })
        {
            return Err(CheckpointValidationError::InvalidSequence {
                field: "latest_observation.confirms_action_sequence",
            });
        }
        Ok(())
    }

    fn validate_mutation(&self) -> Result<(), CheckpointValidationError> {
        let Some(mutation) = &self.unresolved_mutation else {
            if self.summary.total_actions > 0
                && self
                    .latest_observation
                    .as_ref()
                    .and_then(|observation| observation.confirms_action_sequence)
                    != Some(self.summary.total_actions)
            {
                return Err(CheckpointValidationError::InvalidSequence {
                    field: "latest_observation.confirms_action_sequence",
                });
            }
            return Ok(());
        };
        if mutation.action_sequence == 0
            || mutation.action_sequence != self.summary.total_actions
            || self
                .latest_observation
                .as_ref()
                .and_then(|observation| observation.confirms_action_sequence)
                .is_some_and(|sequence| sequence >= mutation.action_sequence)
        {
            return Err(CheckpointValidationError::InvalidSequence {
                field: "unresolved_mutation.action_sequence",
            });
        }
        validate_string("unresolved_mutation.operation_id", &mutation.operation_id)?;
        if !matches!(mutation.tool.as_str(), "click" | "drag" | "focus_click") {
            return Err(CheckpointValidationError::UnknownMutationTool {
                tool: mutation.tool.clone(),
            });
        }
        if mutation.action_sha256.len() != 64
            || !mutation
                .action_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CheckpointValidationError::InvalidActionHash);
        }
        Ok(())
    }

    fn validate_lifecycle(&self) -> Result<(), CheckpointValidationError> {
        match &self.state {
            DurableCampaignState::Running => Ok(()),
            DurableCampaignState::Paused { .. } => {
                if self
                    .unresolved_mutation
                    .as_ref()
                    .is_some_and(|mutation| mutation.result == DurableMutationResult::Pending)
                {
                    Err(CheckpointValidationError::InvalidLifecycleState)
                } else {
                    Ok(())
                }
            }
            DurableCampaignState::Won { evidence_reference } => {
                if self.unresolved_mutation.is_some()
                    || self
                        .latest_observation
                        .as_ref()
                        .map(|observation| &observation.reference)
                        != Some(evidence_reference)
                {
                    Err(CheckpointValidationError::InvalidLifecycleState)
                } else {
                    Ok(())
                }
            }
        }
    }
}

fn validate_state_strings(
    state: &DurableCampaignState,
) -> Result<(), CheckpointValidationError> {
    match state {
        DurableCampaignState::Running
        | DurableCampaignState::Paused {
            reason: PauseReason::UnexpectedExit | PauseReason::Operator,
        } => Ok(()),
        DurableCampaignState::Paused {
            reason:
                PauseReason::HelperUnavailable { summary }
                | PauseReason::DurabilityFailure { summary },
        } => validate_string("pause_reason.summary", summary),
        DurableCampaignState::Won { evidence_reference } => {
            validate_string("state.evidence_reference", evidence_reference)
        }
    }
}

fn validate_string(
    field: &'static str,
    value: &str,
) -> Result<(), CheckpointValidationError> {
    if value.is_empty() {
        return Err(CheckpointValidationError::EmptyString { field });
    }
    validate_bounded_string(field, value)
}

fn validate_bounded_string(
    field: &'static str,
    value: &str,
) -> Result<(), CheckpointValidationError> {
    if value.len() > MAX_CONTROL_STRING_BYTES {
        return Err(CheckpointValidationError::StringTooLarge {
            field,
            max_bytes: MAX_CONTROL_STRING_BYTES,
        });
    }
    Ok(())
}

fn validate_path(
    field: &'static str,
    path: &Path,
) -> Result<(), CheckpointValidationError> {
    if !path.is_absolute() {
        return Err(CheckpointValidationError::PathNotAbsolute { field });
    }
    if path.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
        return Err(CheckpointValidationError::PathTooLarge {
            field,
            max_bytes: MAX_PATH_BYTES,
        });
    }
    Ok(())
}

fn validate_encoded_size(actual: usize) -> Result<(), CheckpointValidationError> {
    if actual > MAX_CHECKPOINT_BYTES {
        return Err(CheckpointValidationError::CheckpointTooLarge {
            actual,
            max: MAX_CHECKPOINT_BYTES,
        });
    }
    Ok(())
}

fn json_error(error: serde_json::Error) -> CheckpointValidationError {
    CheckpointValidationError::Json {
        message: error.to_string(),
    }
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
