# Game Runner Durable Recovery Stage 4B2 Design

## Summary

Stage 4B2 makes the headless game campaign crash-safe and controllable without
moving gameplay policy into a UI. It persists one bounded campaign checkpoint,
resumes the same native Codex rollout, prevents physical-action replay, and
provides typed Start, Pause, Resume, and Stop commands for the later TUI.

The default safety posture is conservative. An unexpected process exit always
reloads as paused. Resume replaces the owner generation, injects the retained
strategy once, and requires fresh pixels and a new plan before another
mutation. A helper outage receives bounded automatic recovery; exhausted
recovery pauses the campaign instead of converting infrastructure trouble into
a game loss or destroying an otherwise winnable campaign.

Stage 4B2 does not add TUI rendering, helper packaging, configuration-selected
behavior, unattended real-game execution, or workspace stripping.

## Goals

- Recover a campaign from process exit without losing its Codex thread,
  attempt counters, strategy, or safety state.
- Guarantee that a physical action is dispatched at most once by the runner.
- Make Pause and Stop wait for a safe tool-call boundary while preventing the
  next mutation from starting.
- Resume the canonical native Codex rollout instead of reconstructing or
  rewriting model history.
- Replace helper ownership on Resume and helper reconnection with a durable,
  monotonically increasing generation.
- Keep helper outages recoverable after bounded automatic attempts.
- Expose bounded status and activity events sufficient for the focused TUI to
  monitor the agent without owning campaign semantics.
- Prove loss, compaction, crash, resume, visible restart, and eventual victory
  through the real runner, Code Mode, MCP policy, image bridge, and fake UDS
  helper path.

## Non-goals

- TUI layout, rendering, input handling, or snapshots.
- Importing or packaging `GameControlHelper.app`.
- Running an unattended real-game campaign.
- A second conversation log, event-sourced campaign database, fact database,
  embedding store, or screenshot archive.
- Replaying a physical action to reconstruct state.
- Adding gameplay-policy or retry alternatives to configuration.
- Changing Codex context history, compaction, inference retries, or MCP
  lifecycle ownership.
- Removing unrelated Codex crates or products.

## Chosen architecture

Use one strict, versioned JSON sidecar plus the existing native Codex rollout.
The sidecar is the source of truth only for bounded campaign control state.
The rollout remains the sole source of truth for messages, reasoning, tool
calls, tool results, screenshots, and compaction history.

This is intentionally smaller than the rejected alternatives:

- A separate event journal would duplicate rollout history and require replay
  logic to rebuild the current campaign state.
- A new table in Codex's state database would couple a macOS-specific runner to
  core schema and migrations without improving model or helper correctness.

The checkpoint lives at:

```text
<codex_home>/game-runner/campaign.json
```

The controller holds an exclusive standard-library file lock on:

```text
<codex_home>/game-runner/campaign.lock
```

for its lifetime. A second runner fails before reading or changing campaign
state. Operating-system lock release handles process crashes; no stale-PID
cleanup protocol is introduced.

## Component boundaries

### Campaign checkpoint store

`CampaignCheckpointStore` owns path validation, size limits, strict decoding,
crash normalization, and durable replacement. It has no Codex, MCP, helper, or
model dependencies.

The store:

- rejects a checkpoint larger than 256 KiB before decoding;
- uses `deny_unknown_fields` for every persisted structure;
- rejects version zero and any version newer than version 1;
- rejects symlinked checkpoint, temporary, and lock paths;
- validates the checkpoint's thread, deployment, counters, strategy, and
  bounded strings before returning it;
- preserves malformed or incompatible input unchanged for diagnosis; and
- maps a valid `running` checkpoint to `paused { reason: unexpectedExit }` by
  durable replacement before recovery can contact the helper or model.

Writes create a unique same-directory file with create-new semantics, apply
user-only permissions, encode one trailing-newline-terminated JSON value,
`fsync` the file, atomically rename it over the destination, then `fsync` the
parent directory. Failure before rename leaves the previous checkpoint valid.
Failure after rename is reported and closes the mutation lane because
durability cannot be proven.

### Campaign controller

`CampaignController` owns the live worker, command serialization, durable
transitions, helper recovery, and runtime replacement. It accepts typed
commands:

```rust
pub enum CampaignCommand {
    Start,
    Pause,
    Resume,
    Stop,
}
```

Only one command transition may be active. Invalid transitions return typed
errors rather than silently becoming no-ops. Important examples are Start
while a paused campaign exists, Resume while running, Pause while idle, and a
second Stop while stopping.

