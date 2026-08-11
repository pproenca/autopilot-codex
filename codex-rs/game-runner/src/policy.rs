use std::sync::Arc;
use std::sync::atomic::AtomicBool;
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

use crate::CampaignPersistence;
use crate::DecisionGate;
use crate::GAME_SERVER_NAME;
use crate::MutationCheckpointUpdate;
use crate::OwnerLeaseState;

pub struct GameCallPolicy {
    lease: Arc<OwnerLeaseState>,
    gate: Arc<DecisionGate>,
    persistence: Option<Arc<CampaignPersistence>>,
    mutation_lane_open: AtomicBool,
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
            lease: Arc::new(OwnerLeaseState::new(epoch, generation)),
            gate,
            persistence: None,
            mutation_lane_open: AtomicBool::new(true),
            unknown_tool_attempts: AtomicU64::new(0),
        }
    }

    pub fn durable(
        lease: Arc<OwnerLeaseState>,
        gate: Arc<DecisionGate>,
        persistence: Arc<CampaignPersistence>,
    ) -> Self {
        Self {
            lease,
            gate,
            persistence: Some(persistence),
            mutation_lane_open: AtomicBool::new(true),
            unknown_tool_attempts: AtomicU64::new(0),
        }
    }

    pub fn lease(&self) -> OwnerLease {
        self.lease.current()
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
        let lease = self.lease.current();
        let mut additional_request_meta = Map::new();
        additional_request_meta.insert("epoch".to_string(), Value::String(lease.epoch));
        additional_request_meta.insert(
            "generation".to_string(),
            Value::Number(lease.generation.into()),
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

    pub fn close_mutation_lane(&self) {
        self.mutation_lane_open.store(false, Ordering::Release);
    }

    pub fn mutation_lane_is_open(&self) -> bool {
        self.mutation_lane_open.load(Ordering::Acquire)
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
                    if !self.mutation_lane_is_open() {
                        return McpToolCallPolicyDecision::Deny {
                            reason: "campaign mutation lane is closed".to_string(),
                        };
                    }
                    let arguments = input.arguments.unwrap_or(&Value::Null);
                    match self
                        .gate
                        .prepare_mutation(input.tool_name, arguments, input.call_id)
                    {
                        Ok(authorization) => {
                            if let Some(persistence) = &self.persistence {
                                let update = MutationCheckpointUpdate {
                                    authorization: authorization.clone(),
                                    decision_audit: self.gate.snapshot().audit,
                                    policy_audit: self.audit(),
                                };
                                if persistence.begin_mutation(&update).await.is_err() {
                                    self.close_mutation_lane();
                                    return McpToolCallPolicyDecision::Deny {
                                        reason: "campaign checkpoint write failed before mutation dispatch"
                                            .to_string(),
                                    };
                                }
                            }
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
