# Game Runner Planned-Action Stage 4A Design

## Summary

Stage 4A adds the smallest safe physical-action campaign slice on top of the
proven Stage 3 observation runner. One persistent GPT-5.6-Sol thread may
observe, compare candidate moves, record an exact tactical plan, perform at
most one matching mutation, and inspect the resulting pixels. The runner then
stops with an evidence-linked report.

This stage proves that planning is mechanically consumed before input reaches
the signed helper. It does not yet attempt an unrestricted game campaign. The
live completion gate is one deliberately bounded navigation canary; durable
loss recovery and strategy state follow in Stage 4B.

## Goals

- Keep one non-ephemeral Sol/high thread alive across early assistant
  completions.
- Expose strict `record_plan` and `report_outcome` dynamic tools through
  Codex's existing code-mode surface.
- Bind every plan to the newest successful full-frame game observation.
- Bind the chosen plan to one exact mutating MCP tool and argument object.
- Consume a plan before any mutation can reach the helper.
- Supply the helper's durable `operation_id` and `action_sha256` metadata.
- Never automatically retry an indeterminate mutation.
- Require a fresh post-mutation screenshot and correlate it with the plan,
  action, helper result, and rollout.
- Prove the whole path with a hermetic fake game and one real signed-helper
  canary.
- Keep all model-visible text and runner-owned records explicitly bounded.

## Non-goals

- Unlimited autonomous gameplay or a real-game victory attempt.
- Durable campaign recovery, attempt history, or strategy persistence.
- Continuing automatically after a real loss.
- Pause, Resume, Stop, or a terminal UI.
- Importing or packaging the external helper.
- Changing the generic Codex MCP transport, context, retry, or model loop.
- A solver, rule engine, board parser, OCR layer, or game-specific move veto.
- `zoom`; it is removed from the Stage 4 and final runner tool surface.
- Shell, web, apps, skills, plugins, subagents, project instructions, or
  arbitrary MCP servers.

## Stage Decomposition

The former Stage 4 boundary is split into two reviewable stages:

- **Stage 4A — planned-action kernel:** persistent thread, dynamic planning
  and outcome tools, exact plan enforcement, operation metadata, automatic
  continuation, bounded one-mutation campaign, and fake-game victory proof.
- **Stage 4B — durable campaign learning:** crash recovery, bounded strategic
  state, repeated losses, attempt transitions, Resume, unrestricted automatic
  continuation, and a fake multi-loss eventual victory.

The split isolates physical-action safety from persistence and learning. The
Stage 4B design does not begin until the Stage 4A live canary passes.

## Chosen Approach

Use Codex dynamic tools for `record_plan` and `report_outcome`, serviced by the
runner's serial event loop. A small shared `DecisionGate` is also registered
with the existing game MCP call policy. The event loop writes observations and
plans into the gate; the policy atomically consumes and validates the plan
immediately before a mutation is dispatched.

This preserves Codex's native Responses, code-mode, MCP, rollout, retry, and
thread behavior. It requires only narrow re-exports of existing dynamic-tool
response types through `codex-core-api`, not a new generic core feature.

Two alternatives are rejected:

- Adding extension-owned tool executors would avoid the dynamic-tool event
  round trip but add another generic Codex extension seam for a runner-only
  need.
- Asking Sol for a final JSON plan and dispatching the action from the runner
  would recreate an agent harness outside Codex and separate planning from the
  native model/tool loop.

## Components

### `DecisionGate`

`DecisionGate` owns the live decision authority shared between the campaign
event loop and `GameCallPolicy`. It contains:

- the current owner lease generation;
- the newest successful full-frame observation generation, call ID, and
  artifact reference;
- one optional unconsumed tactical plan;
- whether one mutation has been attempted and authorized;
- whether a post-mutation observation is required; and
- bounded audit counters and denial reasons.

The gate uses one short non-async mutex. No lock is held across model, MCP,
filesystem, or helper I/O. Methods return complete state transitions so tests
can compare whole values rather than inspect private fields individually.

### `CampaignTools`

`CampaignTools` owns the namespaced dynamic-tool specifications, argument
decoding, size validation, gate transitions, and model-visible responses. The
code-mode bindings are:

- `tools.game_runner__record_plan(...)`
- `tools.game_runner__report_outcome(...)`

