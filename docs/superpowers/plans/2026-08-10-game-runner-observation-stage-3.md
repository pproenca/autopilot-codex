# Game Runner Observation Stage 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a dedicated headless runner that launches the existing signed helper and produces one evidence-linked, read-only GPT-5.6-Sol observation of the live game.

**Architecture:** A new `codex-game-runner` crate loads a locked-down Codex configuration through `codex-core-api`, launches `AutoPilotHelper.app` through LaunchServices, and runs one non-ephemeral thread. A runner-owned MCP policy supplies the helper lease and rejects mutations; an observation accumulator correlates the newest successful `get_app_state` event with schema-constrained model output and the canonical rollout.

**Tech Stack:** Rust 2024, Tokio, Clap, Serde/serde_json, thiserror, UUID, `codex-core-api`, `codex-stdio-to-uds`, mocked Responses SSE, canonical MCP JSON-RPC over Unix sockets.

## Global Constraints

- Keep GPT-5.6-Sol and reasoning effort `high` fixed in Stage 3; do not add runtime model or effort switches.
- The runner's only Codex-facing dependency is `codex-core-api`; narrowly re-export missing public types there instead of depending directly on `codex-core` or `codex-extension-api`.
- Reuse `codex-stdio-to-uds`; do not add a transport, gateway, or second MCP protocol.
- Use only the `game` MCP server and `get_app_state`, `wait`, and `zoom` tools.
- Deny `click`, `drag`, `focus_click`, and unknown game tools before MCP dispatch.
- Do not modify or import the signed helper, and never execute its bare Mach-O binary.
- The game must already be open and visible; the runner does not launch or reset it.
- Exclude shell, web, apps, skills, plugins, subagents, project instructions, and arbitrary MCP servers.
- Keep the thread non-ephemeral and retain the canonical rollout.
- Live execution is macOS-only; the crate and hermetic tests must compile on Linux, macOS, and Windows.
- Do not add code to `codex-core`; extend only the existing facade and the new crate.
- Add no new third-party dependency versions. All dependencies must already exist in `[workspace.dependencies]`.
- Keep new modules below 500 lines and the Stage 3 implementation diff below 800 changed lines. If the diff reaches 800 lines before the live slice is complete, stop at the last green task and split the remaining tasks into a follow-up change.
- Use `pretty_assertions::assert_eq` in tests and compare complete values where possible.
- Never run `cargo test`; use `just test` from `codex-rs`.
- After manifest changes run `just bazel-lock-update`; after implementation run scoped `just fix` and then `just fmt`, with no tests after fix or formatting.

---

## File Map

- `codex-rs/Cargo.toml`: register `game-runner` and its workspace dependency.
- `codex-rs/Cargo.lock`: record the new workspace package.
- `MODULE.bazel.lock`: refresh Bazel's Cargo dependency graph.
- `codex-rs/core-api/src/lib.rs`: re-export the existing configuration, MCP policy, MCP server, and reasoning types required by the runner.
- `codex-rs/game-runner/Cargo.toml`: define the library/binary and existing workspace dependencies.
- `codex-rs/game-runner/BUILD.bazel`: register the crate with the standard Bazel macro.
- `codex-rs/game-runner/src/lib.rs`: define the minimal public crate API and typed error surface.
- `codex-rs/game-runner/src/config.rs`: define deployment facts and build the locked-down Codex/MCP configuration.
- `codex-rs/game-runner/src/config_tests.rs`: verify the complete fixed configuration and model-visible tool surface inputs.
- `codex-rs/game-runner/src/helper.rs`: build the LaunchServices request and implement bounded socket readiness.
- `codex-rs/game-runner/src/helper_tests.rs`: verify the exact launch request and readiness outcomes without starting a real app.
- `codex-rs/game-runner/src/policy.rs`: attach owner metadata, reject unsafe tools, and retain bounded audit counters.
- `codex-rs/game-runner/src/policy_tests.rs`: verify complete policy decisions and counters.
- `codex-rs/game-runner/src/observation.rs`: submit the one turn, accumulate events, validate model output, correlate evidence, and cleanly shut down.
- `codex-rs/game-runner/src/observation_tests.rs`: exercise failure paths with synthetic events.
- `codex-rs/game-runner/tests/live_path.rs`: run the hermetic mocked-Responses plus fake-UDS vertical path.
- `codex-rs/game-runner/src/main.rs`: parse deployment paths, host the hidden byte bridge mode, construct runtime services, launch the helper, run the observation, and emit JSON.
- `codex-rs/game-runner/src/main_tests.rs`: verify the CLI accepts deployment facts and no behavioral switches.

