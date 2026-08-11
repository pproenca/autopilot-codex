use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use anyhow::ensure;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::sync::Notify;

use super::method;
use super::respond;
use super::write_spooled_jpeg;

#[derive(Clone)]
pub enum ResponseTiming {
    Immediate,
    Held(Arc<Notify>),
    Disconnect,
}

#[derive(Clone)]
pub enum ExpectedCall {
    Capture {
        jpeg: Vec<u8>,
        timing: ResponseTiming,
    },
    Mutation {
        tool: String,
        arguments: Value,
        action_sha256: String,
        timing: ResponseTiming,
    },
}

pub struct ConnectionScript {
    pub generation: u64,
    pub calls: Vec<ExpectedCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameCallTrace {
    pub connection: usize,
    pub method: String,
    pub generation: u64,
    pub operation_id: Option<String>,
    pub action_sha256: Option<String>,
    pub arguments: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DurableGameTrace {
    pub connections: Vec<u64>,
    pub calls: Vec<GameCallTrace>,
    pub duplicate_operation_ids: Vec<String>,
}

pub struct RunningDurableGame {
    pub socket_path: PathBuf,
    pub spool_root: PathBuf,
    pub trace: Arc<Mutex<DurableGameTrace>>,
    pub task: tokio::task::JoinHandle<anyhow::Result<DurableGameTrace>>,
}

impl RunningDurableGame {
    pub async fn wait_for_calls(&self, count: usize) -> anyhow::Result<DurableGameTrace> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot = self
                    .trace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if snapshot.calls.len() >= count {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("timed out waiting for durable helper calls")
    }
}

pub fn capture(jpeg: Vec<u8>) -> ExpectedCall {
    ExpectedCall::Capture {
        jpeg,
        timing: ResponseTiming::Immediate,
    }
}

pub fn click(x: i64, y: i64, action_sha256: &str) -> ExpectedCall {
    ExpectedCall::Mutation {
        tool: "click".to_string(),
        arguments: json!({"x": x, "y": y}),
        action_sha256: action_sha256.to_string(),
        timing: ResponseTiming::Immediate,
    }
}

pub fn start(temp: &TempDir, scripts: Vec<ConnectionScript>) -> anyhow::Result<RunningDurableGame> {
    let socket_path = temp.path().join("durable-game.sock");
    let listener = UnixListener::bind(&socket_path)?;
    let spool_root = temp.path().join("screenshot-spool");
    let trace = Arc::new(Mutex::new(DurableGameTrace::default()));
    let task = tokio::spawn(serve(
        listener,
        spool_root.clone(),
        scripts,
        Arc::clone(&trace),
    ));
    Ok(RunningDurableGame {
        socket_path,
        spool_root,
        trace,
        task,
    })
}

async fn serve(
    listener: UnixListener,
    spool_root: PathBuf,
    scripts: Vec<ConnectionScript>,
    trace: Arc<Mutex<DurableGameTrace>>,
) -> anyhow::Result<DurableGameTrace> {
    let mut scripts = VecDeque::from(scripts);
    let mut operation_ids = HashSet::new();
    let mut capture_number = 0_u64;
    let mut connection_number = 0_usize;
    while let Some(script) = scripts.front() {
        let (stream, _) = listener.accept().await?;
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        let Some(line) = lines.next_line().await? else {
            continue;
        };
        let initialize: Value = serde_json::from_str(&line)?;
        if method(&initialize)? != "initialize" {
            anyhow::bail!("first helper message was not initialize: {initialize}");
        }
        connection_number += 1;
        let generation = script.generation;
        handshake(&mut lines, &mut writer, &initialize).await?;
        trace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .connections
            .push(generation);
        let script = scripts.pop_front().context("missing connection script")?;
        let mut disconnected = false;
        for expected in script.calls {
            let request = next_call(&mut lines).await?;
            let timing = expected.timing().clone();
            let call = validate_call(connection_number, generation, &request, &expected)?;
            let duplicate = call
                .operation_id
                .as_ref()
                .is_some_and(|operation_id| !operation_ids.insert(operation_id.clone()));
            {
                let mut shared = trace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if duplicate {
                    shared.duplicate_operation_ids.push(
                        call.operation_id
                            .clone()
                            .context("duplicate mutation omitted operation id")?,
                    );
                }
                shared.calls.push(call);
            }
            if duplicate {
                respond(
                    &mut writer,
                    &request,
                    json!({
                        "content": [{"type": "text", "text": "duplicate operation rejected"}],
                        "isError": true,
                    }),
                )
                .await?;
                continue;
            }
            match timing {
                ResponseTiming::Immediate => {}
                ResponseTiming::Held(release) => release.notified().await,
                ResponseTiming::Disconnect => {
                    disconnected = true;
                    break;
                }
            }
            match expected {
                ExpectedCall::Capture { jpeg, .. } => {
                    capture_number += 1;
                    respond_capture(
                        &mut writer,
                        &request,
                        &spool_root,
                        capture_number,
                        &jpeg,
                    )
                    .await?;
                }
                ExpectedCall::Mutation { .. } => {
                    respond(
                        &mut writer,
                        &request,
                        json!({
                            "content": [{"type": "text", "text": "mutation complete"}],
                            "structuredContent": {"mutated": true},
                            "isError": false,
                        }),
                    )
                    .await?;
                }
            }
        }
        if !disconnected {
            while lines.next_line().await?.is_some() {}
        }
    }
    Ok(trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone())
}

impl ExpectedCall {
    fn timing(&self) -> &ResponseTiming {
        match self {
            Self::Capture { timing, .. } | Self::Mutation { timing, .. } => timing,
        }
    }
}

async fn handshake(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    initialize: &Value,
) -> anyhow::Result<()> {
    respond(
        writer,
        initialize,
        json!({
            "protocolVersion": initialize["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "durable-game", "version": "1.0.0"},
        }),
    )
    .await?;
    let initialized = next_value(lines).await?;
    assert_eq!(method(&initialized)?, "notifications/initialized");
    let tools_list = next_value(lines).await?;
    assert_eq!(method(&tools_list)?, "tools/list");
    respond(writer, &tools_list, json!({"tools": tool_specs()})).await
}

async fn next_call(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
) -> anyhow::Result<Value> {
    let request = next_value(lines).await?;
    ensure!(method(&request)? == "tools/call", "expected tools/call");
    Ok(request)
}

async fn next_value(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
) -> anyhow::Result<Value> {
    let line = lines
        .next_line()
        .await?
        .context("MCP client closed the durable helper socket")?;
    Ok(serde_json::from_str(&line)?)
}

fn validate_call(
    connection: usize,
    generation: u64,
    request: &Value,
    expected: &ExpectedCall,
) -> anyhow::Result<GameCallTrace> {
    let (expected_tool, expected_arguments, expected_hash) = match expected {
        ExpectedCall::Capture { .. } => ("get_app_state", json!({}), None),
        ExpectedCall::Mutation {
            tool,
            arguments,
            action_sha256,
            ..
        } => (tool.as_str(), arguments.clone(), Some(action_sha256.as_str())),
    };
    assert_eq!(request["params"]["name"], expected_tool);
    assert_eq!(request["params"]["arguments"], expected_arguments);
    assert_eq!(request["params"]["_meta"]["generation"], generation);
    let metadata = &request["params"]["_meta"];
    let operation_id = metadata["operation_id"].as_str().map(str::to_string);
    let action_sha256 = metadata["action_sha256"].as_str().map(str::to_string);
    if let Some(expected_hash) = expected_hash {
        assert_eq!(metadata["call_id"], metadata["callId"]);
        assert_eq!(metadata["operation_id"], metadata["callId"]);
        assert_eq!(action_sha256.as_deref(), Some(expected_hash));
    } else {
        assert_eq!(operation_id, None);
        assert_eq!(action_sha256, None);
    }
    Ok(GameCallTrace {
        connection,
        method: expected_tool.to_string(),
        generation,
        operation_id,
        action_sha256,
        arguments: expected_arguments,
    })
}

async fn respond_capture(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    request: &Value,
    spool_root: &std::path::Path,
    capture_number: u64,
    jpeg: &[u8],
) -> anyhow::Result<()> {
    let blob_id = format!("00000000-0000-4000-8000-{capture_number:012}");
    let sha256 = write_spooled_jpeg(spool_root, &blob_id, jpeg)?;
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
        mutation_spec("click", json!({"x": {"type": "integer"}, "y": {"type": "integer"}})),
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
