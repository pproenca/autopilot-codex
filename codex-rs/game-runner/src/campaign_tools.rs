use std::sync::Arc;

use codex_core_api::DynamicToolCallOutputContentItem;
use codex_core_api::DynamicToolCallRequest;
use codex_core_api::DynamicToolFunctionSpec;
use codex_core_api::DynamicToolNamespaceSpec;
use codex_core_api::DynamicToolNamespaceTool;
use codex_core_api::DynamicToolResponse;
use codex_core_api::DynamicToolSpec;
use serde_json::Value;
use serde_json::json;

use crate::DecisionGate;
use crate::OutcomeDraft;
use crate::PlanDraft;

pub const CAMPAIGN_TOOL_NAMESPACE: &str = "game_runner";

pub struct CampaignTools {
    gate: Arc<DecisionGate>,
}

#[derive(Debug, thiserror::Error)]
pub enum CampaignToolError {
    #[error("unexpected dynamic tool {namespace:?}/{tool}")]
    UnexpectedTool {
        namespace: Option<String>,
        tool: String,
    },
    #[error("failed to encode a dynamic tool response")]
    ResponseEncoding {
        #[source]
        source: serde_json::Error,
    },
}

impl CampaignTools {
    pub fn new(gate: Arc<DecisionGate>) -> Self {
        Self { gate }
    }

    pub fn specs() -> Vec<DynamicToolSpec> {
        vec![DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
            name: CAMPAIGN_TOOL_NAMESPACE.to_string(),
            description: "Record bounded game decisions and visible evidence-linked outcomes."
                .to_string(),
            tools: vec![
                DynamicToolNamespaceTool::Function(record_plan_spec()),
                DynamicToolNamespaceTool::Function(report_outcome_spec()),
            ],
        })]
    }

    pub fn handle(
        &self,
        request: &DynamicToolCallRequest,
    ) -> Result<DynamicToolResponse, CampaignToolError> {
        match (request.namespace.as_deref(), request.tool.as_str()) {
            (Some(CAMPAIGN_TOOL_NAMESPACE), "record_plan") => Ok(self.record_plan(request)),
            (Some(CAMPAIGN_TOOL_NAMESPACE), "report_outcome") => Ok(self.report_outcome(request)),
            _ => Err(CampaignToolError::UnexpectedTool {
                namespace: request.namespace.clone(),
                tool: request.tool.clone(),
            }),
        }
    }

    fn record_plan(&self, request: &DynamicToolCallRequest) -> DynamicToolResponse {
        let result = bounded_decode::<PlanDraft>(&request.arguments, 12 * 1024).and_then(|draft| {
            self.gate
                .record_plan(draft)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(plan) => response(
                true,
                json!({
                    "action_sha256": plan.action_sha256,
                    "observation_reference": plan.observation.reference,
                    "plan_id": plan.id,
                }),
            ),
            Err(message) => rejected(message),
        }
    }

    fn report_outcome(&self, request: &DynamicToolCallRequest) -> DynamicToolResponse {
        let result =
            bounded_decode::<OutcomeDraft>(&request.arguments, 8 * 1024).and_then(|draft| {
                self.gate
                    .report_outcome(draft)
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(outcome) => response(
                true,
                json!({
                    "observation_reference": outcome.observation.reference,
                    "outcome": outcome.draft.outcome,
                }),
            ),
            Err(message) => rejected(message),
        }
    }
}

fn bounded_decode<T: serde::de::DeserializeOwned>(
    arguments: &Value,
    max_bytes: usize,
) -> Result<T, String> {
    let bytes = serde_json::to_vec(arguments).map_err(|error| error.to_string())?;
    if bytes.len() > max_bytes {
        return Err(format!("arguments exceed the {max_bytes}-byte limit"));
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid arguments: {error}"))
}

fn rejected(message: String) -> DynamicToolResponse {
    response(false, json!({"error": message}))
}

fn response(success: bool, value: Value) -> DynamicToolResponse {
    DynamicToolResponse {
        content_items: vec![DynamicToolCallOutputContentItem::InputText {
            text: value.to_string(),
        }],
        success,
    }
}

fn record_plan_spec() -> DynamicToolFunctionSpec {
    DynamicToolFunctionSpec {
        name: "record_plan".to_string(),
        description: "Record two to four candidate moves and one exact chosen game action."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": [
                "observation_reference", "objective", "visible_state_summary", "candidates",
                "chosen_action", "reason", "expected_visible_result", "invalidation_condition"
            ],
            "properties": {
                "observation_reference": string_schema(),
                "objective": string_schema(),
                "visible_state_summary": string_schema(),
                "candidates": {
                    "type": "array",
                    "minItems": 2,
                    "maxItems": 4,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["action", "predicted_visible_consequence"],
                        "properties": {
                            "action": string_schema(),
                            "predicted_visible_consequence": string_schema()
                        }
                    }
                },
                "chosen_action": {"oneOf": [
                    action_schema("click", json!({
                        "x": coordinate_schema(),
                        "y": coordinate_schema(),
                        "button": {"type": "string", "enum": ["left", "right"]},
                        "count": {"type": "integer", "minimum": 1, "maximum": 3}
                    }), &["x", "y"]),
                    action_schema("drag", json!({
                        "from_x": coordinate_schema(),
                        "from_y": coordinate_schema(),
                        "to_x": coordinate_schema(),
                        "to_y": coordinate_schema()
                    }), &["from_x", "from_y", "to_x", "to_y"]),
                    action_schema("focus_click", json!({
                        "x": coordinate_schema(),
                        "y": coordinate_schema()
                    }), &["x", "y"])
                ]},
                "reason": string_schema(),
                "expected_visible_result": string_schema(),
                "invalidation_condition": string_schema()
            }
        }),
        defer_loading: false,
    }
}

fn report_outcome_spec() -> DynamicToolFunctionSpec {
    DynamicToolFunctionSpec {
        name: "report_outcome".to_string(),
        description:
            "Classify fresh evidence as the expected canary result, a win, a loss, or a terminal block."
                .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": [
                "outcome", "observation_reference", "visible_evidence_summary", "lesson"
            ],
            "properties": {
                "outcome": {
                    "type": "string",
                    "enum": ["canary_complete", "loss", "win", "terminal_block"]
                },
                "observation_reference": string_schema(),
                "visible_evidence_summary": string_schema(),
                "lesson": string_schema()
            }
        }),
        defer_loading: false,
    }
}

fn action_schema(tool: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["tool", "arguments"],
        "properties": {
            "tool": {"const": tool},
            "arguments": {
                "type": "object",
                "additionalProperties": false,
                "required": required,
                "properties": properties
            }
        }
    })
}

fn coordinate_schema() -> Value {
    json!({"type": "integer", "minimum": 0})
}

fn string_schema() -> Value {
    json!({"type": "string", "maxLength": 2048})
}

#[cfg(test)]
#[path = "campaign_tools_tests.rs"]
mod tests;
