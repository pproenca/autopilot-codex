use std::time::Duration;

use codex_core_api::CallToolResult;
use codex_core_api::McpInvocation;
use codex_core_api::McpToolCallEndEvent;
use pretty_assertions::assert_eq;

use super::GameToolFailureError;
use super::WorkerDirective;
use super::game_tool_failure_signal;

#[test]
fn failed_known_game_tool_creates_one_bounded_directive_exchange() -> anyhow::Result<()> {
    let error = "é".repeat(2 * 1024);
    let (signal, response) = game_tool_failure_signal(&tool_end("click", Err(error)))?
        .expect("failed game call should produce a signal");

    assert_eq!(signal.tool, "click");
    assert!(signal.summary.len() <= 2 * 1024);
    assert!(signal.summary.is_char_boundary(signal.summary.len()));
    signal
        .response
        .send(WorkerDirective::PauseForRecovery)
        .expect("directive receiver should remain open");
    assert_eq!(response.blocking_recv()?, WorkerDirective::PauseForRecovery);
    Ok(())
}

#[test]
fn success_is_ignored_and_unknown_failure_is_rejected() -> anyhow::Result<()> {
    assert!(
        game_tool_failure_signal(&tool_end(
            "get_app_state",
            Ok(CallToolResult {
                content: Vec::new(),
                structured_content: None,
                is_error: Some(false),
                meta: None,
            }),
        ))?
        .is_none()
    );
    assert!(matches!(
        game_tool_failure_signal(&tool_end("unknown", Err("offline".to_string()))),
        Err(GameToolFailureError::UnknownTool { tool }) if tool == "unknown"
    ));
    Ok(())
}

#[test]
fn structured_error_uses_a_fixed_non_payload_summary() -> anyhow::Result<()> {
    let (signal, _response) = game_tool_failure_signal(&tool_end(
        "drag",
        Ok(CallToolResult {
            content: Vec::new(),
            structured_content: Some(serde_json::json!({"private": "payload"})),
            is_error: Some(true),
            meta: None,
        }),
    ))?
    .expect("error result should produce a signal");

    assert_eq!(signal.summary, "game tool returned an error result");
    Ok(())
}

fn tool_end(tool: &str, result: Result<CallToolResult, String>) -> McpToolCallEndEvent {
    McpToolCallEndEvent {
        call_id: "call-1".to_string(),
        invocation: McpInvocation {
            server: "game".to_string(),
            tool: tool.to_string(),
            arguments: None,
        },
        connector_id: None,
        mcp_app_resource_uri: None,
        link_id: None,
        app_name: None,
        action_name: None,
        plugin_id: None,
        read_only_hint: None,
        duration: Duration::ZERO,
        result,
    }
}
