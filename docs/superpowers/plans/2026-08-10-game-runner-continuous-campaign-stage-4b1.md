# Game Runner Continuous Campaign Stage 4B1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the Stage 4A one-action canary into a bounded-per-turn, continuous headless GPT-5.6-Sol campaign that learns across visible losses and stops normally only on a fresh-evidence-linked full-game win.

**Architecture:** Keep the existing Codex thread, code-mode, dynamic-tool, MCP policy, UDS image bridge, and signed-helper path. Add typed bounded strategy/outcome values, an eight-action `ActionBatch` inside `DecisionGate`, a focused `CampaignProgress` reducer for checked counters and loss transitions, and a serial event loop that automatically opens fresh turns until victory. Prove the behavior through the real runner surface with a scripted fake game that loses twice and then wins.

**Tech Stack:** Rust 1.95, Tokio, serde/serde_json, Codex core API, code-mode, MCP over Unix sockets, `pretty_assertions`, `core_test_support`, and `just`/Nextest.

## Global Constraints

- Implement inline in the existing checkout on `main`; do not create a worktree or dispatch subagents.
- Do not add dependencies or change `Cargo.toml`, `Cargo.lock`, or `MODULE.bazel.lock`.
- Do not change `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR`, `CODEX_SANDBOX_ENV_VAR`, or generic MCP behavior.
- Keep the game MCP surface exactly `get_app_state`, `wait`, `click`, `drag`, and `focus_click`; `zoom` remains denied.
- Preserve the exact owner metadata, durable `operation_id`, action SHA-256, signed-helper launch, image verification, and per-mutation focus borrowing from Stage 4A.
- Every physical mutation requires one fresh full-frame observation, one accepted exact plan, one consumed authorization, and one fresh post-mutation observation.
- One Codex turn authorizes at most eight mutations; a healthy campaign has no total turn, action, attempt, loss, or elapsed-time stopping limit.
- Use a 15-minute turn deadline, five-minute post-mutation deadline, and 30-second safe-interrupt deadline.
- Keep the strategy record at or below 16 KiB serialized JSON, outcome payloads at or below 24 KiB, plans at or below 12 KiB, and individual non-strategy strings at or below 2 KiB.
- Retain at most 64 recent turn IDs; the canonical rollout remains the unbounded detailed history owned by Codex.
- Use checked `u64` campaign/audit counters; overflow becomes `terminal_block` and never wraps.
- Product code remains macOS-specific, while hermetic unit and integration tests continue to compile on all repository-supported platforms.
- Do not add durability, crash recovery, helper reconnection, Pause/Resume/Stop, TUI code, helper packaging, or real-game unattended execution in Stage 4B1.
- Keep complex commits below 500 changed lines, all commits below 800 changed lines, and production Rust modules below 500 lines excluding tests.
- Follow TDD for every behavior change: observe the focused test fail, implement the minimum behavior, then run the focused test and `just test -p codex-game-runner` before committing.
- Use `pretty_assertions::assert_eq` and compare complete values where practical.
- After all code changes, run scoped tests, then `just fix -p codex-game-runner`, then `just fmt`; do not rerun tests after the final fix/format pass.

---

## File Map

### New production modules

- `codex-rs/game-runner/src/strategy.rs`: typed `StrategyRecord`, field/aggregate limits, and validation errors.
- `codex-rs/game-runner/src/outcome.rs`: exhaustive loss/win/terminal-block payloads, shared evidence accessors, and aggregate validation.
- `codex-rs/game-runner/src/action_batch.rs`: fixed eight-action batch state independent of the decision mutex.
- `codex-rs/game-runner/src/campaign_progress.rs`: checked campaign counters, bounded recent turn IDs, deadlines, loss application, and continuation directives.
- `codex-rs/game-runner/src/campaign_prompt.rs`: stable initial, ordinary-continuation, and next-attempt prompts.

### New tests and fixtures

- `codex-rs/game-runner/src/strategy_tests.rs`
- `codex-rs/game-runner/src/outcome_tests.rs`
- `codex-rs/game-runner/src/action_batch_tests.rs`
- `codex-rs/game-runner/src/campaign_progress_tests.rs`
- `codex-rs/game-runner/src/campaign_prompt_tests.rs`
- `codex-rs/game-runner/tests/support/continuous_game.rs`: ordered fake-game MCP script and trace.
- `codex-rs/game-runner/tests/continuous_campaign_path.rs`: repeated-action and multi-loss eventual-victory verticals.

### Existing modules changed

- `decision.rs` / `decision_tests.rs`: delegate batch and outcome validation; permit repeated verified cycles.
- `campaign_tools.rs` / `campaign_tools_tests.rs`: expose the exhaustive bounded outcome schema.
- `campaign.rs` / `campaign_tests.rs`: retain only public runner types and migrate Stage 4A terminal semantics.
- `campaign_loop.rs` / `campaign_loop_tests.rs`: continuous event loop, safe loss/timeout interruption, and repeated turns.
- `campaign_report.rs`: bounded aggregate report rather than an unbounded turn-ID list.
- `policy.rs` / `policy_tests.rs`: checked audit types and batch-exhaustion denial text.
- `lib.rs`: explicit exports for the new public report/outcome/strategy types.
- `main.rs` / `main_tests.rs`: use Stage 4B1 production limits.
- `tests/campaign_path.rs` and `tests/support/campaign.rs`: migrate Stage 4A fixtures to new limit/outcome/report types while retaining mismatch and missing-evidence safety coverage.

---

### Task 1: Add the bounded strategic record

**Files:**
- Create: `codex-rs/game-runner/src/strategy.rs`
- Create: `codex-rs/game-runner/src/strategy_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Consumes: serde serialization already available in `codex-game-runner`.
- Produces: `StrategyRecord`, `StrategyValidationError`, and `StrategyRecord::validate(&self) -> Result<(), StrategyValidationError>` for Task 2 and campaign reporting.

- [ ] **Step 1: Add failing validation and round-trip tests**

Add `mod strategy;` in `lib.rs`, export the two public types, and create `strategy_tests.rs` with complete-value assertions:

```rust
use pretty_assertions::assert_eq;

use super::StrategyRecord;
use super::StrategyValidationError;

