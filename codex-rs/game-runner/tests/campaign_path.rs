#![cfg(unix)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use codex_core_api::AskForApproval;
use codex_core_api::Config;
use codex_core_api::Constrained;
use codex_core_api::Feature;
use codex_core_api::Features;
use codex_core_api::McpServerConfig;
use codex_core_api::PermissionProfile;
use codex_core_api::Permissions;
use codex_core_api::WebSearchMode;
use codex_game_runner::CampaignLimits;
use codex_game_runner::CampaignRun;
use codex_game_runner::CampaignTerminalState;
use codex_game_runner::CampaignTools;
use codex_game_runner::DecisionGate;
use codex_game_runner::GAME_SERVER_NAME;
use codex_game_runner::GENERATION;
use codex_game_runner::GameCallPolicy;
use codex_game_runner::MutationResult;
use codex_game_runner::OutcomeKind;
use codex_game_runner::RunnerRuntime;
use codex_game_runner::ShutdownMode;
use core_test_support::responses;
use core_test_support::responses::mount_sse_once;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::UnixListener;

mod support;
#[path = "support/campaign.rs"]
mod campaign_support;

use campaign_support::ACTION_SHA256;
use campaign_support::FakeGameScenario;
use campaign_support::serve_fake_game_mcp;

const EXEC_CALL_ID: &str = "campaign-exec-1";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn planned_action_crosses_dynamic_tools_policy_and_real_uds_bridge() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_remote!(Ok(()), "the fake Unix socket is host-local");

    let server = responses::start_mock_server().await;
    let temp = TempDir::new()?;
    let socket_path = temp.path().join("game.sock");
    let listener = UnixListener::bind(&socket_path)?;
    let spool_root = temp.path().join("screenshot-spool");
    let helper_task = tokio::spawn(serve_fake_game_mcp(
        listener,
        spool_root.clone(),
        FakeGameScenario::WinningMutation,
    ));
    let exec_response = mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("response-1"),
            responses::ev_custom_tool_call(EXEC_CALL_ID, "exec", campaign_script()),
            responses::ev_completed("response-1"),
        ]),
    )
    .await;
    let completion_response = mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("response-2"),
            responses::ev_assistant_message("message-2", "Stage 4A canary complete."),
            responses::ev_completed("response-2"),
        ]),
    )
    .await;
    let game_runner_bin = codex_utils_cargo_bin::cargo_bin("codex-game-runner")?;
    let code_mode_host_bin = codex_utils_cargo_bin::cargo_bin("codex-code-mode-host")?;
    let game_server = serde_json::from_value::<McpServerConfig>(json!({
        "command": game_runner_bin,
        "args": ["__stdio-to-uds", socket_path],
        "required": true,
        "supports_parallel_tool_calls": false,
        "default_tools_approval_mode": "approve",
        "enabled_tools": ["get_app_state", "wait", "click", "drag", "focus_click"],
        "startup_timeout_sec": 15,
        "tool_timeout_sec": 5,
    }))?;
    let base = test_codex()
        .with_model_info_override("gpt-5.6-sol", |model| model.supports_search_tool = false)
        .with_config(configure_runner_surface)
        .build_with_auto_env(&server)
        .await?;
    base.codex.shutdown_and_wait().await?;
    let mut config = base.config.clone();
    config.codex_self_exe = Some(game_runner_bin);
    config
        .mcp_servers
        .set(HashMap::from([(GAME_SERVER_NAME.to_string(), game_server)]))
        .context("set fake game MCP server")?;

    let gate = Arc::new(DecisionGate::new(GENERATION));
    let policy = Arc::new(GameCallPolicy::new(
        "test-epoch".to_string(),
        GENERATION,
        Arc::clone(&gate),
    ));
    let runtime = RunnerRuntime::start_with_code_mode_host(
        config,
        Arc::clone(&policy),
        CampaignTools::specs(),
        code_mode_host_bin,
    )
    .await
    .context("start runner runtime")?;
    wait_for_mcp_server(&runtime.thread, GAME_SERVER_NAME)
        .await
        .context("wait for fake game MCP server")?;
    let thread_id = runtime.thread_id;
    let rollout_path = runtime
        .session_configured
        .rollout_path
        .clone()
        .context("campaign thread should retain a rollout")?;

    let report = CampaignRun::new(CampaignLimits {
        max_turns: 2,
        total_timeout: Duration::from_secs(30),
        post_mutation_timeout: Duration::from_secs(10),
    })
    .execute(
        &runtime.thread,
        &runtime.session_configured,
        policy.as_ref(),
        Arc::clone(&gate),
        "Gambonanza",
    )
    .await
    .context("execute planned campaign")?;
    let cleanup_errors = runtime.shutdown(ShutdownMode::Completed).await;
    let exec_request = exec_response.single_request();
    let exec_request_body = exec_request.body_json();
    anyhow::ensure!(
        report.terminal_state == CampaignTerminalState::Won,
        "campaign returned {report:?}; initial request: {}; input: {}",
        exec_request.body_contains_text("Gambonanza"),
        exec_request_body["input"],
    );
    let trace = helper_task
        .await
        .context("fake game MCP task panicked")?
        .context("serve fake game MCP")?;

    assert_eq!(cleanup_errors, Vec::<String>::new());
    assert_eq!(report.terminal_state, CampaignTerminalState::Won);
    assert_eq!(report.thread_id, thread_id.to_string());
    assert_eq!(report.rollout_path, rollout_path);
    assert_eq!(report.turn_ids.len(), 1);
    assert_eq!(
        report
            .before
            .as_ref()
            .map(|item| (&item.call_id, &item.reference)),
        Some((&trace.before_call_id, &trace.before_reference))
    );
    assert_eq!(
        report
            .after
            .as_ref()
            .map(|item| (Some(&item.call_id), Some(&item.reference))),
        Some((trace.after_call_id.as_ref(), trace.after_reference.as_ref()))
    );
    let plan = report.accepted_plan.as_ref().context("accepted plan")?;
    assert_eq!((plan.id.as_str(), plan.action_sha256.as_str()), ("plan-1-1", ACTION_SHA256));
    assert_eq!(plan.draft.observation_reference, trace.before_reference);
    assert_eq!(plan.draft.candidates.len(), 2);
    let mutation = report.mutation.as_ref().context("authorized mutation")?;
    assert_eq!(
        (
            mutation.call_id.as_str(),
            mutation.operation_id.as_str(),
            mutation.action_sha256.as_str(),
            mutation.tool.as_str(),
            &mutation.arguments,
            report.mutation_result,
        ),
        (
            trace.mutation_call_id.as_deref().context("helper mutation")?,
            trace.mutation_call_id.as_deref().context("helper mutation")?,
            ACTION_SHA256,
            "click",
            &json!({"x": 180, "y": 640}),
            Some(MutationResult::Success),
        )
    );
    let outcome = report.outcome.as_ref().context("reported outcome")?;
    assert_eq!(outcome.draft.outcome, OutcomeKind::Win);
    assert_eq!(
        Some(&outcome.observation.reference),
        trace.after_reference.as_ref()
    );
    assert_eq!(report.owner_lease.epoch, "test-epoch");
    assert_eq!(report.owner_lease.generation, 1);
    assert_eq!(report.decision_audit.plans_accepted, 1);
    assert_eq!(report.decision_audit.mutation_attempts, 1);
    assert_eq!(report.decision_audit.mutation_authorizations, 1);
    assert_eq!(report.policy_audit.unknown_tool_attempts, 0);
    assert_eq!(report.terminal_failure, None);
    assert_eq!(
        trace.methods,
        vec![
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call:get_app_state",
            "tools/call:click",
            "tools/call:get_app_state",
        ]
    );
    assert_eq!(std::fs::read_dir(spool_root)?.count(), 0);

    let first_request = exec_response.single_request();
    assert!(first_request.body_contains_text("Gambonanza"));
    let first_request_body = first_request.body_json();
    let description = exec_description(&first_request_body)?;
    for required in [
        "mcp__game__get_app_state",
        "mcp__game__click",
        "game_runner__record_plan",
        "game_runner__report_outcome",
    ] {
        assert!(description.contains(required), "missing `{required}`");
    }
    assert!(!description.contains("mcp__game__zoom"));
    assert!(
        completion_response
            .single_request()
            .custom_tool_call_output(EXEC_CALL_ID)
            .is_object()
    );
    server.verify().await;
    Ok(())
}

