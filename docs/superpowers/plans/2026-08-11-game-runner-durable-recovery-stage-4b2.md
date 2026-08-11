# Game Runner Durable Recovery Stage 4B2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task inline. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add crash-safe campaign checkpoints, native Codex rollout resume, at-most-once mutation recovery, typed Start/Pause/Resume/Stop control, bounded helper reconnection, and a fake loss-compaction-crash-resume-victory proof.

**Architecture:** Keep one strict atomic JSON checkpoint beside Codex's canonical rollout. A runner-owned controller serializes commands, drives the existing campaign loop, replaces owner generations on resume, and rebuilds a damaged runtime from the same rollout; the game MCP policy persists unresolved mutation authority before returning `Allow`.

**Tech Stack:** Rust 1.95, Tokio, serde/serde_json, Codex `ThreadManager`/`CodexThread`, Code Mode, dynamic tools, MCP call-policy extensions, Unix-domain sockets, Nextest through `just test`, and hermetic Responses fixtures.

## Global Constraints

- Source design: `docs/superpowers/specs/2026-08-10-game-runner-durable-recovery-stage-4b2-design.md` at commit `cefbb48f1e`.
- Execute inline in the current checkout. Do not create worktrees or dispatch subagents.
- Keep campaign state and helper behavior in `codex-game-runner`; do not add them to `codex-core`.
- The sole expected external seam is re-exporting `ClientMcpExtensions` from `codex-core-api`; stop and revise this plan before any other core/common/protocol change.
- Keep every new production module under 500 lines and every complex commit under 500 changed lines.
- Do not grow `campaign_loop.rs` or `decision.rs` past 500 lines; extract focused modules before adding behavior.
- Preserve exact plan authority, action SHA-256, owner metadata, post-mutation observation, screenshot verification, and focus borrowing.
- Use native RPITIT with explicit `Send` futures for new traits. Do not use `async_trait`.
- Use named constructors and enums rather than opaque boolean or `Option` arguments.
- Use `apply_patch` for edits, `rg` for searches, `rustup run 1.95.0`, and `just test`; never run `cargo test` directly.
- If `Cargo.toml` changes, run `just bazel-lock-update` at the repository root and commit `MODULE.bazel.lock` if it changes.
- After all tests pass, run `just fix -p codex-game-runner`, then `just fmt`; do not rerun tests after the final fix/format pass.
- Do not add TUI code, helper packaging/import, real-game execution, behavioral configuration alternatives, or workspace stripping.

## File structure

- `checkpoint.rs`: version-1 durable data model and validation only.
- `checkpoint_store.rs`: exclusive process lock, bounded reads, atomic replacement, durable removal, and crash normalization.
- `campaign_persistence.rs`: serialized transactional updates to the active checkpoint and mutation protocol.
- `owner_lease.rs`: durable epoch/generation state shared by the policy and controller.
- `controller_types.rs`: public commands, statuses, failures, events, and the pure transition reducer.
- `controller.rs`: controller actor, public handle, runtime ownership, and command orchestration.
- `helper_recovery.rs`: exact three-cycle helper recovery budget and socket health classification.
- `campaign_event.rs`: extracted game-event reduction and report construction from the near-limit loop module.
- `runtime.rs`: fresh and native-resume runtime construction.
- `tests/support/durable_game.rs`: strict fake helper and crash/resume fixtures.
- `tests/recovery_campaign_path.rs`: same-rollout no-replay recovery cases.
- `tests/durable_campaign_vertical.rs`: loss, compaction, crash, explicit resume, visible restart, and verified victory.

---

### Task 1: Define and validate the version-1 checkpoint

**Files:**
- Create: `codex-rs/game-runner/src/checkpoint.rs`
- Create: `codex-rs/game-runner/src/checkpoint_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign_progress.rs`
- Modify: `codex-rs/game-runner/src/decision.rs`
- Modify: `codex-rs/game-runner/src/policy.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Produces `CampaignCheckpoint`, `CheckpointDeployment`, `DurableCampaignState`, `PauseReason`, `DurableObservation`, `DurableMutation`, `DurableMutationResult`, `CheckpointValidationError`, `CHECKPOINT_VERSION`, and `MAX_CHECKPOINT_BYTES`.
- Makes `CampaignSummary`, `DecisionAudit`, and `PolicyAudit` strictly deserializable so the checkpoint can reuse the existing bounded public values.

- [ ] **Step 1: Write failing whole-value and boundary tests**

Create a sibling test module with fixtures that construct a complete checkpoint and assert:

```rust
#[test]
fn version_one_checkpoint_round_trips_as_one_value() -> anyhow::Result<()> {
    let checkpoint = valid_checkpoint();
    let encoded = checkpoint.encode()?;
    assert_eq!(CampaignCheckpoint::decode(&encoded)?, checkpoint);
    Ok(())
}

#[test]
fn checkpoint_validation_rejects_every_unbounded_or_incompatible_field() {
    let cases = invalid_checkpoints();
    assert_eq!(
        cases.into_iter().map(|value| value.validate()).collect::<Vec<_>>(),
        expected_validation_errors()
    );
}
```

Include exact cases for version 0/2, empty or oversized epoch/thread/failure/operation IDs, relative rollout/helper/socket paths, empty target app, zero generation or attempt number, inconsistent campaign counters, more than 64 recent IDs, invalid strategy, unknown mutation tool, non-lowercase/non-64-byte hash, inconsistent observation/action sequences, and encoded size above 256 KiB. Add raw-JSON tests proving unknown fields are rejected.

- [ ] **Step 2: Run the checkpoint tests and observe RED**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner checkpoint::tests --no-capture --retries 0
```

Expected: compilation fails because `checkpoint` and the durable types do not exist.

- [ ] **Step 3: Implement the exact persisted model**

Use strict serde tagging and no optional-field omission:

```rust
pub const CHECKPOINT_VERSION: u32 = 1;
pub const MAX_CHECKPOINT_BYTES: usize = 256 * 1024;
const MAX_CONTROL_STRING_BYTES: usize = 2 * 1024;
const MAX_PATH_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CampaignCheckpoint {
    pub schema_version: u32,
    pub epoch: String,
    pub thread_id: String,
    pub rollout_path: PathBuf,
    pub deployment: CheckpointDeployment,
    pub state: DurableCampaignState,
    pub summary: CampaignSummary,
    pub owner_generation: u64,
    pub decision_audit: DecisionAudit,
    pub policy_audit: PolicyAudit,
    pub latest_observation: Option<DurableObservation>,
    pub unresolved_mutation: Option<DurableMutation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointDeployment {
    pub helper_app: PathBuf,
    pub socket_path: PathBuf,
    pub target_app: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
pub enum DurableCampaignState {
    Running,
    Paused { reason: PauseReason },
    Won { evidence_reference: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "reason", rename_all = "camelCase", deny_unknown_fields)]
pub enum PauseReason {
    UnexpectedExit,
    Operator,
    HelperUnavailable { summary: String },
    DurabilityFailure { summary: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DurableMutation {
    pub action_sequence: u64,
    pub operation_id: String,
    pub action_sha256: String,
    pub tool: String,
    pub result: DurableMutationResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DurableObservation {
    pub observation_sequence: u64,
    pub confirms_action_sequence: Option<u64>,
    pub reference: String,
}
```

Define `DurableMutationResult::{Pending, Success, CleanFailure, Indeterminate}`. `CampaignCheckpoint::validate`, `encode`, and `decode` enforce every test case before returning a value. Validate `thread_id` with `ThreadId::from_string`; require absolute rollout/helper/socket paths; cap encoded paths at 16 KiB and every other scalar control string, turn ID, operation ID, failure summary, and evidence reference at 2 KiB. Reuse `StrategyRecord::validate` for its existing 16 KiB aggregate cap. Require generation and attempt number to be nonzero, `attempt_number == losses + 1`, recent IDs at most 64, lowercase 64-byte action hashes, and checked monotonic observation/action sequences. Zero total turns/actions/losses remain valid initial counters.

- [ ] **Step 4: Run focused tests GREEN**

Run the Step 2 command. Expected: all checkpoint tests pass.

- [ ] **Step 5: Commit**

```bash
git add codex-rs/game-runner/src/checkpoint.rs \
  codex-rs/game-runner/src/checkpoint_tests.rs \
  codex-rs/game-runner/src/campaign_progress.rs \
  codex-rs/game-runner/src/decision.rs \
  codex-rs/game-runner/src/policy.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): define durable campaign checkpoint"
```

---

### Task 2: Add the exclusive lock and atomic checkpoint store

**Files:**
- Create: `codex-rs/game-runner/src/checkpoint_store.rs`
- Create: `codex-rs/game-runner/src/checkpoint_store_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Consumes `CampaignCheckpoint`, `RunnerDeployment`, and the checkpoint validation API from Task 1.
- Produces public `CheckpointStoreError`, `CampaignCheckpointStore::open`, `load_and_normalize`, `replace`, `remove`, `path`, and the lifetime-owning `CampaignStoreGuard`.

- [ ] **Step 1: Write failing filesystem-ordering tests**

Define a recording fake filesystem and compare its complete operation log. Cover successful lock/write/remove, second-lock rejection, oversized pre-read rejection, all symlink targets, deployment mismatch, preservation on pre-rename failure, reported post-rename directory-sync failure, running-to-paused normalization, and conversion of a crash-left `Pending` mutation to `Indeterminate`. The key assertion is:

```rust
assert_eq!(
    fs.operations(),
    vec![
        FsOperation::CreateTemp,
        FsOperation::Write,
        FsOperation::SyncFile,
        FsOperation::Rename,
        FsOperation::SyncDirectory,
    ]
);
```

- [ ] **Step 2: Run the store tests and observe RED**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner checkpoint_store::tests --no-capture --retries 0
```

Expected: compilation fails because the checkpoint store is absent.

- [ ] **Step 3: Implement the durable filesystem boundary and store**

Add a documented, object-safe synchronous trait so failure ordering is testable without `async_trait`:

```rust
trait DurableCheckpointFs: Send + Sync {
    fn acquire_lock(&self, path: &Path) -> std::io::Result<Box<dyn Send>>;
    fn read_limited(&self, path: &Path, max_bytes: usize) -> std::io::Result<Option<Vec<u8>>>;
    fn replace(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()>;
    fn remove(&self, path: &Path) -> std::io::Result<()>;
    fn reject_symlink(&self, path: &Path) -> std::io::Result<()>;
}

pub struct CampaignCheckpointStore {
    root: PathBuf,
    filesystem: Arc<dyn DurableCheckpointFs>,
}

impl CampaignCheckpointStore {
    pub fn open(codex_home: &Path) -> Result<(Self, CampaignStoreGuard), CheckpointStoreError>;
    pub fn load_and_normalize(
        &self,
        deployment: &RunnerDeployment,
    ) -> Result<Option<CampaignCheckpoint>, CheckpointStoreError>;
    pub fn replace(&self, checkpoint: &CampaignCheckpoint) -> Result<(), CheckpointStoreError>;
    pub fn remove(&self) -> Result<(), CheckpointStoreError>;
    pub fn path(&self) -> &Path;
}
```

The local implementation creates `<codex_home>/game-runner` with user-only permissions, rejects symlinks before use, holds `File::try_lock` for the guard lifetime, uses a UUID-suffixed create-new temp file, sets mode `0o600` on Unix, cleans up failed temporary files, syncs file and parent directory, and durably syncs checkpoint removal. `CheckpointStoreError` distinguishes failures before rename/unlink from post-rename/post-unlink `DurabilityUncertain` failures. `load_and_normalize` rewrites `Running` to `Paused { UnexpectedExit }` and rewrites any crash-left unresolved `Pending` result to `Indeterminate` in the same atomic replacement before returning it.

- [ ] **Step 4: Run focused tests GREEN**

Run the Step 2 command. Expected: all store and lock tests pass.

- [ ] **Step 5: Commit**

```bash
git add codex-rs/game-runner/src/checkpoint_store.rs \
  codex-rs/game-runner/src/checkpoint_store_tests.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): store campaign checkpoints atomically"
```