fn strategy() -> StrategyRecord {
    StrategyRecord {
        summary: "Develop economy before committing to the boss".to_string(),
        confirmed_mechanics: vec!["Shop purchases use drag-to-buy".to_string()],
        failed_approaches: vec!["Early all-in left no boss answer".to_string()],
        shop_and_boss_notes: vec!["Preserve one reroll for the boss shop".to_string()],
        next_attempt_priorities: vec![
            "Buy mobility".to_string(),
            "Keep a defensive superpower".to_string(),
        ],
    }
}

#[test]
fn bounded_strategy_round_trips_as_one_typed_value() -> anyhow::Result<()> {
    let expected = strategy();
    expected.validate()?;
    assert_eq!(
        serde_json::from_slice::<StrategyRecord>(&serde_json::to_vec(&expected)?)?,
        expected
    );
    Ok(())
}

#[test]
fn strategy_rejects_field_collection_and_aggregate_overflow() {
    let mut oversized_field = strategy();
    oversized_field.confirmed_mechanics[0] = "x".repeat(513);
    assert_eq!(
        oversized_field.validate(),
        Err(StrategyValidationError::StringTooLarge {
            field: "confirmed_mechanics[0]".to_string(),
            max_bytes: 512,
        })
    );

    let mut missing_priority = strategy();
    missing_priority.next_attempt_priorities.clear();
    assert!(matches!(
        missing_priority.validate(),
        Err(StrategyValidationError::InvalidItemCount { .. })
    ));

    let aggregate = StrategyRecord {
        summary: "x".repeat(2 * 1024),
        confirmed_mechanics: vec!["x".repeat(512); 24],
        failed_approaches: vec!["x".repeat(512); 16],
        shop_and_boss_notes: vec!["x".repeat(512); 24],
        next_attempt_priorities: vec!["x".repeat(512); 8],
    };
    assert_eq!(
        aggregate.validate(),
        Err(StrategyValidationError::StrategyTooLarge {
            max_bytes: 16 * 1024,
        })
    );
}
```

- [ ] **Step 2: Run the focused test and observe red**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner strategy::tests
```

Expected: compilation fails because `StrategyRecord` and `StrategyValidationError` do not exist.

- [ ] **Step 3: Implement the typed record and exact validation**

Create `strategy.rs` with these public shapes and no additional storage abstraction:

```rust
use serde::Deserialize;
use serde::Serialize;

const SUMMARY_BYTES: usize = 2 * 1024;
const ITEM_BYTES: usize = 512;
const STRATEGY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StrategyRecord {
    pub summary: String,
    pub confirmed_mechanics: Vec<String>,
    pub failed_approaches: Vec<String>,
    pub shop_and_boss_notes: Vec<String>,
    pub next_attempt_priorities: Vec<String>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum StrategyValidationError {
    #[error("{field} exceeds the {max_bytes}-byte limit")]
    StringTooLarge { field: String, max_bytes: usize },
    #[error("{field} must contain between {min} and {max} items")]
    InvalidItemCount {
        field: String,
        min: usize,
        max: usize,
    },
    #[error("strategy exceeds the {max_bytes}-byte limit")]
    StrategyTooLarge { max_bytes: usize },
    #[error("failed to encode strategy")]
    Encoding,
}
```

Implement `validate` with exact collection bounds `0..=24`, `0..=16`, `0..=24`, and `1..=8`; validate byte lengths with `str::len`; then serialize once and enforce `<= STRATEGY_BYTES`. Use small reusable validation functions because every collection calls them more than once.

- [ ] **Step 4: Run focused and crate tests**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner strategy::tests
rustup run 1.95.0 just test -p codex-game-runner
```

Expected: the strategy tests and all existing runner tests pass.

- [ ] **Step 5: Commit**

```bash
git add codex-rs/game-runner/src/strategy.rs \
  codex-rs/game-runner/src/strategy_tests.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): add bounded campaign strategy"
```

---

### Task 2: Replace canary outcomes with exhaustive campaign outcomes

**Files:**
- Create: `codex-rs/game-runner/src/outcome.rs`
- Create: `codex-rs/game-runner/src/outcome_tests.rs`
- Modify: `codex-rs/game-runner/src/decision.rs`
- Modify: `codex-rs/game-runner/src/decision_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign_tools.rs`
- Modify: `codex-rs/game-runner/src/campaign_tools_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign_tests.rs`
- Modify: `codex-rs/game-runner/tests/campaign_path.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Consumes: `StrategyRecord::validate` from Task 1 and `ObservationEvidence` from `decision.rs`.
- Produces: exhaustive `OutcomeDraft::{Loss, Win, TerminalBlock}`, `OutcomeKind`, `ReportedOutcome`, `OutcomeValidationError`, and shared accessors `kind`, `observation_reference`, and `validate`.

- [ ] **Step 1: Write failing exhaustive outcome tests**

Create `outcome_tests.rs` with a valid strategy fixture and these behaviors:

```rust
#[test]
fn loss_requires_and_preserves_a_complete_replacement_strategy() -> anyhow::Result<()> {
    let draft = OutcomeDraft::Loss {
        observation_reference: "sha256:loss".to_string(),
        visible_evidence_summary: "The defeat screen is visible".to_string(),
        lesson: "The build lacked boss mobility".to_string(),
        strategy: strategy(),
    };
    draft.validate()?;
    assert_eq!(draft.kind(), OutcomeKind::Loss);
    assert_eq!(draft.observation_reference(), "sha256:loss");
    assert_eq!(
        serde_json::to_value(&draft)?,
        json!({
            "outcome": "loss",
            "observation_reference": "sha256:loss",
            "visible_evidence_summary": "The defeat screen is visible",
            "lesson": "The build lacked boss mobility",
            "strategy": strategy(),
        })
    );
    Ok(())
}

#[test]
fn outcomes_are_exhaustive_and_bounded() {
    let win = OutcomeDraft::Win {
        observation_reference: "sha256:win".to_string(),
        visible_evidence_summary: "x".repeat(2049),
        lesson: "won".to_string(),
    };
    assert!(matches!(
        win.validate(),
        Err(OutcomeValidationError::StringTooLarge { .. })
    ));
    assert!(serde_json::from_value::<OutcomeDraft>(json!({
        "outcome": "canary_complete",
        "observation_reference": "sha256:old",
        "visible_evidence_summary": "old",
        "lesson": "old"
    })).is_err());
}
```

