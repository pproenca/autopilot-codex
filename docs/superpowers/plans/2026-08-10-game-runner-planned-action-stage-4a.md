# Game Runner Planned-Action Stage 4A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a bounded headless campaign that requires one exact evidence-bound plan before one physical game mutation, observes the result, and emits a correlated report.

**Architecture:** Keep Codex's native Sol/code-mode/MCP loop and service two runner-owned dynamic tools from a serial `CampaignRun` event loop. A shared `DecisionGate` records full-frame evidence and plans; the existing MCP policy atomically consumes a matching plan and adds durable operation metadata immediately before dispatch. Stage 4A stops after the first authorized mutation and fresh post-action screenshot.

**Tech Stack:** Rust 1.95, Tokio, serde/serde_json, SHA-256, Codex `ThreadManager`, Codex dynamic tools, existing MCP policy contributors, existing image-aware UDS bridge, mocked Responses integration fixtures, fake canonical MCP helper.

## Global Constraints

- Execute inline in the current checkout with `superpowers:executing-plans`; do not create worktrees or dispatch subagents.
- Keep GPT-5.6-Sol and reasoning effort `high` fixed.
- Expose exactly `get_app_state`, `wait`, `click`, `drag`, and `focus_click` from the game MCP server.
- Remove `zoom` from the final runner surface and deny it if unexpectedly advertised.
- Expose only `game_runner.record_plan` and `game_runner.report_outcome` as runner-owned dynamic tools.
- Require the newest successful full-frame artifact reference before accepting a plan.
- Require the actual mutating tool and complete JSON arguments to exactly equal the planned action.
- Consume a plan on every mutation attempt, including mismatches and pre-dispatch failures.
- Permit at most one authorized or indeterminate mutation in Stage 4A.
- Never automatically retry an indeterminate mutation.
- Use the Codex call ID as both `call_id` and `operation_id`.
- Compute `action_sha256` from recursively key-sorted compact JSON containing `arguments` and `tool`.
- Cap `record_plan` input at 12 KiB, `report_outcome` input at 8 KiB, and every model-authored string at 2 KiB.
- Keep MCP parallel tool calls disabled and never hold the decision lock across I/O.
- Preserve the canonical non-ephemeral Codex rollout; do not add another event log or database.
- Do not add TUI, Pause/Resume/Stop, crash recovery, strategy persistence, helper reconnection, or repeated real-game actions.
- Add no workspace crate, dependency, config schema, app-server API, or generic `codex-core` behavior.
- Keep each complex implementation commit below 500 changed lines and every total commit below 800 changed lines.
- Keep production modules below 500 lines excluding their sibling test files.
- Use `pretty_assertions::assert_eq` and compare complete objects where practical.
- Use `just test`, never `cargo test` directly.
- Run Rust commands through `rustup run 1.95.0` because the login environment currently selects Rust 1.86.
- For local builds that compile V8, set the verified Stage 3 archive and binding environment shown in Task 7.
- After all tests, run `just fix -p codex-core-api`, `just fix -p codex-game-runner`, and `just fmt`; do not rerun tests after `fix` or `fmt`.

---

## File Structure

- `codex-rs/game-runner/src/decision.rs`: typed planned actions, canonical action hashes, observation evidence, plans, outcomes, and the synchronized decision gate.
- `codex-rs/game-runner/src/decision_tests.rs`: action and gate transition tests, added in two reviewable tasks.
- `codex-rs/game-runner/src/policy.rs`: owner metadata plus read invalidation and planned mutation authorization.
- `codex-rs/game-runner/src/policy_tests.rs`: complete game-policy decisions and audits.
- `codex-rs/game-runner/src/campaign_tools.rs`: dynamic-tool schemas, bounded decoding, gate updates, and responses.
- `codex-rs/game-runner/src/campaign_tools_tests.rs`: dynamic-tool behavioral tests.
- `codex-rs/game-runner/src/campaign.rs`: prompt, limits, serial event loop, continuation, and terminal classification.
- `codex-rs/game-runner/src/campaign_tests.rs`: pure campaign progress and limit tests.
- `codex-rs/game-runner/src/campaign_report.rs`: evidence-linked report types and construction from a gate snapshot.
- `codex-rs/game-runner/src/runtime.rs`: reusable `ThreadManager` startup and cleanup extracted from the binary.
- `codex-rs/game-runner/src/runtime_tests.rs`: cleanup/result precedence tests if extraction needs new behavioral coverage.
- `codex-rs/game-runner/src/config.rs`: final game MCP allowlist and Stage 4A errors.
- `codex-rs/game-runner/src/config_tests.rs`: fixed complete configuration projection.
- `codex-rs/game-runner/src/main.rs`: deployment parsing, bridge dispatch, runtime wiring, JSON output, and exit status only.
- `codex-rs/game-runner/src/lib.rs`: private modules and minimal public runner API.
- `codex-rs/game-runner/tests/campaign_path.rs`: mocked Responses, code mode, dynamic tools, image bridge, policy, and fake-helper verticals.
- `codex-rs/core-api/src/lib.rs`: narrow re-exports of existing dynamic-tool request/response types.

---

### Task 1: Define exact planned actions and canonical hashes

**Files:**
- Create: `codex-rs/game-runner/src/decision.rs`
- Create: `codex-rs/game-runner/src/decision_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Produces: `MouseButton`, `ClickArguments`, `DragArguments`, and `FocusClickArguments`.
- Produces: `PlannedAction::{Click, Drag, FocusClick}`.
- Produces: `PlannedAction::tool_name() -> &'static str`.
- Produces: `PlannedAction::arguments() -> serde_json::Value`.
- Produces: `PlannedAction::validate(width: u32, height: u32) -> Result<(), DecisionError>`.
- Produces: `PlannedAction::action_sha256() -> Result<String, DecisionError>`.
- Produces: `DecisionError`, extended by later tasks without changing existing variants.