---

### Task 1: Add the runner crate and locked-down configuration

**Files:**
- Modify: `codex-rs/Cargo.toml`
- Modify: `codex-rs/Cargo.lock`
- Modify: `MODULE.bazel.lock`
- Modify: `codex-rs/core-api/src/lib.rs`
- Create: `codex-rs/game-runner/Cargo.toml`
- Create: `codex-rs/game-runner/BUILD.bazel`
- Create: `codex-rs/game-runner/src/lib.rs`
- Create: `codex-rs/game-runner/src/config.rs`
- Create: `codex-rs/game-runner/src/config_tests.rs`

**Interfaces:**
- Consumes: existing `ConfigBuilder`, `ConfigOverrides`, `McpServerConfig`, `McpServerTransportConfig`, `AppToolApproval`, `ReasoningEffort`, `Feature`, and `Permissions` types through `codex-core-api`.
- Produces: `RunnerDeployment`, `RunnerError`, `load_runner_config(&RunnerDeployment, &Path) -> Result<Config, RunnerError>`, and constants `GAME_SERVER_NAME`, `MODEL`, and `GENERATION`.

- [ ] **Step 1: Create the manifest, Bazel target, test module, and failing configuration test**

Add `"game-runner"` to workspace members and
`codex-game-runner = { path = "game-runner" }` to workspace dependencies.
Create a manifest using only existing workspace dependencies:

```toml
[package]
name = "codex-game-runner"
version.workspace = true
edition.workspace = true
license.workspace = true

[lib]
name = "codex_game_runner"
path = "src/lib.rs"

[lints]
workspace = true

[dependencies]
anyhow = { workspace = true }
clap = { workspace = true, features = ["derive"] }
codex-core-api = { workspace = true }
codex-stdio-to-uds = { workspace = true }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["macros", "net", "process", "rt-multi-thread", "time"] }
tracing = { workspace = true }
uuid = { workspace = true, features = ["v4"] }

[dev-dependencies]
core_test_support = { workspace = true }
codex-utils-cargo-bin = { workspace = true }
pretty_assertions = { workspace = true }
tempfile = { workspace = true }
```

Create the standard Bazel target:

```starlark
load("//:defs.bzl", "codex_rust_crate")

codex_rust_crate(
    name = "game-runner",
    crate_name = "codex_game_runner",
)
```

Declare the sibling test module in `config.rs`:

```rust
#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
```

The first test must build a base config from a temporary Codex home, call
`load_runner_config`, and compare one complete projection rather than separate
field assertions:

```rust
#[derive(Debug, PartialEq, Eq)]
struct ConfigProjection {
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    ephemeral: bool,
    include_permissions_instructions: bool,
    include_apps_instructions: bool,
    include_collaboration_mode_instructions: bool,
    include_skill_instructions: bool,
    include_environment_context: bool,
    orchestrator_skills_enabled: bool,
    orchestrator_mcp_enabled: bool,
    agents_enabled: bool,
    request_user_input_enabled: bool,
    update_plan_enabled: bool,
    project_doc_max_bytes: usize,
    web_search_mode: WebSearchMode,
    code_mode_enabled: bool,
    code_mode_only_enabled: bool,
    excluded_code_mode_namespaces: Vec<String>,
    mcp_server_names: Vec<String>,
    game_tools: Option<Vec<String>>,
    game_required: bool,
    game_approval: Option<AppToolApproval>,
    game_command: String,
    game_args: Vec<String>,
}

#[tokio::test]
async fn runner_config_is_fixed_to_read_only_sol() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let deployment = RunnerDeployment {
        helper_app: temp.path().join("AutoPilotHelper.app"),
        socket_path: temp.path().join("game.sock"),
        target_app: "Gambonanza".to_string(),
        codex_home: temp.path().to_path_buf(),
    };
    let runner_executable = temp.path().join("codex-game-runner");
    let config = load_runner_config(&deployment, &runner_executable).await?;
    assert_eq!(project(&config), ConfigProjection {
        model: Some("gpt-5.6-sol".to_string()),
        reasoning_effort: Some(ReasoningEffort::High),
        ephemeral: false,
        include_permissions_instructions: false,
        include_apps_instructions: false,
        include_collaboration_mode_instructions: false,
        include_skill_instructions: false,
        include_environment_context: false,
        orchestrator_skills_enabled: false,
        orchestrator_mcp_enabled: false,
        agents_enabled: false,
        request_user_input_enabled: false,
        update_plan_enabled: false,
        project_doc_max_bytes: 0,
        web_search_mode: WebSearchMode::Disabled,
        code_mode_enabled: true,
        code_mode_only_enabled: true,
        excluded_code_mode_namespaces: vec!["functions".to_string()],
        mcp_server_names: vec!["game".to_string()],
        game_tools: Some(vec!["get_app_state".into(), "wait".into(), "zoom".into()]),
        game_required: true,
        game_approval: Some(AppToolApproval::Approve),
        game_command: runner_executable.display().to_string(),
        game_args: vec!["__stdio-to-uds".into(), temp.path().join("game.sock").display().to_string()],
    });
    Ok(())
}
```