Update the campaign-tool schema test to expect exactly `loss`, `win`, and `terminal_block`, with `strategy` required only in the loss branch.

- [ ] **Step 2: Run the focused tests and observe red**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner outcome::tests
rustup run 1.95.0 just test -p codex-game-runner specs_expose_only_two_strict_direct_tools
```

Expected: the new module/types are missing and the existing schema still contains `canary_complete`.

- [ ] **Step 3: Implement `outcome.rs` and delegate decision validation**

Use an internally tagged exhaustive enum so loss cannot deserialize without a strategy:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutcomeDraft {
    Loss {
        observation_reference: String,
        visible_evidence_summary: String,
        lesson: String,
        strategy: StrategyRecord,
    },
    Win {
        observation_reference: String,
        visible_evidence_summary: String,
        lesson: String,
    },
    TerminalBlock {
        observation_reference: String,
        visible_evidence_summary: String,
        lesson: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Loss,
    Win,
    TerminalBlock,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportedOutcome {
    pub observation: ObservationEvidence,
    pub draft: OutcomeDraft,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum OutcomeValidationError {
    #[error("{field} exceeds the {max_bytes}-byte limit")]
    StringTooLarge { field: String, max_bytes: usize },
    #[error("outcome exceeds the {max_bytes}-byte limit")]
    OutcomeTooLarge { max_bytes: usize },
    #[error(transparent)]
    Strategy(#[from] StrategyValidationError),
    #[error("failed to encode outcome")]
    Encoding,
}
```

Move outcome types out of `decision.rs`; make `DecisionError` contain `InvalidOutcome(#[from] OutcomeValidationError)`; replace its old 8 KiB/string validation with `draft.validate()` and `draft.observation_reference()`. `OutcomeDraft::validate` enforces each common string at 2 KiB, validates loss strategy, and enforces the serialized 24 KiB aggregate.

Replace `report_outcome_spec` with `oneOf` branches whose required fields exactly match the enum. Keep `additionalProperties: false` at every object level. Change the tool description from canary language to full campaign terminal evidence.

- [ ] **Step 4: Migrate existing fixtures without weakening them**

Replace struct-literal outcomes with enum variants. The Stage 4A canary-complete unit case is removed because that product behavior is removed; retain win, loss, terminal-block, stale-evidence, before-mutation, and missing-after-evidence assertions. Update JSON scripts that report `win` or `terminal_block`; they do not receive a strategy. Any JSON loss fixture must include the complete typed strategy.

- [ ] **Step 5: Run focused and full runner tests**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner outcome::tests
rustup run 1.95.0 just test -p codex-game-runner campaign_tools::tests
rustup run 1.95.0 just test -p codex-game-runner decision::tests
rustup run 1.95.0 just test -p codex-game-runner
```

Expected: all tests pass and no serialized tool schema exposes `canary_complete`.

- [ ] **Step 6: Commit**

```bash
git add codex-rs/game-runner/src/outcome.rs \
  codex-rs/game-runner/src/outcome_tests.rs \
  codex-rs/game-runner/src/decision.rs \
  codex-rs/game-runner/src/decision_tests.rs \
  codex-rs/game-runner/src/campaign_tools.rs \
  codex-rs/game-runner/src/campaign_tools_tests.rs \
  codex-rs/game-runner/src/campaign_tests.rs \
  codex-rs/game-runner/tests/campaign_path.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): define continuous campaign outcomes"
```

---

### Task 3: Make the plan gate repeatable within bounded turns

**Files:**
- Create: `codex-rs/game-runner/src/action_batch.rs`
- Create: `codex-rs/game-runner/src/action_batch_tests.rs`
- Modify: `codex-rs/game-runner/src/decision.rs`
- Modify: `codex-rs/game-runner/src/decision_tests.rs`
- Modify: `codex-rs/game-runner/src/policy.rs`
- Modify: `codex-rs/game-runner/src/policy_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Consumes: exact action planning and post-mutation evidence already owned by `DecisionGate`.
- Produces: `MAX_ACTIONS_PER_TURN: u8 = 8`, private `ActionBatch`, `DecisionGate::begin_turn()`, repeatable verified action cycles, and checked `u64` audits.

- [ ] **Step 1: Write failing `ActionBatch` tests**

Create the sibling test module and compare complete state:

```rust
#[test]
fn batch_authorizes_exactly_eight_actions_and_resets() -> anyhow::Result<()> {
    let mut batch = ActionBatch::new();
    for used in 1..=MAX_ACTIONS_PER_TURN {
        batch.authorize()?;
        assert_eq!((batch.used(), batch.is_closed()), (used, false));
    }
    assert_eq!(batch.authorize(), Err(ActionBatchError::Exhausted));
    batch.reset();
    assert_eq!(batch, ActionBatch::new());
    Ok(())
}

#[test]
fn closed_batch_rejects_actions_until_reset() {
    let mut batch = ActionBatch::new();
    batch.close();
    assert_eq!(batch.authorize(), Err(ActionBatchError::Closed));
}
```

- [ ] **Step 2: Write the failing repeatable-gate test**

Replace `authorized_mutation_exhausts_the_stage_budget` with a loop that performs eight complete `observe -> plan -> authorize -> result -> observe` cycles, asserts the ninth exact plan is denied without deleting the latest observation, then calls `begin_turn` and proves a ninth campaign action can be planned only after a new capture.

Also add a complete snapshot assertion showing `batch_actions == 8`, `mutation_authorizations == 8`, no outstanding post-mutation evidence, and the eighth mutation retained as the latest evidence.

- [ ] **Step 3: Run focused tests and observe red**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner action_batch::tests
rustup run 1.95.0 just test -p codex-game-runner repeatable_action_batch
```

Expected: the new batch module and `begin_turn` do not exist; the old gate denies the second action.

- [ ] **Step 4: Implement the fixed batch and checked counters**

Create `action_batch.rs` with:

```rust
pub const MAX_ACTIONS_PER_TURN: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActionBatch {
    used: u8,
    closed: bool,
}

