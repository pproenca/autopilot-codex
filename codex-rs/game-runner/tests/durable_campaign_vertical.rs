#![cfg(unix)]

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use codex_game_runner::BridgeFocus;
use codex_game_runner::CampaignCheckpoint;
use codex_game_runner::CampaignCheckpointStore;
use codex_game_runner::CampaignCommand;
use codex_game_runner::CampaignController;
use codex_game_runner::CampaignEvent;
use codex_game_runner::CampaignLimits;
use codex_game_runner::CampaignStatus;
use codex_game_runner::CampaignSummary;
use codex_game_runner::CampaignTerminalState;
use codex_game_runner::ControllerConfig;
use codex_game_runner::DurableCampaignState;
use codex_game_runner::PauseReason;
use codex_game_runner::RunnerDeployment;
use codex_game_runner::StrategyRecord;
use codex_game_runner::MAX_CHECKPOINT_BYTES;
use core_test_support::responses;
use core_test_support::responses::mount_compact_user_history_with_summary_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::process::Child;

mod support;

use support::durable_game::ConnectionScript;
use support::durable_game::capture;
use support::durable_game::click;
use support::durable_game::start;

const CHILD_MODE: &str = "CODEX_GAME_RUNNER_VERTICAL_FIXTURE";
const CHILD_COMPACTED: &str = "CODEX_GAME_RUNNER_VERTICAL_COMPACTED";
const CHILD_FINISH: &str = "CODEX_GAME_RUNNER_VERTICAL_FINISH";
const MOBILITY_MARKER: &str = "mobility-before-boss";
const LOSING_ACTION_SHA256: &str =
    "f709b60a7bbf91028aa10498db469d2a47fd96669d5167f6163200718809b3e1";
const RESTART_ACTION_SHA256: &str =
    "566d921e26e3bbfafaa5e1bdb4357f1f95e57ee1ebc2e70722f11d22ce85f289";
