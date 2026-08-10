use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use codex_config::types::McpServerConfig;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::McpToolCallPolicyContributor;
use codex_extension_api::McpToolCallPolicyDecision;
use codex_extension_api::McpToolCallPolicyFuture;
use codex_extension_api::McpToolCallPolicyInput;
use codex_protocol::models::PermissionProfile;
use core_test_support::responses;
use core_test_support::responses::mount_sse_once;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::net::unix::OwnedReadHalf;
use tokio::net::unix::OwnedWriteHalf;

const SERVER_NAME: &str = "game";
const CALL_ID: &str = "game-observation-1";
const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);

struct OwnerLeasePolicy;

impl McpToolCallPolicyContributor for OwnerLeasePolicy {
    fn evaluate<'a>(
        &'a self,
        input: McpToolCallPolicyInput<'a>,
    ) -> McpToolCallPolicyFuture<'a> {
        Box::pin(async move {
            if input.server_name != SERVER_NAME {
                return McpToolCallPolicyDecision::Allow {
                    additional_request_meta: Map::new(),
                };
            }

            assert_eq!(input.tool_name, "get_app_state");
            assert_eq!(input.arguments, Some(&json!({})));

            let Value::Object(additional_request_meta) = json!({
                "epoch": "campaign-epoch",
                "generation": 7,
                "call_id": input.call_id,
            }) else {
                unreachable!("owner lease fixture must be an object");
            };

            McpToolCallPolicyDecision::Allow {
                additional_request_meta,
            }
        })
    }
}