---

### Task 3: Persist mutation authority before MCP dispatch

**Files:**
- Create: `codex-rs/game-runner/src/campaign_persistence.rs`
- Create: `codex-rs/game-runner/src/campaign_persistence_tests.rs`
- Create: `codex-rs/game-runner/src/owner_lease.rs`
- Create: `codex-rs/game-runner/src/owner_lease_tests.rs`
- Modify: `codex-rs/game-runner/src/policy.rs`
- Modify: `codex-rs/game-runner/src/policy_tests.rs`
- Modify: `codex-rs/game-runner/Cargo.toml`
- Modify: `codex-rs/game-runner/src/lib.rs`
- Modify if generated: `MODULE.bazel.lock`

**Interfaces:**
- Produces public `PersistenceError`, `CampaignPersistence`, `MutationCheckpointUpdate`, `OwnerLeaseState`, and named `GameCallPolicy::durable` construction.
- Guarantees that a durable unresolved mutation exists before a policy returns `Allow` and that persistence failure returns `Deny` with the mutation lane closed.

- [ ] **Step 1: Write failing transactional and policy tests**

Test the complete checkpoint before/after every operation: install, summary update, plan audit update, mutation begin, mutation result, observation confirmation, pause, running generation replacement, won, and removal. Add a policy test whose store fails during mutation begin:

```rust
let decision = policy.evaluate(mutation_input("click", call_id, &arguments)).await;
assert_eq!(
    decision,
    McpToolCallPolicyDecision::Deny {
        reason: "campaign checkpoint write failed before mutation dispatch".to_string(),
    }
);
assert_eq!(persistence.snapshot().await?.unresolved_mutation, None);
assert_eq!(policy.mutation_lane_is_open(), false);
```

Also assert a successful decision persists `Pending` before the fake policy call returns, assigns action sequence 1, and emits matching epoch/generation/call/operation/hash metadata.

- [ ] **Step 2: Run focused tests and observe RED**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner campaign_persistence::tests --no-capture --retries 0
rustup run 1.95.0 just test -p codex-game-runner policy::tests --no-capture --retries 0
```

Expected: compilation fails for the missing persistence and lease APIs.

- [ ] **Step 3: Implement serialized checkpoint transactions**

Enable Tokio's `sync` feature and implement:

```rust
pub struct CampaignPersistence {
    store: Arc<CampaignCheckpointStore>,
    state: tokio::sync::Mutex<PersistenceState>,
}

struct PersistenceState {
    checkpoint: Option<CampaignCheckpoint>,
    active_call_id: Option<String>,
}

impl CampaignPersistence {
    pub fn empty(store: Arc<CampaignCheckpointStore>) -> Self;
    pub async fn install(&self, checkpoint: CampaignCheckpoint) -> Result<(), PersistenceError>;
    pub async fn snapshot(&self) -> Result<CampaignCheckpoint, PersistenceError>;
    pub async fn persist_summary(&self, summary: CampaignSummary, decision: DecisionAudit, policy: PolicyAudit) -> Result<(), PersistenceError>;
    pub async fn begin_mutation(&self, update: &MutationCheckpointUpdate) -> Result<DurableMutation, PersistenceError>;
    pub async fn finish_mutation(&self, call_id: &str, result: MutationResult) -> Result<(), PersistenceError>;
    pub async fn confirm_observation(&self, observation: &ObservationEvidence) -> Result<(), PersistenceError>;
    pub async fn set_state(&self, state: DurableCampaignState, owner_generation: u64) -> Result<(), PersistenceError>;
}

pub struct MutationCheckpointUpdate {
    pub authorization: AuthorizedMutation,
    pub decision_audit: DecisionAudit,
    pub policy_audit: PolicyAudit,
}
```

Every method locks `PersistenceState`, clones the current checkpoint, validates and durably writes the candidate with `spawn_blocking`, then replaces the in-memory value only after confirmed success. A pre-rename failure leaves both prior copies unchanged. A post-rename `DurabilityUncertain` error leaves the in-memory copy unchanged, closes mutation authority, and forces `Blocked`; the candidate may be on disk and must be normalized safely on the next process start. `begin_mutation` requires no existing active call, assigns `summary.total_actions + 1` with checked arithmetic, writes the `Pending` mutation, incremented action count, and supplied post-authorization decision/policy audits atomically, and records the call ID only in memory after the write succeeds. `finish_mutation` matches and clears that in-memory call ID while retaining the durable unresolved mutation with its result classification. `confirm_observation` writes the confirming observation and clears the unresolved mutation in one replacement. A restored persistence instance deliberately has no active call ID; its durable unresolved mutation is recovery context, never a finishable or replayable live call.

Implement `OwnerLeaseState::{new,current,increment_generation}` with checked arithmetic. Change policy storage to use `Arc<OwnerLeaseState>`, add an `AtomicBool` mutation lane with named `close_mutation_lane`/`mutation_lane_is_open`, retain `GameCallPolicy::new` for ephemeral tests, and add:

```rust
pub fn durable(
    lease: Arc<OwnerLeaseState>,
    gate: Arc<DecisionGate>,
    persistence: Arc<CampaignPersistence>,
) -> Self;
```

On an exact planned mutation, call `begin_mutation` after `DecisionGate::prepare_mutation` and before returning `Allow`. Do not persist denied or unknown calls. Run `just bazel-lock-update` because `Cargo.toml` changed.

- [ ] **Step 4: Run focused tests GREEN**

Run both Step 2 commands. Expected: persistence and policy tests pass.

- [ ] **Step 5: Commit**

```bash
git add codex-rs/game-runner/src/campaign_persistence.rs \
  codex-rs/game-runner/src/campaign_persistence_tests.rs \
  codex-rs/game-runner/src/owner_lease.rs \
  codex-rs/game-runner/src/owner_lease_tests.rs \
  codex-rs/game-runner/src/policy.rs \
  codex-rs/game-runner/src/policy_tests.rs \
  codex-rs/game-runner/Cargo.toml \
  codex-rs/game-runner/src/lib.rs MODULE.bazel.lock