The tools are direct, not deferred. They are the only runner-local tools.

### `GameCallPolicy`

The existing game-only policy keeps supplying `epoch`, `generation`, and
`call_id`. Stage 4A extends it to:

- allow `get_app_state` and `wait` as the only read-only game calls;
- deny `zoom` and every unknown game tool;
- invalidate decision authority before a new full-frame observation attempt;
- invalidate a plan before a positive wait;
- consume one plan on every attempted `click`, `drag`, or `focus_click`;
- require the actual tool and arguments to exactly equal the chosen action;
- deny every second mutation attempt; and
- add `operation_id` and `action_sha256` to an authorized mutation.

### `CampaignRun`

`CampaignRun` is a serial state machine around one Codex thread. It submits the
initial canary prompt, streams events, responds to dynamic-tool requests,
updates the gate from MCP results, submits stable continuation turns, enforces
fixed limits, and performs deterministic cleanup.

Stage 4A campaign state is transient. Process restart does not resume it;
durability belongs to Stage 4B. The canonical non-ephemeral Codex rollout is
still preserved on every terminal path.

### `CampaignReport`

The final report contains runner-owned evidence rather than relying on model
assertions for transport facts. It correlates:

- thread and turn identifiers;
- before and after observation call IDs and artifact references;
- the complete accepted plan and runner-assigned plan ID;
- the mutation call ID, exact tool and arguments, and action hash;
- the helper result classification;
- an optional validated model outcome;
- policy attempts, denials, and authorizations;
- the rollout path; and
- the terminal canary state.

The report excludes image bytes. Images remain in the canonical rollout.

## Model-visible Surface

The game MCP server exposes exactly:

- `get_app_state`
- `wait`
- `click`
- `drag`
- `focus_click`

`zoom` is absent from configuration and denied by policy if an unexpected
catalog advertises it. Sol plans from the latest full-frame screenshot. The
runner continues to exclude generic functions, shell, web, apps, skills,
plugins, subagents, project documentation, collaboration tools, and other MCP
servers.

`focus_click` retains the helper catalog's narrow recovery contract: Sol may
choose it only when a structured helper input error explicitly directs
app-local focus recovery. It is not ordinary navigation or a response to a
capture failure. Stage 4A does not add a second focus heuristic in the runner.

The Stage 4A prompt states:

- the one-action canary objective;
- the pixel-only evidence boundary;
- the requirement to compare two to four candidate moves;
- the exact plan/action matching contract;
- the need to observe after every attempted mutation;
- that an indeterminate result must never be retried;
- that only full-game victory pixels justify `win`; and
- that an ordinary assistant final answer does not end the campaign.

The live prompt asks for the safest useful visible navigation that does not
start or resume gameplay. It does not hard-code coordinates or claim hidden
game semantics.

## Planning Contract

`record_plan` accepts one strict object with no unknown fields:

- `observation_reference`: exact newest full-frame artifact reference;
- `objective`: current tactical objective;
- `visible_state_summary`: concise pixel-grounded state;
- `candidates`: two to four entries, each containing a concise move and
  predicted visible consequence;
- `chosen_action`: a tagged `click`, `drag`, or `focus_click` plus the complete
  argument object that will be sent to MCP;
- `reason`: why this candidate was selected;
- `expected_visible_result`: pixels expected after the action; and
- `invalidation_condition`: visible evidence that would make the plan stale.

The chosen action schemas mirror the helper's current public arguments:

- `click`: integer `x`, integer `y`, optional `button`, and optional `count`;
- `drag`: integer `from_x`, `from_y`, `to_x`, and `to_y`;
- `focus_click`: integer `x` and integer `y`.

The plan binds the exact JSON shape, including whether optional click fields
are omitted or present. A later call that relies on a default when the plan
spelled that default explicitly, or vice versa, is a mismatch.

The serialized request is capped at 12 KiB. Scalar strings are capped at 2
KiB. A valid plan receives a runner-assigned opaque plan ID and returns the
plan ID, observation reference, and expected action hash to Sol.

The plan is a concise decision record, not private chain-of-thought. Candidate
consequences and rationale are bounded summaries.

## Outcome Contract

`report_outcome` accepts:

- `outcome`: `loss`, `win`, or `terminal_block`;
- `observation_reference`: exact newest post-mutation artifact reference;
- `visible_evidence_summary`: concise pixel evidence;
- `lesson`: one bounded lesson; and
- optional `strategic_update`: a bounded replacement strategy for Stage 4B.