#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ActionBatchError {
    #[error("the eight-action turn batch is exhausted; verify the latest action and finish this turn")]
    Exhausted,
    #[error("the action batch is closed by a reported campaign outcome")]
    Closed,
}
```

`authorize` uses `checked_add`, rejects `closed`, and rejects `used >= MAX_ACTIONS_PER_TURN`. `reset` returns to open/zero; `close` preserves `used`.
Expose crate-private read-only `used(&self) -> u8` and `is_closed(&self) -> bool`
methods for the decision snapshot and sibling tests; do not expose mutation of
batch state outside this module.

In `DecisionState`, replace `mutation_budget_consumed: bool` with `batch: ActionBatch`. Add `batch_actions: u8` to `DecisionSnapshot` and change every `DecisionAudit` field to `u64`. Use one private checked-increment function returning `DecisionError::CounterOverflow { counter }`; directly test that helper with `u64::MAX` from `decision_tests.rs` rather than adding a test-only public API.

`DecisionGate::begin_turn` must reset the batch, clear observation/plan/mutation/outcome authority, and keep observation generation, plan sequence, owner generation, and cumulative audits. `report_outcome` closes the batch.

For ordinary mutation denial, continue consuming the attempted plan and requiring re-observation. For batch exhaustion specifically, consume only the ninth plan, retain the newest observation for possible loss/win reporting, leave `requires_post_mutation_observation` false, and return the explicit batch error. This guarantees that the eighth action can still be classified.

- [ ] **Step 5: Update policy audit types and assertions**

Change `PolicyAudit` counters to `u64`. Preserve exact owner/action metadata tests and add an assertion that the denied ninth call never carries `operation_id` or reaches the helper-facing allow result.

- [ ] **Step 6: Run focused and full runner tests**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner action_batch::tests
rustup run 1.95.0 just test -p codex-game-runner decision::tests
rustup run 1.95.0 just test -p codex-game-runner policy::tests
rustup run 1.95.0 just test -p codex-game-runner
```

Expected: eight cycles pass, the ninth is denied safely, a new turn demands fresh pixels, and all Stage 4A safety tests remain green.

- [ ] **Step 7: Commit**

```bash
git add codex-rs/game-runner/src/action_batch.rs \
  codex-rs/game-runner/src/action_batch_tests.rs \
  codex-rs/game-runner/src/decision.rs \
  codex-rs/game-runner/src/decision_tests.rs \
  codex-rs/game-runner/src/policy.rs \
  codex-rs/game-runner/src/policy_tests.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): allow bounded repeated actions"
```

---

### Task 4: Add continuous campaign progress and bounded reporting

**Files:**
- Create: `codex-rs/game-runner/src/campaign_progress.rs`
- Create: `codex-rs/game-runner/src/campaign_progress_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign.rs`
- Modify: `codex-rs/game-runner/src/campaign_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign_report.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`
- Modify: `codex-rs/game-runner/tests/campaign_path.rs`

**Interfaces:**
- Consumes: `ReportedOutcome`, `OutcomeDraft`, `StrategyRecord`, checked decision audits, and latest gate evidence.
- Produces: `CampaignLimits::stage_4b1`, private `CampaignProgress`, `CampaignDirective`, `ContinuationReason`, and the bounded aggregate `CampaignReport` used by Task 5.

- [ ] **Step 1: Write failing campaign-progress tests**

Create tests for the complete public projection:

```rust
#[test]
fn two_losses_replace_strategy_and_never_complete_the_campaign() -> anyhow::Result<()> {
    let mut progress = CampaignProgress::new(limits());
    progress.on_turn_started("turn-1".to_string())?;
    assert_eq!(
        progress.accept_outcome(&loss_outcome("sha256:loss-1", strategy("economy")))?,
        CampaignDirective::InterruptThenContinue(ContinuationReason::NewAttempt)
    );
    progress.on_turn_started("turn-2".to_string())?;
    assert_eq!(
        progress.accept_outcome(&loss_outcome("sha256:loss-2", strategy("mobility")))?,
        CampaignDirective::InterruptThenContinue(ContinuationReason::NewAttempt)
    );
    assert_eq!(
        progress.summary(),
        CampaignSummary {
            attempt_number: 3,
            total_turns: 2,
            total_actions: 0,
            losses: 2,
            strategy: Some(strategy("mobility")),
            recent_turn_ids: vec!["turn-1".to_string(), "turn-2".to_string()],
        }
    );
    Ok(())
}

#[test]
fn recent_turn_ids_keep_only_the_newest_sixty_four() -> anyhow::Result<()> {
    let mut progress = CampaignProgress::new(limits());
    for turn in 1..=65 {
        progress.on_turn_started(format!("turn-{turn}"))?;
    }
    assert_eq!(progress.summary().recent_turn_ids.len(), 64);
    assert_eq!(progress.summary().recent_turn_ids[0], "turn-2");
    Ok(())
}
```

Add tests for `Win -> Complete(Won)`, `TerminalBlock -> Block`, checked counter overflow using the private increment helper, post-mutation deadline clearing after fresh evidence, 15-minute turn deadline reset, and a 30-second expected-interrupt deadline.
Also reject a retained turn ID above 2 KiB before it enters the deque.

- [ ] **Step 2: Run focused tests and observe red**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner campaign_progress::tests
```

Expected: the progress module, new limits, summary, and directives do not exist.

- [ ] **Step 3: Extract and implement `CampaignProgress`**

Move transient progress/deadline logic out of `campaign.rs`. Define:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CampaignLimits {
    pub turn_timeout: Duration,
    pub post_mutation_timeout: Duration,
    pub interrupt_timeout: Duration,
}

impl CampaignLimits {
    pub fn stage_4b1() -> Self {
        Self {
            turn_timeout: Duration::from_secs(15 * 60),
            post_mutation_timeout: Duration::from_secs(5 * 60),
            interrupt_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CampaignSummary {
    pub attempt_number: u64,
    pub total_turns: u64,
    pub total_actions: u64,
    pub losses: u64,
    pub strategy: Option<StrategyRecord>,
    pub recent_turn_ids: Vec<String>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub(crate) enum CampaignProgressError {
    #[error("campaign counter {counter} overflowed")]
    CounterOverflow { counter: &'static str },
    #[error("mutation authorization audit regressed from {previous} to {actual}")]
    ActionAuditRegressed { previous: u64, actual: u64 },
    #[error("campaign outcome was applied more than once")]
    OutcomeAlreadyApplied,
    #[error("safe interruption is already pending")]
    InterruptAlreadyPending,
    #[error("no safe interruption is pending")]
    MissingPendingInterrupt,
    #[error("turn id exceeds the 2048-byte limit")]
    TurnIdTooLarge,
}
```

