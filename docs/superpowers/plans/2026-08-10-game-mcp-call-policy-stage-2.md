# Game MCP Call Policy Stage 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a host-registered MCP call policy seam that can deny a call or append collision-safe request metadata, then prove it carries the external game helper's owner lease through the existing Sol code-mode and stdio-to-UDS path.

**Architecture:** Define the public contributor contract in `codex-extension-api`, register contributors through the existing immutable extension registry, and evaluate them from a new focused `codex-core` module. The MCP dispatch path calls that module after approval, argument normalization, and standard Codex metadata construction but before the prepared call is dispatched. A hermetic integration-test contributor supplies `epoch`, `generation`, and `call_id`; production code remains game-agnostic and the UDS bridge remains byte-transparent.

**Tech Stack:** Rust 1.95, `codex-extension-api`, `codex-core`, Tokio, serde JSON, existing `core_test_support` Responses fixtures, existing `codex-code-mode-host`, existing `codex stdio-to-uds` bridge.

## Global Constraints

- Do not add configuration fields, schemas, dependencies, workspace crates, or app-server API surface.
- Do not change `codex-stdio-to-uds`, `codex-uds`, or the external AutoPilot helper.
- Do not add game, Gambonanza, AutoPilot, epoch, or generation behavior to generic production code; those names appear only in the characterization test.
- With no contributor registered, preserve the existing MCP arguments and request metadata exactly.
- Contributors run in registration order and may only add metadata keys. A denial or duplicate key blocks dispatch with a model-visible error.
- Evaluate policy inside `PreparedMcpCall::call_with_preparation`, after approval and argument normalization, immediately before the underlying MCP client call.
- Keep the new core module below 500 lines including its dedicated sibling test file.
- Keep the complete non-mechanical change below 500 changed lines and the total change below 800 lines.
- Use native boxed futures already established by `codex-extension-api`; do not use `#[async_trait]` or `#[allow(async_fn_in_trait)]`.
- Newly added traits must document their role and implementation contract.
- Use `pretty_assertions::assert_eq` and compare complete maps or values.
- Use `just test`, never `cargo test`. Run commands with `rustup run 1.95.0` because the shell currently overrides the repository toolchain.
- Ask before the final workspace-wide `just test` because this stage changes `codex-core`.
- After tests, run `just fix -p codex-extension-api`, `just fix -p codex-core`, and `just fmt`; do not rerun tests after `fix` or `fmt`.

---

### Task 1: Define and test the ordered MCP policy contract

**Files:**
- Create: `codex-rs/ext/extension-api/src/contributors/mcp_tool_call_policy.rs`
- Modify: `codex-rs/ext/extension-api/src/contributors.rs`
- Modify: `codex-rs/ext/extension-api/src/lib.rs`
- Modify: `codex-rs/ext/extension-api/src/registry.rs`
- Create: `codex-rs/core/src/mcp_tool_call_policy.rs`
- Create: `codex-rs/core/src/mcp_tool_call_policy_tests.rs`
- Modify: `codex-rs/core/src/lib.rs`

**Interfaces:**
- Produces: `McpToolCallPolicyInput<'a>` containing `server_name`, `tool_name`, `call_id`, `arguments`, and the current `request_meta` map.
- Produces: `McpToolCallPolicyDecision::{Allow { additional_request_meta }, Deny { reason }}`.
- Produces: `McpToolCallPolicyFuture<'a>` and the `McpToolCallPolicyContributor` trait.
- Produces: `ExtensionRegistryBuilder::mcp_tool_call_policy_contributor` and `ExtensionRegistry::mcp_tool_call_policy_contributors`.
- Produces: `apply_mcp_tool_call_policies(...) -> anyhow::Result<Option<serde_json::Value>>` for Task 2.

- [ ] **Step 1: Register the missing core test module and write the policy tests first**

Add this beside `mod mcp_tool_call;` in `codex-rs/core/src/lib.rs`:

```rust
mod mcp_tool_call_policy;
#[cfg(test)]
#[path = "mcp_tool_call_policy_tests.rs"]
mod mcp_tool_call_policy_tests;
```

Create `codex-rs/core/src/mcp_tool_call_policy_tests.rs`. Define small real contributor implementations rather than mocking the registry:

```rust
use std::sync::Arc;

use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::McpToolCallPolicyContributor;
use codex_extension_api::McpToolCallPolicyDecision;
use codex_extension_api::McpToolCallPolicyFuture;
use codex_extension_api::McpToolCallPolicyInput;
use pretty_assertions::assert_eq;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::config::Config;
use crate::mcp_tool_call_policy::apply_mcp_tool_call_policies;

struct AddFields {
    expected_existing_key: Option<&'static str>,
    fields: Map<String, Value>,
}

impl McpToolCallPolicyContributor for AddFields {
    fn evaluate<'a>(
        &'a self,
        input: McpToolCallPolicyInput<'a>,
    ) -> McpToolCallPolicyFuture<'a> {
        Box::pin(async move {
            if let Some(key) = self.expected_existing_key {
                assert!(input.request_meta.contains_key(key));
            }
            McpToolCallPolicyDecision::Allow {
                additional_request_meta: self.fields.clone(),
            }
        })
    }
}

struct Deny(&'static str);

impl McpToolCallPolicyContributor for Deny {
    fn evaluate<'a>(
        &'a self,
        _input: McpToolCallPolicyInput<'a>,
    ) -> McpToolCallPolicyFuture<'a> {
        Box::pin(async move {
            McpToolCallPolicyDecision::Deny {
                reason: self.0.to_string(),
            }
        })
    }
}
```

Add four tests:

1. `empty_policy_registry_preserves_request_meta` passes arguments plus `{"callId":"call-1"}` through an empty registry and deep-compares the complete result.
2. `policy_contributors_add_metadata_in_registration_order` registers an `epoch` contributor followed by a contributor that requires `epoch` to be visible and adds `generation`; compare the complete merged map.
3. `policy_denial_returns_model_visible_reason` registers `Deny("record a plan first")` and asserts the full error text names the `game/click` call and includes that reason.
4. `policy_metadata_collision_is_rejected` attempts to add `callId` when it already exists and asserts the full collision error.

Each invocation must use the wished-for signature:

```rust
let arguments = json!({});
let request_meta = json!({"callId": "call-1"});

apply_mcp_tool_call_policies(
    &registry,
    "game",
    "get_app_state",
    "call-1",
    Some(&arguments),
    Some(request_meta),
)
.await
```

- [ ] **Step 2: Run the focused tests and verify the red failure**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-core mcp_tool_call_policy
```

Expected: compilation fails because `mcp_tool_call_policy.rs` and the contributor types do not exist. This is the required red signal; do not implement until the failure has been observed.

- [ ] **Step 3: Add the public contributor types**

Create `codex-rs/ext/extension-api/src/contributors/mcp_tool_call_policy.rs`:

```rust
use std::future::Future;
use std::pin::Pin;

use serde_json::Map;
use serde_json::Value;

/// Future returned while a host-owned MCP call policy evaluates one call.
pub type McpToolCallPolicyFuture<'a> =
    Pin<Box<dyn Future<Output = McpToolCallPolicyDecision> + Send + 'a>>;

/// Read-only MCP call information supplied immediately before dispatch.
pub struct McpToolCallPolicyInput<'a> {
    pub server_name: &'a str,
    pub tool_name: &'a str,
    pub call_id: &'a str,
    pub arguments: Option<&'a Value>,
    pub request_meta: &'a Map<String, Value>,
}

/// Host policy decision for one prepared MCP call.
#[derive(Debug, PartialEq)]
pub enum McpToolCallPolicyDecision {
    /// Permit dispatch and append metadata fields that do not already exist.
    Allow {
        additional_request_meta: Map<String, Value>,
    },
    /// Reject dispatch and return the reason to the model.
    Deny { reason: String },
}

/// Host-owned policy evaluated for every prepared MCP call.
///
/// Implementations must treat arguments and existing metadata as read-only.
/// They may deny a call or return additional metadata. Codex evaluates
/// contributors in registration order and rejects duplicate metadata keys.
pub trait McpToolCallPolicyContributor: Send + Sync {
    fn evaluate<'a>(
        &'a self,
        input: McpToolCallPolicyInput<'a>,
    ) -> McpToolCallPolicyFuture<'a>;
}
```

In `contributors.rs`, declare the private module and publicly re-export all four types. In `lib.rs`, re-export them from `contributors`. In `registry.rs`, add an `mcp_tool_call_policy_contributors` vector, initialize it empty, add a builder registration method, and add an immutable slice accessor. Preserve registration order.

- [ ] **Step 4: Implement the isolated core evaluator**

Create `codex-rs/core/src/mcp_tool_call_policy.rs` with one `pub(crate)` async function. It must:

1. Convert `None` metadata into an empty map and reject a non-object metadata value.
2. Iterate `extensions.mcp_tool_call_policy_contributors()` in order.
3. Pass each contributor the current merged map.
4. Return an error formatted as ``MCP call policy denied `{server}/{tool}`: {reason}`` for `Deny`.
5. Before inserting additions, reject any occupied key with ``MCP call policy for `{server}/{tool}` attempted to overwrite request metadata field `{key}```.
6. Return `None` only when the final map is empty; otherwise return `Some(Value::Object(map))`.

Use exhaustive matching on `McpToolCallPolicyDecision` and `serde_json::map::Entry`; do not use wildcard arms.

- [ ] **Step 5: Run the focused tests and verify green**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-core mcp_tool_call_policy
```

