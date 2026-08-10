use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use anyhow::bail;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_core_api::AskForApproval;
use codex_core_api::Config;
use codex_core_api::Constrained;
use codex_core_api::Feature;
use codex_core_api::Features;
use codex_core_api::McpServerConfig;
use codex_core_api::PermissionProfile;
use codex_core_api::Permissions;
use codex_core_api::WebSearchMode;
use codex_game_runner::CampaignTools;
use codex_game_runner::DecisionGate;
use codex_game_runner::GAME_SERVER_NAME;
use codex_game_runner::GENERATION;
use codex_game_runner::GameCallPolicy;
use codex_game_runner::RunnerRuntime;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;

use crate::support::method;
use crate::support::next_message;
use crate::support::respond;
use crate::support::write_spooled_jpeg;

pub const ACTION_SHA256: &str = "f709b60a7bbf91028aa10498db469d2a47fd96669d5167f6163200718809b3e1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeGameScenario {
    NoMutation,
    WinningMutation,
    FailedAfterCapture,
}

#[derive(Debug, PartialEq, Eq)]
pub struct HelperTrace {
    pub methods: Vec<String>,
    pub before_call_id: String,
    pub mutation_call_id: Option<String>,
    pub after_call_id: Option<String>,
    pub before_reference: String,
    pub after_reference: Option<String>,
}

pub struct RunningCampaign {
    pub runtime: RunnerRuntime,
    pub gate: Arc<DecisionGate>,
    pub policy: Arc<GameCallPolicy>,
    pub helper_task: tokio::task::JoinHandle<anyhow::Result<HelperTrace>>,
    pub spool_root: PathBuf,
}

pub async fn start_runtime(
    base_config: &Config,
    temp: &TempDir,
    scenario: FakeGameScenario,
) -> anyhow::Result<RunningCampaign> {
    let socket_path = temp.path().join("game.sock");
    let listener = UnixListener::bind(&socket_path)?;
    let spool_root = temp.path().join("screenshot-spool");
    let helper_task = tokio::spawn(serve_fake_game_mcp(listener, spool_root.clone(), scenario));
    let game_runner_bin = codex_utils_cargo_bin::cargo_bin("codex-game-runner")?;
    let code_mode_host_bin = codex_utils_cargo_bin::cargo_bin("codex-code-mode-host")?;
    let game_server = serde_json::from_value::<McpServerConfig>(json!({
        "command": game_runner_bin,
        "args": ["__stdio-to-uds", socket_path],
        "required": true,
        "supports_parallel_tool_calls": false,
        "default_tools_approval_mode": "approve",
        "enabled_tools": ["get_app_state", "wait", "click", "drag", "focus_click"],
        "startup_timeout_sec": 15,
        "tool_timeout_sec": 5,
    }))?;
    let mut config = base_config.clone();
    config.codex_self_exe = Some(game_runner_bin);
    config
        .mcp_servers
        .set(HashMap::from([(GAME_SERVER_NAME.to_string(), game_server)]))
        .context("set fake game MCP server")?;
    let gate = Arc::new(DecisionGate::new(GENERATION));
    let policy = Arc::new(GameCallPolicy::new(
        "test-epoch".to_string(),
        GENERATION,
        Arc::clone(&gate),
    ));
    let runtime = RunnerRuntime::start_with_code_mode_host(
        config,
        Arc::clone(&policy),
        CampaignTools::specs(),
        code_mode_host_bin,
    )
    .await
    .context("start runner runtime")?;
    wait_for_mcp_server(&runtime.thread, GAME_SERVER_NAME)
        .await
        .context("wait for fake game MCP server")?;
    Ok(RunningCampaign {
        runtime,
        gate,
        policy,
        helper_task,
        spool_root,
    })
}