- [ ] **Step 2: Run the test to verify the crate fails to compile**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner runner_config_is_fixed_to_read_only_sol
```

Expected: FAIL because `RunnerDeployment` and `load_runner_config` do not exist.

- [ ] **Step 3: Re-export the narrow existing API and implement the minimal fixed config**

Add these existing types to `core-api/src/lib.rs`:

```rust
pub use codex_config::AppToolApproval;
pub use codex_config::McpServerConfig;
pub use codex_config::McpServerTransportConfig;
pub use codex_core::config::ConfigBuilder;
pub use codex_core::config::ConfigOverrides;
pub use codex_protocol::openai_models::ReasoningEffort;
```

Define deployment facts without boolean or ambiguous optional parameters:

```rust
pub const GAME_SERVER_NAME: &str = "game";
pub const MODEL: &str = "gpt-5.6-sol";
pub const GENERATION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerDeployment {
    pub helper_app: PathBuf,
    pub socket_path: PathBuf,
    pub target_app: String,
    pub codex_home: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("codex-game-runner live execution requires macOS")]
    UnsupportedPlatform,
    #[error("helper app is not a readable app bundle: {path}", path = path.display())]
    InvalidHelperApp { path: PathBuf },
    #[error("LaunchServices could not start the signed helper")]
    LaunchServices {
        #[source]
        source: std::io::Error,
    },
    #[error("LaunchServices returned unsuccessful status {status}")]
    LaunchServicesExit { status: String },
    #[error("helper socket did not become ready: {path}", path = path.display())]
    SocketReadinessTimeout { path: PathBuf },
    #[error("failed to construct the fixed runner configuration")]
    Config {
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to start the observation thread")]
    ThreadStartup {
        #[source]
        source: anyhow::Error,
    },
    #[error("observation turn exceeded its deadline")]
    TurnTimeout,
    #[error("observation turn failed: {message}")]
    TurnFailed { message: String },
    #[error("the turn completed without a successful game/get_app_state call")]
    NoSuccessfulObservation,
    #[error("the model attempted {count} mutating game calls")]
    MutationAttempted { count: usize },
    #[error("the model attempted {count} unknown game tools")]
    UnknownGameToolAttempted { count: usize },
    #[error("{count} mutating game calls reached MCP dispatch")]
    MutationDispatched { count: usize },
    #[error("model observation report is invalid: {message}")]
    InvalidModelReport { message: String },
    #[error("non-ephemeral thread did not expose a rollout path")]
    MissingRolloutPath,
    #[error("run failed and cleanup also failed: {cleanup}")]
    RunAndCleanupFailed {
        #[source]
        primary: Box<RunnerError>,
        cleanup: String,
    },
}
```

`load_runner_config` must load the user's selected Codex home for auth and
storage, then overwrite the agent surface. The projection helper sorts MCP
server names before comparison. Set `model`,
`model_reasoning_effort`, `ephemeral`, all instruction inclusion flags,
orchestrator flags, `agents_enabled`, request-user-input/update-plan flags,
`project_doc_max_bytes`, `web_search_mode`, and the complete MCP server map.
Clear project-doc fallback filenames. Enable `Feature::CodeMode` and
`Feature::CodeModeOnly`, exclude the built-in `functions` namespace from code
mode, and leave `mcp__game` available. Build the MCP transport with the current
runner executable and the hidden bridge arguments:

```rust
let game_server = serde_json::from_value::<McpServerConfig>(serde_json::json!({
    "command": runner_executable,
    "args": ["__stdio-to-uds", deployment.socket_path],
    "enabled": true,
    "required": true,
    "supports_parallel_tool_calls": false,
    "default_tools_approval_mode": "approve",
    "enabled_tools": ["get_app_state", "wait", "zoom"],
    "startup_timeout_sec": 15,
    "tool_timeout_sec": 30,
}))?;
```

Use the existing validated deserializer rather than adding a one-caller
constructor to `codex-config`.

- [ ] **Step 4: Prove the fixed configuration and refresh dependency locks**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner runner_config_is_fixed_to_read_only_sol
rustup run 1.95.0 just test -p codex-core-api
cd ..
just bazel-lock-update
```

