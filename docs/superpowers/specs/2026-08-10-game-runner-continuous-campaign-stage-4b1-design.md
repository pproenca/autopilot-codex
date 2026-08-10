# Game Runner Continuous Campaign Stage 4B1 Design

## Summary

Stage 4B1 turns the proven Stage 4A one-action canary into a headless campaign
that repeatedly observes, plans, acts, and verifies until GPT-5.6-Sol reports a
visually confirmed full-game victory. A visible game loss ends one attempt, not
the campaign. The same Codex thread retains detailed history while a small,
bounded in-memory strategy record carries the model's explicit lesson into the
next attempt.

This stage proves gameplay continuity and learning with a hermetic fake game.
It does not add persistence, crash recovery, helper reconnection, Pause, Stop,
or the TUI. Those remain separate stages so the agent loop can be evaluated
without rebuilding the orchestration bloat this fork is intended to remove.

## Goals

- Continue automatically across ordinary Codex turn completion until victory.
- Permit repeated physical actions while preserving the existing
  observation-plan-action-verification gate before every mutation.
- Preserve short-term Sol reasoning by allowing bounded multi-action turns.
- Treat visible losses as attempt boundaries and continue with an updated
  bounded strategy.
- Keep every counter, retained identifier list, model-authored field, tool
  payload, deadline, and per-turn action batch explicitly bounded.
- Prove two fake losses, visible restart actions, and an eventual verified win
  through the real Sol code-mode, dynamic-tool, policy, UDS bridge path.
- Preserve the signed-helper focus borrowing and durable-operation metadata
  introduced in Stage 4A.

## Non-goals

- Durable campaign state or recovery after process exit.
- Helper reconnection or owner-lease replacement during a campaign.
- Start, Pause, Resume, Stop, or any terminal UI.
- An unattended real-game campaign before operator controls exist.
- A second history store, event journal, rule engine, transition database,
  embeddings, or semantic retrieval.
- Out-of-band game reset, game-file inspection, accessibility-derived game
  state, or any input other than fresh helper pixels.
- Multiple gameplay policies, configurable batch strategies, or model routing.

## Selected Loop Shape

Stage 4B1 uses bounded action batches. One Codex turn may authorize at most
eight physical mutations. The campaign itself has no healthy-gameplay turn,
attempt, loss, or action limit.

```text
Codex turn
  -> fresh full-frame observation
  -> record two to four candidates and one exact plan
  -> authorize and dispatch one matching mutation
  -> capture fresh post-action pixels
  -> inspect the effect
  -> repeat, up to eight mutations
  -> finish at a safe turn boundary
  -> automatically start another turn
  -> continue until verified win or terminal infrastructure block
```

This is preferred over one mutation per turn because it preserves Sol's
short-term tactical reasoning and avoids a full turn boundary after every
move. It is preferred over an unlimited turn because it bounds pause latency
for later stages, request duration, runaway tool use, and the amount of local
state one response can accumulate.

## Architecture

The existing `CampaignRun` remains the single serial coordinator. It owns the
Codex turn lifecycle and reduces runner, dynamic-tool, and MCP events into a
small `CampaignProgress` value. It does not become a second agent runtime.

The existing boundaries remain authoritative:

- `DecisionGate` owns the newest observation, accepted plan, current mutation,
  post-mutation evidence requirement, and per-turn mutation budget.
- `GameCallPolicy` remains the only mutation dispatch gate and attaches the
  exact owner, call, operation, and action-hash metadata.
- `CampaignTools` decode bounded `record_plan` and `report_outcome` payloads.
- `CampaignRun` decides whether to continue the current attempt, begin another
  attempt, complete on victory, or block on infrastructure failure.
- The image bridge continues borrowing application focus only around physical
  mutations and restoring the previously frontmost application afterward.
- The non-ephemeral Codex thread and rollout remain the canonical detailed
  history.

New state and reducers should live in focused modules rather than extending a
central file beyond the repository's 500-line production-module target.

## Decision Gate Lifecycle

At the beginning of every Codex turn, the runner opens a new action batch and
invalidates all observation and plan authority from the previous turn. Sol
must capture a fresh full frame before recording another plan.

