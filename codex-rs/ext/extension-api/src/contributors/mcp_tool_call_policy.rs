use std::future::Future;
use std::pin::Pin;

use serde_json::Map;
use serde_json::Value;

/// Future returned while a host-owned MCP call policy evaluates one call.
pub type McpToolCallPolicyFuture<'a> =
    Pin<Box<dyn Future<Output = McpToolCallPolicyDecision> + Send + 'a>>;

/// Read-only MCP call information supplied immediately before dispatch.
pub struct McpToolCallPolicyInput<'a> {
    pub server_name: &'a str,
    pub tool_name: &'a str,
    pub call_id: &'a str,
    pub arguments: Option<&'a Value>,
    pub request_meta: &'a Map<String, Value>,
}

/// Host policy decision for one prepared MCP call.
#[derive(Debug, PartialEq)]
pub enum McpToolCallPolicyDecision {
    /// Permit dispatch and append metadata fields that do not already exist.
    Allow {
        additional_request_meta: Map<String, Value>,
    },
    /// Reject dispatch and return the reason to the model.
    Deny { reason: String },
}

/// Host-owned policy evaluated for every prepared MCP call.
///
/// Implementations must treat arguments and existing metadata as read-only.
/// They may deny a call or return additional metadata. Codex evaluates
/// contributors in registration order and rejects duplicate metadata keys.
pub trait McpToolCallPolicyContributor: Send + Sync {
    fn evaluate<'a>(
        &'a self,
        input: McpToolCallPolicyInput<'a>,
    ) -> McpToolCallPolicyFuture<'a>;
}
