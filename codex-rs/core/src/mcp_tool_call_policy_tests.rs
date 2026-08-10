use std::sync::Arc;
use std::time::Duration;

use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::McpToolCallPolicyContributor;
use codex_extension_api::McpToolCallPolicyDecision;
use codex_extension_api::McpToolCallPolicyFuture;
use codex_extension_api::McpToolCallPolicyInput;
use codex_protocol::mcp::CallToolResult;
use codex_protocol::models::ResponseInputItem;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::approx_token_count;
use codex_utils_output_truncation::truncate_text;
use pretty_assertions::assert_eq;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::config::Config;
use crate::mcp_tool_call_policy::apply_mcp_tool_call_policies;
use crate::tools::context::McpToolOutput;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;

struct AddFields {
    expected_existing_key: Option<&'static str>,
    fields: Map<String, Value>,
}

impl McpToolCallPolicyContributor for AddFields {
    fn evaluate<'a>(&'a self, input: McpToolCallPolicyInput<'a>) -> McpToolCallPolicyFuture<'a> {
        Box::pin(async move {
            if let Some(key) = self.expected_existing_key {
                assert!(input.request_meta.contains_key(key));
            }
            McpToolCallPolicyDecision::Allow {
                additional_request_meta: self.fields.clone(),
            }
        })
    }
}

struct Deny(String);

impl McpToolCallPolicyContributor for Deny {
    fn evaluate<'a>(&'a self, _input: McpToolCallPolicyInput<'a>) -> McpToolCallPolicyFuture<'a> {
        Box::pin(async move {
            McpToolCallPolicyDecision::Deny {
                reason: self.0.to_string(),
            }
        })
    }
}

#[tokio::test]
async fn empty_policy_registry_preserves_request_meta() {
    let registry = ExtensionRegistryBuilder::<Config>::new().build();
    let arguments = json!({"direction": "left"});
    let request_meta = json!({"callId": "call-1"});

    let actual = apply_mcp_tool_call_policies(
        &registry,
        "game",
        "get_app_state",
        "call-1",
        Some(&arguments),
        Some(request_meta.clone()),
    )
    .await
    .expect("an empty policy registry should allow the call");

    assert_eq!(actual, Some(request_meta));
}

#[tokio::test]
async fn policy_contributors_add_metadata_in_registration_order() {
    let Value::Object(epoch_fields) = json!({"epoch": "campaign-epoch"}) else {
        unreachable!("epoch fixture must be an object");
    };
    let Value::Object(generation_fields) = json!({"generation": 7}) else {
        unreachable!("generation fixture must be an object");
    };
    let mut builder = ExtensionRegistryBuilder::<Config>::new();
    builder.mcp_tool_call_policy_contributor(Arc::new(AddFields {
        expected_existing_key: None,
        fields: epoch_fields,
    }));
    builder.mcp_tool_call_policy_contributor(Arc::new(AddFields {
        expected_existing_key: Some("epoch"),
        fields: generation_fields,
    }));
    let registry = builder.build();
    let arguments = json!({});
    let request_meta = json!({"callId": "call-1"});

    let actual = apply_mcp_tool_call_policies(
        &registry,
        "game",
        "get_app_state",
        "call-1",
        Some(&arguments),
        Some(request_meta),
    )
    .await
    .expect("additive policies should allow the call");

    assert_eq!(
        actual,
        Some(json!({
            "callId": "call-1",
            "epoch": "campaign-epoch",
            "generation": 7,
        }))
    );
}

#[tokio::test]
async fn policy_denial_returns_model_visible_reason() {
    let mut builder = ExtensionRegistryBuilder::<Config>::new();
    builder.mcp_tool_call_policy_contributor(Arc::new(Deny("record a plan first".to_string())));
    let registry = builder.build();
    let arguments = json!({"x": 2, "y": 3});

    let error =
        apply_mcp_tool_call_policies(&registry, "game", "click", "call-1", Some(&arguments), None)
            .await
            .expect_err("a denied call should return an error");

    assert_eq!(
        error.to_string(),
        "MCP call policy denied `game/click`: record a plan first"
    );
}

#[tokio::test]
async fn policy_denial_bounds_model_visible_reason() {
    let oversized_reason = "\0".repeat(4_096);
    let oversized_server_name = "\0".repeat(4_096);
    let oversized_tool_name = "\0".repeat(4_096);
    let expected_message = truncate_text(
        &format!(
            "MCP call policy denied `{oversized_server_name}/{oversized_tool_name}`: {oversized_reason}"
        ),
        TruncationPolicy::Tokens(64),
    );
    let mut builder = ExtensionRegistryBuilder::<Config>::new();
    builder.mcp_tool_call_policy_contributor(Arc::new(Deny(oversized_reason)));
    let registry = builder.build();

    let error = apply_mcp_tool_call_policies(
        &registry,
        &oversized_server_name,
        &oversized_tool_name,
        "call-1",
        None,
        None,
    )
    .await
    .expect_err("a denied call should return an error");
    let response = McpToolOutput {
        result: CallToolResult::from_error_text(format!("tool call error: {error}")),
        tool_input: serde_json::json!({}),
        wall_time: Duration::from_millis(1),
        original_image_detail_supported: false,
        truncation_policy: TruncationPolicy::Tokens(10_000),
    }
    .to_response_item(
        "call-1",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );
    let ResponseInputItem::FunctionCallOutput { output, .. } = response else {
        panic!("MCP denial should produce a function-call output");
    };
    let model_output = output
        .body
        .to_text()
        .expect("MCP denial output should serialize as text");

    assert_eq!(error.to_string(), expected_message);
    assert!(
        approx_token_count(&model_output) <= 512,
        "the wrapped model-visible denial must fit its token budget"
    );
}

#[tokio::test]
async fn policy_metadata_collision_is_rejected() {
    let Value::Object(fields) = json!({"callId": "replacement"}) else {
        unreachable!("collision fixture must be an object");
    };
    let mut builder = ExtensionRegistryBuilder::<Config>::new();
    builder.mcp_tool_call_policy_contributor(Arc::new(AddFields {
        expected_existing_key: Some("callId"),
        fields,
    }));
    let registry = builder.build();

    let error = apply_mcp_tool_call_policies(
        &registry,
        "game",
        "click",
        "call-1",
        None,
        Some(json!({"callId": "call-1"})),
    )
    .await
    .expect_err("a policy must not overwrite existing request metadata");

    assert_eq!(
        error.to_string(),
        "MCP call policy for `game/click` attempted to overwrite request metadata field `callId`"
    );
}
