# Game MCP Bridge Stage 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove that an otherwise unmodified Codex thread can discover and call the existing AutoPilot game helper through Codex's existing stdio MCP transport and `stdio-to-uds` bridge.

**Architecture:** Keep the signed Swift helper as the canonical MCP server on its owner-only socket. Configure Codex's normal stdio MCP client to launch the current `codex stdio-to-uds <socket>` hidden command. Add one Unix-only `codex-core` integration test that crosses the complete Responses-to-MCP path with a fake line-framed MCP socket server, then perform a read-only smoke test against the real signed helper and Gambonanza.

**Tech Stack:** Rust, Tokio Unix sockets, JSON-RPC/MCP, `core_test_support`, Wiremock Responses fixtures, existing `codex-stdio-to-uds`, existing signed Swift `AutoPilotHelper.app`, GPT-5.6-Sol.

**Global Constraints:** Do not add a new MCP transport, config variant, crate, dependency, schema, or production-code path. Do not modify the helper in this stage. Keep the automated test hermetic, Unix-gated, bounded by timeouts, and compatible with Cargo and Bazel. Use `just test`, never `cargo test`. Because this touches `codex-core` tests, ask the user before the final workspace-wide `just test`. Run `just fix -p codex-core`, then `just fmt`, and do not rerun tests after those formatting/fix commands. The real-helper smoke test is read-only and must call only `get_app_state`.

---

## File Map

- Modify: `codex-rs/core/tests/suite/mod.rs`
  - Register the new Unix-only integration-test module.
- Create: `codex-rs/core/tests/suite/mcp_uds_bridge.rs`
  - Own the fake canonical MCP socket server and the end-to-end characterization test.
- Reuse unchanged: `codex-rs/stdio-to-uds/src/lib.rs`
  - Relay raw bytes between stdio and the socket.
- Reuse unchanged: `codex-rs/cli/src/main.rs`
  - Expose the hidden `codex stdio-to-uds <socket-path>` command used by MCP configuration.
- Reuse unchanged: `../auto-pilot/Packages/CUCtl/.build/AutoPilotHelper.app`
  - Provide the signed, LaunchServices-started real game MCP for the manual acceptance check.

## Behavior Under Test

The single integration test must prove this complete sequence:

```text
mock Responses API
  -> Codex thread includes mcp__game__get_app_state in the code-mode exec declaration
  -> model emits an exec call that invokes tools.mcp__game__get_app_state
  -> Codex stdio MCP launcher starts `codex stdio-to-uds <socket>`
  -> bridge relays canonical MCP initialize/list/call messages
  -> fake socket helper returns structured observation
  -> code mode emits the structured observation in the next Responses request
  -> turn completes
```

This code-mode path is required for GPT-5.6-Sol. Do not model the game tool as
a direct top-level Responses function; reuse Codex's generated JavaScript MCP
binding and existing code-mode host.

The fake helper must support only the protocol needed by this test:

- `initialize`
- `notifications/initialized`
- `tools/list`
- `tools/call` for `get_app_state`

Any other request is a test failure. Each socket read gets a five-second timeout so a protocol regression fails with a useful error instead of hanging.

### Task 1: Add the full-path MCP bridge characterization test

**Files:**

- Modify: `codex-rs/core/tests/suite/mod.rs`
- Create: `codex-rs/core/tests/suite/mcp_uds_bridge.rs`

- [ ] **Step 1: Register the missing test module to establish the red state**

Add this near the other MCP suite modules in `codex-rs/core/tests/suite/mod.rs`:

```rust
#[cfg(unix)]
mod mcp_uds_bridge;
```

Run:

```bash
cd codex-rs
just test -p codex-core mcp_uds_bridge
```

Expected: FAIL at compile time because `tests/suite/mcp_uds_bridge.rs` does not exist yet. This confirms the intended test target is wired into the real `codex-core` integration suite.

- [ ] **Step 2: Create a bounded fake canonical MCP socket server**

Create `codex-rs/core/tests/suite/mcp_uds_bridge.rs` with Unix-only test support. Keep protocol helpers private to the test module. The implementation should follow this shape:

```rust
use std::time::Duration;

use anyhow::Context;
use codex_config::types::McpServerConfig;
use codex_protocol::models::PermissionProfile;
use core_test_support::responses;
use core_test_support::responses::mount_sse_once;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;

const SERVER_NAME: &str = "game";
const CALL_ID: &str = "game-observation-1";
const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);

async fn write_message(
    writer: &mut OwnedWriteHalf,
    message: &Value,
) -> anyhow::Result<()> {
    let mut encoded = serde_json::to_vec(message)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

async fn next_message(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
) -> anyhow::Result<Value> {
    let line = tokio::time::timeout(SOCKET_TIMEOUT, lines.next_line())
        .await
        .context("timed out waiting for an MCP message")??
        .context("MCP client closed the socket")?;
    serde_json::from_str(&line).context("failed to parse MCP message")
}
```