The current headless binary issues Start. If startup discovers a paused or
crash-normalized campaign, it returns a typed `CampaignRequiresResume` error
with the checkpoint path instead of overwriting it. Stage 5 drives Resume
through the controller; Stage 4B2 integration tests exercise the same public
controller command directly without adding a temporary CLI protocol.

The durable states are:

- `running`
- `paused { reason }`
- `won`

Absence of `campaign.json` means `idle`. `pausing`, `recovering`, `stopping`,
and `blocked` are transient live statuses. A process exit during any transient
status reloads the last durable checkpoint as paused before any work resumes.

Start is allowed from idle and won. Starting after won creates a new epoch and
replaces the old active checkpoint; the previous Codex rollout remains in
native thread storage. Stop from running or paused durably removes the active
checkpoint after the safe shutdown sequence, making the controller idle.

### Owner lease state

`OwnerLeaseState` owns one random campaign epoch and a durable nonnegative
generation. A new campaign starts at generation 1. Resume and successful
helper-runtime replacement increment it with checked arithmetic before any
new game call.

The game policy reads the current lease from this shared state for every MCP
call. The decision gate's owner generation changes in the same transition and
invalidates all observations and plans. Epoch never changes inside one
campaign. Stop followed by Start creates a new epoch.

### Resumable runtime

`RunnerRuntime` gains a runner-owned resume constructor that rebuilds the same
fixed configuration, extension registry, game policy, and Code Mode provider,
then calls Codex's native rollout-resume path. The existing resume path restores
persisted dynamic tool definitions from history when the caller supplies no
replacement list, so `game_runner.record_plan` and
`game_runner.report_outcome` remain available after resume.

No new `codex-core` API is expected. If implementation proves an unavoidable
missing seam, the only acceptable core change is a narrow reusable resume
option; campaign state and helper behavior remain outside `codex-core`.

### Helper recovery

`HelperRecovery` classifies a game-tool error by probing the configured Unix
socket. A healthy socket leaves the error in the normal model-visible flow. An
unavailable socket closes the mutation lane and starts recovery.

Recovery performs no more than three cycles. Each cycle invokes the existing
signed-helper launch/readiness path and waits at most 15 seconds. The pauses
between unsuccessful cycles are one second and two seconds. A successful cycle
replaces the damaged runtime, increments the owner generation, resumes the
same rollout, and follows the normal Resume safety sequence.

After three unsuccessful cycles, the controller writes
`paused { reason: helperUnavailable }`. An operator can repair the helper and
Resume later. Recovery failure never increments the gameplay loss counter and
never produces a model-authored `terminal_block` outcome.

## Durable checkpoint

Version 1 contains only bounded control data:

- schema version;
- campaign epoch;
- Codex thread ID and rollout path;
- target application, helper bundle path, and socket path;
- durable lifecycle state and bounded pause/failure reason;
- attempt number, total turns, total authorized actions, and game losses;
- current bounded `StrategyRecord`;
- at most 64 recent turn IDs, each at most 2 KiB;
- owner generation;
- cumulative decision and policy audit counters;
- latest observation sequence and the action sequence it confirms; and
- optional unresolved mutation metadata.

Unresolved mutation metadata contains only its action sequence, operation ID,
action SHA-256, tool name, and result classification. Tool arguments are not
duplicated because their canonical value already exists in the Codex rollout.
Operation IDs and failure strings are individually capped at 2 KiB. Tool names
must be one of `click`, `drag`, or `focus_click`; action hashes must be 64
lowercase hexadecimal characters.

The checkpoint stores no screenshot bytes, plan prose, model response text,
or chain-of-thought. `won` retains the bounded final campaign summary and
latest evidence references needed by the future TUI; detailed evidence stays
in the rollout.

## Mutation durability protocol

Every physical mutation uses this ordering:

```text
fresh observation
  -> accepted exact plan
  -> allocate checked action sequence
  -> durably write unresolved mutation
  -> policy permits one dispatch with operation metadata
  -> record MCP result classification durably
  -> require a fresh full-frame observation
  -> durably record the confirmed action sequence
  -> clear unresolved mutation
```

The policy must finish the unresolved-mutation checkpoint write before it
returns `Allow`. A checkpoint failure returns `Deny`, closes the mutation lane,
and dispatches nothing.

A crash after the unresolved write but before known dispatch is deliberately
treated as indeterminate. This may force an unnecessary observation, but it
cannot duplicate a physical action. A crash after helper execution but before
the result or screenshot is persisted produces the same safe recovery state.

