use anyhow::bail;
use codex_extension_api::ExtensionRegistry;
use codex_extension_api::McpToolCallPolicyDecision;
use codex_extension_api::McpToolCallPolicyInput;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::truncate_text;
use serde_json::Map;
use serde_json::Value;
use serde_json::map::Entry;

use crate::config::Config;

const DENIAL_MESSAGE_TRUNCATION_BUDGET_TOKENS: usize = 432;

pub(crate) async fn apply_mcp_tool_call_policies(
    extensions: &ExtensionRegistry<Config>,
    server_name: &str,
    tool_name: &str,
    call_id: &str,
    arguments: Option<&Value>,
    request_meta: Option<Value>,
) -> anyhow::Result<Option<Value>> {
    let mut request_meta = match request_meta {
        Some(Value::Object(request_meta)) => request_meta,
        Some(_) => bail!("MCP request metadata must be a JSON object"),
        None => Map::new(),
    };

    for contributor in extensions.mcp_tool_call_policy_contributors() {
        let decision = contributor
            .evaluate(McpToolCallPolicyInput {
                server_name,
                tool_name,
                call_id,
                arguments,
                request_meta: &request_meta,
            })
            .await;
        match decision {
            McpToolCallPolicyDecision::Allow {
                additional_request_meta,
            } => {
                for (key, value) in additional_request_meta {
                    match request_meta.entry(key) {
                        Entry::Vacant(entry) => {
                            entry.insert(value);
                        }
                        Entry::Occupied(entry) => {
                            bail!(
                                "MCP call policy for `{server_name}/{tool_name}` attempted to overwrite request metadata field `{}`",
                                entry.key()
                            );
                        }
                    }
                }
            }
            McpToolCallPolicyDecision::Deny { reason } => {
                let message =
                    format!("MCP call policy denied `{server_name}/{tool_name}`: {reason}");
                // Reserve room for the truncation marker plus the outer tool-error and wall-time
                // wrappers so the complete model-visible output remains below 512 tokens.
                let message = truncate_text(
                    &message,
                    TruncationPolicy::Tokens(DENIAL_MESSAGE_TRUNCATION_BUDGET_TOKENS),
                );
                bail!("{message}");
            }
        }
    }

    if request_meta.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::Object(request_meta)))
    }
}
