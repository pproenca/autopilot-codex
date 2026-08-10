use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use codex_core_api::McpToolCallPolicyContributor;
use codex_core_api::McpToolCallPolicyDecision;
use codex_core_api::McpToolCallPolicyFuture;
use codex_core_api::McpToolCallPolicyInput;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::GAME_SERVER_NAME;

pub struct GameCallPolicy {
    epoch: String,
    generation: u64,
    mutation_attempts: AtomicUsize,
    unknown_tool_attempts: AtomicUsize,
    mutation_authorizations: AtomicUsize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnerLease {
    pub epoch: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PolicyAudit {
    pub mutation_attempts: usize,
    pub unknown_tool_attempts: usize,
    pub mutation_authorizations: usize,
}

impl GameCallPolicy {
    pub fn new(epoch: String, generation: u64) -> Self {
        Self {
            epoch,
            generation,
            mutation_attempts: AtomicUsize::new(0),
            unknown_tool_attempts: AtomicUsize::new(0),
            mutation_authorizations: AtomicUsize::new(0),
        }
    }

    pub fn lease(&self) -> OwnerLease {
        OwnerLease {
            epoch: self.epoch.clone(),
            generation: self.generation,
        }
    }

    pub fn audit(&self) -> PolicyAudit {
        PolicyAudit {
            mutation_attempts: self.mutation_attempts.load(Ordering::Relaxed),
            unknown_tool_attempts: self.unknown_tool_attempts.load(Ordering::Relaxed),
            mutation_authorizations: self.mutation_authorizations.load(Ordering::Relaxed),
        }
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

            let tool_name = input.tool_name;
            match tool_name {
                "get_app_state" | "wait" | "zoom" => {
                    let mut additional_request_meta = Map::new();
                    additional_request_meta
                        .insert("epoch".to_string(), Value::String(self.epoch.clone()));
                    additional_request_meta.insert(
                        "generation".to_string(),
                        Value::Number(self.generation.into()),
                    );
                    additional_request_meta.insert(
                        "call_id".to_string(),
                        Value::String(input.call_id.to_string()),
                    );
                    McpToolCallPolicyDecision::Allow {
                        additional_request_meta,
                    }
                }
                "click" | "drag" | "focus_click" => {
                    self.mutation_attempts.fetch_add(1, Ordering::Relaxed);
                    McpToolCallPolicyDecision::Deny {
                        reason: format!(
                            "game tool `{tool_name}` is mutating and disabled during observation"
                        ),
                    }
                }
                _ => {
                    self.unknown_tool_attempts.fetch_add(1, Ordering::Relaxed);
                    McpToolCallPolicyDecision::Deny {
                        reason: format!(
                            "unknown game tool `{tool_name}` is disabled during observation"
                        ),
                    }
                }
            }
        })
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