Neither automatic helper recovery nor operator Resume reuses an operation ID,
observation, or plan. The first post-resume turn always observes before it may
plan. An unresolved action remains visible to the model as bounded recovery
context, but the prompt explicitly forbids retrying it without using the new
pixels to decide what happened.

## Lifecycle ordering

### Start

```text
acquire controller lock
  -> verify no paused/running checkpoint
  -> create non-ephemeral Codex thread
  -> materialize and flush rollout
  -> write running checkpoint at generation 1
  -> submit initial campaign turn
```

No game call is possible before the first checkpoint succeeds. A crash after
thread creation but before the checkpoint may leave an unused native rollout,
but it cannot leave an untracked physical action.

### Pause

```text
close mutation lane immediately
  -> if a tool call is active, allow that call to reach its bounded result
  -> classify an unresolved mutation conservatively
  -> submit Op::Interrupt
  -> wait for the existing bounded interrupt completion
  -> flush native rollout
  -> write paused checkpoint
```

Reads may finish while Pause is pending. An active click or atomic drag may
finish. No later mutation may start. If the active call reaches its existing
tool timeout, its outcome is indeterminate and Pause continues; the controller
does not wait without a deadline.

### Resume

```text
validate paused checkpoint and exact deployment identity
  -> ensure helper readiness
  -> checked generation increment
  -> create a fresh decision gate with cumulative counters but no authority
  -> resume the same native Codex rollout
  -> write running checkpoint
  -> submit one bounded recovery prompt containing the retained strategy
  -> require fresh pixels before planning or mutation
```

The recovery prompt includes the 16 KiB-capped strategy once. It includes the
unresolved operation summary when present. It does not duplicate screenshots,
plans, or prior tool results. Normal short continuation prompts resume after
the first recovery turn.

### Stop

Stop closes the mutation lane and reaches the same safe interrupt boundary as
Pause. It then flushes the rollout, shuts down the thread, removes the active
checkpoint durably, and enters idle. The controller retains its process lock
for its lifetime so another process cannot race a later Start. Stop does not
kill the helper during an atomic drag and does not attempt to undo a completed
action.

### Win

A win remains valid only when `report_outcome` references the newest verified
post-mutation screenshot. The controller flushes the rollout, writes `won`,
stops automatic turns, and shuts down the live runtime. The won checkpoint is
retained until Start creates a new campaign or a later UI explicitly clears
it.

## Status and activity interface

The controller publishes the latest `CampaignStatus` through a watch channel
and activity through a broadcast channel with capacity 256. Lag is explicit;
the controller never blocks safety transitions on a slow observer. The future
TUI owns its separate 2,000-item in-memory projection.

Activity is a bounded exhaustive enum:

```rust
pub enum CampaignEvent {
    StatusChanged(CampaignStatus),
    Progress(CampaignSummary),
    Observation(ObservationEvidence),
    Plan(AcceptedPlan),
    Mutation(AuthorizedMutation),
    MutationFinished(MutationResult),
    Outcome(ReportedOutcome),
    Failure(CampaignFailure),
}
```

Events contain evidence references and bounded summaries, never image bytes or
private reasoning. The controller emits events only after the corresponding
in-memory transition succeeds; events describing durable authority are emitted
only after its checkpoint succeeds.

`CampaignFailure` contains an exhaustive failure category plus a display
summary capped at 2 KiB. It does not embed arbitrary error chains or tool
payloads.

## Error semantics

Gameplay and infrastructure remain separate:

- A visible game loss increments losses, replaces strategy, and starts another
  attempt.
- A model or API failure uses Codex's existing bounded retry behavior.
- A stale observation, stale plan, mismatched mutation, or exhausted action
  batch remains a recoverable model-visible denial.
- An indeterminate mutation requires observation and is never replayed.
- A transient helper outage enters bounded recovery.
- Exhausted helper recovery becomes durable paused state.
- Checkpoint or rollout-flush failure closes mutation authority and exposes a
  blocked status; the runner does not continue on unpersisted state.
- A corrupt, oversized, symlinked, unsupported, wrong-target, or wrong-thread
  checkpoint is preserved and exposed as blocked.
- Only visibly verified full-game victory creates `won`.

If the controller cannot persist a paused failure state, it reports both the
primary and persistence errors and shuts down the runtime after reaching the
safest available boundary. It never claims the checkpoint is durable.

## Testing

### Checkpoint store