The serialized request is capped at 8 KiB and every string at 2 KiB. Stage 4A
validates and includes the outcome in `CampaignReport` but does not persist the
lesson or strategy.

`win` is accepted only against the newest successful post-mutation full-frame
observation. The prompt defines full-game victory and explicitly excludes a
round, stage, shop, boss-transition, or results screen without final-victory
evidence. The runner validates evidence identity and ordering; semantic pixel
interpretation remains Sol's responsibility.

A model-reported `loss` or `terminal_block` ends the Stage 4A canary. Stage 4B
will turn loss into another attempt and persist bounded strategy.

## Decision Lifecycle

The successful lifecycle is:

```text
get_app_state attempt invalidates old authority
  -> successful full frame installs observation N
  -> record_plan binds one exact action to N
  -> matching mutation consumes the plan before dispatch
  -> policy adds owner and durable-operation metadata
  -> helper returns success, clean failure, or indeterminate
  -> get_app_state installs post-mutation observation N+1
  -> optional report_outcome binds to N+1
  -> Stage 4A emits CampaignReport and stops
```

Detailed invariants:

1. A `get_app_state` attempt invalidates any current observation and plan
   before dispatch because the helper closes its coordinate gate at capture
   start. Only a successful result installs replacement authority.
2. A positive `wait` invalidates observation and plan before dispatch. A zero
   wait does not change authority.
3. Interruption, connection loss, lease generation change, dynamic-tool
   cancellation, and turn abort invalidate the plan.
4. Every mutation attempt atomically takes the plan before checking the tool
   and arguments. A mismatch is denied and the plan remains consumed.
5. An authorized or indeterminate mutation uses the single Stage 4A mutation
   budget. It is never automatically retried.
6. After any mutation attempt, a fresh full-frame observation is required
   before outcome reporting or terminal success.
7. Once one mutation was authorized or returned indeterminate, a second
   mutation is denied even after a new plan. Stage 4B changes the campaign
   budget, not these per-action freshness rules.

## Durable Operation Metadata

For an authorized mutation:

- `operation_id` is the immutable Codex MCP call ID.
- `action_sha256` is lowercase SHA-256 of a compact canonical JSON envelope:

```json
{"arguments":{},"tool":"click"}
```

The actual object contains the real arguments. Canonicalization recursively
sorts every object key lexicographically, preserves array order and JSON scalar
types, and emits no insignificant whitespace. The same canonicalization is
used while accepting the plan and while authorizing the call.

The helper's durable journal remains the at-most-once authority. The runner
does not introduce another operation journal or retry an operation under a new
ID.

## Automatic Continuation and Limits

If a Sol turn completes without a terminal outcome or completed canary, the
runner submits the stable message:

> Continue the planned-action canary from the newest visible evidence.

The same thread, dynamic tools, gate, and rollout remain active. Codex owns
provider retries and persisted reasoning.

Stage 4A has fixed, non-configurable safety limits:

- one authorized mutation;
- at most six model turns;
- fifteen minutes for the complete canary; and
- five minutes after mutation authorization to obtain fresh post-action
  evidence.

Reaching a limit is a failed canary or terminal infrastructure block, never a
game loss. These are stage gates, not gameplay policy knobs.

## Failure Handling

Failures remain classified by authority and dispatch certainty:

- Invalid or oversized `record_plan`: model-visible rejection; no mutation
  authority is created.
- Observation-reference mismatch: model-visible rejection; Sol must capture
  and plan again.
- Mismatched or malformed mutation: plan consumed, dispatch denied, fresh
  observation required.
- `stale_observation`: no automatic retry; capture and plan again.
- Clean pre-dispatch helper rejection: capture before another plan.
- `indeterminate`: mutation budget consumed, never retry, capture to determine
  visible state.
- Missing post-action evidence: terminal block with the primary mutation
  result retained.
- Helper disconnect or persistent capture failure: terminal block in Stage
  4A; bounded reconnection belongs to Stage 4B.
- Forbidden approval, permission, shell, patch, user-input, or unrelated
  dynamic-tool event: terminal runner failure.
- Cleanup failure: preserve the primary campaign result and attach cleanup
  context.