async fn write_message(writer: &mut OwnedWriteHalf, message: &Value) -> anyhow::Result<()> {
    let mut encoded = serde_json::to_vec(message)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

async fn next_message(
    lines: &mut tokio::io::Lines<BufReader<OwnedReadHalf>>,
) -> anyhow::Result<Value> {
    let line = tokio::time::timeout(SOCKET_TIMEOUT, lines.next_line())
        .await
        .context("timed out waiting for an MCP message")??
        .context("MCP client closed the socket")?;
    serde_json::from_str(&line).context("failed to parse MCP message")
}

async fn serve_fake_game_mcp(listener: UnixListener) -> anyhow::Result<Vec<String>> {
    let (stream, _) = listener
        .accept()
        .await
        .context("failed to accept MCP socket connection")?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut methods = Vec::new();

    let initialize = next_message(&mut lines).await?;
    assert_eq!(
        initialize.get("method").and_then(Value::as_str),
        Some("initialize")
    );
    methods.push("initialize".to_string());
    let initialize_id = initialize
        .get("id")
        .cloned()
        .context("initialize request is missing its id")?;
    let protocol_version = initialize
        .pointer("/params/protocolVersion")
        .cloned()
        .context("initialize request is missing its protocol version")?;
    write_message(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": initialize_id,
            "result": {
                "protocolVersion": protocol_version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-game", "version": "1.0.0"},
            },
        }),
    )
    .await?;

    let initialized = next_message(&mut lines).await?;
    assert_eq!(
        initialized.get("method").and_then(Value::as_str),
        Some("notifications/initialized")
    );
    methods.push("notifications/initialized".to_string());

    let tools_list = next_message(&mut lines).await?;
    assert_eq!(
        tools_list.get("method").and_then(Value::as_str),
        Some("tools/list")
    );
    methods.push("tools/list".to_string());
    let tools_list_id = tools_list
        .get("id")
        .cloned()
        .context("tools/list request is missing its id")?;
    write_message(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": tools_list_id,
            "result": {
                "tools": [{
                    "name": "get_app_state",
                    "description": "Capture the current game state.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false,
                    },
                    "annotations": {"readOnlyHint": true},
                }],
            },
        }),
    )
    .await?;

    let tools_call = next_message(&mut lines).await?;
    assert_eq!(
        tools_call.get("method").and_then(Value::as_str),
        Some("tools/call")
    );
    let mut tools_call_params = tools_call
        .get("params")
        .cloned()
        .context("tools/call request is missing its params")?;
    let metadata = tools_call_params
        .as_object_mut()
        .context("tools/call params should be an object")?
        .remove("_meta")
        .context("tools/call params should include Codex metadata")?;
    assert_eq!(
        tools_call_params,
        json!({
            "name": "get_app_state",
            "arguments": {},
        })
    );
    let call_id = metadata
        .get("callId")
        .and_then(Value::as_str)
        .context("Codex call metadata should contain callId")?;
    assert!(metadata.get("threadId").and_then(Value::as_str).is_some());
    assert!(
        metadata
            .get("x-codex-turn-metadata")
            .and_then(Value::as_object)
            .is_some()
    );
    assert_eq!(
        metadata.get("call_id").and_then(Value::as_str),
        Some(call_id)
    );
    assert_eq!(
        metadata.get("epoch").and_then(Value::as_str),
        Some("campaign-epoch")
    );
    assert_eq!(
        metadata.get("generation").and_then(Value::as_u64),
        Some(7)
    );
    methods.push("tools/call".to_string());
    let tools_call_id = tools_call
        .get("id")
        .cloned()
        .context("tools/call request is missing its id")?;
    write_message(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": tools_call_id,
            "result": {
                "content": [{
                    "type": "text",
                    "text": "Captured observation obs-1.",
                }],
                "structuredContent": {
                    "app": "Gambonanza",
                    "observation_id": "obs-1",
                },
                "isError": false,
            },
        }),
    )
    .await?;

    Ok(methods)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_calls_game_tool_through_stdio_to_uds() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_remote!(
        Ok(()),
        "the local bridge and fake Unix socket must share a filesystem"
    );

    let responses_server = responses::start_mock_server().await;
    let temp_dir = TempDir::new().context("failed to create MCP socket directory")?;
    let socket_path = temp_dir.path().join("game.sock");
    let listener = UnixListener::bind(&socket_path).context("failed to bind game MCP socket")?;
    let helper_task = tokio::spawn(serve_fake_game_mcp(listener));

    let tool_response = mount_sse_once(
        &responses_server,
        responses::sse(vec![
            responses::ev_response_created("response-1"),
            responses::ev_custom_tool_call(
                CALL_ID,
                "exec",
                r#"
const hasGameTool = typeof tools.mcp__game__get_app_state === "function";
const result = await tools.mcp__game__get_app_state({});
text(JSON.stringify({ hasGameTool, structuredContent: result.structuredContent }));
"#,
            ),
            responses::ev_completed("response-1"),
        ]),
    )
    .await;
    let completion_response = mount_sse_once(
        &responses_server,
        responses::sse(vec![
            responses::ev_response_created("response-2"),
            responses::ev_assistant_message("message-2", "Observation received."),
            responses::ev_completed("response-2"),
        ]),
    )
    .await;

    let codex_bin = codex_utils_cargo_bin::cargo_bin("codex")?;
    let code_mode_host_bin = codex_utils_cargo_bin::cargo_bin("codex-code-mode-host")?;
    let game_server = serde_json::from_value::<McpServerConfig>(json!({
        "command": codex_bin,
        "args": ["stdio-to-uds", socket_path],
        "required": true,
        "enabled_tools": ["get_app_state"],
        "startup_timeout_sec": 15,
        "tool_timeout_sec": 5,
    }))?;
    let mut extensions = ExtensionRegistryBuilder::<codex_core::config::Config>::new();
    extensions.mcp_tool_call_policy_contributor(Arc::new(OwnerLeasePolicy));
    let fixture = test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .with_code_mode_host_program(code_mode_host_bin)
        .with_model_info_override("gpt-5.6-sol", |model| model.supports_search_tool = false)
        .with_config(move |config| {
            let mut servers = config.mcp_servers.get().clone();
            servers.insert(SERVER_NAME.to_string(), game_server);
            config
                .mcp_servers
                .set(servers)
                .expect("test config should accept the game MCP server");
        })
        .build_with_auto_env(&responses_server)
        .await?;

    wait_for_mcp_server(&fixture.codex, SERVER_NAME).await?;
    fixture
        .submit_turn_with_permission_profile(
            "Call game get_app_state exactly once.",
            PermissionProfile::read_only(),
        )
        .await?;
    let first_request = tool_response.single_request();
    let first_request_body = first_request.body_json();
    let exec_definition = first_request_body["input"]
        .as_array()
        .and_then(|input| {
            input
                .iter()
                .find(|item| item.get("role").and_then(Value::as_str) == Some("developer"))
        })
        .and_then(|developer| developer.get("tools"))
        .and_then(Value::as_array)
        .and_then(|namespaces| {
            namespaces
                .iter()
                .find(|namespace| namespace.get("name").and_then(Value::as_str) == Some("functions"))
        })
        .and_then(|namespace| namespace.get("tools"))
        .and_then(Value::as_array)
        .and_then(|tools| {
            tools
                .iter()
                .find(|tool| tool.get("name").and_then(Value::as_str) == Some("exec"))
        })
        .context("Sol code-mode exec tool is missing from the developer input")?;
    assert!(
        exec_definition
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|description| description.contains("mcp__game__get_app_state")),
        "game MCP declaration is missing from the Sol exec tool"
    );
    let completion_request = completion_response.single_request();
    let output = completion_request.custom_tool_call_output(CALL_ID);
    let output_json = match &output["output"] {
        Value::String(text) => text.as_str(),
        Value::Array(items) => items
            .iter()
            .rev()
            .find_map(|item| item.get("text").and_then(Value::as_str))
            .context("code-mode output should contain the game result text")?,
        Value::Object(object) => object
            .get("content")
            .and_then(Value::as_str)
            .context("code-mode output object should contain result text")?,
        Value::Null | Value::Bool(_) | Value::Number(_) => {
            anyhow::bail!("unexpected code-mode output: {output}")
        }
    };
    assert_eq!(
        serde_json::from_str::<Value>(output_json)
            .with_context(|| format!("failed to parse code-mode game result: {output_json:?}"))?,
        json!({
            "hasGameTool": true,
            "structuredContent": {
                "app": "Gambonanza",
                "observation_id": "obs-1",
            },
        })
    );

    let methods = helper_task
        .await
        .context("fake game MCP task panicked")??;
    assert_eq!(
        methods,
        vec![
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call",
        ]
    );
    fixture.codex.shutdown_and_wait().await?;
    responses_server.verify().await;
    Ok(())
}
