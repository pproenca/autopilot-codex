use codex_core_api::McpToolCallPolicyContributor;
use codex_core_api::McpToolCallPolicyDecision;
use codex_core_api::McpToolCallPolicyInput;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::GameCallPolicy;
use super::PolicyAudit;

#[tokio::test]
async fn read_only_call_receives_exact_owner_lease() {
    let policy = GameCallPolicy::new("epoch-1".to_string(), 1);
    let request_meta = serde_json::Map::new();
    let arguments = json!({});

    let decision = policy
        .evaluate(McpToolCallPolicyInput {
            server_name: "game",
            tool_name: "get_app_state",
            call_id: "call-7",
            arguments: Some(&arguments),
            request_meta: &request_meta,
        })
        .await;

    assert_eq!(
        decision,
        McpToolCallPolicyDecision::Allow {
            additional_request_meta: json!({
                "epoch": "epoch-1",
                "generation": 1,
                "call_id": "call-7",
            })
            .as_object()
            .expect("metadata fixture must be an object")
            .clone(),
        }
    );
}

#[tokio::test]
async fn mutation_and_unknown_calls_are_denied_and_audited() {
    let policy = GameCallPolicy::new("epoch-1".to_string(), 1);
    let request_meta = serde_json::Map::new();
    for (tool_name, reason) in [
        ("click", "game tool `click` is mutating and disabled during observation"),
        ("drag", "game tool `drag` is mutating and disabled during observation"),
        ("focus_click", "game tool `focus_click` is mutating and disabled during observation"),
        ("unexpected_tool", "unknown game tool `unexpected_tool` is disabled during observation"),
    ] {
        assert_eq!(
            policy
                .evaluate(McpToolCallPolicyInput {
                    server_name: "game",
                    tool_name,
                    call_id: "call-unsafe",
                    arguments: None,
                    request_meta: &request_meta,
                })
                .await,
            McpToolCallPolicyDecision::Deny {
                reason: reason.to_string(),
            }
        );
    }
    assert_eq!(
        policy.audit(),
        PolicyAudit {
            mutation_attempts: 3,
            unknown_tool_attempts: 1,
            mutation_authorizations: 0,
        }
    );
}

#[tokio::test]
async fn non_game_server_is_not_changed() {
    let policy = GameCallPolicy::new("epoch-1".to_string(), 1);
    let request_meta = serde_json::Map::new();

    assert_eq!(
        policy
            .evaluate(McpToolCallPolicyInput {
                server_name: "other",
                tool_name: "click",
                call_id: "call-other",
                arguments: None,
                request_meta: &request_meta,
            })
            .await,
        McpToolCallPolicyDecision::Allow {
            additional_request_meta: serde_json::Map::new(),
        }
    );
}
