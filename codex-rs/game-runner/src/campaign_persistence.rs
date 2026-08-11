use std::sync::Arc;

use crate::CampaignCheckpoint;
use crate::CampaignCheckpointStore;
use crate::CampaignSummary;
use crate::CheckpointStoreError;
use crate::DecisionAudit;
use crate::DurableCampaignState;
use crate::DurableMutation;
use crate::DurableMutationResult;
use crate::MutationResult;
use crate::ObservationEvidence;
use crate::PolicyAudit;
use crate::AuthorizedMutation;

pub struct CampaignPersistence {
    store: Arc<CampaignCheckpointStore>,
    state: tokio::sync::Mutex<PersistenceState>,
}

struct PersistenceState {
    checkpoint: Option<CampaignCheckpoint>,
    active_call_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("campaign persistence has no installed checkpoint")]
    MissingCheckpoint,
    #[error("campaign checkpoint write failed")]
    Store {
        #[source]
        source: CheckpointStoreError,
    },
    #[error("campaign checkpoint task failed")]
    Task {
        #[source]
        source: tokio::task::JoinError,
    },
    #[error("a live mutation call is already being persisted")]
    ActiveMutation,
    #[error("no live mutation call is available")]
    MissingActiveMutation,
    #[error("mutation result for call {actual} does not match active call {expected}")]
    MutationCallMismatch { expected: String, actual: String },
    #[error("campaign action sequence overflowed")]
    ActionSequenceOverflow,
}

pub struct MutationCheckpointUpdate {
    pub authorization: AuthorizedMutation,
    pub decision_audit: DecisionAudit,
    pub policy_audit: PolicyAudit,
}

impl CampaignPersistence {
    pub fn empty(store: Arc<CampaignCheckpointStore>) -> Self {
        Self {
            store,
            state: tokio::sync::Mutex::new(PersistenceState {
                checkpoint: None,
                active_call_id: None,
            }),
        }
    }

    pub async fn install(
        &self,
        checkpoint: CampaignCheckpoint,
    ) -> Result<(), PersistenceError> {
        let mut state = self.state.lock().await;
        self.write_checkpoint(&checkpoint).await?;
        state.checkpoint = Some(checkpoint);
        state.active_call_id = None;
        Ok(())
    }

    pub async fn snapshot(&self) -> Result<CampaignCheckpoint, PersistenceError> {
        self.state
            .lock()
            .await
            .checkpoint
            .clone()
            .ok_or(PersistenceError::MissingCheckpoint)
    }

    pub async fn persist_summary(
        &self,
        summary: CampaignSummary,
        decision_audit: DecisionAudit,
        policy_audit: PolicyAudit,
    ) -> Result<(), PersistenceError> {
        let mut state = self.state.lock().await;
        let mut candidate = state
            .checkpoint
            .clone()
            .ok_or(PersistenceError::MissingCheckpoint)?;
        candidate.summary = summary;
        candidate.decision_audit = decision_audit;
        candidate.policy_audit = policy_audit;
        self.write_checkpoint(&candidate).await?;
        state.checkpoint = Some(candidate);
        Ok(())
    }

    pub async fn begin_mutation(
        &self,
        update: &MutationCheckpointUpdate,
    ) -> Result<DurableMutation, PersistenceError> {
        let mut state = self.state.lock().await;
        if state.active_call_id.is_some() {
            return Err(PersistenceError::ActiveMutation);
        }
        let mut candidate = state
            .checkpoint
            .clone()
            .ok_or(PersistenceError::MissingCheckpoint)?;
        let action_sequence = candidate
            .summary
            .total_actions
            .checked_add(1)
            .ok_or(PersistenceError::ActionSequenceOverflow)?;
        let mutation = DurableMutation {
            action_sequence,
            operation_id: update.authorization.operation_id.clone(),
            action_sha256: update.authorization.action_sha256.clone(),
            tool: update.authorization.tool.clone(),
            result: DurableMutationResult::Pending,
        };
        candidate.summary.total_actions = action_sequence;
        candidate.decision_audit = update.decision_audit;
        candidate.policy_audit = update.policy_audit;
        candidate.unresolved_mutation = Some(mutation.clone());
        self.write_checkpoint(&candidate).await?;
        state.checkpoint = Some(candidate);
        state.active_call_id = Some(update.authorization.call_id.clone());
        Ok(mutation)
    }

    pub async fn finish_mutation(
        &self,
        call_id: &str,
        result: MutationResult,
    ) -> Result<(), PersistenceError> {
        let mut state = self.state.lock().await;
        let active_call_id = state
            .active_call_id
            .as_deref()
            .ok_or(PersistenceError::MissingActiveMutation)?;
        if active_call_id != call_id {
            return Err(PersistenceError::MutationCallMismatch {
                expected: active_call_id.to_string(),
                actual: call_id.to_string(),
            });
        }
        let mut candidate = state
            .checkpoint
            .clone()
            .ok_or(PersistenceError::MissingCheckpoint)?;
        let mutation = candidate
            .unresolved_mutation
            .as_mut()
            .ok_or(PersistenceError::MissingActiveMutation)?;
        mutation.result = match result {
            MutationResult::Success => DurableMutationResult::Success,
            MutationResult::CleanFailure => DurableMutationResult::CleanFailure,
            MutationResult::Indeterminate => DurableMutationResult::Indeterminate,
        };
        self.write_checkpoint(&candidate).await?;
        state.checkpoint = Some(candidate);
        state.active_call_id = None;
        Ok(())
    }

    pub async fn confirm_observation(
        &self,
        observation: &ObservationEvidence,
    ) -> Result<(), PersistenceError> {
        let mut state = self.state.lock().await;
        let mut candidate = state
            .checkpoint
            .clone()
            .ok_or(PersistenceError::MissingCheckpoint)?;
        let confirms_action_sequence = candidate
            .unresolved_mutation
            .as_ref()
            .map(|mutation| mutation.action_sequence);
        candidate.latest_observation = Some(crate::DurableObservation {
            observation_sequence: observation.generation,
            confirms_action_sequence,
            reference: observation.reference.clone(),
        });
        candidate.unresolved_mutation = None;
        self.write_checkpoint(&candidate).await?;
        state.checkpoint = Some(candidate);
        Ok(())
    }

    pub async fn set_state(
        &self,
        durable_state: DurableCampaignState,
        owner_generation: u64,
    ) -> Result<(), PersistenceError> {
        let mut state = self.state.lock().await;
        let mut candidate = state
            .checkpoint
            .clone()
            .ok_or(PersistenceError::MissingCheckpoint)?;
        candidate.state = durable_state;
        candidate.owner_generation = owner_generation;
        self.write_checkpoint(&candidate).await?;
        state.checkpoint = Some(candidate);
        Ok(())
    }

    pub async fn remove(&self) -> Result<(), PersistenceError> {
        let mut state = self.state.lock().await;
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.remove())
            .await
            .map_err(|source| PersistenceError::Task { source })?
            .map_err(|source| PersistenceError::Store { source })?;
        state.checkpoint = None;
        state.active_call_id = None;
        Ok(())
    }

    async fn write_checkpoint(
        &self,
        checkpoint: &CampaignCheckpoint,
    ) -> Result<(), PersistenceError> {
        let store = Arc::clone(&self.store);
        let checkpoint = checkpoint.clone();
        tokio::task::spawn_blocking(move || store.replace(&checkpoint))
            .await
            .map_err(|source| PersistenceError::Task { source })?
            .map_err(|source| PersistenceError::Store { source })
    }
}

#[cfg(test)]
#[path = "campaign_persistence_tests.rs"]
pub(crate) mod tests;