Implement `serve_fake_game_mcp(listener) -> anyhow::Result<Vec<String>>` as a four-state protocol loop:

1. Accept one socket connection.
2. Read `initialize`, record its method, and answer with JSON-RPC `2.0`, the same request ID, the requested `protocolVersion`, `capabilities: {"tools": {}}`, and stable `serverInfo`.
3. Read and record `notifications/initialized`; it has no response.
4. Read `tools/list`, then return exactly one tool:

```json
{
  "name": "get_app_state",
  "description": "Capture the current game state.",
  "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
  "annotations": {"readOnlyHint": true}
}
```

5. Read `tools/call`; deep-compare its params to:

```json
{"name": "get_app_state", "arguments": {}}
```

6. Return a successful MCP call result with both text content and the exact structured content:

```json
{
  "content": [{"type": "text", "text": "Captured observation obs-1."}],
  "structuredContent": {"app": "Gambonanza", "observation_id": "obs-1"},
  "isError": false
}
```

7. Return the complete method trace and allow the connection to close.

Use exhaustive string matching for the four expected methods. For every JSON-RPC response, copy the request's `id`; do not assume numeric IDs. Use `pretty_assertions::assert_eq` for the complete call params and final method trace.

- [ ] **Step 3: Drive the fake helper through a real Codex thread**

Add the test below the helpers:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_calls_game_tool_through_stdio_to_uds() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

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
const result = await tools.mcp__game__get_app_state({});
text(JSON.stringify(result.structuredContent));
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
        "startup_timeout_sec": 5,
        "tool_timeout_sec": 5,
    }))?;
    let fixture = test_codex()
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
    assert!(
        first_request
            .body_json()
            .to_string()
            .contains("mcp__game__get_app_state")
    );
    let completion_request = completion_response.single_request();
    let output = completion_request.custom_tool_call_output(CALL_ID);
    assert!(output.to_string().contains("Gambonanza"));
    assert!(output.to_string().contains("obs-1"));

    assert_eq!(
        helper_task.await.context("fake game MCP task panicked")??,
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
```

Treat this as a shape, not text to paste blindly: use the repository's actual current APIs and imports if signatures differ. Preserve the assertions and full path.

- [ ] **Step 4: Run the focused test and fix only Stage 1 defects**

Run:

```bash
cd codex-rs
just test -p codex-core mcp_uds_bridge
```

Expected: PASS with exactly the new bridge test selected. If it fails, diagnose at the boundary indicated by the failure—Responses advertisement, bridge startup, MCP handshake, call arguments, or continuation output. Do not introduce a new transport or edit production code to work around a test-fixture mistake.

- [ ] **Step 5: Run the crate test suite**

Run:

```bash
cd codex-rs
just test -p codex-core
```

Expected: PASS.

- [ ] **Step 6: Commit the characterization test**

Run:

```bash
git add codex-rs/core/tests/suite/mod.rs codex-rs/core/tests/suite/mcp_uds_bridge.rs
git commit -m "test(core): cover MCP calls through UDS bridge"
```

Expected: one focused test-only commit. Confirm `git show --stat --oneline HEAD` contains only the two listed files.

### Task 2: Prove the bridge against the signed AutoPilot helper

**Files:** None. This is a read-only runtime acceptance check; keep probe output in a temporary directory and do not commit local paths, screenshots, or credentials.

- [ ] **Step 1: Verify the game, signed helper, and Codex binary prerequisites**

The Gambonanza game must be running. Use the existing helper app so its TCC identity and permissions remain stable:

```bash
cd /Users/pedroproenca/Documents/Projects/codex
test -d ../auto-pilot/Packages/CUCtl/.build/AutoPilotHelper.app
codesign --verify --deep --strict ../auto-pilot/Packages/CUCtl/.build/AutoPilotHelper.app
cd codex-rs
cargo build -p codex-cli --bin codex
```

Expected: the helper signature verifies and `target/debug/codex` builds. If the helper app is absent or its signature is invalid, stop this smoke check and restore it through AutoPilot's existing stable-certificate build flow; do not launch an ad-hoc or bare helper executable.

- [ ] **Step 2: Launch the helper through LaunchServices on an isolated socket**

Use task-specific variables and a bounded readiness loop:

```bash
cd /Users/pedroproenca/Documents/Projects/codex
GAME_PROBE_DIR="$(mktemp -d /tmp/codex-game-mcp.XXXXXX)"
GAME_SOCKET="$GAME_PROBE_DIR/helper.sock"
GAME_HELPER="$(pwd)/../auto-pilot/Packages/CUCtl/.build/AutoPilotHelper.app"
GAME_CODEX_BIN="$(pwd)/codex-rs/target/debug/codex"
open -g -j "$GAME_HELPER" --args --serve "$GAME_SOCKET"
for attempt in {1..100}; do
  if test -S "$GAME_SOCKET"; then
    break
  fi
  sleep 0.1
done
test -S "$GAME_SOCKET"
```

Expected: the socket exists within ten seconds. Do not execute `AutoPilotHelper.app/Contents/MacOS/AutoPilotHelper` directly; that changes the permission identity.

- [ ] **Step 3: Run one GPT-5.6-Sol read-only observation turn**

Run from the repository root, using only CLI config overrides:

```bash
"$GAME_CODEX_BIN" \
  -c "mcp_servers.game.command=\"$GAME_CODEX_BIN\"" \
  -c "mcp_servers.game.args=[\"stdio-to-uds\",\"$GAME_SOCKET\"]" \
  -c 'mcp_servers.game.required=true' \
  -c 'mcp_servers.game.enabled_tools=["get_app_state"]' \
  exec \
  --ignore-user-config \
  --model gpt-5.6-sol \
  --sandbox read-only \
  --skip-git-repo-check \
  --json \
  'Call the game get_app_state tool exactly once. Report only whether a fresh Gambonanza observation was returned; do not click, drag, focus, wait, zoom, or use any other tool.' \
  | tee "$GAME_PROBE_DIR/codex.jsonl"
```

Expected acceptance evidence in `codex.jsonl`:

- The `game` MCP server starts successfully.
- Codex invokes `get_app_state` exactly once.
- The tool result identifies Gambonanza and contains a fresh observation or screenshot reference.
- No mutating game tool is advertised or called.
- The turn exits successfully without AutoPilot's Elixir control plane or HTTP gateway running.

If the call fails because of macOS Screen Recording or Accessibility permission, preserve the signed helper identity and repair the existing TCC grant. Do not bypass the constraint through accessibility APIs, game internals, logs, browser automation, or a different capture path.

- [ ] **Step 4: Remove only the temporary probe artifacts**

After manually inspecting `codex.jsonl`, remove the exact temporary directory created above. Resolve and print it first:

```bash
test -n "$GAME_PROBE_DIR"
case "$GAME_PROBE_DIR" in
  /tmp/codex-game-mcp.*) printf '%s\n' "$GAME_PROBE_DIR" ;;
  *) exit 1 ;;
