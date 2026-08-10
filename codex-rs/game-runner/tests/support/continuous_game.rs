use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use codex_core_api::Config;
use codex_core_api::McpServerConfig;
use codex_game_runner::CampaignTools;
use codex_game_runner::DecisionGate;
use codex_game_runner::GAME_SERVER_NAME;
use codex_game_runner::GENERATION;
use codex_game_runner::GameCallPolicy;
use codex_game_runner::RunnerRuntime;
use codex_game_runner::StrategyRecord;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;

use super::method;
use super::next_message;
use super::respond;
use super::write_spooled_jpeg;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedCall {
    Capture {
        jpeg: Vec<u8>,
    },
    Click {
        arguments: Value,
        action_sha256: String,
    },
}

pub struct ScriptedGame {
    pub calls: Vec<ExpectedCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationTrace {
    pub call_id: String,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationTrace {
    pub call_id: String,
    pub operation_id: String,
    pub action_sha256: String,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuousGameTrace {
    pub methods: Vec<String>,
    pub captures: Vec<ObservationTrace>,
    pub mutations: Vec<MutationTrace>,
}

pub struct PlannedClickStep {
    pub objective: String,
    pub visible_state_summary: String,
    pub x: i64,
    pub y: i64,
    pub expected_visible_result: String,
}

pub enum ScriptedOutcome {
    Loss {
        visible_evidence_summary: String,
        lesson: String,
        strategy: StrategyRecord,
    },
    Win {
        visible_evidence_summary: String,
        lesson: String,
    },
}

pub struct RunningContinuousCampaign {
    pub runtime: RunnerRuntime,
    pub gate: Arc<DecisionGate>,
    pub policy: Arc<GameCallPolicy>,
    pub helper_task: tokio::task::JoinHandle<anyhow::Result<ContinuousGameTrace>>,
    pub spool_root: PathBuf,
}

pub async fn start_runtime(
    base_config: &Config,
    temp: &TempDir,
    game: ScriptedGame,
) -> anyhow::Result<RunningContinuousCampaign> {
    let socket_path = temp.path().join("continuous-game.sock");
    let listener = UnixListener::bind(&socket_path)?;
    let spool_root = temp.path().join("screenshot-spool");
    let helper_task = tokio::spawn(serve(listener, spool_root.clone(), game));
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
        .context("set scripted game MCP server")?;
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
    .context("start continuous runner runtime")?;
    wait_for_mcp_server(&runtime.thread, GAME_SERVER_NAME)
        .await
        .context("wait for scripted game MCP server")?;
    Ok(RunningContinuousCampaign {
        runtime,
        gate,
        policy,
        helper_task,
        spool_root,
    })
}

pub fn turn_script(
    steps: &[PlannedClickStep],
    outcome: &ScriptedOutcome,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        !steps.is_empty(),
        "a scripted turn needs at least one action"
    );
    let mut script = String::new();
    for (offset, step) in steps.iter().enumerate() {
        let number = offset + 1;
        writeln!(
            script,
            "const before{number} = await tools.mcp__game__get_app_state({{}});"
        )?;
        writeln!(script, "await tools.game_runner__record_plan({{")?;
        writeln!(
            script,
            "  observation_reference: before{number}.structuredContent.artifact_uri,"
        )?;
        writeln!(
            script,
            "  objective: {},",
            serde_json::to_string(&step.objective)?
        )?;
        writeln!(
            script,
            "  visible_state_summary: {},",
            serde_json::to_string(&step.visible_state_summary)?
        )?;
        writeln!(
            script,
            "  candidates: [{{action: \"Advance\", predicted_visible_consequence: \"The next state appears\"}}, {{action: \"Wait\", predicted_visible_consequence: \"The current state remains\"}}],"
        )?;
        writeln!(
            script,
            "  chosen_action: {{tool: \"click\", arguments: {{x: {}, y: {}}}}},",
            step.x, step.y
        )?;
        writeln!(
            script,
            "  reason: \"Advance follows the fixture objective\","
        )?;
        writeln!(
            script,
            "  expected_visible_result: {},",
            serde_json::to_string(&step.expected_visible_result)?
        )?;
        writeln!(
            script,
            "  invalidation_condition: \"The visible state changes before the click\""
        )?;
        writeln!(script, "}});")?;
        writeln!(
            script,
            "await tools.mcp__game__click({{x: {}, y: {}}});",
            step.x, step.y
        )?;
    }
    writeln!(
        script,
        "const after = await tools.mcp__game__get_app_state({{}});"
    )?;
    writeln!(script, "await tools.game_runner__report_outcome({{")?;
    match outcome {
        ScriptedOutcome::Loss {
            visible_evidence_summary,
            lesson,
            strategy,
        } => {
            writeln!(script, "  outcome: \"loss\",")?;
            write_outcome_strings(&mut script, visible_evidence_summary, lesson)?;
            writeln!(script, "  strategy: {}", serde_json::to_string(strategy)?)?;
        }
        ScriptedOutcome::Win {
            visible_evidence_summary,
            lesson,
        } => {
            writeln!(script, "  outcome: \"win\",")?;
            write_outcome_strings(&mut script, visible_evidence_summary, lesson)?;
        }
    }
    writeln!(script, "}});")?;
    writeln!(script, "text(\"scripted campaign outcome recorded\");")?;
    Ok(script)
}

fn write_outcome_strings(
    script: &mut String,
    visible_evidence_summary: &str,
    lesson: &str,
) -> anyhow::Result<()> {
    writeln!(
        script,
        "  observation_reference: after.structuredContent.artifact_uri,"
    )?;
    writeln!(
        script,
        "  visible_evidence_summary: {},",
        serde_json::to_string(visible_evidence_summary)?
    )?;
    writeln!(script, "  lesson: {},", serde_json::to_string(lesson)?)?;
    Ok(())
}

async fn serve(
    listener: UnixListener,
    spool_root: PathBuf,
    game: ScriptedGame,
) -> anyhow::Result<ContinuousGameTrace> {
    let (stream, _) = listener.accept().await?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut trace = handshake(&mut lines, &mut writer).await?;
    let mut calls = VecDeque::from(game.calls);
    let mut capture_number = 0_u64;

    while let Some(expected) = calls.pop_front() {
        let call_number = trace.captures.len() + trace.mutations.len() + 1;
        let request = next_message(&mut lines)
            .await
            .with_context(|| format!("read scripted call {call_number}: {expected:?}"))?;
        let tool = request["params"]["name"]
            .as_str()
            .context("tool call has no name")?;
        let call_id = validate_call(&request, tool)?;
        trace.methods.push(format!("tools/call:{tool}"));
        match expected {
            ExpectedCall::Capture { jpeg } => {
                assert_eq!(tool, "get_app_state");
                capture_number = capture_number
                    .checked_add(1)
                    .context("capture counter overflow")?;
                let blob_id = format!("00000000-0000-4000-8000-{capture_number:012}");
                let sha256 = write_spooled_jpeg(&spool_root, &blob_id, &jpeg)?;
                respond_capture(&mut writer, &request, &blob_id, &jpeg, &sha256).await?;
                trace.captures.push(ObservationTrace {
                    call_id,
                    reference: format!("sha256:{sha256}"),
                });
            }
            ExpectedCall::Click {
                arguments,
                action_sha256,
            } => {
                record_mutation(
                    &mut trace,
                    &mut writer,
                    &request,
                    tool,
                    "click",
                    arguments,
                    action_sha256,
                    call_id,
                )
                .await?;
            }
        }
    }
    Ok(trace)
}

async fn handshake(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) -> anyhow::Result<ContinuousGameTrace> {
    let initialize = next_message(lines).await.context("read initialize")?;
    respond(
        writer,
        &initialize,
        json!({
            "protocolVersion": initialize["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "scripted-game", "version": "1.0.0"},
        }),
    )
    .await?;
    let initialized = next_message(lines)
        .await
        .context("read initialized notification")?;
    let tools_list = next_message(lines).await.context("read tools/list")?;
    respond(writer, &tools_list, json!({"tools": tool_specs()})).await?;
    Ok(ContinuousGameTrace {
        methods: vec![
            method(&initialize)?.to_string(),
            method(&initialized)?.to_string(),
            method(&tools_list)?.to_string(),
        ],
        captures: Vec::new(),
        mutations: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn record_mutation(
    trace: &mut ContinuousGameTrace,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    request: &Value,
    actual_tool: &str,
    expected_tool: &str,
    arguments: Value,
    action_sha256: String,
    call_id: String,
) -> anyhow::Result<()> {
    assert_eq!(actual_tool, expected_tool);
    assert_eq!(request["params"]["arguments"], arguments);
    let metadata = request["params"]["_meta"]
        .as_object()
        .context("mutation metadata must be an object")?;
    assert_eq!(metadata.get("call_id"), metadata.get("callId"));
    assert_eq!(metadata.get("operation_id"), metadata.get("callId"));
    assert_eq!(metadata.get("action_sha256"), Some(&json!(action_sha256)));
    let operation_id = metadata["operation_id"]
        .as_str()
        .context("mutation metadata has no operation_id")?
        .to_string();
    respond(
        writer,
        request,
        json!({
            "content": [{"type": "text", "text": "mutation complete"}],
            "structuredContent": {"mutated": true},
            "isError": false,
        }),
    )
    .await?;
    trace.mutations.push(MutationTrace {
        call_id,
        operation_id,
        action_sha256,
        tool: expected_tool.to_string(),
        arguments,
    });
    Ok(())
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
        mutation_spec(
            "click",
            json!({"x": {"type": "integer"}, "y": {"type": "integer"}}),
        ),
        mutation_spec(
            "drag",
            json!({
                "from_x": {"type": "integer"}, "from_y": {"type": "integer"},
                "to_x": {"type": "integer"}, "to_y": {"type": "integer"}
            }),
        ),
        mutation_spec(
            "focus_click",
            json!({"x": {"type": "integer"}, "y": {"type": "integer"}}),
        ),
    ]
}

fn mutation_spec(name: &str, properties: Value) -> Value {
    json!({
        "name": name,
        "description": format!("Perform a {name} mutation."),
        "inputSchema": {"type": "object", "properties": properties},
    })
}