The helper continues to serialize stateful operations. The game MCP config
keeps parallel tool calls disabled. The runner event loop is the only dynamic
tool responder.

## File Boundaries

Stage 4A adds focused files rather than growing central modules:

- `codex-rs/game-runner/src/decision.rs`: observations, typed actions, plans,
  gate transitions, canonical action hash, and audit state.
- `codex-rs/game-runner/src/decision_tests.rs`: complete transition and hash
  tests.
- `codex-rs/game-runner/src/campaign_tools.rs`: dynamic-tool specs, decoding,
  bounds, responses, and outcome validation.
- `codex-rs/game-runner/src/campaign_tools_tests.rs`: tool validation tests.
- `codex-rs/game-runner/src/campaign.rs`: serial event loop, continuation,
  limits, and deterministic cleanup.
- `codex-rs/game-runner/src/campaign_tests.rs`: state-machine tests not
  requiring mocked Responses.
- `codex-rs/game-runner/src/campaign_report.rs`: final evidence envelope.
- `codex-rs/game-runner/src/runtime.rs`: extracted thread-manager construction
  and thread startup currently owned by `main.rs`.
- `codex-rs/game-runner/src/policy.rs`: gate-backed mutation policy and
  operation metadata.
- `codex-rs/game-runner/tests/campaign_path.rs`: mocked Responses plus fake
  signed-helper protocol verticals.
- `codex-rs/core-api/src/lib.rs`: only existing dynamic-tool response type
  re-exports required by the runner.

No new workspace crate, configuration schema, app-server API, or generic
`codex-core` behavior is introduced.

## Automated Verification

Focused tests cover:

- full observation, plan, attempt, authorization, and invalidation
  transitions;
- exact matching for click defaults, drag coordinates, and focus-click;
- recursive canonical JSON and stable action hashes;
- one-shot plan consumption on success, mismatch, and denial;
- invalidation on capture attempt, positive wait, interruption, and lease
  change;
- denial of `zoom`, unknown tools, unplanned mutations, and second mutations;
- bounded dynamic-tool inputs and evidence-reference validation;
- early assistant completion and same-thread continuation;
- indeterminate mutation without retry;
- post-action observation timeout and cleanup;
- fixed model-visible tool surface; and
- cross-platform compilation of the runner crate.

The hermetic vertical fake helper requires `epoch`, `generation`, `call_id`,
`operation_id`, and the expected `action_sha256`. The mocked Sol path performs:

```text
get_app_state
  -> record_plan
  -> click
  -> get_app_state
  -> report_outcome(win)
```

The test asserts the screenshot reaches code mode, the plan and actual call
match, exactly one helper mutation occurs, the after observation reaches Sol,
the win references that observation, and the report preserves the canonical
rollout. Separate verticals prove that a mismatched action never reaches the
helper and missing post-action evidence cannot complete the canary.

## Live Canary and Completion Gate

The real canary requires:

1. Gambonanza visibly open at its main menu.
2. The installed Developer-signed helper launched through LaunchServices with
   existing Screen Recording and Accessibility grants.
3. GPT-5.6-Sol/high obtaining a full-frame screenshot.
4. A valid plan comparing two to four candidates.
5. Selection of one safe navigation action that does not start or resume
   gameplay, such as Settings, Collection, or Credits.
6. Exactly one matching helper mutation with accepted durable-operation
   metadata.
7. A fresh post-action screenshot visibly matching the predicted result.
8. No second mutation attempt or dispatch.
9. A clean shutdown, retained rollout, and complete `CampaignReport`.
10. Manual visual confirmation of the before image, chosen action, and after
    image.

Choosing Play/Continue, failing to plan, dispatching a mismatched action,
missing after evidence, or producing a visually incorrect prediction fails
the canary and blocks Stage 4B.

## Stage Boundary

Stage 4A ends with a trustworthy planned physical-action path, not a winning
agent. Stage 4B may then reuse the same gate and event loop while adding:

- durable `idle`, `running`, `paused`, and `won` campaign state;
- crash recovery into `paused`;
- attempt counters and loss continuation;
- bounded strategic state and contextual injection;
- helper reconnection and lease-generation changes;
- unrestricted repeated planned actions; and
- fake multi-loss eventual-victory coverage.

The TUI remains blocked until the headless Stage 4B campaign core is proven.
