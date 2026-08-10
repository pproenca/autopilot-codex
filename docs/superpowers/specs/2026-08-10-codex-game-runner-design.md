# Codex Game Runner Design

## Summary

Build a single-purpose autonomous game runner inside the Codex fork. It uses
GPT-5.6-Sol, the proven AutoPilot computer-use MCP, and Codex's existing model
integration to play one difficult turn-based macOS game until it wins.

The product is a terminal UI with Start, Pause, Resume, and Stop controls. It
shows observations, concise plans, actions, verified effects, losses, strategy
updates, and infrastructure failures. It is not a general coding agent or an
automation platform.

The runner must optimize for real-game wins. Automated correctness is
necessary, but a passing test suite is not evidence that the product works.

## Goals

- Play autonomously through repeated losses until GPT-5.6-Sol visually
  verifies a full-game victory.
- Preserve the current CUCtl exact-window capture and input implementation.
- Require a fresh, structured plan before every mutating game action.
- Carry strategy and confirmed game knowledge across attempts and Codex
  context compactions.
- Let an operator monitor the campaign and Start, Pause, Resume, or Stop it.
- Reuse Codex's model, session, context, MCP, retry, rollout, and authentication
  machinery instead of recreating an agent harness.
- Keep every new state store and model-context injection explicitly bounded.

## Non-goals

- A general-purpose Codex interface.
- Chat, code editing, shell execution, web search, skills, plugins, subagents,
  approvals, or arbitrary MCP servers.
- An Elixir control plane, Swift operator UI, FlightDeck protocol, event bus,
  journal database, experience database, embeddings, or semantic search.
- Automated interpretation of game rules outside the model.
- Access to game files, logs, accessibility data, network traffic, guides,
  external rules, or human gameplay help.
- A runtime model router, experiment flags, or multiple gameplay policies in
  one revision.

## System Architecture

### `codex-game-runner`

A new Rust binary in `codex-rs` owns:

- the terminal UI;
- the small campaign state machine;
- one persistent Codex thread;
- automatic continuation until verified victory;
- the plan gate;
- the bounded campaign strategy record;
- outcome handling; and
- projection of Codex and MCP events into operator-visible activity.

The binary depends on `codex-core-api` as its only Codex-facing dependency,
following the existing `codex-thread-manager-sample` boundary. Missing public
capabilities are exposed narrowly through `codex-core-api`; the runner does not
reach into private `codex-core` modules.

### `GameControlHelper.app`

Extract the current `../auto-pilot/Packages/CUCtl` package into the new product
without first rewriting its computer-use implementation. Preserve:

- exact-window screen capture;
- target-window resolution;
- fresh-observation gating;
- click and focus-click behavior;
- atomic drag and held-button cleanup;
- operation IDs and durable at-most-once replay;
- owner-only Unix socket access;
- screenshot encoding and zoom support; and
- stable signed macOS identity for Screen Recording and Accessibility grants.

The helper continues to launch through LaunchServices as a signed app. The
runner never launches its bare executable because doing so changes the TCC
identity.

### Existing stdio-to-UDS MCP bridge

Reuse the existing `codex-stdio-to-uds` byte bridge and cross-platform
`codex-uds` stream. Codex launches the bridge through its existing stdio MCP
transport, and the bridge connects to the helper's canonical MCP JSON-RPC
socket. The bridge forwards bytes without interpreting MCP, so it eliminates
AutoPilot's Elixir HTTP gateway without adding a second protocol or a new
global MCP transport configuration.

For GPT-5.6-Sol, Codex exposes the discovered game tools inside its existing
code-mode `exec` tool. Model-authored JavaScript calls bindings such as
`tools.mcp__game__get_app_state(...)`; Codex then performs the normal MCP call
and returns the result to the code-mode continuation. The runner should reuse
this model-visible surface instead of adding direct Responses tools or a
Sol-specific dispatcher.

The game MCP surface initially remains limited to:

- `get_app_state`
- `click`
- `drag`
- `focus_click`
- `wait`
- `zoom`

The runner exposes its two local tools through Codex's existing dynamic-tool
mechanism rather than adding them to the Swift helper.

### MCP call policy extension

The helper's canonical MCP server requires three flat `_meta` fields on every
tool call: `epoch`, `generation`, and `call_id`. AutoPilot previously created
them inside its campaign and gateway layers. Codex already provides a unique
call ID and thread identity, but it must not globally synthesize AutoPilot
metadata for unrelated MCP servers.