Expected: the focused test passes, `codex-core-api` compiles, and both lockfiles
contain the new workspace package without introducing a new registry version.

- [ ] **Step 5: Commit the crate foundation**

```bash
git add codex-rs/Cargo.toml codex-rs/Cargo.lock MODULE.bazel.lock \
  codex-rs/core-api/src/lib.rs codex-rs/game-runner
git commit -m "feat(game-runner): add fixed observation config"
```

---

### Task 2: Launch the signed helper and bound socket readiness

**Files:**
- Create: `codex-rs/game-runner/src/helper.rs`
- Create: `codex-rs/game-runner/src/helper_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Consumes: `RunnerDeployment` from Task 1.
- Produces: `HelperLauncher::new(ReadinessLimits)`, `HelperLauncher::launch_request(&RunnerDeployment) -> LaunchRequest`, and `HelperLauncher::ensure_serving(&RunnerDeployment) -> Result<(), RunnerError>`.

- [ ] **Step 1: Write failing tests for the exact LaunchServices request and readiness deadline**

Define self-documenting limits and compare the complete launch request:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessLimits {
    pub timeout: Duration,
    pub poll_interval: Duration,
}

#[test]
fn helper_launch_uses_signed_app_and_serve_socket() {
    let launcher = HelperLauncher::new(ReadinessLimits {
        timeout: Duration::from_secs(15),
        poll_interval: Duration::from_millis(100),
    });
    assert_eq!(launcher.launch_request(&deployment()), LaunchRequest {
        program: PathBuf::from("/usr/bin/open"),
        args: vec![
            "-n".into(), "-g".into(), "-j".into(),
            "/signed/AutoPilotHelper.app".into(),
            "--args".into(), "--serve".into(), "/private/game.sock".into(),
        ],
    });
}

#[tokio::test]
async fn missing_socket_reaches_bounded_timeout() {
    let launcher = HelperLauncher::new(ReadinessLimits {
        timeout: Duration::from_millis(40),
        poll_interval: Duration::from_millis(5),
    });
    let error = launcher
        .wait_for_socket(Path::new("/missing/game.sock"))
        .await
        .expect_err("missing socket should time out");
    assert!(matches!(
        error,
        RunnerError::SocketReadinessTimeout { path }
            if path == PathBuf::from("/missing/game.sock")
    ));
}
```

Also bind a temporary `tokio::net::UnixListener` and assert that
`wait_for_socket` returns `Ok(())`. Unix-only socket tests must use an explicit
`#[cfg(unix)]`; Windows retains compile coverage through the unsupported
runtime path.

- [ ] **Step 2: Run the helper tests to observe the red state**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner helper::tests
```

Expected: FAIL because the helper lifecycle types are absent.

- [ ] **Step 3: Implement LaunchServices activation and readiness**

`LaunchRequest` is a data-only value used by both production and tests. On
macOS, `ensure_serving` validates the `.app` directory, creates the socket
parent with owner-only permissions when missing, executes the request with
`tokio::process::Command`, rejects a nonzero status, and polls the socket with
`tokio::net::UnixStream::connect` until the deadline.

Use the production helper contract exactly:

```text
/usr/bin/open -n -g -j <AutoPilotHelper.app> --args --serve <socket-path>
```

On non-macOS targets return `RunnerError::UnsupportedPlatform` before spawning
a process. Do not terminate the helper during cleanup.

- [ ] **Step 4: Run all helper tests**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner helper::tests
```

Expected: all launch-request, connected-socket, timeout, invalid-app, and
unsupported-platform tests pass on their applicable targets.

- [ ] **Step 5: Commit the helper lifecycle**

```bash
git add codex-rs/game-runner/src/helper.rs \
  codex-rs/game-runner/src/helper_tests.rs codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): launch signed game helper"
```

---

### Task 3: Enforce the owner lease and read-only game policy

