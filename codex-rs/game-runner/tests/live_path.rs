#![cfg(unix)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_core_api::Config;
use codex_core_api::ExtensionRegistryBuilder;
use codex_core_api::Feature;
use codex_core_api::Features;
use codex_core_api::McpServerConfig;
use codex_core_api::WebSearchMode;
use codex_game_runner::GAME_SERVER_NAME;
use codex_game_runner::GENERATION;
use codex_game_runner::DecisionGate;
use codex_game_runner::GameCallPolicy;
use codex_game_runner::ModelObservation;
use codex_game_runner::ObservationLimits;
use codex_game_runner::ObservationRun;
use core_test_support::responses;
use core_test_support::responses::mount_sse_once;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::net::unix::OwnedReadHalf;
use tokio::net::unix::OwnedWriteHalf;

const CALL_ID: &str = "game-observation-1";
const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
struct HelperTrace {
    methods: Vec<String>,
    call_id: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sol_observation_crosses_the_real_code_mode_and_uds_path() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_remote!(Ok(()), "the fake Unix socket is host-local");

    let server = responses::start_mock_server().await;
    let temp = TempDir::new()?;
    let socket_path = temp.path().join("game.sock");
    let listener = UnixListener::bind(&socket_path)?;
    let spool_root = temp.path().join("screenshot-spool");
    let helper_task = tokio::spawn(serve_fake_game_mcp(listener, spool_root.clone()));
    let tool_response = mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("response-1"),
            responses::ev_custom_tool_call(
                CALL_ID,
                "exec",
                r#"const result = await tools.mcp__game__get_app_state({});
for (const content of result.content || []) {
  if (content.type === "image") image(content);
  else if (content.type === "text") text(content.text);
}"#,
            ),
            responses::ev_completed("response-1"),
        ]),
    )
    .await;
    let completion_response = mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("response-2"),
            responses::ev_assistant_message(
                "message-2",
                &serde_json::to_string(&expected_model())?,
            ),
            responses::ev_completed("response-2"),
        ]),
    )
    .await;

    let game_runner_bin = codex_utils_cargo_bin::cargo_bin("codex-game-runner")?;
    let code_mode_host_bin = codex_utils_cargo_bin::cargo_bin("codex-code-mode-host")?;
    let game_server = serde_json::from_value::<McpServerConfig>(json!({
        "command": game_runner_bin,
        "args": ["__stdio-to-uds", socket_path],
        "required": true,
        "enabled_tools": ["get_app_state", "wait", "click", "drag", "focus_click"],
        "startup_timeout_sec": 15,
        "tool_timeout_sec": 5,
    }))?;
    let gate = Arc::new(DecisionGate::new(GENERATION));
    let policy = Arc::new(GameCallPolicy::new(
        "test-epoch".to_string(),
        GENERATION,
        gate,
    ));
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.mcp_tool_call_policy_contributor(policy.clone());
    let fixture = test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .with_code_mode_host_program(code_mode_host_bin)
        .with_model_info_override("gpt-5.6-sol", |model| model.supports_search_tool = false)
        .with_config(move |config| configure_runner_surface(config, game_server))
        .build_with_auto_env(&server)
        .await?;

    wait_for_mcp_server(&fixture.codex, GAME_SERVER_NAME).await?;
    let expected_rollout = fixture
        .session_configured
        .rollout_path
        .clone()
        .context("test thread should retain a rollout")?;
    let report = ObservationRun::new(ObservationLimits {
        turn_timeout: Duration::from_secs(20),
    })
    .execute(
        &fixture.codex,
        &fixture.session_configured,
        policy.as_ref(),
        "Gambonanza",
    )
    .await?;
    let helper_trace = helper_task.await.context("fake game MCP task panicked")??;

    assert!(!report.turn_id.is_empty());
    assert!(!helper_trace.call_id.is_empty());
    assert_eq!(
        (
            report.thread_id.as_str(),
            report.observation_call_id.as_str(),
            report.observation_reference.as_deref(),
            report.rollout_path.as_path(),
            report.epoch.as_str(),
            report.generation,
            report.mutation_attempts,
            report.mutation_dispatches,
            &report.model,
        ),
        (
            fixture.session_configured.thread_id.to_string().as_str(),
            helper_trace.call_id.as_str(),
            Some("sha256:32461d5bd1773012acef0ba15636752949bd7c2ce50f9172159d9f56cf0dd9af"),
            expected_rollout.as_path(),
            "test-epoch",
            1,
            0,
            0,
            &expected_model(),
        )
    );

    let request = tool_response.single_request();
    assert!(request.body_contains_text("Gambonanza"));
    let body = request.body_json();
    assert_eq!(
        body["text"]["format"]["schema"]["additionalProperties"],
        false
    );
    let description = exec_description(&body)?;
    assert!(description.contains("mcp__game__get_app_state"));
    for forbidden in [
        "### `exec_command`",
        "### `apply_patch`",
        "### `mcp__codex_apps",
        "### `spawn_agent`",
        "project instructions",
    ] {
        assert!(
            !description.contains(forbidden),
            "unexpected `{forbidden}` tool surface"
        );
    }
    assert!(
        completion_response
            .single_request()
            .custom_tool_call_output(CALL_ID)
            .is_object()
    );
    assert_eq!(std::fs::read_dir(spool_root)?.count(), 0);
    assert_eq!(
        helper_trace.methods,
        vec![
            "initialize".to_string(),
            "notifications/initialized".to_string(),
            "tools/list".to_string(),
            "tools/call:get_app_state".to_string(),
        ]
    );
    fixture.codex.shutdown_and_wait().await?;
    server.verify().await;
    Ok(())
}