For each action:

1. `get_app_state` installs a fresh observation and invalidates any older plan.
2. `record_plan` validates two to four candidates and one complete chosen
   action against that observation.
3. The MCP policy atomically consumes the plan before allowing the exact
   matching `click`, `drag`, or `focus_click`.
4. The helper result is classified as success, clean failure, or indeterminate.
5. Another fresh full-frame observation is mandatory after any dispatched
   mutation.
6. Once that observation arrives, the consumed mutation cycle is closed and a
   new plan may be accepted.

The gate increments the current batch count only when a mutation is authorized.
After eight authorizations it denies further mutations with a model-visible
instruction to inspect any outstanding result and finish the turn. Read-only
capture and terminal outcome reporting remain available so the eighth action
can still be verified. The next turn resets the batch counter but not campaign
or attempt counters.

Starting a turn, ending a turn, interruption, a positive wait, a new capture,
or owner-generation replacement invalidates any unconsumed plan. Parallel
mutations remain disabled.

## Campaign State

Stage 4B1 keeps one in-memory campaign record:

- attempt number, starting at one;
- exact total turn, authorized-action, and loss counts as checked `u64`
  counters;
- current batch usage, bounded from zero through eight;
- latest observation, accepted plan, mutation, and terminal outcome evidence;
- a model-authored strategy record capped at 16 KiB of UTF-8 JSON; and
- the 64 most recent turn IDs for the compact terminal report.

Counter overflow is an infrastructure block rather than wrapping. The runner
does not retain an unbounded vector of turn IDs, plans, screenshots, lessons,
or actions. The rollout already retains the full conversation and tool history.

The final `CampaignReport` projects the aggregate counts, latest correlated
evidence, current bounded strategy, recent turn-ID tail, owner lease, audits,
terminal state, and rollout path. It never embeds image bytes.

The strategy is a typed replacement value rather than arbitrary prose:

- `summary`: one string capped at 2 KiB;
- `confirmed_mechanics`: at most 24 strings capped at 512 bytes each;
- `failed_approaches`: at most 16 strings capped at 512 bytes each;
- `shop_and_boss_notes`: at most 24 strings capped at 512 bytes each; and
- `next_attempt_priorities`: one to eight strings capped at 512 bytes each.

The canonical serialized strategy must also fit within 16 KiB, so aggregate
validation still fails closed when individually valid fields combine into an
oversized record.

## Outcomes and Strategy

`report_outcome` is reserved for campaign-significant terminal evidence:

- `loss` requires the newest fresh screenshot, a concise visible evidence
  summary, one lesson, and a complete replacement strategy record;
- `win` requires the newest fresh screenshot and visible evidence of full-game
  victory rather than a round, stage, shop, or boss transition; and
- `terminal_block` records an unrecoverable infrastructure or control failure.

The Stage 4A-only `canary_complete` outcome is removed when its historical tests
are migrated to the continuous-campaign semantics.

The complete outcome payload is capped at 24 KiB. Non-strategy outcome strings
retain their 2 KiB field caps. This leaves a small explicit envelope around the
16 KiB strategy while remaining far below the repository's 10K-token context
item limit.

A loss closes the current action batch. No further mutation can be authorized
in that turn. `CampaignProgress` validates and installs the replacement
strategy and increments the loss and attempt counters. After returning the
dynamic-tool result, the runner interrupts at that safe non-physical boundary
and starts a new Codex turn. That next turn must observe the loss or results
screen and use a planned visible restart control. The runner never resets or
relaunches the game to begin an attempt. Win and terminal-block reports close
the mutation lane through the same safe-boundary mechanism but do not start
another turn.

Within an attempt, the normal Codex thread retains tactical discoveries,
shops, bosses, resources, and prediction failures. The bounded strategy record
is replaced at loss boundaries so failed approaches do not grow an append-only
lesson log. Because Stage 4B1 keeps the same thread alive, the accepted
`report_outcome` call and its strategy already exist in model-visible history;
no custom context fragment or repeated strategy injection is added. Durable
reinjection after resume belongs to Stage 4B2.

Routine successful actions do not call `report_outcome`. Their verification
boundary is the required fresh observation, followed by the next plan's
visible-state summary and predicted action.