fn campaign_script() -> &'static str {
    r#"const before = await tools.mcp__game__get_app_state({});
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
text(JSON.stringify({plan, mutation, outcome}));"#
}

fn configure_runner_surface(config: &mut Config) {
    config.permissions = Permissions::from_approval_and_profile(
        Constrained::allow_any(AskForApproval::Never),
        Constrained::allow_any(PermissionProfile::read_only()),
    )
    .expect("set runner permissions");
    let mut features = Features::default();
    features.enable(Feature::CodeMode);
    features.enable(Feature::CodeModeHost);
    features.enable(Feature::CodeModeOnly);
    assert!(config.features.set(features).is_ok());
    config.code_mode.excluded_tool_namespaces = vec!["functions".to_string()];
    assert!(config.web_search_mode.set(WebSearchMode::Disabled).is_ok());
    config.ephemeral = false;
    config.agents_enabled = false;
    config.project_doc_max_bytes = 0;
    config.include_permissions_instructions = false;
    config.include_apps_instructions = false;
    config.include_collaboration_mode_instructions = false;
    config.include_skill_instructions = false;
    config.include_environment_context = false;
    config.orchestrator_skills_enabled = false;
    config.orchestrator_mcp_enabled = false;
    config.experimental_request_user_input_enabled = false;
    config.update_plan_enabled = false;
}

fn exec_description(body: &Value) -> anyhow::Result<&str> {
    body["input"]
        .as_array()
        .and_then(|input| input.iter().find(|item| item["role"] == "developer"))
        .and_then(|developer| developer["tools"].as_array())
        .and_then(|namespaces| {
            namespaces
                .iter()
                .find(|namespace| namespace["name"] == "functions")
        })
        .and_then(|namespace| namespace["tools"].as_array())
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == "exec"))
        .and_then(|tool| tool["description"].as_str())
        .context("Sol code-mode exec description is missing")
}
