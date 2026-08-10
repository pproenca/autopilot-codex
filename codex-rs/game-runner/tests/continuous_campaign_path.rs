#![cfg(unix)]

use std::time::Duration;

use anyhow::Context;
use codex_game_runner::CampaignLimits;
use codex_game_runner::CampaignRun;
use codex_game_runner::CampaignTerminalState;
use codex_game_runner::ShutdownMode;
use codex_game_runner::StrategyRecord;
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
const RESTART_ACTION_SHA256: &str =
    "566d921e26e3bbfafaa5e1bdb4357f1f95e57ee1ebc2e70722f11d22ce85f289";
const LOSING_ACTION_SHA256: &str =
    "bcea741f5cf88d51a502946a7394f4eb87009090ad0a80a67b0f9bcfef4be630";
const WINNING_ACTION_SHA256: &str =
    "061a8c138ec80a943941efe218d3bc90d77be1b64d08232e35545a94bd019cf7";

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

    let report = CampaignRun::new(test_limits())
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_losses_restart_visibly_and_eventually_win() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_remote!(Ok(()), "the fake Unix socket is host-local");

    let server = responses::start_mock_server().await;
    let temp = TempDir::new()?;
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
            step(
                "attempt three winning move",
                260,
                640,
                "full victory screen",
            ),
        ],
        &ScriptedOutcome::Win {
            visible_evidence_summary: "The full fake victory screen is visible".to_string(),
            lesson: "The mobility strategy defeated the final boss".to_string(),
        },
    )?;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            exec_response("response-loss-1", "campaign-loss-1", &turn_1),
            exec_response("response-loss-2", "campaign-loss-2", &turn_2),
            exec_response("response-win", "campaign-win", &turn_3),
        ],
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
                capture(1),
                click(180, 640, FIRST_ACTION_SHA256),
                capture(2),
                capture(3),
                click(510, 540, RESTART_ACTION_SHA256),
                capture(4),
                click(220, 640, LOSING_ACTION_SHA256),
                capture(5),
                capture(6),
                click(510, 540, RESTART_ACTION_SHA256),
                capture(7),
                click(260, 640, WINNING_ACTION_SHA256),
                capture(8),
            ],
        },
    )
    .await?;

    let report = CampaignRun::new(test_limits())
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
    assert_eq!(
        (
            report.decision_audit.plans_accepted,
            report.decision_audit.mutation_authorizations,
            report.policy_audit.mutation_authorizations,
            report.policy_audit.unknown_tool_attempts,
            trace.captures.len(),
            trace.mutations.len(),
            response_mock.requests().len(),
        ),
        (5, 5, 5, 0, 8, 5, 3)
    );
    assert_eq!(
        trace
            .mutations
            .iter()
            .map(|mutation| mutation.action_sha256.as_str())
            .collect::<Vec<_>>(),
        vec![
            FIRST_ACTION_SHA256,
            RESTART_ACTION_SHA256,
            LOSING_ACTION_SHA256,
            RESTART_ACTION_SHA256,
            WINNING_ACTION_SHA256,
        ]
    );
    let final_reference = &trace.captures[7].reference;
    assert_eq!(
        (
            report.after.as_ref().map(|after| &after.reference),
            report
                .outcome
                .as_ref()
                .map(|outcome| &outcome.observation.reference),
        ),
        (Some(final_reference), Some(final_reference))
    );
    assert_eq!(cleanup_errors, Vec::<String>::new());
    assert_eq!(std::fs::read_dir(spool_root)?.count(), 0);
    server.verify().await;
    Ok(())
}

fn test_limits() -> CampaignLimits {
    CampaignLimits {
        turn_timeout: Duration::from_secs(30),
        post_mutation_timeout: Duration::from_secs(10),
        interrupt_timeout: Duration::from_secs(5),
    }
}

fn step(objective: &str, x: i64, y: i64, expected_visible_result: &str) -> PlannedClickStep {
    PlannedClickStep {
        objective: objective.to_string(),
        visible_state_summary: format!("Visible state before {objective}"),
        x,
        y,
        expected_visible_result: expected_visible_result.to_string(),
    }
}

fn loss(strategy: StrategyRecord) -> ScriptedOutcome {
    ScriptedOutcome::Loss {
        visible_evidence_summary: "The full fake loss screen is visible".to_string(),
        lesson: "The previous attempt needs a replacement strategy".to_string(),
        strategy,
    }
}

fn economy_strategy() -> StrategyRecord {
    StrategyRecord {
        summary: "Preserve currency for the boss shop".to_string(),
        confirmed_mechanics: vec!["Shop purchases improve later encounters".to_string()],
        failed_approaches: vec!["Spending currency immediately".to_string()],
        shop_and_boss_notes: vec!["Save for the boss counter".to_string()],
        next_attempt_priorities: vec!["Build economy before the boss".to_string()],
    }
}

fn mobility_strategy() -> StrategyRecord {
    StrategyRecord {
        summary: "Prioritize mobility for the final boss".to_string(),
        confirmed_mechanics: vec!["Moving tiles open safe boss lanes".to_string()],
        failed_approaches: vec!["Static defense cannot survive the boss".to_string()],
        shop_and_boss_notes: vec!["Buy the mobility superpower".to_string()],
        next_attempt_priorities: vec!["Reach the boss with mobility ready".to_string()],
    }
}

fn capture(number: u8) -> ExpectedCall {
    ExpectedCall::Capture {
        jpeg: vec![0xff, 0xd8, 0xff, 0xd0 + number],
    }
}

fn click(x: i64, y: i64, action_sha256: &str) -> ExpectedCall {
    ExpectedCall::Click {
        arguments: json!({"x": x, "y": y}),
        action_sha256: action_sha256.to_string(),
    }
}

fn exec_response(response_id: &str, call_id: &str, script: &str) -> String {
    responses::sse(vec![
        responses::ev_response_created(response_id),
        responses::ev_custom_tool_call(call_id, "exec", script),
        responses::ev_completed(response_id),
    ])
}