Use `VecDeque<String>` internally and pop the front before pushing item 65.
Validate turn-ID bytes before retention. Synchronize `total_actions` from the
monotonic `DecisionAudit::mutation_authorizations`, rejecting regression or
overflow. Apply each accepted outcome exactly once; for loss, install the
replacement strategy and checked-increment losses and attempt. Reducer methods
return `Result<_, CampaignProgressError>`; `campaign_loop.rs` converts any such
error into an evidence-preserving `terminal_block` report.

Use these exact reducer enums so loop code and tests agree on transition names:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuationReason {
    Ordinary,
    NewAttempt,
    TurnTimeout,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CampaignDirective {
    SubmitContinuation(ContinuationReason),
    InterruptThenContinue(ContinuationReason),
    Complete(CampaignTerminalState),
    Block(String),
}
```

`CampaignProgress::accept_outcome(&ReportedOutcome)` returns
`InterruptThenContinue(NewAttempt)` for loss, `Complete(Won)` for win, and
`Block` for a model-reported terminal block. `on_turn_complete` returns
`SubmitContinuation(Ordinary)` when no outcome was accepted. Store one
`Option<ContinuationReason>` pending safe interruption; `begin_interrupt`,
`complete_expected_interrupt`, and the deadline reducer are the only methods
that mutate it.

`CampaignTerminalState` becomes exhaustive `Won | TerminalBlock`; only `Won` is success. Delete `CanaryComplete` and `LossObserved`. There is no `max_turns` or total campaign deadline.

- [ ] **Step 4: Expand the bounded report**

Replace `turn_ids` with the flattened `CampaignSummary` fields in `CampaignReport`:

```rust
pub struct CampaignReport {
    pub terminal_state: CampaignTerminalState,
    pub thread_id: String,
    pub attempt_number: u64,
    pub total_turns: u64,
    pub total_actions: u64,
    pub losses: u64,
    pub strategy: Option<StrategyRecord>,
    pub recent_turn_ids: Vec<String>,
    // existing rollout/evidence/lease/audit/failure fields remain
}
```

Update `CampaignReportContext` to carry `CampaignSummary`. Preserve latest before/after/plan/mutation/outcome projection and the no-image-bytes assertion.

- [ ] **Step 5: Migrate existing limit/report fixtures**

Replace each literal `CampaignLimits { max_turns, total_timeout, ... }` with explicit `turn_timeout`, `post_mutation_timeout`, and `interrupt_timeout`. Update old report assertions to use `recent_turn_ids` and aggregate counters. Do not add a test-only total campaign cap.

- [ ] **Step 6: Run focused and full runner tests**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner campaign_progress::tests
rustup run 1.95.0 just test -p codex-game-runner campaign::tests
rustup run 1.95.0 just test -p codex-game-runner
```

Expected: progress/report tests pass, losses are nonterminal, and all compile-time migrations are complete.

- [ ] **Step 7: Commit**

```bash
git add codex-rs/game-runner/src/campaign_progress.rs \
  codex-rs/game-runner/src/campaign_progress_tests.rs \
  codex-rs/game-runner/src/campaign.rs \
  codex-rs/game-runner/src/campaign_tests.rs \
  codex-rs/game-runner/src/campaign_report.rs \
  codex-rs/game-runner/src/lib.rs \
  codex-rs/game-runner/tests/campaign_path.rs
git commit -m "feat(game-runner): track continuous campaign progress"
```

---

### Task 5: Run automatic bounded turns until victory

**Files:**
- Create: `codex-rs/game-runner/src/campaign_prompt.rs`
- Create: `codex-rs/game-runner/src/campaign_prompt_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign_loop.rs`
- Modify: `codex-rs/game-runner/src/campaign_loop_tests.rs`
- Modify: `codex-rs/game-runner/src/campaign.rs`
- Modify: `codex-rs/game-runner/tests/campaign_path.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Consumes: `CampaignProgress`, `CampaignDirective`, `ContinuationReason`, `DecisionGate::begin_turn`, and terminal outcomes from Tasks 2–4.
- Produces: stable full-game prompts and a serial loop that begins fresh batches, safely interrupts on loss/timeout, automatically continues ordinary finals, and returns only on win/block.

- [ ] **Step 1: Write failing prompt snapshots as complete strings**

Create prompt tests that compare exact text, including all behavioral requirements rather than substring-only tests:

```rust
#[test]
fn initial_prompt_requires_pixel_planning_batches_losses_and_victory() {
    assert_eq!(
        initial_prompt("Gambonanza"),
        concat!(
            "Play the currently visible Gambonanza game until a full-game victory is visibly verified. ",
            "Use only fresh full-frame pixels from mcp__game__get_app_state. Before every click, drag, ",
            "or focus-click, call game_runner.record_plan with two to four candidates and one exact action. ",
            "After every dispatched mutation, capture fresh pixels before planning again. A turn permits at ",
            "most eight mutations; finish the turn when the batch is exhausted and the runner will continue. ",
            "Call game_runner.report_outcome only for a visible loss, full-game win, or terminal infrastructure ",
            "block. A loss must include a complete replacement strategy and ends only the attempt. Use visible ",
            "restart controls on the next attempt. Difficulty, an unchanged screen, or a failed strategy is not ",
            "a stopping condition. Never retry an indeterminate mutation without observing first."
        )
    );
}
```

Add exact ordinary-continuation and `new_attempt_prompt(attempt_number)` tests. The latter refers to “the replacement strategy you just recorded” and does not duplicate the 16 KiB strategy in another user message.

- [ ] **Step 2: Write failing loop reducer tests**

Add tests around small event-handling helpers rather than constructing a fake `CodexThread`:

- normal `TurnComplete` returns `SubmitContinuation(Ordinary)`;
- accepted loss returns `InterruptThenContinue(NewAttempt)`;
- the expected `TurnAborted` after loss starts a new attempt instead of blocking;
- unexpected abort still blocks;
- win returns `Complete(Won)` immediately after the dynamic response;
- terminal block returns `Block` immediately;
- turn timeout with no unresolved mutation requests a safe interrupt;
- turn timeout or interrupt timeout with unresolved physical state blocks;
- post-mutation deadline always blocks.

Use complete directive assertions with the Task 4 signatures:

```rust
assert_eq!(
    progress.on_turn_complete(&gate.snapshot())?,
    CampaignDirective::SubmitContinuation(ContinuationReason::Ordinary)
);
assert_eq!(
    progress.accept_outcome(&loss_outcome())?,
    CampaignDirective::InterruptThenContinue(ContinuationReason::NewAttempt)
);
progress.begin_interrupt(ContinuationReason::NewAttempt, now)?;
assert_eq!(
    progress.complete_expected_interrupt()?,
    CampaignDirective::SubmitContinuation(ContinuationReason::NewAttempt)
);
assert!(matches!(
    progress.deadline_directive(&unresolved_snapshot(), now),
    Some(CampaignDirective::Block(_))
));
```

- [ ] **Step 3: Run focused tests and observe red**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner campaign_prompt::tests
rustup run 1.95.0 just test -p codex-game-runner campaign_loop::tests
```

Expected: the prompt module and continuous directives are missing; the old loop still stops at loss and finite turn count.

- [ ] **Step 4: Implement stable prompt construction**

Create `campaign_prompt.rs` with only three functions:

```rust
pub(crate) fn initial_prompt(target_app: &str) -> String;
pub(crate) fn continuation_prompt(attempt_number: u64) -> String;
pub(crate) fn new_attempt_prompt(attempt_number: u64) -> String;
```

Keep the full contract only in the initial prompt. Continuations are short and cache-stable aside from the decimal attempt number.

- [ ] **Step 5: Implement the continuous event loop**

Before submitting every initial/continuation user turn, call `gate.begin_turn()` so no previous pixels or plan cross the turn boundary. Feed every MCP completion into `observe_game_call_end`, then synchronize progress with the new gate snapshot so post-mutation deadlines clear as soon as fresh evidence arrives.

After a successful `report_outcome` dynamic response:

- `Loss`: apply the loss once, close the mutation lane, submit `Op::Interrupt`, set an expected-interrupt deadline, and wait for `TurnAborted` or `TurnComplete`; then open the next attempt turn.
- `Win`: build and return `CampaignReport { terminal_state: Won, ... }` after the tool response is accepted; shutdown later stops the thread.
- `TerminalBlock`: build and return a terminal-block report with the model's evidence.

On ordinary `TurnComplete`, submit the ordinary continuation. On a turn deadline with no unresolved mutation, submit `Op::Interrupt` and continue after the expected abort. On any timeout with `requires_post_mutation_observation`, or if the expected interrupt itself exceeds 30 seconds, fail closed. Do not immediately resubmit input while the old turn is still active.

The eighth action may still receive its required capture and terminal outcome. A denied ninth action is not an infrastructure block; the loop waits for the model to finish or the bounded turn deadline.

- [ ] **Step 6: Migrate old safety integrations to continuous semantics**

The mismatched-plan fixture must still assert that the mismatched mutation never reaches the helper. Because there is no test-only turn cap, let the mock response sequence end and assert the resulting missing-model-response infrastructure block rather than a gameplay limit. Keep the missing-after-evidence test tied to the post-mutation deadline.

- [ ] **Step 7: Run focused and full runner tests**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner campaign_prompt::tests
rustup run 1.95.0 just test -p codex-game-runner campaign_loop::tests
rustup run 1.95.0 just test -p codex-game-runner
```

Expected: every loop/prompt test passes, one-action win remains green, and safety fixtures terminate without any campaign-wide action/turn cap.

- [ ] **Step 8: Commit**

```bash
git add codex-rs/game-runner/src/campaign_prompt.rs \
  codex-rs/game-runner/src/campaign_prompt_tests.rs \
  codex-rs/game-runner/src/campaign_loop.rs \
  codex-rs/game-runner/src/campaign_loop_tests.rs \
  codex-rs/game-runner/src/campaign.rs \
  codex-rs/game-runner/tests/campaign_path.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): continue bounded turns until victory"