git commit -m "feat(game-runner): persist mutation authority before dispatch"
```

---

### Task 4: Restore progress and resume the native Codex rollout

**Files:**
- Modify: `codex-rs/core-api/src/lib.rs`
- Modify: `codex-rs/game-runner/src/runtime.rs`
- Create: `codex-rs/game-runner/src/runtime_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign_progress.rs`
- Modify: `codex-rs/game-runner/src/campaign_progress_tests.rs`
- Modify: `codex-rs/game-runner/src/decision.rs`
- Modify: `codex-rs/game-runner/src/decision_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign_prompt.rs`
- Modify: `codex-rs/game-runner/src/campaign_prompt_tests.rs`

**Interfaces:**
- Produces `CampaignProgress::restore`, `DecisionGate::restore`, `resume_prompt`, `RunnerRuntime::resume`, and `RunnerRuntime::resume_with_code_mode_host`.
- Re-exports `ClientMcpExtensions` from the public core facade so the runner can call the already-public resume method without depending directly on `codex-protocol`.

- [ ] **Step 1: Write failing restore and resume tests**

Add whole-value reducer tests proving restored counters/audits are cumulative and authority is empty:

```rust
assert_eq!(restored_progress.summary(), checkpoint.summary);
assert_eq!(
    restored_gate.snapshot(),
    DecisionSnapshot {
        owner_generation: 2,
        next_observation_generation: 9,
        observation: None,
        plan: None,
        mutation: None,
        outcome: None,
        requires_post_mutation_observation: false,
        batch_actions: 0,
        audit: checkpoint.decision_audit,
    }
);
```

Add a runtime integration that starts a persistent thread with `CampaignTools::specs`, flushes and shuts it down, resumes from the rollout, asserts the thread ID is unchanged, and verifies a resumed Responses request still exposes both dynamic tools.

Add an exact prompt test asserting the resumed strategy JSON and unresolved operation appear once, while screenshot references, prior plan prose, and tool output do not.

- [ ] **Step 2: Run focused tests and observe RED**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner runtime::tests --no-capture --retries 0
rustup run 1.95.0 just test -p codex-game-runner campaign_progress::tests --no-capture --retries 0
rustup run 1.95.0 just test -p codex-game-runner campaign_prompt::tests --no-capture --retries 0
```

Expected: missing restore, prompt, facade export, and runtime resume APIs.

- [ ] **Step 3: Implement restoration and resume**

Add this facade export only:

```rust
pub use codex_protocol::mcp::ClientMcpExtensions;
```

Refactor runtime manager construction once, then add named resume constructors that call:

```rust
thread_manager
    .resume_thread_from_rollout(
        config,
        rollout_path,
        auth_manager,
        /*parent_trace*/ None,
        ClientMcpExtensions::default(),
    )
    .await
```

Verify the returned thread ID equals the checkpoint before returning `RunnerRuntime`. `CampaignProgress::restore` validates its summary and seeds `last_action_audit`; `DecisionGate::restore` takes owner generation, next observation generation, and cumulative audit while deliberately restoring no observation, plan, mutation, or outcome.

Implement `resume_prompt(attempt_number, strategy, unresolved_mutation) -> Result<String, RunnerError>` as one bounded message beginning with “Resume attempt”, containing serialized strategy and optional operation ID/hash/result, and requiring fresh full-frame pixels before planning.

- [ ] **Step 4: Run focused tests GREEN and check the facade**

Run the three Step 2 commands, then:

```bash
rustup run 1.95.0 cargo check -p codex-core-api
```

Expected: all focused tests and the facade check pass.

- [ ] **Step 5: Commit**

```bash
git add codex-rs/core-api/src/lib.rs \
  codex-rs/game-runner/src/runtime.rs \
  codex-rs/game-runner/src/runtime_tests.rs \
  codex-rs/game-runner/src/campaign_progress.rs \
  codex-rs/game-runner/src/campaign_progress_tests.rs \
  codex-rs/game-runner/src/decision.rs \
  codex-rs/game-runner/src/decision_tests.rs \
  codex-rs/game-runner/src/campaign_prompt.rs \
  codex-rs/game-runner/src/campaign_prompt_tests.rs
git commit -m "feat(game-runner): resume native campaign rollouts"
```

---

### Task 5: Define controller commands, statuses, events, and transitions

**Files:**
- Create: `codex-rs/game-runner/src/controller_types.rs`
- Create: `codex-rs/game-runner/src/controller_types_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Produces the public TUI-facing value layer, `CommandTransitionError`, and private exhaustive command/status reducers. Task 7 consumes these exact types.

- [ ] **Step 1: Write failing exhaustive reducer tests**

Cover every valid design transition and invalid command as complete values. Required cases include idle Start, won Start, running Pause, paused Resume, running/paused Stop, duplicate transitions, blocked commands, helper recovery success/exhaustion, win, and crash normalization.

```rust
assert_eq!(
    reduce_command(&CampaignStatus::Running { attempt_number: 2 }, CampaignCommand::Pause),
    Ok(ControllerDirective::BeginPause)
);
assert_eq!(
    reduce_command(&CampaignStatus::Paused { reason: PauseReason::Operator }, CampaignCommand::Resume),
    Ok(ControllerDirective::BeginResume)
);
```

- [ ] **Step 2: Run reducer tests and observe RED**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner controller_types::tests --no-capture --retries 0
```

Expected: missing controller types.

- [ ] **Step 3: Implement the complete value layer**

Define `CampaignCommand::{Start,Pause,Resume,Stop}` and:

```rust
pub enum CampaignStatus {
    Idle,
    Running { attempt_number: u64 },
    Pausing,
    Paused { reason: PauseReason },
    Recovering { cycle: u8 },
    Stopping,
    Won { summary: CampaignSummary },
    Blocked { failure: CampaignFailure },
}

pub enum CampaignFailureKind {
    Checkpoint,
    Rollout,
    Helper,
    Runtime,
    Command,
    Campaign,
}

pub struct CampaignFailure {
    pub kind: CampaignFailureKind,
    pub summary: String,
}

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

Derive `Debug`, `Clone`, `PartialEq`, and `Eq` on these value types. Cap failure summaries at 2 KiB in a checked constructor. Define a public `CommandTransitionError { status: CampaignStatus, command: CampaignCommand }` that derives `thiserror::Error`, and private `ControllerDirective::{BeginStart,BeginPause,BeginResume,BeginStop}`.

Implement exhaustive `reduce_command(status, command)` and `reduce_status(status, event)` functions without wildcard arms. The private `ControllerStatusEvent` enum covers `StartCommitted`, `PauseStarted`, `PauseCommitted`, `ResumeStarted`, `RecoveryCycle`, `RunningCommitted`, `StopStarted`, `StopCommitted`, `VictoryCommitted`, `Blocked`, and `CrashNormalized`. Its private `StatusTransitionError` reports invalid actor transitions. Tests must enumerate every status/command pair and every legal internal event so recovery, win, blocked, and crash normalization are proven rather than assigned ad hoc in the actor.

- [ ] **Step 4: Run reducer tests GREEN**

Run the Step 2 command. Expected: all reducer/value tests pass.

- [ ] **Step 5: Commit**

```bash
git add codex-rs/game-runner/src/controller_types.rs \
  codex-rs/game-runner/src/controller_types_tests.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): define campaign controller protocol"
```

---

### Task 6: Extract event reduction and add durable campaign execution

**Files:**
- Create: `codex-rs/game-runner/src/campaign_event.rs`
- Create: `codex-rs/game-runner/src/campaign_event_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign_loop.rs`
- Modify: `codex-rs/game-runner/src/campaign_loop_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign.rs`

**Interfaces:**
- Consumes persistence, restored progress/gate, prompts, and event values from Tasks 3–5.
- Produces `CampaignStart`, `CampaignExecutionContext`, `CampaignExit`, and `CampaignRun::execute_controlled`; retains `CampaignRun::execute` as the Stage 4B1 compatibility wrapper.

- [ ] **Step 1: Move existing reducers without behavior change**

Move `observe_game_call_end`, `full_frame_metadata`, accepted-outcome/turn reducers, and report construction into `campaign_event.rs`. Move their tests into the sibling test file. Do not edit logic while moving it.

- [ ] **Step 2: Prove the mechanical extraction GREEN**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner campaign::campaign_loop::tests --no-capture --retries 0
rustup run 1.95.0 just test -p codex-game-runner campaign::campaign_event::tests --no-capture --retries 0
```

Expected: both suites pass and `campaign_loop.rs` is below 350 lines before new behavior.

- [ ] **Step 3: Write failing durable execution tests**

Add tests that feed synthetic events and assert exact persistence/event ordering for turn start, accepted plan, authorized mutation result, confirmed observation, loss, and win. Assert `CampaignStart::Resumed` uses restored progress and `resume_prompt`, while `Fresh` uses the initial prompt.

```rust
assert_eq!(
    observer.operations(),
    vec![
        DurableOperation::Persist,
        DurableOperation::Publish(CampaignEvent::MutationFinished(MutationResult::Success)),
    ]
);
```

- [ ] **Step 4: Implement controlled execution context**

Define:

```rust
pub(crate) enum CampaignStart {
    Fresh { target_app: String },
    Resumed { checkpoint: CampaignCheckpoint },
}

pub(crate) enum CampaignExecutionContext {
    Ephemeral { start: CampaignStart },
    Durable {
        persistence: Arc<CampaignPersistence>,
        events: tokio::sync::broadcast::Sender<CampaignEvent>,
        start: CampaignStart,
    },
}

pub(crate) enum CampaignExit {
    VerifiedWin(CampaignReport),
    Paused,
    Stopped,
    Blocked(CampaignReport),
}
```

`execute_controlled` initializes fresh or restored progress, persists before publishing durable-authority events, and finishes/observes mutations through `CampaignPersistence`. A verified win stops automatic turns and returns `CampaignExit::VerifiedWin` without publishing the winning outcome or writing `Won`; Task 7 must flush the rollout and persist `Won` first. The existing `execute` creates `CampaignExecutionContext::Ephemeral`, maps `VerifiedWin` and `Blocked` back to the Stage 4B1 report contract, and preserves all current callers and tests. Define the recording test's local `DurableOperation::{Persist,Publish}` enum in `campaign_loop_tests.rs`; it is not production API.

- [ ] **Step 5: Run focused and safety tests GREEN**

Run the Step 2 commands plus:

```bash
rustup run 1.95.0 just test -p codex-game-runner --test campaign_path --no-capture --retries 0
rustup run 1.95.0 just test -p codex-game-runner --test continuous_campaign_path --no-capture --retries 0
```

Expected: durable ordering and all existing policy/image safety paths pass.

- [ ] **Step 6: Commit**

```bash
git add codex-rs/game-runner/src/campaign_event.rs \
  codex-rs/game-runner/src/campaign_event_tests.rs \
  codex-rs/game-runner/src/campaign_loop.rs \
  codex-rs/game-runner/src/campaign_loop_tests.rs \
  codex-rs/game-runner/src/campaign.rs
git commit -m "feat(game-runner): drive campaigns from durable state"
```

---

### Task 7: Add the controller actor and safe Pause/Stop boundaries

**Files:**
- Create: `codex-rs/game-runner/src/controller.rs`
- Create: `codex-rs/game-runner/src/controller_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign_loop.rs`
- Modify: `codex-rs/game-runner/src/campaign_loop_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Produces `CampaignController::open`, `command`, `status`, `subscribe`, `wait_for_report`, and the internal serialized actor.
- Adds `WorkerCommand::{Pause,Stop}` to controlled execution and makes Pause/Stop wait for an active game call before interruption.

- [ ] **Step 1: Write failing actor and safe-boundary tests**

Use a scripted worker harness that can hold a read, click, or drag. For each operation: issue Pause or Stop, assert the policy mutation lane closes immediately, release the active call, assert no second mutation starts, then assert rollout flush occurs before paused persistence or checkpoint removal.

Also test command response ordering, broadcast capacity 256, watch status updates, slow-subscriber lag without worker blocking, active-checkpoint Start rejection, won-to-Start, and controller-lifetime lock retention after Stop.

- [ ] **Step 2: Run controller tests and observe RED**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner controller::tests --no-capture --retries 0
```

