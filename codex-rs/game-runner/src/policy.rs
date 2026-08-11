use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use codex_core_api::McpToolCallPolicyContributor;
use codex_core_api::McpToolCallPolicyDecision;
use codex_core_api::McpToolCallPolicyFuture;
use codex_core_api::McpToolCallPolicyInput;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::DecisionGate;
use crate::GAME_SERVER_NAME;

pub struct GameCallPolicy {
    epoch: String,
    generation: u64,
    gate: Arc<DecisionGate>,
    unknown_tool_attempts: AtomicU64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnerLease {
    pub epoch: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAudit {
    pub mutation_attempts: u64,
    pub unknown_tool_attempts: u64,
    pub mutation_authorizations: u64,
}

impl GameCallPolicy {
    pub fn new(epoch: String, generation: u64, gate: Arc<DecisionGate>) -> Self {
        Self {
            epoch,
            generation,
            gate,
            unknown_tool_attempts: AtomicU64::new(0),
        }
    }

    pub fn lease(&self) -> OwnerLease {
        OwnerLease {
            epoch: self.epoch.clone(),
            generation: self.generation,
        }
    }

    pub fn audit(&self) -> PolicyAudit {
        let decision_audit = self.gate.snapshot().audit;
        PolicyAudit {
            mutation_attempts: decision_audit.mutation_attempts,
            unknown_tool_attempts: self.unknown_tool_attempts.load(Ordering::Relaxed),
            mutation_authorizations: decision_audit.mutation_authorizations,
        }
    }

    fn allow_with_owner_metadata(&self, call_id: &str) -> McpToolCallPolicyDecision {
        let mut additional_request_meta = Map::new();
        additional_request_meta.insert("epoch".to_string(), Value::String(self.epoch.clone()));
        additional_request_meta.insert(
            "generation".to_string(),
            Value::Number(self.generation.into()),
        );
        additional_request_meta.insert("call_id".to_string(), Value::String(call_id.to_string()));
        McpToolCallPolicyDecision::Allow {
            additional_request_meta,
        }
    }

    fn record_unknown_tool_attempt(&self) {
        let _ = self.unknown_tool_attempts.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |value| value.checked_add(1),
        );
    }
}

impl McpToolCallPolicyContributor for GameCallPolicy {
    fn evaluate<'a>(&'a self, input: McpToolCallPolicyInput<'a>) -> McpToolCallPolicyFuture<'a> {
        Box::pin(async move {
            if input.server_name != GAME_SERVER_NAME {
                return McpToolCallPolicyDecision::Allow {
                    additional_request_meta: Map::new(),
                };
            }

            match input.tool_name {
                "get_app_state" => {
                    self.gate.begin_full_observation();
                    self.allow_with_owner_metadata(input.call_id)
                }
                "wait" => {
                    self.gate.before_wait(input.arguments);
                    self.allow_with_owner_metadata(input.call_id)
                }
                "click" | "drag" | "focus_click" => {
                    let arguments = input.arguments.unwrap_or(&Value::Null);
                    match self
                        .gate
                        .prepare_mutation(input.tool_name, arguments, input.call_id)
                    {
                        Ok(authorization) => {
                            let McpToolCallPolicyDecision::Allow {
                                mut additional_request_meta,
                            } = self.allow_with_owner_metadata(input.call_id)
                            else {
                                unreachable!("owner metadata is always allowed")
                            };
                            additional_request_meta.insert(
                                "operation_id".to_string(),
                                Value::String(authorization.operation_id),
                            );
                            additional_request_meta.insert(
                                "action_sha256".to_string(),
                                Value::String(authorization.action_sha256),
                            );
                            McpToolCallPolicyDecision::Allow {
                                additional_request_meta,
                            }
                        }
                        Err(error) => McpToolCallPolicyDecision::Deny {
                            reason: error.to_string(),
                        },
                    }
                }
                "zoom" => {
                    self.record_unknown_tool_attempt();
                    McpToolCallPolicyDecision::Deny {
                        reason: "unknown game tool `zoom` is disabled".to_string(),
                    }
                }
                tool_name => {
                    self.record_unknown_tool_attempt();
                    McpToolCallPolicyDecision::Deny {
                        reason: format!("unknown game tool `{tool_name}` is disabled"),
                    }
                }
            }
        })
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