const WINNING_ACTION_SHA256: &str =
    "061a8c138ec80a943941efe218d3bc90d77be1b64d08232e35545a94bd019cf7";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loss_compaction_crash_resume_restart_and_victory() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_remote!(Ok(()), "the fake Unix socket is host-local");

    let server = responses::start_mock_server().await;
    let temp = TempDir::new()?;
    write_mock_config(temp.path(), &server.uri())?;
    let game = start(
        &temp,
        vec![
            ConnectionScript {
                generation: 1,
                calls: vec![
                    capture(jpeg(1)),
                    click(180, 640, LOSING_ACTION_SHA256),
                    capture(jpeg(2)),
                ],
            },
            ConnectionScript {
                generation: 2,
                calls: vec![
                    capture(jpeg(3)),
                    click(510, 540, RESTART_ACTION_SHA256),
                    capture(jpeg(4)),
                    click(260, 640, WINNING_ACTION_SHA256),
                    capture(jpeg(5)),
                ],
            },
        ],
    )?;
    let response_mock = mount_sse_sequence(
        &server,
        vec![
            exec_response("loss-response", "loss-exec", loss_script()),
            exec_response("hold-response", "hold-exec", hold_script()),
            exec_response("win-response", "win-exec", victory_script()),
        ],
    )
    .await;
    let compact_mock =
        mount_compact_user_history_with_summary_once(&server, "attempt one compacted").await;
    let runner_executable = codex_utils_cargo_bin::cargo_bin("codex-game-runner")?;
    let compacted_path = temp.path().join("context-compacted");
    let finish_path = temp.path().join("finish-vertical-fixture");
    let mut child = spawn_child(
        temp.path(),
        &game.socket_path,
        &runner_executable,
        &compacted_path,
        &finish_path,
    )?;

    wait_for_file(&compacted_path).await?;
    let original = wait_for_checkpoint(temp.path(), |checkpoint| {
        checkpoint.summary.attempt_number == 2
            && checkpoint.summary.losses == 1
            && checkpoint.summary.total_actions == 1
            && checkpoint.summary.strategy.as_ref() == Some(&mobility_strategy())
    })
    .await?;
    std::fs::write(&finish_path, [])?;
    let status = child.wait().await?;
    anyhow::ensure!(status.success(), "vertical fixture failed to shut down cleanly");
    restore_running_checkpoint(temp.path(), &original)?;

    let mut controller = CampaignController::open(controller_config(
        temp.path(),
        game.socket_path.clone(),
        runner_executable,
    ))
    .await?;
    assert_eq!(
        controller.status(),
        CampaignStatus::Paused {
            reason: PauseReason::UnexpectedExit,
        }
    );
    assert_eq!(response_mock.requests().len(), 2);
    controller.command(CampaignCommand::Resume).await?;
    let report = controller.wait_for_report().await?;
    controller.shutdown().await?;
    let trace = game.task.await??;
    let final_checkpoint = read_checkpoint(temp.path())?;

    assert_eq!(report.thread_id, original.thread_id);
    assert_eq!(report.owner_lease.generation, 2);
    assert_eq!(report.terminal_state, CampaignTerminalState::Won);
    assert_eq!(
        (
            report.attempt_number,
            report.losses,
            report.total_actions,
            report.strategy.as_ref(),
        ),
        (2, 1, 3, Some(&mobility_strategy()))
    );
    let DurableCampaignState::Won { evidence_reference } = &final_checkpoint.state else {
        anyhow::bail!("final checkpoint was not won: {final_checkpoint:?}");
    };
    assert_eq!(
        Some(evidence_reference),
        final_checkpoint
            .latest_observation
            .as_ref()
            .map(|observation| &observation.reference)
    );
    assert_eq!(
        report
            .outcome
            .as_ref()
            .map(|outcome| &outcome.observation.reference),
        Some(evidence_reference)
    );
    assert_eq!(
        final_checkpoint.summary,
        CampaignSummary {
            attempt_number: report.attempt_number,
            total_turns: report.total_turns,
            total_actions: report.total_actions,
            losses: report.losses,
            strategy: report.strategy.clone(),
            recent_turn_ids: report.recent_turn_ids.clone(),
        }
    );
    assert_eq!(final_checkpoint.owner_generation, 2);
    assert_eq!(trace.connections, vec![1, 2]);
    assert_eq!(trace.duplicate_operation_ids, Vec::<String>::new());
    assert_eq!(
        trace
            .calls
            .iter()
            .filter_map(|call| call.action_sha256.as_deref())
            .collect::<Vec<_>>(),
        vec![
            LOSING_ACTION_SHA256,
            RESTART_ACTION_SHA256,
            WINNING_ACTION_SHA256,
        ]
    );
    assert_eq!(
        trace
            .calls
            .iter()
            .filter_map(|call| call.operation_id.as_ref())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert_eq!(
        trace.calls.iter().filter(|call| call.method == "get_app_state").count(),
        5
    );
    assert_eq!(
        trace
            .calls
            .iter()
            .find(|call| call.generation == 2)
            .map(|call| call.method.as_str()),
        Some("get_app_state")
    );
    assert_eq!(trace.calls.last().map(|call| call.method.as_str()), Some("get_app_state"));
    assert_ne!(
        report.before.as_ref().map(|observation| &observation.reference),
        original
            .latest_observation
            .as_ref()
            .map(|observation| &observation.reference)
    );
    assert_eq!(
        report.after.as_ref().map(|observation| &observation.reference),
        Some(evidence_reference)
    );
    let resumed_request = &response_mock.requests()[2];
    let resumed_body = resumed_request.body_json().to_string();
    assert_eq!(resumed_body.matches(MOBILITY_MARKER).count(), 1);
    assert_eq!(compact_mock.requests().len(), 1);
    assert!(final_checkpoint.encode()?.len() <= MAX_CHECKPOINT_BYTES);
    assert_eq!(std::fs::read_dir(game.spool_root)?.count(), 0);
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vertical_fixture_child() -> anyhow::Result<()> {
    let Some(codex_home) = std::env::var_os(CHILD_MODE).map(PathBuf::from) else {
        return Ok(());
    };
    let socket_path = env_path("CODEX_GAME_RUNNER_VERTICAL_SOCKET")?;
    let runner_executable = env_path("CODEX_GAME_RUNNER_VERTICAL_BINARY")?;
    let compacted_path = env_path(CHILD_COMPACTED)?;
    let finish_path = env_path(CHILD_FINISH)?;
    let mut controller = CampaignController::open(controller_config(
        &codex_home,
        socket_path,
        runner_executable,
    ))
    .await?;
    let mut events = controller.subscribe();
    controller.command(CampaignCommand::Start).await?;
    loop {
        if matches!(events.recv().await?, CampaignEvent::Outcome(_)) {
            break;
        }
    }
    controller.compact().await?;
    loop {
        let event = events.recv().await?;
        if matches!(event, CampaignEvent::ContextCompacted) {
            break;
        }
    }
    loop {
        if matches!(
            events.recv().await?,
            CampaignEvent::Progress(summary) if summary.total_turns >= 2
        ) {
            break;
        }
    }
    std::fs::write(compacted_path, [])?;
    wait_for_file(&finish_path).await?;
    controller.command(CampaignCommand::Pause).await?;
    controller.shutdown().await?;
    Ok(())
}

fn spawn_child(
    codex_home: &Path,
    socket_path: &Path,
    runner_executable: &Path,
    compacted_path: &Path,
    finish_path: &Path,
) -> anyhow::Result<Child> {
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .arg("--exact")
        .arg("vertical_fixture_child")
        .arg("--nocapture")
        .env(CHILD_MODE, codex_home)
        .env("CODEX_GAME_RUNNER_VERTICAL_SOCKET", socket_path)
        .env("CODEX_GAME_RUNNER_VERTICAL_BINARY", runner_executable)
        .env(CHILD_COMPACTED, compacted_path)
        .env(CHILD_FINISH, finish_path)
        .kill_on_drop(true);
    Ok(command.spawn()?)
}

fn controller_config(
    codex_home: &Path,
    socket_path: PathBuf,
    runner_executable: PathBuf,
) -> ControllerConfig {
    ControllerConfig {
        deployment: RunnerDeployment {
            helper_app: codex_home.join("missing-helper.app"),
            socket_path,
            target_app: "Gambonanza".to_string(),
            codex_home: codex_home.to_path_buf(),
        },
        runner_executable,
        bridge_focus: BridgeFocus::PreserveCurrent,
        limits: CampaignLimits {
            turn_timeout: Duration::from_secs(30),
            post_mutation_timeout: Duration::from_secs(10),
            interrupt_timeout: Duration::from_secs(5),
        },
    }
}

fn write_mock_config(codex_home: &Path, server_uri: &str) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"
model_provider = "mock_provider"

[model_providers.mock_provider]
name = "OpenAI"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#
        ),
    )
}