## Turn Continuation

The initial prompt defines the full-game objective, pixel-only evidence rule,
planning contract, batch limit, loss semantics, and the rule that difficulty
is not a stopping condition.

If a turn completes without a reported loss, win, or terminal block, the
runner starts another turn in the same thread. The continuation message is
short and stable: continue the campaign, capture fresh pixels, and play until
victory. At a loss boundary it additionally identifies the new attempt number
and directs the model to use the strategy it just recorded.

A normal assistant final answer does not end a campaign. Reaching the
eight-action batch limit does not end a campaign. A game loss does not end a
campaign. Only a fresh-evidence-linked win is the normal automatic completion
condition.

## Deadlines and Failure Handling

There is no total healthy-campaign deadline in Stage 4B1. Individual work
remains bounded:

- each Codex turn has a 15-minute deadline;
- every dispatched mutation has a five-minute post-observation deadline;
- plan, action, outcome, and image limits remain explicit and fail closed; and
- Codex retains its existing bounded model/network retry behavior.

A turn timeout may begin a fresh turn only when no physical action has an
unresolved outcome. If a mutation may have reached the game, the runner first
requires observation and never blindly repeats the action.

Clean helper failures invalidate the plan and require re-observation before a
new decision. Indeterminate mutations are never automatically retried. An
unchanged screen is evidence for Sol to diagnose timing, target selection,
gesture semantics, or a game rule; it is not itself a terminal block or a
reason to replay the same operation.

A missing post-mutation observation, corrupt or oversized screenshot,
unresolved physical action, helper disconnection, invalid owner state, or
exhausted underlying model retry becomes `terminal_block`. Helper reconnection
and lease-generation recovery are deferred to Stage 4B2. Gameplay losses are
never counted as infrastructure failures.

## Test Strategy

Agent-loop changes require integration coverage through the public runner
surface. Unit tests remain appropriate for exact state transitions, bounds,
and reducers.

Required unit coverage includes:

- a fresh post-action observation opens authority for the next plan;
- every mutation still consumes exactly one matching plan;
- the ninth mutation in one turn is denied while post-action capture and
  outcome reporting remain available;
- a new turn resets only the batch budget and stale observation authority;
- loss increments attempts and losses and atomically replaces strategy;
- oversized, malformed, or stale strategy updates are rejected;
- checked counter overflow blocks instead of wrapping; and
- retained turn IDs and terminal reports remain bounded.

Required integration coverage includes:

- multiple planned mutations within one Sol turn, each using a distinct fresh
  observation;
- ordinary turn completion automatically opening another batch;
- clean and indeterminate helper failures forcing observation before another
  authorized mutation;
- all existing plan mismatch, one-action-at-a-time, owner metadata, durable
  operation, UDS image bridge, and focus-restoration protections; and
- one fake-game vertical that visibly loses twice, records replacement
  strategies, uses separately planned restart actions, and eventually reports
  a fresh-evidence-linked full victory.

Tests compare complete state and report values where possible. The fake game
asserts the exact ordered helper calls and metadata so a passing model response
cannot hide an unauthorized or duplicate physical action.

## Delivery and Completion Gate

Implementation remains within the repository's review limits: complex commits
stay below 500 changed lines, total commits below 800 changed lines, and new
production modules below 500 lines excluding tests. Work is split at the gate,
campaign-state, strategy, loop, and integration-test boundaries when needed.

Stage 4B1 is complete only when:

- the full `codex-game-runner` test suite passes without retries or flakes;
- generic MCP policy and `codex-core-api` boundaries remain green when touched;
- scoped Clippy, formatting, and the final binary build pass;
- the fake multi-loss campaign reaches a verified victory;
- every fake physical mutation has one fresh plan and one fresh after image;
- loss and strategy state stay bounded across attempts; and
- no Stage 4B2, TUI, helper-import, or workspace-stripping work is mixed in.

No unattended real-game campaign is launched in Stage 4B1. The next work is
Stage 4B2 durability and recovery, followed by the focused Start/Pause/Resume/
Stop TUI. Real-game play-until-win campaigns begin only after those operator
controls exist.
