#![cfg(unix)]

use std::time::Duration;

use anyhow::Context;
use codex_game_runner::CampaignLimits;
use codex_game_runner::CampaignRun;
use codex_game_runner::CampaignTerminalState;
use codex_game_runner::ShutdownMode;
use core_test_support::responses;
use core_test_support::responses::mount_sse_once;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

#[allow(dead_code)]
#[path = "support/campaign.rs"]
mod campaign_support;
mod support;

use campaign_support::configure_runner_surface;
use support::continuous_game::ExpectedCall;
use support::continuous_game::PlannedClickStep;
use support::continuous_game::RunningContinuousCampaign;
use support::continuous_game::ScriptedGame;
use support::continuous_game::ScriptedOutcome;
use support::continuous_game::start_runtime;
use support::continuous_game::turn_script;

const EXEC_CALL_ID: &str = "continuous-campaign-exec-1";
const FIRST_ACTION_SHA256: &str =
    "f709b60a7bbf91028aa10498db469d2a47fd96669d5167f6163200718809b3e1";
const SECOND_ACTION_SHA256: &str =
    "a7dac61994a15cd79af522c0b96495adec4956f8bc8477906980905d34887dce";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_planned_actions_share_one_bounded_turn() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_remote!(Ok(()), "the fake Unix socket is host-local");

    let server = responses::start_mock_server().await;
    let temp = TempDir::new()?;
    let script = turn_script(
        &[
            PlannedClickStep {
                objective: "Advance the fake game from state one".to_string(),
                visible_state_summary: "Fake state one is visible".to_string(),
                x: 180,
                y: 640,
                expected_visible_result: "Fake state two".to_string(),
            },
            PlannedClickStep {
                objective: "Advance the fake game from state two".to_string(),
                visible_state_summary: "Fake state two is visible".to_string(),
                x: 240,
                y: 640,
                expected_visible_result: "Full victory screen".to_string(),
            },
        ],
        &ScriptedOutcome::Win {
            visible_evidence_summary: "The full fake victory screen is visible".to_string(),
            lesson: "The two planned advances completed the fake game".to_string(),
        },
    )?;
    let exec_response = mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("response-1"),
            responses::ev_custom_tool_call(EXEC_CALL_ID, "exec", &script),
            responses::ev_completed("response-1"),
        ]),
    )
    .await;
    let base = test_codex()
        .with_model_info_override("gpt-5.6-sol", |model| model.supports_search_tool = false)
        .with_config(configure_runner_surface)
        .build_with_auto_env(&server)
        .await?;
    base.codex.shutdown_and_wait().await?;
    let RunningContinuousCampaign {
        runtime,
        gate,
        policy,
        helper_task,
        spool_root,
    } = start_runtime(
        &base.config,
        &temp,
        ScriptedGame {
            calls: vec![
                ExpectedCall::Capture {
                    jpeg: vec![0xff, 0xd8, 0xff, 0xd9],
                },
                ExpectedCall::Click {
                    arguments: json!({"x": 180, "y": 640}),
                    action_sha256: FIRST_ACTION_SHA256.to_string(),
                },
                ExpectedCall::Capture {
                    jpeg: vec![0xff, 0xd8, 0xff, 0xda],
                },
                ExpectedCall::Click {
                    arguments: json!({"x": 240, "y": 640}),
                    action_sha256: SECOND_ACTION_SHA256.to_string(),
                },
                ExpectedCall::Capture {
                    jpeg: vec![0xff, 0xd8, 0xff, 0xdb],
                },
            ],
        },
    )
    .await?;

    let report = CampaignRun::new(CampaignLimits {
        turn_timeout: Duration::from_secs(30),
        post_mutation_timeout: Duration::from_secs(10),
        interrupt_timeout: Duration::from_secs(5),
    })
    .execute(
        &runtime.thread,
        &runtime.session_configured,
        policy.as_ref(),
        gate,
        "Gambonanza",
    )
    .await?;
    let cleanup_errors = runtime.shutdown(ShutdownMode::Completed).await;
    let trace = helper_task
        .await?
        .with_context(|| format!("scripted helper stopped before campaign report {report:?}"))?;

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
    assert_eq!(cleanup_errors, Vec::<String>::new());
    assert_eq!(
        trace.methods,
        vec![
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call:get_app_state",
            "tools/call:click",
            "tools/call:get_app_state",
            "tools/call:click",
            "tools/call:get_app_state",
        ]
    );
    assert_eq!(trace.captures.len(), 3);
    assert_eq!(trace.mutations.len(), 2);
    assert_eq!(
        trace
            .mutations
            .iter()
            .map(|mutation| {
                (
                    mutation.call_id == mutation.operation_id,
                    mutation.action_sha256.as_str(),
                    mutation.tool.as_str(),
                    &mutation.arguments,
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                true,
                FIRST_ACTION_SHA256,
                "click",
                &json!({"x": 180, "y": 640}),
            ),
            (
                true,
                SECOND_ACTION_SHA256,
                "click",
                &json!({"x": 240, "y": 640}),
            ),
        ]
    );
    assert_eq!(std::fs::read_dir(spool_root)?.count(), 0);
    assert!(exec_response.single_request().body_contains_text("Gambonanza"));
    server.verify().await;
    Ok(())
}