Expected: missing controller actor and worker-command handling.

- [ ] **Step 3: Implement the public handle and actor**

Define:

```rust
pub struct ControllerConfig {
    pub deployment: RunnerDeployment,
    pub runner_executable: PathBuf,
    pub limits: CampaignLimits,
}

#[derive(Debug, thiserror::Error)]
pub enum ControllerError {
    #[error("campaign checkpoint operation failed")]
    Checkpoint {
        #[source]
        source: CheckpointStoreError,
    },
    #[error("campaign persistence failed")]
    Persistence {
        #[source]
        source: PersistenceError,
    },
    #[error(transparent)]
    Runner(#[from] RunnerError),
    #[error(transparent)]
    InvalidCommand(#[from] CommandTransitionError),
    #[error("campaign controller actor closed")]
    ActorClosed,
    #[error("campaign at {path} must be resumed explicitly", path = path.display())]
    CampaignRequiresResume { path: PathBuf },
    #[error("campaign paused: {reason:?}")]
    CampaignPaused { reason: PauseReason },
    #[error("campaign stopped before victory")]
    CampaignStopped,
    #[error("campaign blocked: {failure:?}")]
    CampaignBlocked { failure: CampaignFailure },
}

pub struct CampaignController {
    request_tx: tokio::sync::mpsc::Sender<ControllerRequest>,
    status_rx: tokio::sync::watch::Receiver<CampaignStatus>,
    events_tx: tokio::sync::broadcast::Sender<CampaignEvent>,
    actor: tokio::task::JoinHandle<Result<(), ControllerError>>,
}

impl CampaignController {
    pub async fn open(config: ControllerConfig) -> Result<Self, ControllerError>;
    pub async fn command(&self, command: CampaignCommand) -> Result<CampaignStatus, ControllerError>;
    pub fn status(&self) -> CampaignStatus;
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<CampaignEvent>;
    pub async fn wait_for_report(&mut self) -> Result<CampaignReport, ControllerError>;
}
```

`ControllerRequest` contains a `CampaignCommand` and a oneshot `Result<CampaignStatus, ControllerError>` response. `open` acquires and retains `CampaignStoreGuard`, loads/normalizes the checkpoint before helper/model contact, and starts one actor. A corrupt, oversized, symlinked, unsupported, wrong-target, or malformed-thread checkpoint remains untouched and yields an `Ok` controller in `Blocked` status with a bounded failure event; only inability to acquire the store/lock makes `open` fail. A syntactically valid checkpoint whose resumed rollout returns a different thread ID is likewise preserved and becomes `Blocked` during Resume. The actor owns the guard, runtime, policy, gate, persistence, and worker join handle. `wait_for_report` resolves on `Won`; it returns the corresponding typed controller error on Paused, Stop, Blocked, or actor failure instead of waiting forever.

Extend the campaign loop's `tokio::select!` with worker commands. Track active game calls from `McpToolCallBegin`/`End`. Pause/Stop close the policy lane immediately; with no active call they interrupt immediately, otherwise they wait for that exact call's bounded end event before submitting `Op::Interrupt`. After expected abort/complete, flush rollout, then persist paused or remove the checkpoint for Stop. If flush or paused persistence fails, publish one capped failure containing both the primary and cleanup/persistence summaries, enter `Blocked`, shut down at the safest reached boundary, and never claim a durable pause.

When the worker returns `CampaignExit::VerifiedWin(report)`, the actor flushes the native rollout, persists `DurableCampaignState::Won` with the report's newest evidence reference, publishes the outcome/status only after that checkpoint succeeds, stops automatic turns, and shuts down the runtime. Any flush or checkpoint failure closes mutation authority and transitions to `Blocked`; it must never publish a durable win.

- [ ] **Step 4: Run controller and runner tests GREEN**

Run the Step 2 command, then:

```bash
rustup run 1.95.0 just test -p codex-game-runner --no-capture --retries 0
```

Expected: the controller tests and full runner suite pass without retries.

- [ ] **Step 5: Commit**

```bash
git add codex-rs/game-runner/src/controller.rs \
  codex-rs/game-runner/src/controller_tests.rs \
  codex-rs/game-runner/src/campaign_loop.rs \
  codex-rs/game-runner/src/campaign_loop_tests.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): control campaigns at safe boundaries"
```

---

### Task 8: Recover helper outages by replacing the runtime

**Files:**
- Create: `codex-rs/game-runner/src/helper_recovery.rs`
- Create: `codex-rs/game-runner/src/helper_recovery_tests.rs`
- Modify: `codex-rs/game-runner/src/helper.rs`
- Modify: `codex-rs/game-runner/src/campaign_loop.rs`
- Modify: `codex-rs/game-runner/src/campaign_loop_tests.rs`
- Modify: `codex-rs/game-runner/src/controller.rs`
- Modify: `codex-rs/game-runner/src/controller_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Produces `RecoveryLimits::stage_4b2`, documented `HelperReadiness`, `HelperRecovery`, and `RecoveryOutcome`.
- Adds a bounded worker-to-controller game-tool-failure signal with a oneshot directive, so the controller can classify the socket without losing or duplicating the model-visible MCP result.
- The controller either continues the active runtime, resumes the same rollout at generation `N+1`, or persists `Paused { HelperUnavailable }`.

- [ ] **Step 1: Write failing exact-budget tests**

Use a generic fake `HelperReadiness` and paused Tokio time. Assert first/second/third-cycle success, exact one- and two-second backoffs, never a fourth attempt, exhausted paused reason, no loss-counter change, and generation increment only after success. Add controller/loop tests proving a healthy socket receives `WorkerDirective::Continue` and the existing MCP failure remains in normal model-visible flow, while an unavailable socket receives `PauseForRecovery`, interrupts once, and exits the worker before runtime replacement.

```rust
assert_eq!(
    recovery.recover(&deployment).await?,
    RecoveryOutcome::Exhausted {
        attempts: 3,
        reason: PauseReason::HelperUnavailable {
            summary: "helper unavailable after 3 recovery cycles".to_string(),
        },
    }
);
```

- [ ] **Step 2: Run recovery tests and observe RED**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner helper_recovery::tests --no-capture --retries 0
```

