use std::path::PathBuf;
use std::time::Duration;

use codex_core_api::CallToolResult;
use codex_core_api::CodexThread;
use codex_core_api::EventMsg;
use codex_core_api::Op;
use codex_core_api::SessionConfiguredEvent;
use codex_core_api::TurnCompleteEvent;
use codex_core_api::UserInput;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

use crate::GAME_SERVER_NAME;
use crate::GameCallPolicy;
use crate::RunnerError;

const MAX_REPORT_BYTES: usize = 12 * 1024;
const MAX_FIELD_BYTES: usize = 2 * 1024;
const MAX_LIST_ITEMS: usize = 32;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelObservation {
    pub visible_state_summary: String,
    pub game_phase: String,
    pub visible_objects: Vec<String>,
    pub resources_and_choices: Vec<String>,
    pub uncertainties: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ObservationReport {
    pub thread_id: String,
    pub turn_id: String,
    pub observation_call_id: String,
    pub observation_reference: Option<String>,
    pub rollout_path: PathBuf,
    pub epoch: String,
    pub generation: u64,
    pub mutation_attempts: u64,
    pub mutation_dispatches: u64,
    pub model: ModelObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationLimits {
    pub turn_timeout: Duration,
}

pub struct ObservationRun {
    limits: ObservationLimits,
}

impl ObservationRun {
    pub fn new(limits: ObservationLimits) -> Self {
        Self { limits }
    }

    pub async fn execute(
        &self,
        thread: &CodexThread,
        session: &SessionConfiguredEvent,
        policy: &GameCallPolicy,
        target_app: &str,
    ) -> Result<ObservationReport, RunnerError> {
        thread
            .submit(Op::UserInput {
                items: vec![UserInput::Text {
                    text: format!(
                        "Observe the currently visible {target_app} game without changing it. Call `mcp__game__get_app_state` at least once. You may call only `mcp__game__wait` or `mcp__game__zoom` if another read-only view is needed. Never click, drag, focus-click, or invoke any other tool. Report only visible evidence using the required JSON schema; put anything uncertain in `uncertainties`."
                    ),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: Some(observation_schema()),
                responsesapi_client_metadata: None,
                additional_context: Default::default(),
                thread_settings: Default::default(),
            })
            .await
            .map_err(|error| RunnerError::TurnFailed {
                message: error.to_string(),
            })?;

        tokio::time::timeout(self.limits.turn_timeout, async {
            let mut accumulator = ObservationAccumulator::default();
            loop {
                let event = thread
                    .next_event()
                    .await
                    .map_err(|error| RunnerError::TurnFailed {
                        message: error.to_string(),
                    })?;
                accumulator.observe(&event.msg);
                match event.msg {
                    EventMsg::TurnComplete(event) => {
                        return accumulator.finish(
                            &session.thread_id.to_string(),
                            session.rollout_path.clone(),
                            policy,
                            &event,
                        );
                    }
                    EventMsg::Error(event) => {
                        return Err(RunnerError::TurnFailed {
                            message: event.message,
                        });
                    }
                    EventMsg::TurnAborted(event) => {
                        return Err(RunnerError::TurnFailed {
                            message: format!("turn aborted: {:?}", event.reason),
                        });
                    }
                    EventMsg::ExecApprovalRequest(_)
                    | EventMsg::ApplyPatchApprovalRequest(_)
                    | EventMsg::RequestPermissions(_)
                    | EventMsg::RequestUserInput(_)
                    | EventMsg::DynamicToolCallRequest(_) => {
                        return Err(RunnerError::TurnFailed {
                            message: "observation requested a forbidden interactive operation"
                                .to_string(),
                        });
                    }
                    _ => {}
                }
            }
        })
        .await
        .map_err(|_| RunnerError::TurnTimeout)?
    }
}

#[derive(Default)]
struct ObservationAccumulator {
    turn_id: Option<String>,
    observation: Option<ObservationEvidence>,
}

struct ObservationEvidence {
    call_id: String,
    reference: Option<String>,
}

impl ObservationAccumulator {
    fn observe(&mut self, event: &EventMsg) {
        match event {
            EventMsg::TurnStarted(event) => self.turn_id = Some(event.turn_id.clone()),
            EventMsg::McpToolCallEnd(event)
                if event.invocation.server == GAME_SERVER_NAME
                    && event.invocation.tool == "get_app_state"
                    && event.is_success() =>
            {
                self.observation = event
                    .result
                    .as_ref()
                    .ok()
                    .map(|result| ObservationEvidence {
                        call_id: event.call_id.clone(),
                        reference: observation_reference(result),
                    });
            }
            _ => {}
        }
    }

    fn finish(
        self,
        thread_id: &str,
        rollout_path: Option<PathBuf>,
        policy: &GameCallPolicy,
        event: &TurnCompleteEvent,
    ) -> Result<ObservationReport, RunnerError> {
        if let Some(error) = &event.error {
            return Err(RunnerError::TurnFailed {
                message: error.message.clone(),
            });
        }
        let turn_id = self.turn_id.ok_or_else(|| RunnerError::TurnFailed {
            message: "turn completed before TurnStarted".to_string(),
        })?;
        if turn_id != event.turn_id {
            return Err(RunnerError::TurnFailed {
                message: format!(
                    "turn completion `{}` does not match active turn `{turn_id}`",
                    event.turn_id
                ),
            });
        }

        let audit = policy.audit();
        if audit.mutation_authorizations > 0 {
            return Err(RunnerError::MutationDispatched {
                count: audit.mutation_authorizations,
            });
        }
        if audit.mutation_attempts > 0 {
            return Err(RunnerError::MutationAttempted {
                count: audit.mutation_attempts,
            });
        }
        if audit.unknown_tool_attempts > 0 {
            return Err(RunnerError::UnknownGameToolAttempted {
                count: audit.unknown_tool_attempts,
            });
        }

        let observation = self
            .observation
            .ok_or(RunnerError::NoSuccessfulObservation)?;
        let rollout_path = rollout_path.ok_or(RunnerError::MissingRolloutPath)?;
        let message = event
            .last_agent_message
            .as_deref()
            .ok_or_else(|| invalid_report("turn completed without a final model report"))?;
        if message.len() > MAX_REPORT_BYTES {
            return Err(invalid_report("serialized report exceeds 12288 bytes"));
        }
        let model = serde_json::from_str::<ModelObservation>(message)
            .map_err(|error| invalid_report(error.to_string()))?;
        validate_model(&model)?;
        let lease = policy.lease();

        Ok(ObservationReport {
            thread_id: thread_id.to_string(),
            turn_id,
            observation_call_id: observation.call_id,
            observation_reference: observation.reference,
            rollout_path,
            epoch: lease.epoch,
            generation: lease.generation,
            mutation_attempts: audit.mutation_attempts,
            mutation_dispatches: audit.mutation_authorizations,
            model,
        })
    }
}

fn observation_reference(result: &CallToolResult) -> Option<String> {
    result
        .structured_content
        .as_ref()
        .and_then(|content| {
            content
                .get("observation_id")
                .or_else(|| content.get("artifact_uri"))
        })
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            result.content.iter().find_map(|content| {
                content
                    .get("uri")
                    .or_else(|| content.pointer("/resource/uri"))
                    .and_then(serde_json::Value::as_str)
            })
        })
        .filter(|reference| reference.len() <= MAX_FIELD_BYTES)
        .map(str::to_string)
}

fn validate_model(model: &ModelObservation) -> Result<(), RunnerError> {
    for value in [&model.visible_state_summary, &model.game_phase] {
        if value.len() > MAX_FIELD_BYTES {
            return Err(invalid_report("a scalar field exceeds 2048 bytes"));
        }
    }
    for values in [
        &model.visible_objects,
        &model.resources_and_choices,
        &model.uncertainties,
    ] {
        if values.len() > MAX_LIST_ITEMS {
            return Err(invalid_report("a list exceeds 32 items"));
        }
        if values.iter().any(|value| value.len() > MAX_FIELD_BYTES) {
            return Err(invalid_report("a list item exceeds 2048 bytes"));
        }
    }
    Ok(())
}

fn invalid_report(message: impl Into<String>) -> RunnerError {
    RunnerError::InvalidModelReport {
        message: message.into(),
    }
}

fn observation_schema() -> serde_json::Value {
    let bounded_string = json!({"type": "string", "maxLength": 2048});
    let bounded_list = json!({
        "type": "array",
        "maxItems": 32,
        "items": bounded_string,
    });
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "visible_state_summary",
            "game_phase",
            "visible_objects",
            "resources_and_choices",
            "uncertainties",
        ],
        "properties": {
            "visible_state_summary": {"type": "string", "maxLength": 2048},
            "game_phase": {"type": "string", "maxLength": 2048},
            "visible_objects": bounded_list,
            "resources_and_choices": bounded_list,
            "uncertainties": bounded_list,
        },
    })
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