```

---

### Task 6: Prove repeated planned actions through the real runner path

**Files:**
- Create: `codex-rs/game-runner/tests/support/continuous_game.rs`
- Create: `codex-rs/game-runner/tests/continuous_campaign_path.rs`
- Modify: `codex-rs/game-runner/tests/support/mod.rs`

**Interfaces:**
- Consumes: public `CampaignRun`, `CampaignLimits`, `DecisionGate`, `GameCallPolicy`, `CampaignTools`, and the existing fake MCP utility functions.
- Produces: `ScriptedGame`, `ExpectedCall`, `ContinuousGameTrace`, `PlannedClickStep`, `ScriptedOutcome`, and `turn_script(&[PlannedClickStep], &ScriptedOutcome) -> anyhow::Result<String>` plus a real code-mode/dynamic-tool/policy/UDS integration proving two actions in one turn.

- [ ] **Step 1: Define the failing two-action integration**

Create `continuous_campaign_path.rs` with one mocked Sol `exec` turn that performs:

```javascript
const before1 = await tools.mcp__game__get_app_state({});
await tools.game_runner__record_plan({
  observation_reference: before1.structuredContent.artifact_uri,
  objective: "Advance the fake game from state one",
  visible_state_summary: "Fake state one is visible",
  candidates: [
    {action: "Advance", predicted_visible_consequence: "Fake state two appears"},
    {action: "Wait", predicted_visible_consequence: "Fake state one remains"}
  ],
  chosen_action: {tool: "click", arguments: {x: 180, y: 640}},
  reason: "Advance follows the fixture objective",
  expected_visible_result: "Fake state two",
  invalidation_condition: "State one changes before the click"
});
await tools.mcp__game__click({x: 180, y: 640});
const before2 = await tools.mcp__game__get_app_state({});
await tools.game_runner__record_plan({
  observation_reference: before2.structuredContent.artifact_uri,
  objective: "Advance the fake game from state two",
  visible_state_summary: "Fake state two is visible",
  candidates: [
    {action: "Finish", predicted_visible_consequence: "Victory appears"},
    {action: "Wait", predicted_visible_consequence: "Fake state two remains"}
  ],
  chosen_action: {tool: "click", arguments: {x: 240, y: 640}},
  reason: "Finish reaches the fixture victory",
  expected_visible_result: "Full victory screen",
  invalidation_condition: "State two changes before the click"
});
await tools.mcp__game__click({x: 240, y: 640});
const after = await tools.mcp__game__get_app_state({});
await tools.game_runner__report_outcome({
  outcome: "win",
  observation_reference: after.structuredContent.artifact_uri,
  visible_evidence_summary: "The full fake victory screen is visible",
  lesson: "The two planned advances completed the fake game"
});
text("verified fake victory");
```

Use complete JavaScript tool calls, distinct coordinates, distinct JPEG bytes/artifact hashes, two to four candidates per plan, and exact `win` evidence from capture 3. Assert the final report as one tuple:

```rust
assert_eq!(
    (
        report.terminal_state,
        report.attempt_number,
        report.total_turns,
        report.total_actions,
        report.losses,
        report.decision_audit.plans_accepted,
        report.decision_audit.mutation_authorizations,
    ),
    (CampaignTerminalState::Won, 1, 1, 2, 0, 2, 2)
);
```

Assert the ordered helper trace contains exactly three captures and two mutations, with each mutation's `call_id == operation_id`, correct canonical action hash, and no orphaned screenshot blobs.

- [ ] **Step 2: Run the integration and observe red**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner --test continuous_campaign_path repeated_planned_actions_share_one_bounded_turn
```

