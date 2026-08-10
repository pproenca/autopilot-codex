use std::path::PathBuf;

use anyhow::Context;
use anyhow::bail;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;

use crate::support::method;
use crate::support::next_message;
use crate::support::respond;
use crate::support::write_spooled_jpeg;

pub const ACTION_SHA256: &str =
    "f709b60a7bbf91028aa10498db469d2a47fd96669d5167f6163200718809b3e1";

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