- [ ] **Step 1: Register the module and write failing action tests**

Add to `src/lib.rs`:

```rust
mod decision;

pub use decision::ClickArguments;
pub use decision::DecisionError;
pub use decision::DragArguments;
pub use decision::FocusClickArguments;
pub use decision::MouseButton;
pub use decision::PlannedAction;
```

Create `decision.rs` with only the sibling test declaration:

```rust
#[cfg(test)]
#[path = "decision_tests.rs"]
mod tests;
```

Create `decision_tests.rs` with complete action projections:

```rust
use pretty_assertions::assert_eq;
use serde_json::json;

use super::ClickArguments;
use super::DecisionError;
use super::MouseButton;
use super::PlannedAction;

#[test]
fn click_action_has_exact_arguments_and_stable_hash() -> anyhow::Result<()> {
    let action = PlannedAction::Click(ClickArguments {
        x: 120,
        y: 240,
        button: None,
        count: Some(1),
    });

    assert_eq!(
        (
            action.tool_name(),
            action.arguments(),
            action.action_sha256()?,
        ),
        (
            "click",
            json!({"count": 1, "x": 120, "y": 240}),
            "bd1c262b95a3f95eaf81bc17481f5dcc19a66895cd96af45145e6fcd6363f01e"
                .to_string(),
        )
    );
    Ok(())
}

#[test]
fn planned_actions_validate_complete_image_bounds() {
    let action = PlannedAction::Click(ClickArguments {
        x: 1051,
        y: 819,
        button: Some(MouseButton::Left),
        count: Some(1),
    });

    assert_eq!(
        action.validate(/*width*/ 1051, /*height*/ 820),
        Err(DecisionError::CoordinateOutOfBounds {
            coordinate: "x".to_string(),
            value: 1051,
            upper_bound: 1050,
        })
    );
}
```

Add one deep-equality test for a drag and one for `focus_click`. Add strict
serde tests proving unknown fields, click count `0`/`4`, negative coordinates,
and invalid button values are rejected. Do not test static description text.