Expected: missing helper recovery interfaces.

- [ ] **Step 3: Implement bounded recovery and controller runtime replacement**

Define the trait with native RPITIT:

```rust
pub trait HelperReadiness: Send + Sync {
    fn socket_is_ready(
        &self,
        socket_path: &Path,
    ) -> impl std::future::Future<Output = bool> + Send;

    fn ensure_serving(
        &self,
        deployment: &RunnerDeployment,
    ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryLimits {
    pub attempts: u8,
    pub readiness_timeout: std::time::Duration,
    pub backoffs: [std::time::Duration; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    Recovered { attempts: u8 },
    Exhausted { attempts: u8, reason: PauseReason },
}

pub struct HelperRecovery<R> {
    readiness: R,
    limits: RecoveryLimits,
}

impl<R: HelperReadiness> HelperRecovery<R> {
    pub async fn recover(
        &self,
        deployment: &RunnerDeployment,
    ) -> Result<RecoveryOutcome, RunnerError>;
}
```

Implement it for `HelperLauncher`. `RecoveryLimits::stage_4b2()` returns attempts 3, readiness timeout 15 seconds, and backoffs `[1s, 2s]`; `recover` applies `tokio::time::timeout` to every `ensure_serving` cycle and sleeps only between failed cycles.

Add these private controller/worker coordination values:

```rust
pub(crate) enum WorkerDirective {
    Continue,
    PauseForRecovery,
}

pub(crate) struct GameToolFailureSignal {
    pub tool: String,
    pub summary: String,
    pub response: tokio::sync::oneshot::Sender<WorkerDirective>,
}
```

Construct `GameToolFailureSignal` through a checked helper that validates the known tool name and caps the display summary at 2 KiB. Pass a bounded worker-event sender in the durable `CampaignExecutionContext`. When the campaign loop receives a failed game MCP `End` event, it first completes the normal durable mutation-result classification, then sends one signal and awaits the actor's directive before advancing to later campaign events. `Continue` processes that same result normally, preserving it for Codex without republishing or synthesizing a second tool result. `PauseForRecovery` submits `Op::Interrupt`, waits for the expected bounded abort/completion, and returns a new `CampaignExit::RecoveryRequired`; it never starts another mutation. Persistence failure bypasses helper recovery and enters `Blocked`.

The actor probes the socket for each signal. A healthy socket replies `Continue`. An unavailable socket closes the policy lane immediately, enters Recovering, conservatively finishes any unresolved mutation as indeterminate, and replies `PauseForRecovery`. After the worker exits, the actor flushes and shuts down the damaged runtime, then runs recovery. On success it increments the lease generation, creates restored progress and a fresh authority-empty gate, resumes the same rollout, writes Running, and submits the one-time resume prompt. Exhaustion writes `Paused { HelperUnavailable }`, shuts down any remaining runtime, and lets `wait_for_report` return `CampaignPaused`; failure never increments gameplay losses or creates a model-authored terminal outcome.

- [ ] **Step 4: Run recovery, controller, and full runner tests GREEN**

Run the Step 2 command, then:

```bash
rustup run 1.95.0 just test -p codex-game-runner controller::tests --no-capture --retries 0
rustup run 1.95.0 just test -p codex-game-runner --no-capture --retries 0
```

Expected: exact recovery budget and full runner suite pass.

- [ ] **Step 5: Commit**

```bash
git add codex-rs/game-runner/src/helper_recovery.rs \
  codex-rs/game-runner/src/helper_recovery_tests.rs \
  codex-rs/game-runner/src/helper.rs \
  codex-rs/game-runner/src/campaign_loop.rs \
  codex-rs/game-runner/src/campaign_loop_tests.rs \
  codex-rs/game-runner/src/controller.rs \
  codex-rs/game-runner/src/controller_tests.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): recover disconnected helpers"
```

---

### Task 9: Prove crash recovery, compaction survival, and eventual victory

**Files:**
- Create: `codex-rs/game-runner/tests/support/durable_game.rs`
- Modify: `codex-rs/game-runner/tests/support/mod.rs`
- Create: `codex-rs/game-runner/tests/recovery_campaign_path.rs`
- Create: `codex-rs/game-runner/tests/durable_campaign_vertical.rs`
- Modify: `codex-rs/game-runner/src/main.rs`
- Modify: `codex-rs/game-runner/src/main_tests.rs`
- Modify: `codex-rs/game-runner/src/config.rs`

**Interfaces:**
- Consumes the complete checkpoint/controller/runtime/recovery interfaces.
- Produces the two required real-path proofs and switches production startup to the controller without adding temporary resume CLI flags.

- [ ] **Step 1: Build the strict durable fake helper**

Extend the Stage 4B1 fixture with an operation-ID set that rejects duplicates, generation expectations per connection, held read/click/drag responses, deliberate disconnect steps, exact capture bytes, and a complete trace containing connection, method, generation, operation ID, hash, and arguments.

- [ ] **Step 2: Write four failing crash-boundary integrations**

In `recovery_campaign_path.rs`, cover crash after plan, after durable authorization, after result, and after confirmed observation. Each case starts a real Code Mode/UDS path, flushes and shuts down the first runtime while deliberately leaving a Running checkpoint, reopens the store to normalize Paused, explicitly resumes, and asserts:

```rust
assert_eq!(resumed.thread_id, original.thread_id);
assert_eq!(resumed.owner_lease.generation, original.owner_lease.generation + 1);
assert_eq!(trace.duplicate_operation_ids, Vec::<String>::new());
assert_eq!(resumed_first_game_call, "get_app_state");
```

For the unresolved cases, assert the old action hash is not dispatched again and the resume prompt contains its indeterminate operation summary.