Expected: all four policy tests pass.

- [ ] **Step 6: Commit the contract and evaluator**

```bash
git add codex-rs/ext/extension-api/src/contributors/mcp_tool_call_policy.rs \
  codex-rs/ext/extension-api/src/contributors.rs \
  codex-rs/ext/extension-api/src/lib.rs \
  codex-rs/ext/extension-api/src/registry.rs \
  codex-rs/core/src/lib.rs \
  codex-rs/core/src/mcp_tool_call_policy.rs \
  codex-rs/core/src/mcp_tool_call_policy_tests.rs
git commit -m "feat(core): add MCP call policy contributors"
```

### Task 2: Apply the policy at the prepared MCP dispatch boundary

**Files:**
- Modify: `codex-rs/core/tests/suite/mcp_uds_bridge.rs`
- Modify: `codex-rs/core/src/mcp_tool_call.rs`

**Interfaces:**
- Consumes: `apply_mcp_tool_call_policies` from Task 1.
- Consumes: `McpToolCallPolicyContributor` from Task 1.
- Produces: policy-enriched `_meta` passed to `PreparedMcpCall::call_with_preparation` without modifying arguments or the bridge.

- [ ] **Step 1: Make the fake helper require the real owner lease**

In `mcp_uds_bridge.rs`, preserve the existing assertions for `callId`, `threadId`, and `x-codex-turn-metadata`, then remove and compare the three policy-owned fields:

```rust
let call_id = metadata
    .get("callId")
    .and_then(Value::as_str)
    .context("Codex call metadata should contain callId")?;
assert_eq!(
    metadata.get("call_id").and_then(Value::as_str),
    Some(call_id)
);
assert_eq!(
    metadata.get("epoch").and_then(Value::as_str),
    Some("campaign-epoch")
);
assert_eq!(metadata.get("generation").and_then(Value::as_u64), Some(7));
```

Define a test-only `OwnerLeasePolicy` implementing the new trait. It returns an empty addition for servers other than `game`; for `game`, it deep-compares the tool name and arguments, then constructs the additional metadata explicitly:

```rust
fn evaluate<'a>(
    &'a self,
    input: McpToolCallPolicyInput<'a>,
) -> McpToolCallPolicyFuture<'a> {
    Box::pin(async move {
        if input.server_name != "game" {
            return McpToolCallPolicyDecision::Allow {
                additional_request_meta: Map::new(),
            };
        }

        assert_eq!(input.tool_name, "get_app_state");
        assert_eq!(input.arguments, Some(&json!({})));

        let Value::Object(additional_request_meta) = json!({
            "epoch": "campaign-epoch",
            "generation": 7,
            "call_id": input.call_id,
        }) else {
            unreachable!("owner lease fixture must be an object");
        };

        McpToolCallPolicyDecision::Allow {
            additional_request_meta,
        }
    })
}
```

Register it with `ExtensionRegistryBuilder::<Config>` and pass the built registry through `test_codex().with_extensions(...)`.