Expected: the new fixture is absent or the second action is rejected by incomplete Stage 4B1 wiring.

- [ ] **Step 3: Implement the ordered fake game fixture**

In `continuous_game.rs`, define an explicit sequence rather than a behavior-heavy fake:

```rust
pub enum ExpectedCall {
    Capture { jpeg: Vec<u8> },
    Click { arguments: Value, action_sha256: String },
    Drag { arguments: Value, action_sha256: String },
    FocusClick { arguments: Value, action_sha256: String },
}

pub struct ScriptedGame {
    pub calls: Vec<ExpectedCall>,
}

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
    TerminalBlock {
        visible_evidence_summary: String,
        lesson: String,
    },
}

pub fn turn_script(
    steps: &[PlannedClickStep],
    outcome: &ScriptedOutcome,
) -> anyhow::Result<String>;
```

`turn_script` emits the complete observe/record-plan/click sequence shown in
Step 1 for every `PlannedClickStep`, numbers local variables deterministically,
serializes the supplied exhaustive outcome as the final
`game_runner__report_outcome` arguments, sets `observation_reference` from the
last live capture variable rather than a fixture constant, and finishes with
`text(...)`.

The server performs the existing initialize/initialized/tools-list handshake, then pops exactly one expected call per request. Every call validates epoch/generation/call ID. Mutation calls additionally validate `operation_id == callId`, canonical `action_sha256`, and exact arguments. Captures write unique canonical UUID blobs and return verified metadata through the real image bridge. Any extra, missing, reordered, or mismatched call fails the helper task.

Keep this support module below 500 lines. Reuse `next_message`, `method`, `respond`, and `write_spooled_jpeg` from `tests/support/mod.rs`; make only those functions `pub(crate)` when required.

- [ ] **Step 4: Run focused and full runner tests**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner --test continuous_campaign_path repeated_planned_actions_share_one_bounded_turn
rustup run 1.95.0 just test -p codex-game-runner
```

Expected: two real code-mode mutations pass through separate fresh plans in one turn, all existing fixtures stay green, and the helper script is fully consumed.

- [ ] **Step 5: Commit**

```bash
git add codex-rs/game-runner/tests/support/continuous_game.rs \
  codex-rs/game-runner/tests/continuous_campaign_path.rs \
  codex-rs/game-runner/tests/support/mod.rs
git commit -m "test(game-runner): prove repeated planned actions"
```

---

### Task 7: Prove two losses, visible restarts, and eventual victory

**Files:**
- Modify: `codex-rs/game-runner/tests/continuous_campaign_path.rs`
- Modify: `codex-rs/game-runner/tests/support/continuous_game.rs`
- Modify: `codex-rs/game-runner/src/main.rs`
- Modify: `codex-rs/game-runner/src/main_tests.rs`

**Interfaces:**
- Consumes: scripted game fixture from Task 6 and complete Stage 4B1 loop/state/report interfaces.
- Produces: the required multi-loss eventual-victory vertical and production `CampaignLimits::stage_4b1()` wiring.

- [ ] **Step 1: Write the failing multi-loss vertical**

Build three scripts with Task 6's `turn_script` interface and these exact
values:

```rust
let turn_1 = turn_script(
    &[step("attempt one move", 180, 640, "loss screen one")],
    &loss(economy_strategy()),
)?;
let turn_2 = turn_script(
    &[
        step("restart attempt two", 510, 540, "new run screen"),
        step("attempt two move", 220, 640, "loss screen two"),
    ],
    &loss(mobility_strategy()),
)?;
let turn_3 = turn_script(
    &[
        step("restart attempt three", 510, 540, "new run screen"),
        step("attempt three winning move", 260, 640, "full victory screen"),
    ],
    &ScriptedOutcome::Win {
        visible_evidence_summary: "The full fake victory screen is visible".to_string(),
        lesson: "The mobility strategy defeated the final boss".to_string(),
    },
)?;
```

The `loss` test helper returns `ScriptedOutcome::Loss` with the supplied
strategy plus fixed bounded visible evidence and lesson strings. The
`step` helper fills all `PlannedClickStep` fields, including two fixed candidate
descriptions. These helpers are ordinary test constructors, not test-only
production APIs.

Every physical action must have its own preceding `record_plan`; no restart is synthesized by the runner. Use distinct coordinates and expected hashes so plans cannot accidentally authorize a neighboring action.

Assert the complete terminal summary:

```rust
assert_eq!(
    (
        report.terminal_state,
        report.attempt_number,
        report.total_turns,
        report.total_actions,
        report.losses,
        report.strategy.as_ref(),
        report.recent_turn_ids.len(),
    ),
    (
        CampaignTerminalState::Won,
        3,
        3,
        5,
        2,
        Some(&mobility_strategy()),
        3,
    )
);
```

Also assert five mutation traces, eight capture traces, five authorizations, no unknown tools, final win evidence tied to capture 8, the final retained strategy equals `mobility_strategy()`, full scripted-helper consumption, and an empty screenshot spool.

- [ ] **Step 2: Run the vertical and observe red**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner --test continuous_campaign_path two_losses_restart_visibly_and_eventually_win
```