fn configure_runner_surface(config: &mut Config, game_server: McpServerConfig) {
    let mut features = Features::default();
    features.enable(Feature::CodeMode);
    features.enable(Feature::CodeModeHost);
    features.enable(Feature::CodeModeOnly);
    assert!(
        config.features.set(features).is_ok(),
        "set code-mode features"
    );
    config.code_mode.excluded_tool_namespaces = vec!["functions".to_string()];
    assert!(
        config
            .mcp_servers
            .set(HashMap::from([
                (GAME_SERVER_NAME.to_string(), game_server,)
            ]))
            .is_ok(),
        "set game MCP server"
    );
    assert!(
        config.web_search_mode.set(WebSearchMode::Disabled).is_ok(),
        "disable web search"
    );
    config.ephemeral = false;
    config.agents_enabled = false;
    config.project_doc_max_bytes = 0;
    config.include_permissions_instructions = false;
    config.include_apps_instructions = false;
    config.include_collaboration_mode_instructions = false;
    config.include_skill_instructions = false;
    config.include_environment_context = false;
    config.orchestrator_skills_enabled = false;
    config.orchestrator_mcp_enabled = false;
    config.experimental_request_user_input_enabled = false;
    config.update_plan_enabled = false;
}

fn exec_description(body: &Value) -> anyhow::Result<&str> {
    body["input"]
        .as_array()
        .and_then(|input| input.iter().find(|item| item["role"] == "developer"))
        .and_then(|developer| developer["tools"].as_array())
        .and_then(|namespaces| {
            namespaces
                .iter()
                .find(|namespace| namespace["name"] == "functions")
        })
        .and_then(|namespace| namespace["tools"].as_array())
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == "exec"))
        .and_then(|tool| tool["description"].as_str())
        .context("Sol code-mode exec description is missing")
}

async fn serve_fake_game_mcp(
    listener: UnixListener,
    spool_root: std::path::PathBuf,
) -> anyhow::Result<HelperTrace> {
    let (stream, _) = listener.accept().await?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut methods = Vec::new();

    let initialize = next_message(&mut lines).await?;
    methods.push(method(&initialize)?.to_string());
    respond(
        &mut writer,
        &initialize,
        json!({
            "protocolVersion": initialize["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake-game", "version": "1.0.0"},
        }),
    )
    .await?;
    let initialized = next_message(&mut lines).await?;
    methods.push(method(&initialized)?.to_string());
    let tools_list = next_message(&mut lines).await?;
    methods.push(method(&tools_list)?.to_string());
    respond(
        &mut writer,
        &tools_list,
        json!({"tools": [{
            "name": "get_app_state",
            "description": "Capture the current game state.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": true},
        }]}),
    )
    .await?;
    let tools_call = next_message(&mut lines).await?;
    assert_eq!(method(&tools_call)?, "tools/call");
    assert_eq!(tools_call["params"]["name"], "get_app_state");
    assert_eq!(tools_call["params"]["arguments"], json!({}));
    let metadata = &tools_call["params"]["_meta"];
    assert_eq!(metadata["epoch"], "test-epoch");
    assert_eq!(metadata["generation"], 1);
    assert_eq!(metadata["call_id"], metadata["callId"]);
    let call_id = metadata["callId"]
        .as_str()
        .context("Codex metadata has no callId")?
        .to_string();
    methods.push("tools/call:get_app_state".to_string());
    let blob_id = "00000000-0000-4000-8000-000000000001";
    let jpeg = BASE64_STANDARD.decode("/9j/2Q==")?;
    std::fs::create_dir_all(&spool_root)?;
    std::fs::write(spool_root.join(format!("{blob_id}.jpg")), &jpeg)?;
    respond(
        &mut writer,
        &tools_call,
        json!({
            "content": [{"type": "text", "text": "screenshot metadata"}],
            "structuredContent": {
                "app": "Gambonanza",
                "image_blob_id": blob_id,
                "image_bytes": jpeg.len(),
                "mime_type": "image/jpeg",
                "sha256": format!("{:x}", Sha256::digest(&jpeg)),
                "width": 2,
                "height": 2,
            },
            "isError": false,
        }),
    )
    .await?;
    Ok(HelperTrace { methods, call_id })
}

fn method(message: &Value) -> anyhow::Result<&str> {
    message["method"]
        .as_str()
        .context("MCP message has no method")
}

async fn next_message(
    lines: &mut tokio::io::Lines<BufReader<OwnedReadHalf>>,
) -> anyhow::Result<Value> {
    let line = tokio::time::timeout(SOCKET_TIMEOUT, lines.next_line())
        .await??
        .context("MCP client closed the socket")?;
    Ok(serde_json::from_str(&line)?)
}

async fn respond(
    writer: &mut OwnedWriteHalf,
    request: &Value,
    result: Value,
) -> anyhow::Result<()> {
    let mut encoded = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": result,
    }))?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

fn expected_model() -> ModelObservation {
    ModelObservation {
        visible_state_summary: "A combat board with a boss".to_string(),
        game_phase: "combat".to_string(),
        visible_objects: vec!["boss".to_string(), "player".to_string()],
        resources_and_choices: vec!["three energy".to_string()],
        uncertainties: vec!["boss intent icon is small".to_string()],
    }
}