- [ ] **Step 2: Run the bridge test and verify the red failure**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-core mcp_uds_bridge
```

Expected: the fake helper fails because `_meta` does not contain `epoch`, `generation`, or `call_id`. This proves the contributor is registered but not yet evaluated by the dispatch path.

- [ ] **Step 3: Call the evaluator immediately before dispatch**

In `mcp_tool_call.rs`, import `apply_mcp_tool_call_policies`. Inside the existing `call_with_preparation` closure, keep approval application, memory-mode marking, OpenAI file argument rewriting, standard Codex metadata, thread ID metadata, and sandbox-state metadata in their current order.

Immediately after `augment_mcp_tool_request_meta_with_sandbox_state(...).await?`, add:

```rust
let request_meta = apply_mcp_tool_call_policies(
    sess.services.extensions.as_ref(),
    &server,
    &tool_name,
    call_id,
    rewritten_arguments.as_ref(),
    request_meta,
)
.await?;
```

Do not reset the client session, move the call outside `call_with_preparation`, or change the tracing wrapper. A denied or colliding policy must return before `mcp_call_trace` starts and before the MCP client is called.

- [ ] **Step 4: Run the policy and bridge tests together**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-core 'mcp_tool_call_policy|mcp_uds_bridge'
```

If nextest treats the filter literally rather than as a regex, run the two focused commands separately. Expected: the four evaluator tests and the full Sol-to-UDS integration test pass.

- [ ] **Step 5: Verify existing MCP call behavior remains green**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just test -p codex-core rmcp_client
rustup run 1.95.0 just test -p codex-core code_mode
```

Expected: all selected existing direct and code-mode MCP tests pass, demonstrating the empty-registry path remains compatible.

- [ ] **Step 6: Commit the dispatch integration**

```bash
git add codex-rs/core/src/mcp_tool_call.rs \
  codex-rs/core/tests/suite/mcp_uds_bridge.rs
git commit -m "test(core): require owner metadata across MCP bridge"
```

### Task 3: Complete Stage 2 verification and handoff

**Files:**
- Modify only if verification exposes a Stage 2 defect: files already listed in Tasks 1 and 2.

**Interfaces:**
- Consumes: the completed policy seam and bridge characterization.
- Produces: a green, reviewable Stage 2 branch ready for the headless observation slice plan.

- [ ] **Step 1: Check scope, size, and unintended API changes**

Run:

```bash
git status --short
git diff --check HEAD~2
git diff --stat HEAD~2
git diff HEAD~2 -- codex-rs/stdio-to-uds codex-rs/uds codex-rs/config codex-rs/app-server-protocol
```

Expected: the last command is empty. Confirm there are no Cargo dependency, lockfile, schema, helper, bridge, configuration, or app-server changes. If the non-mechanical diff exceeds 500 lines, split or simplify before continuing.

- [ ] **Step 2: Run the complete changed-project test suite**

Ensure Cargo-only helper binaries exist before the full core run:

```bash
cd codex-rs
rustup run 1.95.0 cargo build -p codex-cli --bin codex
rustup run 1.95.0 cargo build -p codex-rmcp-client \
  --bin test_stdio_server --bin test_streamable_http_server
rustup run 1.95.0 just test -p codex-core
```

Use the already built `codex-code-mode-host`; if it is absent on Apple Silicon and the upstream V8 artifact is unavailable, follow the repository's `$update-v8-version` troubleshooting workflow and use the verified Codex `rusty-v8-v150.4.0` release assets. Expected core summary: every selected test passes, including `mcp_uds_bridge`.

- [ ] **Step 3: Ask before the workspace-wide test suite**

Ask the user for approval to run:

```bash
cd codex-rs
rustup run 1.95.0 just test
```

If approved, run it before lint or formatting. If declined, record that only the complete `codex-core` suite was run.

- [ ] **Step 4: Run scoped fixes and formatting last**

Run:

```bash
cd codex-rs
rustup run 1.95.0 just fix -p codex-extension-api
rustup run 1.95.0 just fix -p codex-core
rustup run 1.95.0 just fmt
```

Do not rerun tests afterward. Inspect `git status --short` and `git diff --check`; commit any intentional fix/format changes as:

```bash
git add codex-rs/ext/extension-api/src codex-rs/core/src \
  codex-rs/core/tests/suite/mcp_uds_bridge.rs
git commit -m "chore: finish MCP call policy stage"
```

Skip the commit when fix and fmt make no changes.

- [ ] **Step 5: Report Stage 2 evidence and the next boundary**

Report:

- the red failure observed before each implementation;
- focused policy, bridge, rmcp-client, code-mode, and complete core results;
- workspace-wide result or the user's decision not to run it;
- commit hashes and final diff size;
- confirmation that generic MCP calls, the byte bridge, and the signed helper were unchanged;
- the discovered real-helper contracts now represented hermetically: explicit approval plus `epoch`, `generation`, and `call_id`;
- that Stage 3 is the headless `codex-game-runner` observation slice and the first successful signed-helper GPT-5.6-Sol smoke.