Expected: the campaign stops or fails to continue after the first loss until the loss interruption/continuation state is complete.

- [ ] **Step 3: Apply only vertical-trace fixes**

Fix the smallest evidenced gap in production loop/state code. Do not add retries, prompt alternatives, durability, or TUI state. If the fix touches production files beyond those listed for this task, first update the plan's file list and verify the commit remains below 500 changed lines.

- [ ] **Step 4: Switch production from Stage 4A limits**

Change the sole production constructor in `main.rs`:

```rust
let campaign = CampaignRun::new(CampaignLimits::stage_4b1())
```

Update `main_tests.rs` only as needed to assert the production future still returns `CampaignReport`; do not add configuration knobs for behavior limits.

- [ ] **Step 5: Run focused, safety, and full runner tests**

Run:

```bash
rustup run 1.95.0 just test -p codex-game-runner --test continuous_campaign_path
rustup run 1.95.0 just test -p codex-game-runner --test campaign_path
rustup run 1.95.0 just test -p codex-game-runner
```

Expected: the repeated-action and two-loss verticals pass with no retries/flakes; mismatch and missing-after safety paths remain green; all runner tests pass.

- [ ] **Step 6: Commit**

```bash
git add codex-rs/game-runner/tests/continuous_campaign_path.rs \
  codex-rs/game-runner/tests/support/continuous_game.rs \
  codex-rs/game-runner/src/main.rs \
  codex-rs/game-runner/src/main_tests.rs
git commit -m "feat(game-runner): continue through losses to victory"
```

---

### Task 8: Verify Stage 4B1 scope and completion gate

**Files:**
- Modify only if verification exposes a Stage 4B1 defect: the owning Task 1–7 files.

**Interfaces:**
- Consumes: complete Stage 4B1 runner and tests.
- Produces: a clean, review-sized, built headless continuous campaign core ready for Stage 4B2 design.

- [ ] **Step 1: Audit commit and module size**

Run from the repository root:

```bash
stage_4b1_base=9a8fb1c59e
git status --short
git diff "$stage_4b1_base"..HEAD --stat
git diff "$stage_4b1_base"..HEAD --numstat
find codex-rs/game-runner/src -maxdepth 1 -name '*.rs' -print0 | xargs -0 wc -l | sort -n
git diff "$stage_4b1_base"..HEAD --name-only
```

Confirm every complex commit is below 500 changed lines, every commit below
800, every production module below 500 lines, and only this plan,
`codex-game-runner`, plus narrowly required `codex-core-api` files changed. No
external AutoPilot source, TUI, persistence, helper-import, or
workspace-stripping changes are allowed.

- [ ] **Step 2: Run the focused regression boundary**

Run from `codex-rs`:

```bash
rustup run 1.95.0 just test -p codex-game-runner
```

If Task 1–7 changes `codex-core-api` or the generic MCP policy seam, also run:

```bash
rustup run 1.95.0 just test -p codex-core mcp_tool_call_policy
rustup run 1.95.0 cargo check -p codex-core-api
```

Expected: all runner tests pass once with no Nextest retries or flaky classification. The multi-loss integration must report three attempts, two losses, five actions, and `Won`.

- [ ] **Step 3: Run final lint and formatting**

Run:

```bash
rustup run 1.95.0 just fix -p codex-game-runner
rustup run 1.95.0 just fmt
```

If `codex-core-api` changed, run its scoped fix before the runner fix. Expected: Clippy emits no warnings. Do not rerun tests after this final fix/format step.

- [ ] **Step 4: Build the final binary**

Run:

```bash
rustup run 1.95.0 cargo build -p codex-game-runner
```

Expected: `target/debug/codex-game-runner` is produced from the final formatted source.

- [ ] **Step 5: Inspect the fake victory evidence**

Read the final integration assertions and captured request bodies. Confirm:

- every action follows a distinct fresh observation and exact plan;
- the two loss reports carry bounded replacement strategies;
- restart is a visible planned click in attempts 2 and 3;
- no action is replayed after an indeterminate result;
- the final win references the newest capture rather than an intermediate stage;
- all helper mutations contain matching call/operation IDs and action hashes;
- the report retains only bounded recent IDs and aggregate counts; and
- no screenshot blob remains in the spool.

- [ ] **Step 6: Commit any verification-derived correction through its owner**

For any failure, add the smallest red reproducer in the owning task's test file, implement the minimal correction, rerun the focused and full runner tests, then repeat the final fix/format/build order. Use a focused commit subject such as:

```bash
git commit -m "fix(game-runner): correct continuous campaign boundary"
```

Do not create an empty completion commit.

## Completion Criteria

Stage 4B1 is complete only when:

- Tasks 1–7 are committed and within repository size limits;
- the final runner suite passes without retries or flakes;
- strategy, outcome, plan, image, recent-ID, deadline, and action-batch bounds are explicit and tested;
- the scripted game loses twice, uses planned visible restarts, and reaches verified victory;
- every mutation remains exact, single-authority, fresh-observation-linked, and durable-operation-tagged;
- the final report has exact aggregate counts and bounded retained detail;
- final scoped Clippy, formatting, and binary build succeed; and
- the work contains no Stage 4B2 durability, TUI, helper packaging, or unattended real-game execution.