**Files:**
- Modify: `codex-rs/core-api/src/lib.rs`
- Create: `codex-rs/game-runner/src/policy.rs`
- Create: `codex-rs/game-runner/src/policy_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Consumes: the Stage 2 `McpToolCallPolicyContributor` contract and Task 1 constants.
- Produces: `GameCallPolicy::new(String, u64)`, `GameCallPolicy::lease() -> OwnerLease`, `GameCallPolicy::audit() -> PolicyAudit`, and a contributor implementation registered through `ExtensionRegistryBuilder<Config>`.

- [ ] **Step 1: Add failing complete-decision tests**

Re-export the four existing policy types from `codex-core-api`, then write tests
against the facade imports. The read-only test compares the complete decision:

```rust
pub use codex_extension_api::McpToolCallPolicyContributor;
pub use codex_extension_api::McpToolCallPolicyDecision;
pub use codex_extension_api::McpToolCallPolicyFuture;
pub use codex_extension_api::McpToolCallPolicyInput;
```

```rust
#[tokio::test]
async fn read_only_call_receives_exact_owner_lease() {
    let policy = GameCallPolicy::new("epoch-1".to_string(), 1);
    let request_meta = serde_json::Map::new();
    let arguments = json!({});
    let decision = policy.evaluate(McpToolCallPolicyInput {
        server_name: "game",
        tool_name: "get_app_state",
        call_id: "call-7",
        arguments: Some(&arguments),
        request_meta: &request_meta,
    }).await;
    assert_eq!(decision, McpToolCallPolicyDecision::Allow {
        additional_request_meta: json!({
            "epoch": "epoch-1",
            "generation": 1,
            "call_id": "call-7",
        }).as_object().expect("metadata fixture must be an object").clone(),
    });
}
```

Use a table over `click`, `drag`, `focus_click`, and `unexpected_tool` to assert
the exact denial and final audit snapshot:

```rust
assert_eq!(policy.audit(), PolicyAudit {
    mutation_attempts: 3,
    unknown_tool_attempts: 1,
    mutation_authorizations: 0,
});
```

Also prove a non-`game` server receives `Allow` with an empty metadata map so a
runner policy cannot alter generic MCP behavior.

- [ ] **Step 2: Run the policy tests to observe missing implementation**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner policy::tests
```

Expected: FAIL because `GameCallPolicy` is undefined.

- [ ] **Step 3: Implement the policy with bounded atomic counters**

Use atomics rather than a lock or event log:

```rust
pub struct GameCallPolicy {
    epoch: String,
    generation: u64,
    mutation_attempts: AtomicUsize,
    unknown_tool_attempts: AtomicUsize,
    mutation_authorizations: AtomicUsize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnerLease {
    pub epoch: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PolicyAudit {
    pub mutation_attempts: usize,
    pub unknown_tool_attempts: usize,
    pub mutation_authorizations: usize,
}
```

The exhaustive tool match is:

```rust
match input.tool_name {
    "get_app_state" | "wait" | "zoom" => allow_with_lease(input.call_id),
    "click" | "drag" | "focus_click" => deny_mutation(input.tool_name),
    _ => deny_unknown(input.tool_name),
}
```

Keep helper functions only when used by multiple match arms. Bound denial text
to the tool name plus a stable static reason; do not include arguments. The
authorization counter is deliberately separate from lifecycle events: increment
it only immediately before returning an allow decision for a mutating tool.
Stage 3 has no such match arm, so it must remain zero. This makes a policy
regression fail closed instead of misclassifying Codex's pre-policy MCP begin
event as a transport dispatch.

- [ ] **Step 4: Run policy and existing generic policy tests**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner policy::tests
rustup run 1.95.0 just test -p codex-core mcp_tool_call_policy
```

Expected: runner decisions pass and the empty/non-runner registry behavior is
unchanged.

- [ ] **Step 5: Commit the policy**

```bash
git add codex-rs/core-api/src/lib.rs codex-rs/game-runner/src/lib.rs \
  codex-rs/game-runner/src/policy.rs codex-rs/game-runner/src/policy_tests.rs
git commit -m "feat(game-runner): enforce read-only game lease"
```

---

### Task 4: Correlate one Sol observation with model output and rollout evidence

**Files:**
- Create: `codex-rs/game-runner/src/observation.rs`
- Create: `codex-rs/game-runner/src/observation_tests.rs`
- Create: `codex-rs/game-runner/tests/live_path.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Consumes: `CodexThread`, `EventMsg`, `Op::UserInput`, `SessionConfiguredEvent`, `GameCallPolicy`, and the fixed target application identity.
- Produces: `ObservationRun::new(ObservationLimits)`, `ObservationRun::execute(&CodexThread, &SessionConfiguredEvent, &GameCallPolicy, &str) -> Result<ObservationReport, RunnerError>`, `ModelObservation`, and `ObservationReport`.

- [ ] **Step 1: Write failing report and event-correlation tests**

Define bounded model output and the runner-owned envelope:

```rust
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelObservation {
    pub visible_state_summary: String,
    pub game_phase: String,
    pub visible_objects: Vec<String>,
    pub resources_and_choices: Vec<String>,
    pub uncertainties: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ObservationReport {
    pub thread_id: String,
    pub turn_id: String,
    pub observation_call_id: String,
    pub observation_reference: Option<String>,
    pub rollout_path: PathBuf,
    pub epoch: String,
    pub generation: u64,
    pub mutation_attempts: usize,
    pub mutation_dispatches: usize,
    pub model: ModelObservation,
}
```

Use synthetic `TurnStarted`, successful and failed `McpToolCallEnd`,
`McpToolCallBegin`, and `TurnComplete` events to prove:

- the newest successful `game/get_app_state` wins;
- `wait`, `zoom`, non-game events, and failed observations do not become evidence;
- a pre-policy mutation begin is not misreported as a dispatch, while the policy
  audit still fails the run for the attempted mutation;
- completion without a successful observation returns `NoSuccessfulObservation`;
- malformed, unknown-field, oversized, and overlong-list model JSON is rejected;
- a valid completion returns one complete `ObservationReport`.

- [ ] **Step 2: Run the observation tests to verify the red state**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner observation::tests
```

Expected: FAIL because the accumulator, schema, and report types are absent.

- [ ] **Step 3: Implement the bounded schema and accumulator**

Set hard limits in code:

```rust
const MAX_REPORT_BYTES: usize = 12 * 1024;
const MAX_FIELD_BYTES: usize = 2 * 1024;
const MAX_LIST_ITEMS: usize = 32;
```

`ObservationAccumulator` keeps only the turn ID, newest successful observation
call ID/reference, and final message. It does not keep
screenshots, raw event history, or tool outputs. Derive the optional reference
only from a short `structuredContent.observation_id` or artifact URI; never copy
inline image bytes into `ObservationReport`.

Pass a hand-written, closed JSON schema through
`Op::UserInput.final_output_json_schema`. The prompt must state the target app,
require at least one `get_app_state`, allow only `wait` and `zoom` for further
inspection, prohibit all physical actions, and request only visible evidence.
Use this stable prompt template, substituting only the target app:

```text
Observe the currently visible {target_app} game without changing it. Call
`mcp__game__get_app_state` at least once. You may call only
`mcp__game__wait` or `mcp__game__zoom` if another read-only view is needed.
Never click, drag, focus-click, or invoke any other tool. Report only visible
evidence using the required JSON schema; put anything uncertain in
`uncertainties`.
```

The schema sets `additionalProperties: false`, requires all five model fields,
caps every string at 2,048 characters, and caps every array at 32 strings. The
runtime byte checks remain authoritative because JSON Schema character limits
do not enforce UTF-8 byte size.

Wrap the event loop in `tokio::time::timeout`. On `TurnComplete`, reject
`event.error`, parse `last_agent_message`, validate all byte/item limits, merge
the policy lease and audit, and return the report. Set
`ObservationReport.mutation_dispatches` from
`PolicyAudit.mutation_authorizations`, not from MCP begin/end events: Codex emits
the begin event before policy evaluation, so neither lifecycle event proves the
helper received a request. Return
`MutationAttempted` when `mutation_attempts > 0`, `UnknownGameToolAttempted`
when `unknown_tool_attempts > 0`, and `MutationDispatched` when
`mutation_authorizations > 0`; none of these outcomes may emit a successful
report. The fake-helper method trace in the vertical test and the signed-helper
rollout trace in the live smoke are the end-to-end proof that zero mutating
requests crossed the transport boundary.

- [ ] **Step 4: Add the hermetic code-mode plus fake-UDS vertical test**

Build on the protocol sequence characterized by
`core/tests/suite/mcp_uds_bridge.rs` without copying its complete fake server.
Keep a minimal fixture in `game-runner/tests/live_path.rs` that implements only
`initialize`, `notifications/initialized`, `tools/list`, and one `tools/call`.

The mocked Responses sequence must:

1. Issue a code-mode `exec` call that invokes
   `tools.mcp__game__get_app_state({})`.
2. Return a fake observation with `observation_id = "obs-1"`.
3. Return a schema-valid JSON assistant message.

The fake helper must assert the exact flat `_meta` values and record every
method. The final deep equality assertion is:

```rust
assert_eq!(report, ObservationReport {
    thread_id: fixture.codex.thread_id.to_string(),
    turn_id: "turn-1".to_string(),
    observation_call_id: "game-observation-1".to_string(),
    observation_reference: Some("obs-1".to_string()),
    rollout_path: expected_rollout,
    epoch: "test-epoch".to_string(),
    generation: 1,
    mutation_attempts: 0,
    mutation_dispatches: 0,
    model: expected_model_observation(),
});
```

Assert the helper method list contains only initialization, listing, and one
`tools/call`; no mutation name may appear. Also inspect the captured Responses
request and assert that the model-visible code-mode executor description
contains `mcp__game__get_app_state` and contains none of `exec_command`,
`apply_patch`, web, apps, subagents, or project instructions.

- [ ] **Step 5: Run observation and vertical tests**

Build the existing helper binaries required by the Cargo integration fixture,
then test the runner:

```bash
cd codex-rs
rustup run 1.95.0 cargo build -p codex-cli --bin codex
rustup run 1.95.0 cargo build -p codex-code-mode-host
rustup run 1.95.0 cargo build -p codex-rmcp-client \
  --bin test_stdio_server --bin test_streamable_http_server
