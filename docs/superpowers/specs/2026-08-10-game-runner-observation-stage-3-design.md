# Game Runner Observation Stage 3 Design

## Summary

Stage 3 adds the smallest production-shaped `codex-game-runner` slice that can
launch the existing signed game helper, connect through the proven MCP bridge,
and ask GPT-5.6-Sol to describe the live game without changing it. The slice
exists to prove the real perception path before campaign persistence,
automatic continuation, planning enforcement, gameplay actions, or a TUI are
introduced.

The implementation is a dedicated workspace crate rather than a mode in
`codex-thread-manager-sample` or a client of the app server. It uses
`codex-core-api` as its only Codex-facing dependency and preserves the normal
Codex thread, rollout, model, code-mode, and MCP machinery.

## Goals

- Start or activate the existing signed `AutoPilotHelper.app` through macOS
  LaunchServices.
- Connect to the helper's owner-only Unix socket through
  `codex-stdio-to-uds`.
- Run one non-ephemeral GPT-5.6-Sol/high thread with a fixed, game-only
  configuration.
- Obtain at least one real `game/get_app_state` result and a bounded,
  schema-valid model description of the visible game state.
- Attach the helper owner lease to every game call through the Stage 2 MCP
  call policy seam.
- Prove that no physical mutation can reach the helper in this stage.
- Retain the canonical Codex rollout and enough runner-owned trace data to
  compare the model report with its source observation.

## Non-goals

- Launching, resetting, or controlling the game outside MCP.
- Clicking, dragging, focus recovery, or any other physical mutation.
- A persistent campaign, automatic continuation, loss recovery, strategic
  memory, `record_plan`, or `report_outcome`.
- Pause, Resume, Stop, or a terminal UI.
- Importing, modifying, or packaging `GameControlHelper.app`.
- Removing Codex crates or products.
- General shell, web, apps, skills, plugins, subagents, project instructions,
  or arbitrary MCP servers.

## Chosen Approach

Add a dedicated `codex-game-runner` crate with a small library and thin CLI.
This creates the product boundary needed by later campaign and TUI stages
without converting an example into production machinery or adding an app
server subprocess and JSON-RPC control layer.

The alternatives are rejected for this stage:

- Extending `codex-thread-manager-sample` would be initially smaller but would
  mix example and product responsibilities and require an immediate extraction
  in Stage 4.
- Driving `codex app-server` would add process supervision and a second
  control protocol around facilities already available through
  `codex-core-api`.

## Crate Boundary

`codex-rs/game-runner` contains a library plus the `codex-game-runner` binary.
The library owns four focused components:

1. `RunnerConfig` builds the fixed Codex and deployment configuration.
2. `HelperLauncher` activates the signed helper and waits for socket readiness.
3. `GameCallPolicy` supplies owner metadata and enforces read-only game access.
4. `ObservationRun` owns the one-thread, one-turn observation lifecycle.

The binary parses deployment paths, calls the library, prints one final JSON
report on stdout, sends concise progress and errors to stderr, and maps typed
failures to a nonzero exit. It contains no orchestration logic.

`codex-core-api` remains the runner's only Codex-facing dependency. If the
runner needs extension-registry policy types that are not currently available,
`codex-core-api` narrowly re-exports the existing public types. The runner does
not reach into private `codex-core` modules and does not add game concepts to
`codex-core`.

Modules should remain focused and comfortably below the repository's module
size limits. No helper method is introduced solely to hide one call site.

## Fixed Runner Configuration

The runner accepts only deployment facts needed for the live smoke:

- the signed helper app path;
- the helper Unix socket path; and
- the target game application identity.

The model is fixed to GPT-5.6-Sol and reasoning effort is fixed to `high`.
They are not runtime experiment switches in Stage 3. The runner reuses the
user's existing Codex authentication and storage location, marks the thread
non-ephemeral, and assigns a game-runner-specific session source or originator
so its rollout is identifiable.

The constructed Codex configuration excludes project instructions and the
general product surface. It enables only the code-mode machinery required by
Sol and one MCP server named `game`. That server launches the existing
`codex-stdio-to-uds` bridge against the configured socket and exposes only:

- `get_app_state`
- `wait`
- `zoom`

The fixed MCP server and tool allowlists are the unattended approval boundary.
Shell, web search, apps, skills, plugins, subagents, and any other MCP server
remain unavailable.

## Signed Helper Lifecycle

`HelperLauncher` is macOS-specific at runtime but has a cross-platform API so
the crate continues to compile on Linux and Windows. A non-macOS execution
returns `unsupported platform` before starting a Codex thread.

On macOS the launcher:

1. Validates that the configured path identifies an app bundle.
2. Requests activation through LaunchServices, never by executing the app's
   bare helper binary.
3. Polls for the configured Unix socket until a bounded readiness deadline.
4. Verifies that the socket can be reached by the MCP bridge during normal
   Codex server startup.

The runner does not claim to distinguish a newly launched helper from an
already resident instance. On shutdown it closes the Codex thread and bridge
connection but leaves the signed helper resident. Connection teardown releases
the live connection authority; a later campaign begins with a new epoch.

The game itself must already be open and visibly renderable. Failure to locate
or capture it is reported through the helper's normal MCP result and becomes a
typed observation failure. The runner does not launch or reset the game.

## Owner Lease and Read-only Policy

At run start the runner generates a fresh random epoch and uses generation
`1`. Its registered MCP call policy applies only when `server_name == "game"`.
For every allowed game call it adds three flat request metadata fields:

- `epoch`: the fresh run epoch;
- `generation`: the integer `1`; and
- `call_id`: the exact Codex call ID received by the policy.

The existing collision rules remain authoritative. A preexisting or
contributor-owned field cannot be overwritten, and any collision rejects the
call before dispatch.

The policy permits `get_app_state`, `wait`, and `zoom`. It explicitly denies
`click`, `drag`, and `focus_click`, as well as any unknown game tool, before
the helper sees the call. This denial remains in place even if an unexpected
catalog response advertises a mutating tool. A mutation attempt is a failed
Stage 3 run, not a recoverable model mistake.

## Observation Lifecycle

The successful flow is:

1. Validate platform, deployment paths, and Codex authentication inputs.
2. Activate the signed helper and wait for the socket readiness deadline.
3. Create the fresh owner epoch and the game-only extension registry.
4. Construct a `ThreadManager` through the same `codex-core-api` boundary used
   by `codex-thread-manager-sample`.
5. Start one non-ephemeral GPT-5.6-Sol/high thread.
6. Submit one stable prompt that asks Sol to inspect the live game without
   acting, use `get_app_state` at least once, optionally use `wait` or `zoom`,
   and return a bounded structured description.
7. Stream Codex events until the turn completes or a bounded turn deadline is
   reached.
8. Correlate the final model payload with the newest successful
   `get_app_state` call observed by the runner.
9. Emit the verified report, shut down the thread and bridge, and preserve the
   canonical rollout.

The model-facing output schema contains:

- a concise visible-state summary;
- the detected screen or game phase;
- bounded lists of relevant visible objects, resources, and choices; and
- bounded uncertainties that require another observation.

The runner does not ask the model to reproduce transport identifiers it may
not see. Instead, the final emitted `ObservationReport` is an envelope that
combines the schema-validated model payload with runner-owned evidence:

- thread and turn identifiers;
- the successful observation call ID;
- the source observation's artifact or image reference when present;
- the rollout location or stable rollout identity;
- the number of attempted and dispatched mutations; and
- the run epoch and generation.

The report can therefore state authoritatively that no mutation was attempted
or dispatched without relying on a model assertion.

## Failure Handling and Cleanup

The runner exposes typed failures for:

- unsupported platform;
- invalid helper app or socket configuration;
- LaunchServices activation failure;
- socket readiness timeout;
- Codex authentication or model initialization failure;
- MCP startup or discovery failure;
- missing, invalid, or colliding ownership metadata;
- helper capture or target-game failure;
- no successful `get_app_state` call;
- attempted mutation;
- invalid or oversized model report;
- turn deadline expiry; and
- rollout or shutdown failure.

Helper readiness polling is bounded. Model and network retries remain owned by
Codex and retain their existing limits. The runner does not implement another
retry controller.

After a thread exists, every exit path requests interruption when necessary,
waits for thread shutdown within the existing Codex lifecycle, removes the
in-memory thread from the manager, and retains the rollout. Cleanup never
retries, replays, or synthesizes a physical action. The primary run failure is
preserved if cleanup also fails, with cleanup detail attached as context.

## Automated Verification

The implementation includes focused tests for:

- fixed model, effort, instruction, capability, MCP server, and tool settings;
- exact owner metadata and collision rejection;
- read-only tool allowlisting and denial of every mutation and unknown tool;
- cross-platform unsupported behavior;
- the LaunchServices command/request specification without launching a real
  app in tests;
- bounded socket readiness and timeout behavior;
- rejection when no successful observation occurs;
- rejection of invalid or oversized model output;
- mutation-attempt reporting; and
- cleanup after startup, MCP, model, and report failures.

A hermetic vertical integration test uses the existing mocked Responses
infrastructure plus the fake MCP/UDS helper. It proves that:

1. Sol's code-mode path invokes `game/get_app_state`.
2. The helper receives the exact `epoch`, integer `generation`, and Codex
   `call_id`.
3. The helper observation returns to the model.
4. The model returns a schema-valid bounded description.
5. No mutation reaches the helper.
6. The runner emits a correlated report and shuts down cleanly.

Tests do not activate the real helper, require macOS permissions, contact the
live model, or require the game. The crate and its non-live tests remain
cross-platform. Existing bridge, MCP policy, RMCP client, code-mode, and core
tests continue to cover the generic no-runner paths.

## Live Smoke and Completion Gate

Stage 3 is complete only after the automated suites pass and one manual live
smoke on macOS records all of the following:

- LaunchServices starts or activates the existing signed helper.
- The production stdio-to-UDS bridge connects to its owner-only socket.
- GPT-5.6-Sol/high successfully invokes `game/get_app_state`.
- The final model description visibly matches the current game screen.
- The emitted report and canonical rollout identify the source observation.
- The owner metadata is accepted by the production helper.
- Zero mutations are attempted and zero mutations are dispatched.
- The thread and bridge shut down without losing the rollout.

The live smoke is intentionally not a default automated test. It requires an
authenticated Codex installation, the signed helper with existing Screen
Recording and Accessibility grants, and the game already open and visible.
Failure of this smoke blocks Stage 4 even when every automated test passes.

## Stage Boundary

Stage 3 ends with a trustworthy read-only perception path. Stage 4 may then
reuse this crate and add the persistent campaign, automatic continuation,
dynamic planning and outcome tools, plan consumption before mutations,
strategy state, losses, and fake-game victory coverage.

No TUI or physical game action should be added until this Stage 3 completion
gate is satisfied.