- [ ] **Step 3: Run crash integrations and observe RED**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner --test recovery_campaign_path --no-capture --retries 0
```

Expected: at least one boundary exposes incomplete controller or fixture wiring; correct only the owning Stage 4B2 module and keep each fix with this task.

- [ ] **Step 4: Write the failing loss-compaction-resume-victory vertical**

Script this exact campaign in `durable_campaign_vertical.rs`:

1. attempt 1 performs one planned mutation and reports loss with `mobility_strategy()`;
2. submit `Op::Compact` using `mount_compact_user_history_with_summary_once` and wait for `ContextCompacted`;
3. flush/shut down while leaving the checkpoint Running;
4. reopen and assert it is durably Paused before helper/model contact;
5. command Resume and inspect the first request for the retained strategy exactly once;
6. capture fresh pixels, plan and click the visible restart;
7. capture, plan and execute the winning action; and
8. report a newest-capture-linked win.

Assert exact final summary `(attempts=2, losses=1, actions=3, state=Won)`, generation 2, three unique operation IDs, five post-start captures, retained mobility strategy, no stale observation/plan authority, no replay, full helper consumption, and empty screenshot spool.

- [ ] **Step 5: Run the vertical and observe RED**

```bash
rustup run 1.95.0 just test -p codex-game-runner --test durable_campaign_vertical --no-capture --retries 0
```

Expected: failure until all resume, compaction, checkpoint, and controller transitions are integrated.

- [ ] **Step 6: Wire production startup and finish the vertical**

`main::run` opens `CampaignController`, issues Start, and waits for a terminal report. Add `RunnerError::CampaignRequiresResume { path: PathBuf }`; if open finds Paused or crash-normalized state, Start returns that error and does not launch the helper/model or overwrite the checkpoint. Keep the existing successful JSON report output. Do not add `--resume` or other behavior flags.

Apply only failures demonstrated by Steps 3 and 5, then run:

```bash
rustup run 1.95.0 just test -p codex-game-runner --test recovery_campaign_path --no-capture --retries 0
rustup run 1.95.0 just test -p codex-game-runner --test durable_campaign_vertical --no-capture --retries 0
rustup run 1.95.0 just test -p codex-game-runner --test campaign_path --no-capture --retries 0
rustup run 1.95.0 just test -p codex-game-runner --test continuous_campaign_path --no-capture --retries 0
rustup run 1.95.0 just test -p codex-game-runner --no-capture --retries 0
```

Expected: all recovery and legacy safety paths pass once without retries.

- [ ] **Step 7: Commit**

```bash
git add codex-rs/game-runner/tests/support/durable_game.rs \
  codex-rs/game-runner/tests/support/mod.rs \
  codex-rs/game-runner/tests/recovery_campaign_path.rs \
  codex-rs/game-runner/tests/durable_campaign_vertical.rs \
  codex-rs/game-runner/src/main.rs \
  codex-rs/game-runner/src/main_tests.rs \
  codex-rs/game-runner/src/config.rs
git commit -m "feat(game-runner): resume durable campaigns to victory"
```

---

### Task 10: Verify Stage 4B2 completion and scope

**Files:**
- Modify only if verification exposes a defect: the owning Task 1–9 files.

**Interfaces:**
- Produces a clean, built, review-sized Stage 4B2 ready for the focused TUI design.

- [ ] **Step 1: Audit scope and module/commit sizes**

From the repository root:

```bash
git status --short
git diff cefbb48f1e..HEAD --stat
git diff cefbb48f1e..HEAD --numstat
git diff cefbb48f1e..HEAD --name-only
find codex-rs/game-runner/src -maxdepth 1 -name '*.rs' -print0 | xargs -0 wc -l | sort -n
git log --format='%h %s' --reverse cefbb48f1e..HEAD
```

Confirm only the plan, `codex-game-runner`, the one `codex-core-api` re-export, and any generated Bazel lock update changed. Confirm every new production module is below 500 lines and every complex commit below 500 changed lines.

- [ ] **Step 2: Run the final regression boundary before lint/format**

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner --no-capture --retries 0
rustup run 1.95.0 cargo check -p codex-core-api
```

Expected: the complete runner suite passes with no retry, and the facade compiles. Do not run the complete Codex workspace suite because `codex-core`, common, and protocol logic did not change.

- [ ] **Step 3: Inspect completion evidence**

Read the final vertical assertions and checkpoint JSON. Confirm the resumed thread ID is unchanged, generation increments once, strategy survives compaction, first resumed game call is capture, unresolved operations are not replayed, restarts are planned visible clicks, final win references the newest capture, checkpoint remains within 256 KiB, and screenshot spool is empty.

- [ ] **Step 4: Run final scoped fix and formatting**

```bash
rustup run 1.95.0 just fix -p codex-game-runner
rustup run 1.95.0 just fmt
```

Expected: no Clippy warnings and no formatting errors. Do not rerun tests after this step.

- [ ] **Step 5: Build the final binary**

```bash
rustup run 1.95.0 cargo build -p codex-game-runner
```

Expected: `codex-rs/target/debug/codex-game-runner` is produced from final formatted source.

- [ ] **Step 6: Commit only verification-derived corrections**

If verification changed source, add the smallest red reproducer in its owning test file, apply the correction, repeat Steps 2–5 in order, and commit with:

```bash
git commit -m "fix(game-runner): correct durable recovery boundary"
```

Do not create an empty completion commit.

## Completion checklist

- [ ] Running crash state reloads durably as paused before helper/model contact.
- [ ] Resume uses the same native rollout at generation `N+1` with no stale authority.
- [ ] Mutation authority is checkpointed before dispatch and never replayed.
- [ ] Pause and Stop wait for bounded safe boundaries and prevent later actions.
- [ ] Helper recovery uses exactly three cycles or persists a resumable pause.
- [ ] Strategy and counts survive native compaction and process restart.
- [ ] The fake campaign loses, compacts, crashes, resumes, visibly restarts, and wins.
- [ ] Typed bounded controller status/activity is ready for the future TUI.
- [ ] Full runner tests, scoped Clippy, formatting, and final binary build succeed.
- [ ] No TUI, helper packaging, real-game execution, or workspace stripping is mixed in.