rustup run 1.95.0 just test -p codex-game-runner
```

Expected: all model-output, evidence-correlation, denial, cleanup, and vertical
tests pass without launching the real helper or contacting OpenAI.

- [ ] **Step 6: Commit observation correlation**

```bash
git add codex-rs/game-runner/src/lib.rs \
  codex-rs/game-runner/src/observation.rs \
  codex-rs/game-runner/src/observation_tests.rs \
  codex-rs/game-runner/tests/live_path.rs
git commit -m "feat(game-runner): correlate live game observation"
```

---

### Task 5: Wire the CLI, verify the workspace boundary, and run the real smoke

**Files:**
- Modify: `codex-rs/game-runner/Cargo.toml`
- Create: `codex-rs/game-runner/src/main.rs`
- Create: `codex-rs/game-runner/src/main_tests.rs`
- Modify only if verification exposes a Stage 3 defect: files introduced in Tasks 1-4.

**Interfaces:**
- Consumes: all Stage 3 library interfaces plus the existing `ThreadManager` startup pattern from `thread-manager-sample`.
- Produces: the `codex-game-runner` executable and hidden `__stdio-to-uds <socket>` bridge mode.

- [ ] **Step 1: Write the CLI parse test before the binary implementation**

Keep parsing in a small `Args` type and compare the complete value:

```rust
#[derive(Debug, Parser, PartialEq, Eq)]
#[command(name = "codex-game-runner")]
struct Args {
    #[arg(long, value_name = "APP_BUNDLE")]
    helper_app: PathBuf,

    #[arg(long, value_name = "SOCKET")]
    socket: PathBuf,

    #[arg(long, value_name = "APP_NAME")]
    target_app: String,
}

#[test]
fn parses_only_deployment_facts() {
    assert_eq!(Args::try_parse_from([
        "codex-game-runner",
        "--helper-app", "/signed/AutoPilotHelper.app",
        "--socket", "/private/game.sock",
        "--target-app", "Gambonanza",
    ]).expect("valid deployment arguments"), Args {
        helper_app: PathBuf::from("/signed/AutoPilotHelper.app"),
        socket: PathBuf::from("/private/game.sock"),
        target_app: "Gambonanza".to_string(),
    });
}
```

There are no model, effort, prompt, tool, or policy flags.

Add the binary target to `Cargo.toml` in this task:

```toml
[[bin]]
name = "codex-game-runner"
path = "src/main.rs"
```

Declare `main_tests.rs` from `main.rs` with the repository's required sibling
test-module pattern:

```rust
#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
```

- [ ] **Step 2: Run the CLI test to verify the red state**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner parses_only_deployment_facts
```

Expected: FAIL because `main.rs` and `Args` are absent.

- [ ] **Step 3: Implement the hidden bridge mode and production runtime**

Before normal Clap parsing, recognize exactly two internal arguments and run
the existing bridge library:

```rust
if let [mode, socket] = std::env::args_os().skip(1).collect::<Vec<_>>().as_slice()
    && mode == "__stdio-to-uds"
{
    return codex_stdio_to_uds::run(Path::new(socket)).await;
}
```

Reject any extra hidden-mode argument. For normal execution:

```rust
struct NoUserInstructions;

impl UserInstructionsProvider for NoUserInstructions {
    fn load_user_instructions(&self) -> LoadUserInstructionsFuture<'_> {
        Box::pin(async { LoadedUserInstructions::default() })
    }
}
```

