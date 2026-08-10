use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClickArguments {
    pub x: i64,
    pub y: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<MouseButton>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DragArguments {
    pub from_x: i64,
    pub from_y: i64,
    pub to_x: i64,
    pub to_y: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FocusClickArguments {
    pub x: i64,
    pub y: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "tool", content = "arguments", rename_all = "snake_case")]
pub enum PlannedAction {
    Click(ClickArguments),
    Drag(DragArguments),
    FocusClick(FocusClickArguments),
}

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
    #[error("the Stage 4A mutation budget is exhausted")]
    MutationBudgetExhausted,
    #[error("no authorized mutation matches call {call_id}")]
    MissingMutation { call_id: String },
    #[error("an outcome cannot be reported before a mutation")]
    OutcomeBeforeMutation,
    #[error("a fresh post-mutation observation is required")]
    MissingPostMutationObservation,
    #[error("the outcome exceeds the 8 KiB limit")]
    OutcomeTooLarge,
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

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Loss,
    Win,
    TerminalBlock,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutcomeDraft {
    pub outcome: OutcomeKind,
    pub observation_reference: String,
    pub visible_evidence_summary: String,
    pub lesson: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportedOutcome {
    pub observation: ObservationEvidence,
    pub draft: OutcomeDraft,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct DecisionAudit {
    pub plans_accepted: usize,
    pub plan_rejections: usize,
    pub mutation_attempts: usize,
    pub mutation_authorizations: usize,
    pub mutation_denials: usize,
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
    mutation_budget_consumed: bool,
}

impl PlannedAction {
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::Click(_) => "click",
            Self::Drag(_) => "drag",
            Self::FocusClick(_) => "focus_click",
        }
    }

    pub fn arguments(&self) -> Value {
        match self {
            Self::Click(arguments) => {
                let mut value = json!({"x": arguments.x, "y": arguments.y});
                let object = value
                    .as_object_mut()
                    .expect("click arguments fixture must be an object");
                if let Some(button) = arguments.button {
                    object.insert(
                        "button".to_string(),
                        Value::String(
                            match button {
                                MouseButton::Left => "left",
                                MouseButton::Right => "right",
                            }
                            .to_string(),
                        ),
                    );
                }
                if let Some(count) = arguments.count {
                    object.insert("count".to_string(), Value::from(count));
                }
                value
            }
            Self::Drag(arguments) => json!({
                "from_x": arguments.from_x,
                "from_y": arguments.from_y,
                "to_x": arguments.to_x,
                "to_y": arguments.to_y,
            }),
            Self::FocusClick(arguments) => json!({
                "x": arguments.x,
                "y": arguments.y,
            }),
        }
    }

    pub fn validate(&self, width: u32, height: u32) -> Result<(), DecisionError> {
        match self {
            Self::Click(arguments) => {
                validate_coordinate("x", arguments.x, width)?;
                validate_coordinate("y", arguments.y, height)?;
                if !(1..=3).contains(&arguments.count.unwrap_or(1)) {
                    return Err(DecisionError::InvalidClickCount);
                }
            }
            Self::Drag(arguments) => {
                validate_coordinate("from_x", arguments.from_x, width)?;
                validate_coordinate("from_y", arguments.from_y, height)?;
                validate_coordinate("to_x", arguments.to_x, width)?;
                validate_coordinate("to_y", arguments.to_y, height)?;
            }
            Self::FocusClick(arguments) => {
                validate_coordinate("x", arguments.x, width)?;
                validate_coordinate("y", arguments.y, height)?;
            }
        }
        Ok(())
    }

    pub fn action_sha256(&self) -> Result<String, DecisionError> {
        let envelope = recursively_sort(json!({
            "arguments": self.arguments(),
            "tool": self.tool_name(),
        }));
        let bytes = serde_json::to_vec(&envelope).map_err(|_| DecisionError::ActionEncoding)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
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
                    audit: DecisionAudit::default(),
                },
                plan_sequence: 0,
                mutation_budget_consumed: false,
            }),
        }
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
        state.snapshot.next_observation_generation += 1;
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
            draft.chosen_action.validate(observation.width, observation.height)?;
            state.plan_sequence += 1;
            Ok(AcceptedPlan {
                id: format!("plan-{}-{}", observation.generation, state.plan_sequence),
                action_sha256: draft.chosen_action.action_sha256()?,
                observation,
                draft,
            })
        })();
        match result {
            Ok(plan) => {
                state.snapshot.audit.plans_accepted += 1;
                state.snapshot.plan = Some(plan.clone());
                Ok(plan)
            }
            Err(error) => {
                state.snapshot.audit.plan_rejections += 1;
                Err(error)
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
        state.snapshot.audit.mutation_attempts += 1;
        let plan = state.snapshot.plan.take();
        state.snapshot.observation = None;
        state.snapshot.requires_post_mutation_observation = true;
        let result = (|| {
            let plan = plan.ok_or(DecisionError::MissingPlan)?;
            if state.mutation_budget_consumed {
                return Err(DecisionError::MutationBudgetExhausted);
            }
            if plan.draft.chosen_action.tool_name() != tool
                || plan.draft.chosen_action.arguments() != *arguments
            {
                return Err(DecisionError::ActionMismatch);
            }
            let authorization = AuthorizedMutation {
                call_id: call_id.to_string(),
                operation_id: call_id.to_string(),
                action_sha256: plan.action_sha256.clone(),
                tool: tool.to_string(),
                arguments: arguments.clone(),
            };
            state.mutation_budget_consumed = true;
            state.snapshot.mutation = Some(MutationEvidence {
                plan,
                authorization: authorization.clone(),
                result: None,
            });
            Ok(authorization)
        })();
        if result.is_ok() {
            state.snapshot.audit.mutation_authorizations += 1;
        } else {
            state.snapshot.audit.mutation_denials += 1;
        }
        result
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
        validate_outcome_strings(&draft)?;
        if serde_json::to_vec(&draft)
            .map_err(|_| DecisionError::ActionEncoding)?
            .len()
            > 8 * 1024
        {
            return Err(DecisionError::OutcomeTooLarge);
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
        if draft.observation_reference != observation.reference {
            return Err(DecisionError::StaleObservation {
                expected: observation.reference,
                actual: draft.observation_reference,
            });
        }
        let outcome = ReportedOutcome { observation, draft };
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

fn validate_coordinate(
    coordinate: &str,
    value: i64,
    dimension: u32,
) -> Result<(), DecisionError> {
    let upper_bound = i64::from(dimension) - 1;
    if value < 0 || value > upper_bound {
        return Err(DecisionError::CoordinateOutOfBounds {
            coordinate: coordinate.to_string(),
            value,
            upper_bound,
        });
    }
    Ok(())
}

fn recursively_sort(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries = object.into_iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, recursively_sort(value)))
                    .collect::<Map<_, _>>(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(recursively_sort).collect()),
        scalar => scalar,
    }
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
        ("observation_reference", draft.observation_reference.as_str()),
        ("objective", draft.objective.as_str()),
        ("visible_state_summary", draft.visible_state_summary.as_str()),
        ("reason", draft.reason.as_str()),
        ("expected_visible_result", draft.expected_visible_result.as_str()),
        ("invalidation_condition", draft.invalidation_condition.as_str()),
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

fn validate_outcome_strings(draft: &OutcomeDraft) -> Result<(), DecisionError> {
    for (field, value) in [
        ("observation_reference", draft.observation_reference.as_str()),
        ("visible_evidence_summary", draft.visible_evidence_summary.as_str()),
        ("lesson", draft.lesson.as_str()),
    ] {
        validate_string(field, value)?;
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
use std::sync::Mutex;