- [ ] **Step 2: Run the focused tests and verify red**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner decision
```

Expected: compilation fails because the planned-action types do not exist.

- [ ] **Step 3: Implement the strict action types**

Add these public shapes to `decision.rs`:

```rust
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClickArguments {
    pub x: i64,
    pub y: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<MouseButton>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DragArguments {
    pub from_x: i64,
    pub from_y: i64,
    pub to_x: i64,
    pub to_y: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FocusClickArguments {
    pub x: i64,
    pub y: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "tool", content = "arguments", rename_all = "snake_case")]
pub enum PlannedAction {
    Click(ClickArguments),
    Drag(DragArguments),
    FocusClick(FocusClickArguments),
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DecisionError {
    #[error("{coordinate} coordinate {value} is outside 0..={upper_bound}")]
    CoordinateOutOfBounds {
        coordinate: String,
        value: i64,
        upper_bound: i64,
    },
    #[error("click count must be between 1 and 3")]
    InvalidClickCount,
    #[error("failed to encode the planned action")]
    ActionEncoding,
}
```

Implement exhaustive matches for tool name, argument serialization, and every
coordinate. Validate `count.unwrap_or(1)` in `1..=3`. Treat width or height
zero as out of bounds without arithmetic underflow.

Canonical hashing must build `json!({"arguments": self.arguments(), "tool": self.tool_name()})`,
recursively sort object keys, serialize with `serde_json::to_vec`, and format
`Sha256::digest(bytes)` as lowercase hexadecimal. Arrays retain their original
order and scalar types remain unchanged.

- [ ] **Step 4: Run action tests and verify green**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner decision
```

Expected: all planned-action tests pass.

- [ ] **Step 5: Commit the action contract**

```bash
git add codex-rs/game-runner/src/decision.rs \
  codex-rs/game-runner/src/decision_tests.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): define planned game actions"
```

### Task 2: Add the synchronized decision gate

**Files:**
- Modify: `codex-rs/game-runner/src/decision.rs`
- Modify: `codex-rs/game-runner/src/decision_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Consumes: `PlannedAction` and its exact hash from Task 1.
- Produces: `ObservationEvidence`, `PlanCandidate`, `PlanDraft`, `AcceptedPlan`, `AuthorizedMutation`, `OutcomeDraft`, `ReportedOutcome`, `DecisionAudit`, and `DecisionSnapshot`.
- Produces: `DecisionGate::new(owner_generation: u64) -> Self`.
- Produces: `begin_full_observation`, `complete_full_observation`, `before_wait`, `record_plan`, `prepare_mutation`, `record_mutation_result`, `report_outcome`, `invalidate`, and `snapshot`.

- [ ] **Step 1: Write failing complete-transition tests**

Extend `decision_tests.rs` with a shared plan fixture and this primary test:

```rust
#[test]
fn one_observation_plan_mutation_and_after_observation_is_complete() -> anyhow::Result<()> {
    let gate = DecisionGate::new(1);
    gate.begin_full_observation();
    let before = gate.complete_full_observation(
        "capture-before".to_string(),
        "sha256:before".to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;
    let plan = gate.record_plan(plan_draft(
        before.reference.clone(),
        PlannedAction::Click(ClickArguments {
            x: 180,
            y: 640,
            button: None,
            count: None,
        }),
    ))?;
    let authorized = gate.prepare_mutation(
        "click",
        &json!({"x": 180, "y": 640}),
        "mutation-1",
    )?;
    gate.record_mutation_result(
        "mutation-1",
        MutationResult::Success,
    )?;
    gate.begin_full_observation();
    let after = gate.complete_full_observation(
        "capture-after".to_string(),
        "sha256:after".to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;

    assert_eq!(
        gate.snapshot(),
        DecisionSnapshot {
            owner_generation: 1,
            next_observation_generation: 3,
            observation: Some(after),
            plan: None,
            mutation: Some(MutationEvidence {
                plan,
                authorization: authorized,
                result: Some(MutationResult::Success),
            }),
            outcome: None,
            requires_post_mutation_observation: false,
            audit: DecisionAudit {
                plans_accepted: 1,
                plan_rejections: 0,
                mutation_attempts: 1,
                mutation_authorizations: 1,
                mutation_denials: 0,
            },
        }
    );
    Ok(())
}
```

Add focused transition tests proving:

1. A capture attempt invalidates the previous observation and plan even when
   no completion follows.
2. A positive wait invalidates; zero wait preserves authority.
3. An observation-reference mismatch rejects the plan.
4. A mismatched mutation consumes the plan, denies authorization, and requires
   another full observation.
5. An authorized mutation rejects every second mutation.
6. Interruption and owner-generation replacement invalidate the plan.
7. Outcome reporting rejects pre-mutation evidence and accepts only the newest
   post-mutation reference.

- [ ] **Step 2: Run decision tests and verify red**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner decision
```

Expected: compilation fails because `DecisionGate` and its state types do not
exist.

- [ ] **Step 3: Implement the gate and complete state types**

Use these core type shapes in `decision.rs`:

```rust
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ObservationEvidence {
    pub generation: u64,
    pub call_id: String,
    pub reference: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlanCandidate {
    pub action: String,
    pub predicted_visible_consequence: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlanDraft {
    pub observation_reference: String,
    pub objective: String,
    pub visible_state_summary: String,
    pub candidates: Vec<PlanCandidate>,
    pub chosen_action: PlannedAction,
    pub reason: String,
    pub expected_visible_result: String,
    pub invalidation_condition: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AcceptedPlan {
    pub id: String,
    pub observation: ObservationEvidence,
    pub draft: PlanDraft,
    pub action_sha256: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthorizedMutation {
    pub call_id: String,
    pub operation_id: String,
    pub action_sha256: String,
    pub tool: String,
    pub arguments: Value,
}

pub struct DecisionGate {
    state: Mutex<DecisionState>,
}
```

Assign plan IDs deterministically as `plan-{observation_generation}-{plan_sequence}`.
This ID is correlation data, not a security capability. Validate two to four
candidates, every string at 2 KiB, the complete serialized plan at 12 KiB,
the observation reference, image bounds, and chosen action hash inside
`record_plan`.

`prepare_mutation` must take the optional plan before matching. Set the fresh
observation requirement for every mutation attempt. Set the single mutation
budget only after exact match and authorization. Return typed `DecisionError`
variants with model-safe messages for absent plan, action mismatch, second
mutation, stale evidence, invalid bounds, and invalid state.

Define `OutcomeDraft` with `loss`, `win`, and `terminal_block`, enforce the 8
KiB/2 KiB bounds, and accept it only when the current observation was captured
after the authorized mutation.

- [ ] **Step 4: Run decision tests and verify green**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner decision
```

Expected: every action and gate transition test passes.

- [ ] **Step 5: Commit the decision gate**

```bash
git add codex-rs/game-runner/src/decision.rs \
  codex-rs/game-runner/src/decision_tests.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): add planned action gate"
```

### Task 3: Enforce plans in the game MCP policy

**Files:**
- Modify: `codex-rs/game-runner/src/policy.rs`
- Modify: `codex-rs/game-runner/src/policy_tests.rs`
- Modify: `codex-rs/game-runner/src/config.rs`
- Modify: `codex-rs/game-runner/src/config_tests.rs`

**Interfaces:**
- Consumes: `Arc<DecisionGate>` and `AuthorizedMutation` from Task 2.
- Changes: `GameCallPolicy::new(epoch, generation, gate)`.
- Preserves: lease metadata for every allowed game call.
- Produces: operation metadata for one exact planned mutation.
- Changes: fixed MCP allowlist to `get_app_state`, `wait`, `click`, `drag`, and `focus_click`.

- [ ] **Step 1: Replace observation-only policy tests with failing planned-call tests**

Update every constructor to pass `Arc::new(DecisionGate::new(1))`. Keep the
non-game-server preservation test. Replace the old blanket mutation-denial
test with:

```rust
#[tokio::test]
async fn exact_planned_mutation_receives_owner_and_operation_metadata() -> anyhow::Result<()> {
    let gate = Arc::new(DecisionGate::new(1));
    install_click_plan(&gate)?;
    let policy = GameCallPolicy::new("epoch-1".to_string(), 1, Arc::clone(&gate));
    let arguments = json!({"x": 180, "y": 640});
    let request_meta = serde_json::Map::new();

    let decision = policy
        .evaluate(McpToolCallPolicyInput {
            server_name: "game",
            tool_name: "click",
            call_id: "mutation-1",
            arguments: Some(&arguments),
            request_meta: &request_meta,
        })
        .await;

    assert_eq!(
        decision,
        McpToolCallPolicyDecision::Allow {
            additional_request_meta: json!({
                "action_sha256": "f709b60a7bbf91028aa10498db469d2a47fd96669d5167f6163200718809b3e1",
                "call_id": "mutation-1",
                "epoch": "epoch-1",
                "generation": 1,
                "operation_id": "mutation-1",
            })
            .as_object()
            .expect("metadata fixture must be an object")
            .clone(),
        }
    );
    Ok(())
}
```

The hash is the checked SHA-256 of the canonical compact envelope
`{"arguments":{"x":180,"y":640},"tool":"click"}`; do not duplicate hash
logic in the test. Add complete tests for:

- a mismatched click denial with no operation metadata;
- a second mutation denial after one authorization;
- `get_app_state` invalidation before dispatch;
- positive versus zero `wait` invalidation;
- denial and audit of `zoom` and an unknown tool; and
- unchanged non-game MCP metadata.

Update `config_tests.rs` so the complete projected tool list is:

```rust
game_tools: Some(vec![
    "get_app_state".to_string(),
    "wait".to_string(),
    "click".to_string(),
    "drag".to_string(),
    "focus_click".to_string(),
]),
```

- [ ] **Step 2: Run policy and config tests and verify red**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner policy
rustup run 1.95.0 just test -p codex-game-runner config
```

Expected: policy compilation or assertions fail because it does not consult
the gate, and the configuration still exposes `zoom` instead of mutations.

- [ ] **Step 3: Implement the gate-backed policy**

Change the policy state to:

```rust
pub struct GameCallPolicy {
    epoch: String,
    generation: u64,
    gate: Arc<DecisionGate>,
    unknown_tool_attempts: AtomicUsize,
}
```

Use one helper that creates owner metadata for allowed calls. In the exhaustive
tool match:

```rust
match input.tool_name {
    "get_app_state" => {
        self.gate.begin_full_observation();
        allow_with_owner_metadata(self, input.call_id)
    }
    "wait" => {
        self.gate.before_wait(input.arguments);
        allow_with_owner_metadata(self, input.call_id)
    }
    "click" | "drag" | "focus_click" => {
        match input.arguments.and_then(Value::as_object) {
            Some(_) => self.authorize_mutation(input),
            None => self.deny_mutation_without_arguments(input.tool_name),
        }
    }
    "zoom" => self.deny_unknown_tool("zoom"),
    tool_name => self.deny_unknown_tool(tool_name),
}
```

`authorize_mutation` calls `gate.prepare_mutation`, maps a typed error into a
model-visible denial, and on success adds the three lease fields plus
`operation_id` and `action_sha256`. Never add operation metadata before the
gate returns an authorization.

Update `config.rs` to expose only the approved five tools and keep
`supports_parallel_tool_calls: false`.

- [ ] **Step 4: Run policy and config tests and verify green**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner policy
rustup run 1.95.0 just test -p codex-game-runner config
```

Expected: all policy and fixed-configuration tests pass.

- [ ] **Step 5: Run the Stage 3 observation regression**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner observation
```

Expected: Stage 3 observation tests still pass with a fresh empty gate and no
mutation authorization.

- [ ] **Step 6: Commit policy enforcement**

```bash
git add codex-rs/game-runner/src/policy.rs \
  codex-rs/game-runner/src/policy_tests.rs \
  codex-rs/game-runner/src/config.rs \
  codex-rs/game-runner/src/config_tests.rs
git commit -m "feat(game-runner): enforce planned game mutations"
```

### Task 4: Add bounded campaign dynamic tools

**Files:**
- Create: `codex-rs/game-runner/src/campaign_tools.rs`
- Create: `codex-rs/game-runner/src/campaign_tools_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`
- Modify: `codex-rs/core-api/src/lib.rs`

**Interfaces:**
- Consumes: `DecisionGate`, `PlanDraft`, and `OutcomeDraft` from Task 2.
- Produces: `CampaignTools::new(gate: Arc<DecisionGate>) -> Self`.
- Produces: `CampaignTools::specs() -> Vec<DynamicToolSpec>`.
- Produces: `CampaignTools::handle(&DynamicToolCallRequest) -> Result<DynamicToolResponse, CampaignToolError>`.
- Re-exports: existing `DynamicToolCallRequest`, `DynamicToolResponse`, and `DynamicToolCallOutputContentItem` through `codex-core-api`.

- [ ] **Step 1: Add the narrow facade re-exports and failing tool tests**

In `core-api/src/lib.rs`, extend the existing dynamic-tool re-export block:

```rust
pub use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
pub use codex_protocol::dynamic_tools::DynamicToolCallRequest;
pub use codex_protocol::dynamic_tools::DynamicToolResponse;
```

Register `campaign_tools` in the runner library with a sibling test file. Add
this primary test:

```rust
#[test]
fn record_plan_returns_runner_owned_identity_and_hash() -> anyhow::Result<()> {
    let gate = Arc::new(observed_gate());
    let tools = CampaignTools::new(Arc::clone(&gate));
    let request = DynamicToolCallRequest {
        call_id: "dynamic-plan-1".to_string(),
        turn_id: "turn-1".to_string(),
        started_at_ms: 0,
        namespace: Some("game_runner".to_string()),
        tool: "record_plan".to_string(),
        arguments: serde_json::to_value(plan_draft(
            "sha256:before",
            PlannedAction::Click(ClickArguments {
                x: 180,
                y: 640,
                button: None,
                count: None,
            }),
        ))?,
    };

    let response = tools.handle(&request)?;

    assert_eq!(
        response,
        DynamicToolResponse {
            content_items: vec![DynamicToolCallOutputContentItem::InputText {
                text: serde_json::to_string(&json!({
                    "action_sha256": gate.snapshot()
                        .plan
                        .as_ref()
                        .expect("accepted plan")
                        .action_sha256,
                    "observation_reference": "sha256:before",
                    "plan_id": "plan-1-1",
                }))?,
            }],
            success: true,
        }
    );
    Ok(())
}
```

Add tests for invalid namespace/tool, unknown fields, one candidate, five
candidates, oversized plan, stale reference, outcome before post-action
evidence, valid win, and oversized outcome. Compare complete responses and
gate snapshots.

- [ ] **Step 2: Run campaign-tool tests and verify red**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner campaign_tools
```

Expected: compilation fails because `CampaignTools` does not exist.

- [ ] **Step 3: Implement strict specs and synchronous handlers**

Create these public interfaces:

```rust
pub const CAMPAIGN_TOOL_NAMESPACE: &str = "game_runner";

pub struct CampaignTools {
    gate: Arc<DecisionGate>,
}

impl CampaignTools {
    pub fn new(gate: Arc<DecisionGate>) -> Self {
        Self { gate }
    }

    pub fn specs() -> Vec<DynamicToolSpec> {
        vec![DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
            name: CAMPAIGN_TOOL_NAMESPACE.to_string(),
            description: "Record bounded game decisions and visible terminal outcomes."
                .to_string(),
            tools: vec![
                DynamicToolNamespaceTool::Function(record_plan_spec()),
                DynamicToolNamespaceTool::Function(report_outcome_spec()),
            ],
        })]
    }

    pub fn handle(
        &self,
        request: &DynamicToolCallRequest,
    ) -> Result<DynamicToolResponse, CampaignToolError> {
        match (request.namespace.as_deref(), request.tool.as_str()) {
            (Some(CAMPAIGN_TOOL_NAMESPACE), "record_plan") => self.record_plan(request),
            (Some(CAMPAIGN_TOOL_NAMESPACE), "report_outcome") => self.report_outcome(request),
            _ => Err(CampaignToolError::UnexpectedTool {
                namespace: request.namespace.clone(),
                tool: request.tool.clone(),
            }),
        }
    }
}
```

Both `DynamicToolFunctionSpec` values use strict JSON Schema objects with
`additionalProperties: false`. `record_plan` uses a three-way `oneOf` for the
tagged chosen action and enforces `minItems: 2`, `maxItems: 4`, coordinate
minimum `0`, and click count `1..=3`. `report_outcome` uses the exact enum
`["loss", "win", "terminal_block"]`. Keep both tools direct with
`defer_loading: false`.

Serialize request arguments before serde decoding to enforce the aggregate
byte cap. Map `DecisionError` to `CampaignToolError::Rejected` with a concise
model-visible message. Successful responses contain one JSON text item;
schema or gate failures return `DynamicToolResponse { success: false, ... }`
when they are expected model recovery paths. Unexpected namespace/tool is a
runner error, not a model recovery path.

- [ ] **Step 4: Run campaign-tool and facade checks and verify green**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner campaign_tools
rustup run 1.95.0 cargo check -p codex-core-api
```

Expected: all campaign-tool tests pass and the facade compiles.

- [ ] **Step 5: Commit the dynamic tools**

```bash
git add codex-rs/core-api/src/lib.rs \
  codex-rs/game-runner/src/campaign_tools.rs \
  codex-rs/game-runner/src/campaign_tools_tests.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): add campaign decision tools"
```

### Task 5: Add the bounded campaign event loop and report

**Files:**
- Create: `codex-rs/game-runner/src/campaign.rs`
- Create: `codex-rs/game-runner/src/campaign_tests.rs`
- Create: `codex-rs/game-runner/src/campaign_report.rs`
- Modify: `codex-rs/game-runner/src/config.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`

**Interfaces:**
- Consumes: `CodexThread`, `SessionConfiguredEvent`, `CampaignTools`, `DecisionGate`, and `GameCallPolicy`.
- Produces: `CampaignLimits::stage_4a()` with six turns, fifteen minutes total, and five minutes after mutation authorization.
- Produces: `CampaignRun::new(limits) -> Self` and `execute(...) -> Result<CampaignReport, RunnerError>`.
- Produces: `CampaignTerminalState::{CanaryComplete, Won, LossObserved, TerminalBlock}`.
- Produces: `CampaignReport` and nested evidence projections.

- [ ] **Step 1: Write failing pure campaign-progress tests**

Create `campaign.rs` with the sibling test declaration and register both new
modules in `lib.rs`. In `campaign_tests.rs`, test the pure progress tracker:

```rust
fn empty_snapshot() -> DecisionSnapshot {
    DecisionSnapshot {
        owner_generation: 1,
        next_observation_generation: 1,
        observation: None,
        plan: None,
        mutation: None,
        outcome: None,
        requires_post_mutation_observation: false,
        audit: DecisionAudit {
            plans_accepted: 0,
            plan_rejections: 0,
            mutation_attempts: 0,
            mutation_authorizations: 0,
            mutation_denials: 0,
        },
    }
}

#[test]
fn early_turn_completion_continues_until_after_evidence_exists() {
    let mut progress = CampaignProgress::new(CampaignLimits {
        max_turns: 6,
        total_timeout: Duration::from_secs(900),
        post_mutation_timeout: Duration::from_secs(300),
    });

    assert_eq!(
        progress.on_turn_complete(&empty_snapshot()),
        CampaignDirective::Continue
    );
    progress.on_turn_started("turn-2".to_string());
    assert_eq!(progress.turn_ids(), &["turn-2".to_string()]);
}
```

Keep `empty_snapshot` private to the sibling test file; do not add a test-only
constructor to production. Add tests for:

- sixth versus seventh turn boundary;
- authorized mutation starting the post-action deadline;
- full after evidence producing `CanaryComplete` at turn completion;
- accepted win producing `Won`;
- accepted loss producing `LossObserved`; and
- missing after evidence producing `TerminalBlock` at its deadline.

- [ ] **Step 2: Run campaign tests and verify red**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner campaign
```

Expected: compilation fails because campaign progress, limits, directives, and
report types do not exist.

- [ ] **Step 3: Implement campaign progress and report types**

Use named limits rather than positional booleans or numeric arguments:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CampaignLimits {
    pub max_turns: usize,
    pub total_timeout: Duration,
    pub post_mutation_timeout: Duration,
}

impl CampaignLimits {
    pub fn stage_4a() -> Self {
        Self {
            max_turns: 6,
            total_timeout: Duration::from_secs(15 * 60),
            post_mutation_timeout: Duration::from_secs(5 * 60),
        }
    }
}
```

`CampaignReport` must serialize the terminal state, thread ID, all turn IDs,
rollout path, before/after evidence, accepted plan, mutation authorization and
result, optional reported outcome, owner lease, complete decision audit,
unknown-tool policy audit, and optional terminal failure text. It must not
contain screenshot base64.

Keep `CampaignProgress` private and pure. It counts started turns, records the
post-mutation deadline once, and chooses only `Continue`, `Complete(state)`,
or `Block(reason)` through exhaustive matches.

- [ ] **Step 4: Implement the serial Codex event loop**

Add the fixed initial prompt and continuation message as stable constants.
Implement `CampaignRun::execute` with this event handling shape:

```rust
match event.msg {
    EventMsg::TurnStarted(event) => progress.on_turn_started(event.turn_id),
    EventMsg::McpToolCallEnd(event) => {
        observe_game_call_end(&gate, &event)?;
    }
    EventMsg::DynamicToolCallRequest(request) => {
        let response = tools.handle(&request)?;
        thread
            .submit(Op::DynamicToolResponse {
                id: request.call_id,
                response,
            })
            .await
            .map_err(campaign_submit_error)?;
    }
    EventMsg::TurnComplete(_) => match progress.on_turn_complete(&gate.snapshot()) {
        CampaignDirective::Continue => submit_continuation(thread).await?,
        CampaignDirective::Complete(state) => return build_report(state),
        CampaignDirective::Block(reason) => return build_blocked_report(reason),
    },
    EventMsg::Error(event) => return build_blocked_report(event.message),
    EventMsg::TurnAborted(event) => {
        gate.invalidate(InvalidationReason::TurnAborted);
        return build_blocked_report(format!("turn aborted: {:?}", event.reason));
    }
    EventMsg::ExecApprovalRequest(_)
    | EventMsg::ApplyPatchApprovalRequest(_)
    | EventMsg::RequestPermissions(_)
    | EventMsg::RequestUserInput(_) => return forbidden_interaction_report(),
    _ => {}
}
```

Use `tokio::time::timeout` for the complete campaign and a select/deadline for
post-mutation evidence. `observe_game_call_end` must:

- install only successful `game/get_app_state` results with a bounded
  `artifact_uri`, positive width, and positive height;
- classify the authorized mutation result as success, clean failure, or
  indeterminate from the real MCP result;
- leave capture authority empty on failed `get_app_state`; and
- ignore non-game MCP events.

An unexpected dynamic namespace/tool must terminate the campaign. An expected
validation rejection is returned to Sol with `success: false` and the event
loop continues.

- [ ] **Step 5: Run campaign tests and verify green**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner campaign
```

Expected: every pure campaign and report test passes.

- [ ] **Step 6: Commit the campaign core**

```bash
git add codex-rs/game-runner/src/campaign.rs \
  codex-rs/game-runner/src/campaign_tests.rs \
  codex-rs/game-runner/src/campaign_report.rs \
  codex-rs/game-runner/src/config.rs \
  codex-rs/game-runner/src/lib.rs
git commit -m "feat(game-runner): run bounded planned campaign"
```

### Task 6: Wire the runtime and prove the complete fake-game path

**Files:**
- Create: `codex-rs/game-runner/src/runtime.rs`
- Create only if new behavior needs it: `codex-rs/game-runner/src/runtime_tests.rs`
- Modify: `codex-rs/game-runner/src/main.rs`
- Modify: `codex-rs/game-runner/src/main_tests.rs`
- Modify: `codex-rs/game-runner/src/lib.rs`
- Modify: `codex-rs/game-runner/tests/live_path.rs`
- Create: `codex-rs/game-runner/tests/campaign_path.rs`

**Interfaces:**
- Consumes: all Stage 4A library components.
- Produces: `RunnerRuntime::start(config, policy, dynamic_tools) -> Result<Self, RunnerError>`.
- Produces: deterministic `RunnerRuntime::shutdown(interrupt) -> Vec<String>` or an enum-based equivalent with self-documenting call sites.
- Changes: production CLI output from `ObservationReport` to `CampaignReport`.
- Preserves: hidden `__stdio-to-uds`, deployment-only CLI arguments, signed helper launch, and Codex main runtime stack.

- [ ] **Step 1: Update binary tests and write the failing vertical happy path**

Keep the existing argument and runtime-stack tests. Add an assertion that the
binary run path installs `CampaignTools::specs()` in `StartThreadOptions` by
covering it through the vertical request, not a test-only getter.

Create `tests/campaign_path.rs` by extracting only the reusable fake-helper
line protocol mechanics from `live_path.rs`. Do not move Stage 3 assertions.
The happy-path mocked Sol script must use the real code-mode bindings:

```javascript
const before = await tools.mcp__game__get_app_state({});
const beforeRef = before.structuredContent.artifact_uri;
const plan = await tools.game_runner__record_plan({
  observation_reference: beforeRef,
  objective: "Open one safe non-gameplay menu",
  visible_state_summary: "The main menu is visible",
  candidates: [
    {action: "Open Settings", predicted_visible_consequence: "Settings appears"},
    {action: "Open Credits", predicted_visible_consequence: "Credits appears"}
  ],
  chosen_action: {tool: "click", arguments: {x: 180, y: 640}},
  reason: "Settings is reversible and does not begin gameplay",
  expected_visible_result: "A settings screen",
  invalidation_condition: "The main menu changes before the click"
});
const mutation = await tools.mcp__game__click({x: 180, y: 640});
const after = await tools.mcp__game__get_app_state({});
const outcome = await tools.game_runner__report_outcome({
  outcome: "win",
  observation_reference: after.structuredContent.artifact_uri,
  visible_evidence_summary: "The fake game shows its full victory screen",
  lesson: "The planned navigation reached the terminal fixture"
});
text({plan, mutation, outcome});
```

The fake helper must issue real spooled JPEGs for both captures and require
this complete click metadata projection:

```rust
assert_eq!(
    metadata,
    json!({
        "action_sha256": expected_action_sha256,
        "callId": codex_call_id,
        "call_id": codex_call_id,
        "epoch": "test-epoch",
        "generation": 1,
        "operation_id": codex_call_id,
        "threadId": thread_id,
        "x-codex-turn-metadata": turn_metadata,
    })
);
```

Preserve and compare the standard Codex metadata values captured from the
request rather than hard-coding their variable contents.

Assert the complete `CampaignReport`, exact helper method order, one click,
two consumed blobs, dynamic-tool outputs in the second Responses request, and
absence of `zoom` and forbidden namespaces from the exec description.

- [ ] **Step 2: Run the vertical test and verify red**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner campaign_path
```

Expected: compilation or runtime setup fails because the binary still starts
`ObservationRun` without dynamic tools or `CampaignRun`.

- [ ] **Step 3: Extract runtime construction without behavior duplication**

Move `NoUserInstructions`, state DB/auth/environment construction,
`ThreadManager`, thread startup, and cleanup from `main.rs` into `runtime.rs`.
Use this ownership shape:

```rust
pub struct RunnerRuntime {
    thread_manager: ThreadManager,
    pub thread_id: ThreadId,
    pub thread: Arc<CodexThread>,
    pub session_configured: SessionConfiguredEvent,
}

impl RunnerRuntime {
    pub async fn start(
        config: Config,
        policy: Arc<GameCallPolicy>,
        dynamic_tools: Vec<DynamicToolSpec>,
    ) -> Result<Self, RunnerError>;

    pub async fn shutdown(self, mode: ShutdownMode) -> Vec<String>;
}

pub enum ShutdownMode {
    Completed,
    Interrupt,
}
```

`start` builds the extension registry with the supplied policy and passes
`dynamic_tools` through `StartThreadOptions`. `shutdown` preserves the current
primary-result/cleanup-error precedence, shuts down the thread, and removes it
from the manager. Do not create one-use wrapper methods around individual
Codex calls.

- [ ] **Step 4: Wire the Stage 4A CLI**

In `main.rs`:

1. Keep hidden bridge dispatch unchanged.
2. Build `Arc<DecisionGate>` and gate-backed `GameCallPolicy`.
3. Start `RunnerRuntime` with `CampaignTools::specs()`.
4. Run `CampaignRun::new(CampaignLimits::stage_4a())`.
5. Shut down with `Completed` for a terminal report or `Interrupt` for an
   execution error.
6. Emit exactly one compact `CampaignReport` JSON object on stdout.
7. Return nonzero after emitting a report whose state is `LossObserved` or
   `TerminalBlock`.

Use an enum method such as `CampaignTerminalState::is_success()` so the call
site is self-documenting and exhaustive.

- [ ] **Step 5: Add the two failure verticals**

In `campaign_path.rs`, add:

1. `mismatched_planned_action_never_reaches_helper`: plan click `(180,640)`,
   attempt `(181,640)`, use a one-turn injected `CampaignLimits`, and assert
   helper methods contain no click, the plan is consumed, and the report is a
   terminal block.
2. `missing_after_evidence_never_completes_canary`: authorize one click, make
   the second capture fail, use a millisecond post-mutation timeout, and assert
   exactly one helper click, no retry, retained before evidence/action hash,
   and terminal block.

Inject `CampaignLimits` through the normal constructor. Do not add test-only
production functions or mutate process environment.

- [ ] **Step 6: Run all runner tests and verify green**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-game-runner
```

Expected: Stage 3 tests and every Stage 4A unit/vertical test pass.

- [ ] **Step 7: Commit runtime wiring and vertical proof**

```bash
git add codex-rs/game-runner/src/runtime.rs \
  codex-rs/game-runner/src/runtime_tests.rs \
  codex-rs/game-runner/src/main.rs \
  codex-rs/game-runner/src/main_tests.rs \
  codex-rs/game-runner/src/lib.rs \
  codex-rs/game-runner/tests/live_path.rs \
  codex-rs/game-runner/tests/campaign_path.rs
git commit -m "feat(game-runner): wire planned action canary"
```

If `runtime_tests.rs` was not needed and was not created, omit it from the
`git add` command.

### Task 7: Verify scope and run the signed-helper live canary

**Files:**
- Modify only if verification exposes a Stage 4A defect: files introduced or changed in Tasks 1–6.

**Interfaces:**
- Consumes: the complete Stage 4A runner.
- Produces: one real evidence-linked planned navigation report with exactly one mutation.

- [ ] **Step 1: Check change size and repository scope**

Run from the repository root:

```bash
git status --short
stage_4a_base=$(git log -1 --format=%H -- \
  docs/superpowers/plans/2026-08-10-game-runner-planned-action-stage-4a.md)
git diff "$stage_4a_base"..HEAD --stat
git diff "$stage_4a_base"..HEAD --numstat
```

Confirm every complex commit is below 500 changed lines, every total commit is
below 800 changed lines, production modules remain below 500 lines excluding
tests, and no unrelated crate or external AutoPilot source changed. If a task
exceeds the bound, split it at its testable interface before proceeding.

- [ ] **Step 2: Run the focused regression boundary**

Use the verified V8 release assets for this checkout:

```bash
cd codex-rs
export RUSTY_V8_ARCHIVE=https://github.com/openai/codex/releases/download/rusty-v8-v150.4.0/librusty_v8_ptrcomp_sandbox_release_aarch64-apple-darwin.a.gz
export RUSTY_V8_SRC_BINDING_PATH=/tmp/autopilot-codex-v8-150.4.0/src_binding_ptrcomp_sandbox_release_aarch64-apple-darwin.rs
rustup run 1.95.0 just test -p codex-game-runner
rustup run 1.95.0 just test -p codex-core mcp_tool_call_policy
rustup run 1.95.0 cargo check -p codex-core-api
```

Expected: all runner tests, generic MCP policy tests, and the facade check
pass. A workspace-wide `just test` is not required because this stage changes
neither `codex-core`, `common`, nor `protocol` behavior.

- [ ] **Step 3: Run final lint and formatting**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just fix -p codex-core-api
rustup run 1.95.0 just fix -p codex-game-runner
rustup run 1.95.0 just fmt
```

Expected: both scoped Clippy passes and formatting complete cleanly. Do not
rerun tests after this step.

- [ ] **Step 4: Build the live binary**

Run:

```bash
cd codex-rs
export RUSTY_V8_ARCHIVE=https://github.com/openai/codex/releases/download/rusty-v8-v150.4.0/librusty_v8_ptrcomp_sandbox_release_aarch64-apple-darwin.a.gz
export RUSTY_V8_SRC_BINDING_PATH=/tmp/autopilot-codex-v8-150.4.0/src_binding_ptrcomp_sandbox_release_aarch64-apple-darwin.rs
rustup run 1.95.0 cargo build -p codex-game-runner
```

Expected: `codex-rs/target/debug/codex-game-runner` is produced from the final
formatted source without the old Tokio worker stack overflow. This build does
not rerun the test suite.

- [ ] **Step 5: Prepare the visible live state without mutating it**

Confirm:

- Gambonanza is open at its main menu.
- The installed helper exists at
  `/Users/pedroproenca/Library/Application Support/AutoPilot/Helpers/AutoPilotHelper.app`.
- Screen Recording and Accessibility remain granted to that Developer-signed
  app, not the ad-hoc source-tree build.
- No previous helper instance owns the new canary socket.

Do not click the game manually, reset it, or launch the helper's bare
executable.

- [ ] **Step 6: Run the one-action canary**

Use a fresh socket path:

```bash
cd codex-rs
./target/debug/codex-game-runner \
  --helper-app '/Users/pedroproenca/Library/Application Support/AutoPilot/Helpers/AutoPilotHelper.app' \
  --socket /tmp/autopilot-codex-game-runner/AutoPilotHelper-stage-4a.sock \
  --target-app Gambonanza
```

Expected: Sol captures the main menu, records two to four candidates, plans
one safe non-gameplay navigation such as Settings/Collection/Credits, performs
exactly that click, captures the resulting screen, and emits one compact JSON
report. Choosing Play or Continue fails the canary.

- [ ] **Step 7: Inspect the report, rollout, and pixels**

Verify all of the following:

- before and after artifact references are distinct and present in the
  rollout;
- the accepted plan's exact action equals the helper call;
- `operation_id` equals the Codex call ID;
- `action_sha256` matches the canonical action envelope;
- policy audit reports one attempt and one authorization;
- the helper result is successful;
- the after screenshot visibly matches the predicted result;
- no second mutation was attempted or dispatched; and
- cleanup retained the rollout and left no screenshot blob orphan.

Any absent or visually wrong evidence blocks Stage 4B even when automated
tests pass.

- [ ] **Step 8: Fix only evidenced Stage 4A defects**

For each failure, add the smallest reproducing test first, observe red, apply
the minimal fix, rerun the focused test and full runner crate, then repeat the
final lint/format order. Do not tune the prompt from preference; change it only
for a concrete trace failure.

- [ ] **Step 9: Commit any canary-derived fix through its owning task**

If the canary required a code change, return to the task that owns the failed
interface, add the reproducing test there, and use that task's explicit file
list to stage the correction. Commit it as
`fix(game-runner): correct planned action canary`. If no code changed, do not
create an empty completion commit.

## Completion Criteria

Stage 4A is complete only when:

- every Task 1–6 commit is present and within scope limits;
- `just test -p codex-game-runner` passes;
- the generic MCP policy regression passes;
- scoped Clippy and formatting finish cleanly;
- the installed signed helper accepts the owner and durable-operation
  metadata;
- the live report contains one exact accepted plan, one matching mutation,
  and one fresh after observation;
- manual visual review confirms the expected visible result; and
- Stage 4B remains unimplemented until this gate succeeds.