pub fn configure_runner_surface(config: &mut Config) {
    let Ok(permissions) = Permissions::from_approval_and_profile(
        Constrained::allow_any(AskForApproval::Never),
        Constrained::allow_any(PermissionProfile::read_only()),
    ) else {
        unreachable!("unconstrained runner test permissions must be valid");
    };
    config.permissions = permissions;
    let mut features = Features::default();
    features.enable(Feature::CodeMode);
    features.enable(Feature::CodeModeHost);
    features.enable(Feature::CodeModeOnly);
    assert!(config.features.set(features).is_ok());
    config.code_mode.excluded_tool_namespaces = vec!["functions".to_string()];
    assert!(config.web_search_mode.set(WebSearchMode::Disabled).is_ok());
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

pub async fn serve_fake_game_mcp(
    listener: UnixListener,
    spool_root: PathBuf,
    scenario: FakeGameScenario,
) -> anyhow::Result<HelperTrace> {
    let (stream, _) = listener.accept().await?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut methods = Vec::new();

    let initialize = next_message(&mut lines).await.context("read initialize")?;
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
    let initialized = next_message(&mut lines)
        .await
        .context("read initialized notification")?;
    methods.push(method(&initialized)?.to_string());
    let tools_list = next_message(&mut lines).await.context("read tools/list")?;
    methods.push(method(&tools_list)?.to_string());
    respond(&mut writer, &tools_list, json!({"tools": tool_specs()})).await?;

    let mut before_call_id = None;
    let mut mutation_call_id = None;
    let mut after_call_id = None;
    let mut before_reference = None;
    let mut after_reference = None;
    while let Some(line) = lines.next_line().await? {
        let request = serde_json::from_str::<Value>(&line)?;
        let tool = request["params"]["name"]
            .as_str()
            .context("tool call has no name")?;
        match tool {
            "get_app_state" if before_call_id.is_none() => {
                let call_id = validate_call(&request, tool)?;
                methods.push("tools/call:get_app_state".to_string());
                let jpeg = BASE64_STANDARD.decode("/9j/2Q==")?;
                let blob_id = "00000000-0000-4000-8000-000000000001";
                let sha256 = write_spooled_jpeg(&spool_root, blob_id, &jpeg)?;
                respond_capture(&mut writer, &request, blob_id, &jpeg, &sha256).await?;
                before_call_id = Some(call_id);
                before_reference = Some(format!("sha256:{sha256}"));
            }
            "click" => {
                if scenario == FakeGameScenario::NoMutation {
                    bail!("mismatched mutation reached the fake helper");
                }
                if mutation_call_id.is_some() {
                    bail!("more than one mutation reached the fake helper");
                }
                let call_id = validate_call(&request, tool)?;
                assert_eq!(request["params"]["arguments"], json!({"x": 180, "y": 640}));
                assert_click_metadata(&request)?;
                methods.push("tools/call:click".to_string());
                respond(
                    &mut writer,
                    &request,
                    json!({
                        "content": [{"type": "text", "text": "clicked"}],
                        "structuredContent": {"clicked": true},
                        "isError": false,
                    }),
                )
                .await?;
                mutation_call_id = Some(call_id);
            }
            "get_app_state" => {
                if after_call_id.is_some() {
                    bail!("more than two captures reached the fake helper");
                }
                let call_id = validate_call(&request, tool)?;
                methods.push("tools/call:get_app_state".to_string());
                match scenario {
                    FakeGameScenario::WinningMutation => {
                        let jpeg = BASE64_STANDARD.decode("/9j/2g==")?;
                        let blob_id = "00000000-0000-4000-8000-000000000002";
                        let sha256 = write_spooled_jpeg(&spool_root, blob_id, &jpeg)?;
                        respond_capture(&mut writer, &request, blob_id, &jpeg, &sha256).await?;
                        after_reference = Some(format!("sha256:{sha256}"));
                    }
                    FakeGameScenario::FailedAfterCapture => {
                        respond(
                            &mut writer,
                            &request,
                            json!({
                                "content": [{"type": "text", "text": "capture failed"}],
                                "isError": true,
                            }),
                        )
                        .await?;
                    }
                    FakeGameScenario::NoMutation => {
                        bail!("unexpected second capture without a mutation");
                    }
                }
                after_call_id = Some(call_id);
            }
            unexpected => bail!("unexpected fake game tool call: {unexpected}"),
        }
    }

    Ok(HelperTrace {
        methods,
        before_call_id: before_call_id.context("missing before capture")?,
        mutation_call_id,
        after_call_id,
        before_reference: before_reference.context("missing before reference")?,
        after_reference,
    })
}

fn validate_call(request: &Value, expected_tool: &str) -> anyhow::Result<String> {
    assert_eq!(method(request)?, "tools/call");
    assert_eq!(request["params"]["name"], expected_tool);
    assert_eq!(request["params"]["_meta"]["epoch"], "test-epoch");
    assert_eq!(request["params"]["_meta"]["generation"], 1);
    request["params"]["_meta"]["callId"]
        .as_str()
        .map(str::to_string)
        .context("Codex metadata has no callId")
}

fn assert_click_metadata(request: &Value) -> anyhow::Result<()> {
    let metadata = request["params"]["_meta"]
        .as_object()
        .context("click metadata must be an object")?;
    let expected_metadata = json!({
        "action_sha256": ACTION_SHA256,
        "callId": metadata.get("callId"),
        "call_id": metadata.get("call_id"),
        "epoch": "test-epoch",
        "generation": 1,
        "operation_id": metadata.get("operation_id"),
        "threadId": metadata.get("threadId"),
        "x-codex-turn-metadata": metadata.get("x-codex-turn-metadata"),
    });
    let mut runner_metadata = metadata.clone();
    runner_metadata.remove("progressToken");
    assert_eq!(Value::Object(runner_metadata), expected_metadata);
    assert_eq!(metadata.get("call_id"), metadata.get("callId"));
    assert_eq!(metadata.get("operation_id"), metadata.get("callId"));
    Ok(())
}

async fn respond_capture(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    request: &Value,
    blob_id: &str,
    jpeg: &[u8],
    sha256: &str,
) -> anyhow::Result<()> {
    respond(
        writer,
        request,
        json!({
            "content": [{"type": "text", "text": "screenshot metadata"}],
            "structuredContent": {
                "app": "Gambonanza",
                "image_blob_id": blob_id,
                "image_bytes": jpeg.len(),
                "mime_type": "image/jpeg",
                "sha256": sha256,
                "width": 1051,
                "height": 820,
            },
            "isError": false,
        }),
    )
    .await
}

fn tool_specs() -> Vec<Value> {
    vec![
        json!({
            "name": "get_app_state",
            "description": "Capture the current game state.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": true},
        }),
        json!({
            "name": "wait",
            "description": "Wait without changing the game.",
            "inputSchema": {"type": "object", "properties": {"seconds": {"type": "number"}}},
            "annotations": {"readOnlyHint": true},
        }),
        json!({
            "name": "click",
            "description": "Click the game.",
            "inputSchema": {
                "type": "object",
                "properties": {"x": {"type": "integer"}, "y": {"type": "integer"}},
                "required": ["x", "y"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": "drag",
            "description": "Drag in the game.",
            "inputSchema": {"type": "object", "properties": {}},
        }),
        json!({
            "name": "focus_click",
            "description": "Focus and click the game.",
            "inputSchema": {"type": "object", "properties": {}},
        }),
    ]
}