Unit tests compare complete values and cover:

- exact version-1 round-trip;
- 256 KiB input and output limits;
- strict unknown-field and future-version rejection;
- all string, strategy, recent-ID, counter, operation, tool, and hash bounds;
- wrong thread and deployment identity;
- checkpoint, temporary-file, and lock symlinks;
- atomic replacement preserving the previous file on pre-rename failure;
- file and parent-directory synchronization failures through injected file
  operations; and
- durable normalization of a running checkpoint to paused after process exit.

### Controller reducer

Reducer tests exhaustively cover valid and invalid Start, Pause, Resume, Stop,
recovery, blocked, and won transitions. They prove checked generation and
counter increments, one active command transition, immediate mutation-lane
closure, cumulative audit restoration, and absence-as-idle semantics.

### Runtime and safety integrations

Hermetic integrations simulate crashes at four boundaries:

1. after plan but before durable mutation authorization;
2. after durable authorization but before known dispatch;
3. after mutation result but before fresh observation; and
4. after fresh observation and checkpoint confirmation.

Each recovered campaign must resume the same thread and rollout, use generation
`N+1`, begin with no observation or plan authority, and dispatch no duplicate
operation. The fake helper rejects repeated operation IDs.

Pause and Stop tests hold reads, clicks, and atomic drags open. They assert that
the active call reaches its bounded result, no subsequent mutation reaches the
helper, the rollout is flushed before the durable transition, and Resume cannot
reuse pre-pause evidence.

Helper tests distinguish a healthy socket tool error from disconnection, prove
the exact three-cycle budget and backoffs with paused time, verify successful
runtime replacement, and confirm exhausted recovery persists a resumable pause
without incrementing losses.

### Full recovery vertical

The final fake campaign:

1. starts a fresh persistent thread;
2. performs planned actions and loses;
3. records a bounded replacement strategy;
4. undergoes native Codex compaction;
5. crashes with a durable running checkpoint;
6. reloads as paused without contacting the helper;
7. explicitly resumes the same rollout at generation `N+1`;
8. injects the retained strategy once;
9. captures fresh pixels and performs a visibly planned restart; and
10. reaches a newest-screenshot-linked verified win.

The test asserts exact attempt, turn, action, loss, audit, generation, and
operation counts; no operation replay; complete helper script consumption;
bounded retained state; and an empty screenshot spool.

## Repository and delivery constraints

- Keep Stage 4B2 in `codex-game-runner`; resist additions to `codex-core`.
- Execute the implementation inline in the current checkout; do not create
  worktrees or dispatch subagents.
- Add new focused modules instead of growing `campaign_loop.rs` or
  `decision.rs`, which are already near 500 lines.
- Keep every new production module below 500 lines and every complex commit
  below 500 changed lines.
- Preserve exact owner metadata, action SHA-256, one-plan authority,
  post-mutation observation, image verification, and per-mutation focus
  borrowing from Stages 4A and 4B1.
- Use native RPITIT with explicit `Send` futures for any new trait used for
  injected storage, helper, clock, or runtime behavior; do not use
  `async_trait`.
- Keep the game runner macOS-specific while preserving cross-platform builds
  for reused generic Codex crates.
- Run the complete `codex-game-runner` suite, scoped Clippy, formatting, and a
  final binary build. Run generic core tests only if an unavoidable reusable
  core seam changes.
- Do not mix Stage 5 TUI code, helper import/package work, real-game execution,
  or workspace stripping into Stage 4B2.

## Completion criteria

Stage 4B2 is complete only when:

- an interrupted running campaign durably reloads as paused;
- explicit Resume continues the same native Codex rollout with generation
  incremented and no stale authority;
- no crash, pause, reconnect, or resume path can replay a physical mutation;
- Pause and Stop reach bounded safe boundaries and prevent subsequent actions;
- helper disconnection either recovers within the exact budget or becomes a
  resumable durable pause;
- strategy and aggregate campaign progress survive compaction and process
  restart within the checkpoint cap;
- the full fake campaign loses, compacts, crashes, resumes, visibly restarts,
  and reaches verified victory;
- the controller exposes bounded typed status and activity suitable for the
  later TUI;
- the full runner suite, scoped Clippy, formatting, and final binary build
  succeed; and
- no TUI, helper packaging, real-game unattended execution, or workspace
  stripping is included.

The next stage builds the focused Start/Pause/Resume/Stop TUI as a thin client
of this tested controller. Real-game play-until-win campaigns remain blocked
until those operator controls exist.