fn env_path(name: &str) -> anyhow::Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("vertical fixture omitted {name}"))
}

async fn wait_for_file(path: &Path) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("timed out waiting for vertical fixture signal")
}

async fn wait_for_checkpoint(
    codex_home: &Path,
    predicate: impl Fn(&CampaignCheckpoint) -> bool,
) -> anyhow::Result<CampaignCheckpoint> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(checkpoint) = read_checkpoint(codex_home)
                && predicate(&checkpoint)
            {
                return checkpoint;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("timed out waiting for vertical checkpoint")
}

fn restore_running_checkpoint(
    codex_home: &Path,
    checkpoint: &CampaignCheckpoint,
) -> anyhow::Result<()> {
    let (store, guard) = CampaignCheckpointStore::open(codex_home)?;
    store.replace(checkpoint)?;
    drop(guard);
    Ok(())
}

fn read_checkpoint(codex_home: &Path) -> anyhow::Result<CampaignCheckpoint> {
    let bytes = std::fs::read(codex_home.join("game-runner/campaign.json"))?;
    Ok(CampaignCheckpoint::decode(&bytes)?)
}

fn exec_response(response_id: &str, call_id: &str, script: String) -> String {
    responses::sse(vec![
        responses::ev_response_created(response_id),
        responses::ev_custom_tool_call(call_id, "exec", &script),
        responses::ev_completed(response_id),
    ])
}

fn mobility_strategy() -> StrategyRecord {
    StrategyRecord {
        summary: MOBILITY_MARKER.to_string(),
        confirmed_mechanics: vec!["moving tiles require a mobility reserve".to_string()],
        failed_approaches: vec!["spending mobility before the boss".to_string()],
        shop_and_boss_notes: vec!["save the dash purchase for the boss".to_string()],
        next_attempt_priorities: vec!["restart visibly, then preserve mobility".to_string()],
    }
}

fn loss_script() -> String {
    scripted_turn(
        &[(180, 640, "take the losing move", "the loss screen")],
        &format!(
            r#"outcome: "loss",
  observation_reference: after.structuredContent.artifact_uri,
  visible_evidence_summary: "the loss screen is visible",
  lesson: "preserve mobility",
  strategy: {}"#,
            serde_json::to_string(&mobility_strategy()).expect("serialize mobility strategy")
        ),
    )
}

fn victory_script() -> String {
    scripted_turn(
        &[
            (510, 540, "click the visible restart", "the fresh run screen"),
            (260, 640, "take the winning move", "the full victory screen"),
        ],
        r#"outcome: "win",
  observation_reference: after.structuredContent.artifact_uri,
  visible_evidence_summary: "the full victory screen is visible",
  lesson: "retained mobility wins the boss""#,
    )
}

fn scripted_turn(steps: &[(i64, i64, &str, &str)], outcome: &str) -> String {
    use std::fmt::Write;

    let mut script = String::new();
    for (index, (x, y, objective, result)) in steps.iter().enumerate() {
        writeln!(script, "const before{index} = await tools.mcp__game__get_app_state({{}});")
            .expect("write capture script");
        writeln!(
            script,
            r#"await tools.game_runner__record_plan({{
  observation_reference: before{index}.structuredContent.artifact_uri,
  objective: {objective:?},
  visible_state_summary: "the required control is visible",
  candidates: [
    {{action: "Advance", predicted_visible_consequence: {result:?}}},
    {{action: "Wait", predicted_visible_consequence: "the state remains"}}
  ],
  chosen_action: {{tool: "click", arguments: {{x: {x}, y: {y}}}}},
  reason: "the fixture exposes this exact action",
  expected_visible_result: {result:?},
  invalidation_condition: "the visible state changes"
}});
await tools.mcp__game__click({{x: {x}, y: {y}}});"#
        )
        .expect("write planned action script");
    }
    writeln!(script, "const after = await tools.mcp__game__get_app_state({{}});")
        .expect("write outcome capture");
    writeln!(
        script,
        "await tools.game_runner__report_outcome({{\n  {outcome}\n}});"
    )
    .expect("write outcome script");
    script
}

fn hold_script() -> String {
    "await new Promise(resolve => setTimeout(resolve, 60000));".to_string()
}

fn jpeg(number: u8) -> Vec<u8> {
    vec![0xff, 0xd8, number, 0xd9]
}