esac
rm -r "$GAME_PROBE_DIR"
```

Expected: only the temporary socket and JSONL probe output are removed. The helper app, game data, repository, and TCC grants are untouched. The launched helper may remain resident; do not use a broad `pkill`.

### Task 3: Final Stage 1 verification and handoff

**Files:** No new files expected.

- [ ] **Step 1: Review the diff and scope**

Run:

```bash
git status --short
git diff HEAD^ -- codex-rs/core/tests/suite/mod.rs codex-rs/core/tests/suite/mcp_uds_bridge.rs
```

Expected: only the Unix-gated test registration and the focused integration test are present. Confirm there are no changes to `codex-core` production code, MCP configuration types, schemas, Cargo dependencies, Bazel metadata, or the AutoPilot helper.

- [ ] **Step 2: Ask before running the complete workspace suite**

Because `codex-core` changed, the repository instructions require the complete suite but also require explicit user approval first. Ask the user for approval to run:

```bash
cd codex-rs
just test
```

If approved, run it and require PASS. If not approved, record that the focused `codex-core` suite passed and that the workspace suite was not run by user choice.

- [ ] **Step 3: Run lint fixes and formatting last**

Run:

```bash
cd codex-rs
just fix -p codex-core
just fmt
```

Expected: both commands succeed. Per repository instructions, do not rerun tests after `fix` or `fmt`. If either command changes files after the test commit, inspect the changes and create a second narrowly named cleanup commit.

- [ ] **Step 4: Declare Stage 1 complete only with both kinds of evidence**

Stage 1 is complete when all of the following are true:

- The hermetic `codex-core` integration test passes through the actual hidden CLI bridge.
- `just test -p codex-core` passes.
- The real signed helper returns a fresh Gambonanza observation through the same bridge.
- The helper was launched through LaunchServices.
- No AutoPilot HTTP/Elixir control plane participated.
- The source diff remains test-only and focused.

Do not start the runner crate, planning gate, TUI, campaign loop, or code deletion in this stage. Write a fresh Stage 2 plan for the headless persistent-thread vertical slice after this seam is proven.
