use std::sync::Mutex;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::action_batch::ActionBatch;
use crate::action_batch::ActionBatchError;
use crate::outcome::OutcomeDraft;
use crate::outcome::OutcomeValidationError;
use crate::outcome::ReportedOutcome;
use crate::planned_action::PlannedAction;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DecisionError {
    #[error("{coordinate} coordinate {value} is outside 0..={upper_bound}")]
    CoordinateOutOfBounds {
        coordinate: String,
        value: i64,
        upper_bound: i64,
    },
    #[error("click count must be between 1 and 3")]
    InvalidClickCount,
    #[error("failed to encode the planned action")]
    ActionEncoding,
    #[error("a fresh full-frame observation is required")]
    MissingObservation,
    #[error("the plan references {actual}, but the newest observation is {expected}")]
    StaleObservation { expected: String, actual: String },
    #[error("a plan must contain between two and four candidates")]
    InvalidCandidateCount,
    #[error("{field} exceeds the 2 KiB limit")]
    StringTooLarge { field: String },
    #[error("the plan exceeds the 12 KiB limit")]
    PlanTooLarge,
    #[error("no accepted plan authorizes this mutation")]
    MissingPlan,
    #[error("the mutation does not exactly match the accepted plan")]
    ActionMismatch,
    #[error("the eight-action turn batch is exhausted; verify the latest action and finish this turn")]
    ActionBatchExhausted,
    #[error("the action batch is closed by a reported campaign outcome")]
    ActionBatchClosed,
    #[error("no authorized mutation matches call {call_id}")]
    MissingMutation { call_id: String },
    #[error("an outcome cannot be reported before a mutation")]
    OutcomeBeforeMutation,
    #[error("a fresh post-mutation observation is required")]
    MissingPostMutationObservation,
    #[error(transparent)]
    InvalidOutcome(#[from] OutcomeValidationError),
    #[error("counter {counter} overflowed")]
    CounterOverflow { counter: String },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ObservationEvidence {
    pub generation: u64,
    pub call_id: String,
    pub reference: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlanCandidate {
    pub action: String,
    pub predicted_visible_consequence: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlanDraft {
    pub observation_reference: String,
    pub objective: String,
    pub visible_state_summary: String,
    pub candidates: Vec<PlanCandidate>,
    pub chosen_action: PlannedAction,
    pub reason: String,
    pub expected_visible_result: String,
    pub invalidation_condition: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AcceptedPlan {
    pub id: String,
    pub observation: ObservationEvidence,
    pub draft: PlanDraft,
    pub action_sha256: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthorizedMutation {
    pub call_id: String,
    pub operation_id: String,
    pub action_sha256: String,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationResult {
    Success,
    CleanFailure,
    Indeterminate,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MutationEvidence {
    pub plan: AcceptedPlan,
    pub authorization: AuthorizedMutation,
    pub result: Option<MutationResult>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct DecisionAudit {
    pub plans_accepted: u64,
    pub plan_rejections: u64,
    pub mutation_attempts: u64,
    pub mutation_authorizations: u64,
    pub mutation_denials: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DecisionSnapshot {
    pub owner_generation: u64,
    pub next_observation_generation: u64,
    pub observation: Option<ObservationEvidence>,
    pub plan: Option<AcceptedPlan>,
    pub mutation: Option<MutationEvidence>,
    pub outcome: Option<ReportedOutcome>,
    pub requires_post_mutation_observation: bool,
    pub batch_actions: u8,
    pub audit: DecisionAudit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationReason {
    CaptureStarted,
    PositiveWait,
    TurnAborted,
    OwnerGenerationReplaced { owner_generation: u64 },
}

pub struct DecisionGate {
    state: Mutex<DecisionState>,
}

struct DecisionState {
    snapshot: DecisionSnapshot,
    plan_sequence: u64,
    batch: ActionBatch,
}

impl DecisionGate {
    pub fn new(owner_generation: u64) -> Self {
        Self {
            state: Mutex::new(DecisionState {
                snapshot: DecisionSnapshot {
                    owner_generation,
                    next_observation_generation: 1,
                    observation: None,
                    plan: None,
                    mutation: None,
                    outcome: None,
                    requires_post_mutation_observation: false,
                    batch_actions: 0,
                    audit: DecisionAudit::default(),
                },
                plan_sequence: 0,
                batch: ActionBatch::new(),
            }),
        }
    }

    pub fn begin_turn(&self) {
        let mut state = self.lock();
        state.batch.reset();
        state.snapshot.observation = None;
        state.snapshot.plan = None;
        state.snapshot.mutation = None;
        state.snapshot.outcome = None;
        state.snapshot.requires_post_mutation_observation = false;
        state.snapshot.batch_actions = state.batch.used();
    }

    pub fn begin_full_observation(&self) {
        self.invalidate(InvalidationReason::CaptureStarted);
    }

    pub fn complete_full_observation(
        &self,
        call_id: String,
        reference: String,
        width: u32,
        height: u32,
    ) -> Result<ObservationEvidence, DecisionError> {
        if width == 0 || height == 0 {
            return Err(DecisionError::MissingObservation);
        }
        let mut state = self.lock();
        let evidence = ObservationEvidence {
            generation: state.snapshot.next_observation_generation,
            call_id,
            reference,
            width,
            height,
        };
        state.snapshot.next_observation_generation = checked_increment(
            state.snapshot.next_observation_generation,
            "next_observation_generation",
        )?;
        state.snapshot.observation = Some(evidence.clone());
        state.snapshot.plan = None;
        state.snapshot.requires_post_mutation_observation = false;
        Ok(evidence)
    }

    pub fn before_wait(&self, arguments: Option<&Value>) {
        if arguments.is_none_or(has_positive_number) {
            self.invalidate(InvalidationReason::PositiveWait);
        }
    }

    pub fn record_plan(&self, draft: PlanDraft) -> Result<AcceptedPlan, DecisionError> {
        let mut state = self.lock();
        let result = (|| {
            validate_plan_strings(&draft)?;
            if !(2..=4).contains(&draft.candidates.len()) {
                return Err(DecisionError::InvalidCandidateCount);
            }
            if serde_json::to_vec(&draft)
                .map_err(|_| DecisionError::ActionEncoding)?
                .len()
                > 12 * 1024
            {
                return Err(DecisionError::PlanTooLarge);
            }
            let observation = state
                .snapshot
                .observation
                .clone()
                .ok_or(DecisionError::MissingObservation)?;
            if draft.observation_reference != observation.reference {
                return Err(DecisionError::StaleObservation {
                    expected: observation.reference,
                    actual: draft.observation_reference,
                });
            }
            draft
                .chosen_action
                .validate(observation.width, observation.height)?;
            state.plan_sequence = checked_increment(state.plan_sequence, "plan_sequence")?;
            Ok(AcceptedPlan {
                id: format!("plan-{}-{}", observation.generation, state.plan_sequence),
                action_sha256: draft.chosen_action.action_sha256()?,
                observation,
                draft,
            })
        })();
        match result {
            Ok(plan) => {
                state.snapshot.audit.plans_accepted =
                    checked_increment(state.snapshot.audit.plans_accepted, "plans_accepted")?;
                state.snapshot.plan = Some(plan.clone());
                Ok(plan)
            }
            Err(error) => {
                match checked_increment(state.snapshot.audit.plan_rejections, "plan_rejections") {
                    Ok(value) => {
                        state.snapshot.audit.plan_rejections = value;
                        Err(error)
                    }
                    Err(overflow) => Err(overflow),
                }
            }
        }
    }

    pub fn prepare_mutation(
        &self,
        tool: &str,
        arguments: &Value,
        call_id: &str,
    ) -> Result<AuthorizedMutation, DecisionError> {
        let mut state = self.lock();
        state.snapshot.audit.mutation_attempts =
            checked_increment(state.snapshot.audit.mutation_attempts, "mutation_attempts")?;
        let Some(plan) = state.snapshot.plan.take() else {
            deny_mutation(&mut state)?;
            return Err(DecisionError::MissingPlan);
        };
        if plan.draft.chosen_action.tool_name() != tool
            || plan.draft.chosen_action.arguments() != *arguments
        {
            deny_mutation(&mut state)?;
            return Err(DecisionError::ActionMismatch);
        }
        let next_authorizations = checked_increment(
            state.snapshot.audit.mutation_authorizations,
            "mutation_authorizations",
        )?;
        match state.batch.authorize() {
            Ok(()) => {}
            Err(ActionBatchError::Exhausted) => {
                state.snapshot.audit.mutation_denials = checked_increment(
                    state.snapshot.audit.mutation_denials,
                    "mutation_denials",
                )?;
                return Err(DecisionError::ActionBatchExhausted);
            }
            Err(ActionBatchError::Closed) => {
                state.snapshot.audit.mutation_denials = checked_increment(
                    state.snapshot.audit.mutation_denials,
                    "mutation_denials",
                )?;
                return Err(DecisionError::ActionBatchClosed);
            }
        }
        let authorization = AuthorizedMutation {
            call_id: call_id.to_string(),
            operation_id: call_id.to_string(),
            action_sha256: plan.action_sha256.clone(),
            tool: tool.to_string(),
            arguments: arguments.clone(),
        };
        state.snapshot.observation = None;
        state.snapshot.requires_post_mutation_observation = true;
        state.snapshot.audit.mutation_authorizations = next_authorizations;
        state.snapshot.batch_actions = state.batch.used();
        state.snapshot.mutation = Some(MutationEvidence {
            plan,
            authorization: authorization.clone(),
            result: None,
        });
        Ok(authorization)
    }

    pub fn record_mutation_result(
        &self,
        call_id: &str,
        result: MutationResult,
    ) -> Result<(), DecisionError> {
        let mut state = self.lock();
        let mutation = state
            .snapshot
            .mutation
            .as_mut()
            .filter(|mutation| mutation.authorization.call_id == call_id)
            .ok_or_else(|| DecisionError::MissingMutation {
                call_id: call_id.to_string(),
            })?;
        mutation.result = Some(result);
        Ok(())
    }

    pub fn report_outcome(&self, draft: OutcomeDraft) -> Result<ReportedOutcome, DecisionError> {
        let mut state = self.lock();
        draft.validate()?;
        if state.batch.is_closed() {
            return Err(DecisionError::ActionBatchClosed);
        }
        if state.snapshot.mutation.is_none() {
            return Err(DecisionError::OutcomeBeforeMutation);
        }
        if state.snapshot.requires_post_mutation_observation {
            return Err(DecisionError::MissingPostMutationObservation);
        }
        let observation = state
            .snapshot
            .observation
            .clone()
            .ok_or(DecisionError::MissingPostMutationObservation)?;
        if draft.observation_reference() != observation.reference {
            return Err(DecisionError::StaleObservation {
                expected: observation.reference,
                actual: draft.observation_reference().to_string(),
            });
        }
        let outcome = ReportedOutcome { observation, draft };
        state.batch.close();
        state.snapshot.outcome = Some(outcome.clone());
        Ok(outcome)
    }

    pub fn invalidate(&self, reason: InvalidationReason) {
        let mut state = self.lock();
        state.snapshot.observation = None;
        state.snapshot.plan = None;
        if let InvalidationReason::OwnerGenerationReplaced { owner_generation } = reason {
            state.snapshot.owner_generation = owner_generation;
        }
    }

    pub fn snapshot(&self) -> DecisionSnapshot {
        self.lock().snapshot.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DecisionState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn deny_mutation(state: &mut DecisionState) -> Result<(), DecisionError> {
    state.snapshot.observation = None;
    state.snapshot.requires_post_mutation_observation = true;
    state.snapshot.audit.mutation_denials =
        checked_increment(state.snapshot.audit.mutation_denials, "mutation_denials")?;
    Ok(())
}

fn checked_increment(value: u64, counter: &str) -> Result<u64, DecisionError> {
    value
        .checked_add(1)
        .ok_or_else(|| DecisionError::CounterOverflow {
            counter: counter.to_string(),
        })
}

fn has_positive_number(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.as_f64().is_some_and(|value| value > 0.0),
        Value::Array(values) => values.iter().any(has_positive_number),
        Value::Object(object) => object.values().any(has_positive_number),
        Value::Null | Value::Bool(_) | Value::String(_) => false,
    }
}

fn validate_plan_strings(draft: &PlanDraft) -> Result<(), DecisionError> {
    for (field, value) in [
        (
            "observation_reference",
            draft.observation_reference.as_str(),
        ),
        ("objective", draft.objective.as_str()),
        (
            "visible_state_summary",
            draft.visible_state_summary.as_str(),
        ),
        ("reason", draft.reason.as_str()),
        (
            "expected_visible_result",
            draft.expected_visible_result.as_str(),
        ),
        (
            "invalidation_condition",
            draft.invalidation_condition.as_str(),
        ),
    ] {
        validate_string(field, value)?;
    }
    for candidate in &draft.candidates {
        validate_string("candidate.action", &candidate.action)?;
        validate_string(
            "candidate.predicted_visible_consequence",
            &candidate.predicted_visible_consequence,
        )?;
    }
    Ok(())
}

fn validate_string(field: &str, value: &str) -> Result<(), DecisionError> {
    if value.len() > 2 * 1024 {
        return Err(DecisionError::StringTooLarge {
            field: field.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "decision_tests.rs"]
mod tests;