1. Resolve the current executable and Codex home.
2. Construct `RunnerDeployment` and the fixed config.
3. Call `HelperLauncher::ensure_serving`.
4. Register one `Arc<GameCallPolicy>` with `ExtensionRegistryBuilder<Config>`.
5. Construct `ThreadManager` using the same auth, models-manager,
   environment-manager, state DB, thread store, and installation ID setup as
   `thread-manager-sample`. Pass `Arc::new(NoUserInstructions)` rather than the
   Codex-home provider so global or project instructions cannot enter the game
   thread. Do not install image generation or any other extension.
6. Start one thread with `SessionSource::Custom("game_runner".to_string())`.
7. Run `ObservationRun::execute` with a five-minute turn deadline.
8. Always call `shutdown_and_wait` and remove the thread from the manager.
9. Serialize exactly one `ObservationReport` JSON object to stdout.

Use `set_default_originator("codex_game_runner".to_string())` and attach cleanup
failures as context without replacing the primary run error.

- [ ] **Step 4: Run changed-project and regression suites**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner
rustup run 1.95.0 just test -p codex-core-api
rustup run 1.95.0 just test -p codex-core mcp_uds_bridge
rustup run 1.95.0 just test -p codex-core mcp_tool_call_policy
rustup run 1.95.0 just test -p codex-rmcp-client
```

Expected: every Stage 3 and existing generic transport/policy test passes.

- [ ] **Step 5: Commit the CLI**

```bash
git add codex-rs/game-runner/Cargo.toml codex-rs/game-runner/src/main.rs \
  codex-rs/game-runner/src/main_tests.rs
git commit -m "feat(game-runner): add headless observation command"
```

- [ ] **Step 6: Check scope and change size before the live call**

```bash
git diff --check HEAD~4
git diff --stat HEAD~4
git diff HEAD~4 -- codex-rs/core codex-rs/stdio-to-uds codex-rs/uds \
  codex-rs/config codex-rs/app-server-protocol
```

Expected: no production changes under those generic crates, except the planned
`core-api` facade exports outside this path list. Confirm no helper source,
schema, app-server API, new transport, or third-party version changed. If the
total implementation diff exceeds 800 lines, do not run the live smoke; split
at the most recent green commit and review the remaining slice separately.

- [ ] **Step 7: Ask before the workspace-wide suite**

Ask the user for approval to run:

```bash
cd codex-rs
rustup run 1.95.0 just test
```

Record the complete summary. Isolate-retry unrelated loopback or timing
failures; do not modify unrelated crates to make environment-sensitive tests
green.

- [ ] **Step 8: Run scoped fixes and formatting last**

```bash
cd codex-rs
rustup run 1.95.0 just fix -p codex-game-runner
rustup run 1.95.0 just fix -p codex-core-api
rustup run 1.95.0 just fmt
```

Do not rerun tests after `fix` or `fmt`. Inspect `git status --short` and
`git diff --check`; commit any intentional changes:

```bash
git add codex-rs/core-api codex-rs/game-runner codex-rs/Cargo.toml \
  codex-rs/Cargo.lock MODULE.bazel.lock
git commit -m "chore(game-runner): finish observation slice"
```

Skip this commit if fix and formatting make no changes.

- [ ] **Step 9: Prepare and run the signed-helper live smoke**

First build the existing helper in its source repository using its supported
packaging path:

```bash
cd /Users/pedroproenca/Documents/Projects/auto-pilot/Packages/CUCtl
swift build --product AutoPilotHelper
./Scripts/make-app.sh
```

With Gambonanza already open and visible, run from the Codex fork:

```bash
cd /Users/pedroproenca/Documents/Projects/codex/codex-rs
mkdir -p /tmp/autopilot-codex-game-runner
chmod 700 /tmp/autopilot-codex-game-runner
rustup run 1.95.0 cargo run -p codex-game-runner -- \
  --helper-app /Users/pedroproenca/Documents/Projects/auto-pilot/Packages/CUCtl/.build/AutoPilotHelper.app \
  --socket /tmp/autopilot-codex-game-runner/AutoPilotHelper.sock \
  --target-app Gambonanza
```

Expected: LaunchServices starts the signed helper, Sol calls
`game/get_app_state`, stdout contains one valid report tied to the latest call
and rollout, the description visibly matches the screen, and both mutation
counters are zero. Preserve the report and rollout path in the handoff; do not
advance to Stage 4 if the live result is absent or visually wrong.

Report the red/green evidence per task, commit hashes, final diff size, targeted
and workspace test results, the signed-helper command, report/rollout identity,
visible correctness judgment, accepted owner metadata, and zero mutation
counters. State explicitly that Stage 4 remains blocked until the live smoke is
successful.