Add one host-only extension point through Codex's existing extension registry.
An MCP call policy contributor receives the server name, tool name, Codex call
ID, read-only arguments, and the current request metadata. It may either deny
the call with a model-visible reason or add new request metadata fields. Added
fields cannot overwrite metadata already owned by Codex or another
contributor. With no contributor registered, MCP behavior is byte-for-byte
unchanged.

Codex evaluates contributors in registration order after approval and argument
normalization, inside the prepared call's catalog authority, immediately
before dispatch. This placement lets the later plan gate atomically consume a
plan before a mutation without moving MCP lifecycle ownership out of Codex.
Contributor failures reject the call; they never fall through to the helper.

The game runner registers the only initial policy. It applies solely to the
configured `game` server and adds:

- a random `epoch` persisted for the active campaign;
- a nonnegative `generation`, incremented before Resume and after helper
  reconnection;
- `call_id`, copied exactly from Codex's call ID.

Pause blocks new game calls before interrupting the active turn. Stop closes
the thread and starts the next campaign with a new epoch. The runner configures
the game server's approval mode as `approve`; its fixed server and tool
allow-lists plus the in-process policy are the unattended safety boundary.
The stdio-to-UDS bridge remains a byte-transparent transport.

## Codex Reuse Boundary

Reuse the following current Codex facilities:

- `ThreadManager`, `CodexThread`, and `EventMsg` for thread lifecycle and
  streaming;
- `Op::UserInput` for initial and continuation turns;
- `Op::Interrupt` for cooperative Pause and Stop;
- `AuthManager`, built-in OpenAI providers, and the models manager;
- Responses streaming, retry behavior, persisted reasoning, and token usage;
- native thread storage, rollout JSONL, resume, and compaction;
- `McpConnectionManager` for server startup, discovery, cancellation, calls,
  and results;
- dynamic tools and their persisted rollout representation;
- existing terminal lifecycle, image rendering, wrapping, and ratatui styling
  patterns.

Do not depend on the general Codex TUI application state or composer. Reuse or
extract focused presentation helpers when that is smaller than importing the
full TUI product.

The one new core integration point is the narrow extension-registry MCP call
policy described above. The runner first uses it for helper ownership metadata,
then extends the same game-only policy to reject a mutation when no valid plan
exists. Model context, history construction, retries, and tool-result
persistence remain Codex-owned.

## Campaign State Machine

The durable states are:

- `idle`
- `running`
- `paused`
- `won`

`pausing` and `stopping` are transient in-memory states used while an active
tool call or turn reaches a safe boundary.

Transitions are:

```text
idle --Start--> running
paused --Resume--> running
running --Pause--> pausing --safe boundary--> paused
running --Stop--> stopping --flush/shutdown--> idle
paused --Stop--> stopping --flush/shutdown--> idle
running --verified win--> won
won --Start new--> running
```

An unexpected process exit loads the last active campaign as `paused`. Resume
always requires a new live observation and plan before another mutation. No
physical action is replayed to reconstruct state.

The crash-safe campaign record contains:

- Codex thread ID;
- durable campaign state;
- attempt number;
- bounded strategy record;
- latest confirmed observation and action sequence numbers; and
- expected helper connection identity.

Write it by atomic replacement. It is not an event log or a second source of
conversation history.

## Agent Loop

The runner starts one non-ephemeral Codex thread with GPT-5.6-Sol and high
reasoning effort. The model gets one short, stable campaign prompt defining:

- the full-game victory objective;
- the pixel-only evidence boundary;
- the available tools;
- the planning and verification contract;
- loss and victory semantics; and
- the rule that difficulty or a failed strategy is not a stopping condition.

The normal loop is:

```text
observe
  -> model current state and strategic objective
  -> compare candidate moves and short continuations
  -> record_plan
  -> perform exactly one mutation
  -> inspect the returned pixels
  -> revise or continue
```

One Codex turn may contain multiple loops. If a turn completes without a
verified win, Pause, Stop, or terminal infrastructure block, the runner starts
another turn in the same thread with the stable message:

> Continue playing until victory is verified.

This prevents an ordinary assistant final message from ending the campaign.

Parallel mutating calls are disabled. Read-only discovery can remain parallel
only when it cannot make an action depend on stale visual state.

## Planning Contract

`record_plan` is a dynamic tool with these required fields:

- current objective;
- concise visible-state summary;
- two to four candidate moves;
- predicted consequences over the useful visible planning horizon;
- chosen move and reason;
- expected visible result; and
- a condition that would invalidate the plan.

The record is a decision summary, not a request for private chain-of-thought.
Field and aggregate string sizes are capped; one plan cannot exceed 12 KiB of
UTF-8 JSON.

The plan gate tracks the newest fresh observation generation. A successful
`record_plan` creates one plan valid for that generation. Any of the following
invalidates it:

- another fresh observation;
- one attempted mutation, regardless of result;
- Pause, Stop, turn interruption, or helper reconnection; or
- a model-declared invalidation.

`click`, `drag`, and `focus_click` require one fresh, unconsumed plan. The MCP
pre-call policy consumes the plan before dispatch. A second mutation is
rejected until the agent observes and plans again.

Plans have two conceptual levels:

- Tactical plans are required before every mutation.
- The strategic objective is reconsidered at attempt start, shops, bosses, new
  mechanics, major resource changes, and whenever predictions fail.

Both appear in the TUI. Routine observation and zoom calls do not require
visible narration.

## Outcomes and Learning

`report_outcome` is a dynamic tool with:

- outcome: `loss`, `win`, or `terminal_block`;
- the newest observation identifier;
- visible evidence summary;
- one concise lesson; and
- a replacement strategic update when knowledge changed.

A loss ends an attempt, not the campaign. It increments the attempt counter,
records the bounded lesson, and starts another turn. The model observes and
uses the game's visible restart control; the runner does not reset the game
out of band.

A win is accepted only when tied to the newest screenshot and the pixels show
full-game victory rather than a stage transition. It moves the campaign to
`won` and stops automatic turns.

The learning state has two layers:

1. The Codex thread and rollout preserve detailed conversation, screenshots,
   actions, tool results, persisted reasoning, and native compactions.
2. A model-authored strategic record preserves confirmed mechanics, shop and
   boss knowledge, failed strategies, and the current high-level approach.

The strategic record is limited to 16 KiB UTF-8 and is replaced atomically at
attempt boundaries. It is injected as a bounded contextual user fragment when
a new attempt begins and after thread resume. No fact database, transition
archive, embeddings, or evidence graph is introduced.

## Operator TUI

The single-screen TUI has:

- a status bar with campaign state, attempt, model and effort, elapsed time,
  turn, token usage, and MCP health;
- an activity stream with observations, plans, actions, verified effects,
  losses, strategy updates, retries, and failures; and
- a current-decision panel with the latest screenshot, strategic objective,
  tactical plan, and expected result.

Controls are:

- `Enter`: Start from `idle` or Resume from `paused`;
- `Space`: request Pause from `running`;
- `S`: Stop and persist the current thread;
- `Q`: exit, requesting Stop first when a campaign is active; and
- navigation keys: inspect earlier activity without affecting the campaign.

The in-memory activity projection is a 2,000-item ring. Durable detail remains
in the canonical Codex rollout, so the runner does not write a duplicate
activity database. Supported terminals render the newest screenshot using
existing Codex image patterns; unsupported terminals show the artifact
reference and dimensions.

There is no chat box, command palette, approval UI, plugin UI, file browser, or
editable agent state.

## Pause and Stop Semantics

Pause is cooperative. If no tool call is active, submit `Op::Interrupt`
immediately. If a tool call is active, mark Pause pending, allow that call to
return, then interrupt before another call begins. The plan gate rejects new
mutations while Pause is pending.

Resume continues the same Codex thread with a fresh observation requirement.

Stop follows the same safe tool-call boundary, interrupts the turn, flushes the
rollout and campaign record, shuts down the thread, and disconnects from the
helper. It does not kill the helper during an atomic drag or attempt to undo a
completed physical action.

## Failure Handling

Gameplay and infrastructure failures remain distinct:

- A game loss records a lesson and continues.
- A stale observation or missing plan rejects the action and asks the model to
  observe and plan again.
- An indeterminate mutation is never automatically retried. The model must
  observe the game before deciding what occurred.
- Model and network failures use Codex's bounded retry behavior while retaining
  the thread.
- Helper disconnection blocks actions and performs bounded reconnection.
- Persistent capture or control failure becomes `terminal_block`; it is not
  counted as a game loss.

The TUI distinguishes retrying, degraded, paused, blocked, stopped, and won
states. It displays structured runner and Codex events but does not infer game
state itself.

## Configuration

One small TOML file contains deployment facts only:

- model, fixed to GPT-5.6-Sol for the initial product;
- reasoning effort, initially `high`;
- helper app path and Unix socket path;
- target application identity; and
- bounded retry and storage limits.

Behavioral alternatives are evaluated as separate Git revisions. Configuration
does not select between prompts, planners, models, or gameplay policies.

## Verification

### Automated tests

- Campaign state transitions and crash recovery.
- Plan validity, consumption, and invalidation.
- Rejection of every unplanned mutation.
- MCP policy contributor ordering, denial, metadata addition, and collision
  rejection; the no-contributor path must preserve the original call.
- Codex integration tests using existing mocked Responses helpers and a fake
  MCP game server that requires the helper's owner-lease metadata.
- Pause and Stop during reads, clicks, and atomic drags.
- Stale observations, duplicate operation IDs, indeterminate mutations,
  helper disconnects, and model turn completion without victory.
- Strategy retention across loss, compaction, process restart, and thread
  resume.
- CUCtl's existing capture, exact-window, freshness, click, drag cleanup,
  permission, socket, and durable-journal tests.
- `insta` snapshots for every meaningful TUI state.
- One vertical integration test with multiple losses, compaction, resume, and
  eventual verified victory.

### Real-game evaluation

Pin the Git revision, model, effort, prompt, toolset, game start conditions, and
difficulty for each evaluation series. Let each campaign run uninterrupted to
victory, an operator Stop, or a real terminal infrastructure block. Review the
rollout only after the campaign boundary, identify one falsifiable failure
mode, change one variable, and repeat.

Primary measures are:

- verified full-game wins and win rate;
- attempts and elapsed time per win;
- deepest stage or boss reached; and
- tokens and actions per win.

Diagnostic measures are:

- unplanned mutations, which must remain zero;
- repeated ineffective actions;
- invalidated-plan frequency;
- recorded-plan prediction accuracy; and
- losses attributable to perception, planning, execution, or infrastructure.

The first usable release requires at least one pixel-verified end-to-end win on
the real game. Several subsequent clean campaigns establish repeatability.
Sol/high is the baseline. Test xhigh only when complete traces show that
planning depth, rather than perception, timing, or tool execution, is the
bottleneck.

## Scope Control

The initial implementation should add only the thin runner, the narrow MCP
policy seam, wiring through the existing stdio-to-UDS bridge, and the extracted
helper. It should not begin by deleting unused Codex crates. Once the runner
wins and its dependency closure is understood, remove unreachable products and
workspace members in separate, mechanical stages.

This sequence preserves upstream knowledge while avoiding a large, hard-to-
diagnose source-amputation project before gameplay value exists.

## Delivery Stages

This system is intentionally delivered as separate reviewable changes rather
than one large patch:

1. Add canonical MCP coverage to the existing stdio-to-UDS bridge and prove
   the complete Sol code-mode transport path with a hermetic helper. A
   read-only real-helper probe must record any external contract that prevents
   an unmodified Codex client from succeeding.
2. Add the extension-registry MCP call policy seam. Extend the hermetic helper
   to require `epoch`, `generation`, and `call_id`, and prove the runner policy
   supplies them without changing generic MCP calls or the byte bridge.
3. Add a headless `codex-game-runner` observation slice using `codex-core-api`,
   a fixed game-only configuration, and the owner-lease policy. Keep using the
   current signed external helper and require a successful GPT-5.6-Sol live
   observation before proceeding.
4. Add a persistent thread, automatic continuation, the dynamic `record_plan`
   and `report_outcome` tools, plan enforcement in the same MCP policy,
   campaign state, bounded strategy, and fake-game vertical coverage.
5. Add the focused TUI and its snapshots on top of the already tested headless
   campaign core.
6. Import and package `GameControlHelper.app`, preserving the CUCtl tests and
   signed LaunchServices launch path.
7. Run real-game campaigns until the first verified win, then tune only from
   complete traces.
8. Remove unreachable Codex products and workspace members in later mechanical
   changes after the winning runner's dependency closure is known.

Stage 1 is the committed transport characterization. The next implementation
plan covers Stage 2 only: the generic extension seam and hermetic owner-lease
policy proof. Each later stage gets a focused follow-up plan after the
preceding seam is demonstrated.

The game runner and helper are explicitly macOS-specific. The reused
`codex-uds` and stdio bridge remain cross-platform and must continue to compile
and pass their existing tests on Linux, macOS, and Windows.
