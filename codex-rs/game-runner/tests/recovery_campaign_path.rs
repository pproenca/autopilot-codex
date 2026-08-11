#![cfg(unix)]

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use codex_game_runner::BridgeFocus;
use codex_game_runner::CampaignCheckpoint;
use codex_game_runner::CampaignCheckpointStore;
use codex_game_runner::CampaignCommand;
use codex_game_runner::CampaignController;
use codex_game_runner::CampaignLimits;
use codex_game_runner::CampaignStatus;
use codex_game_runner::CampaignTerminalState;
use codex_game_runner::ControllerConfig;
use codex_game_runner::DurableMutationResult;
use codex_game_runner::PauseReason;
use codex_game_runner::RunnerDeployment;
use core_test_support::responses;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::process::Child;

mod support;

use support::durable_game::ConnectionScript;
use support::durable_game::ExpectedCall;
use support::durable_game::ResponseTiming;
use support::durable_game::capture;
use support::durable_game::click;
use support::durable_game::start;

const CHILD_MODE: &str = "CODEX_GAME_RUNNER_CRASH_FIXTURE";
const CHILD_SIGNAL: &str = "CODEX_GAME_RUNNER_CRASH_SIGNAL";
const FIRST_ACTION_SHA256: &str =
    "f709b60a7bbf91028aa10498db469d2a47fd96669d5167f6163200718809b3e1";
const WINNING_ACTION_SHA256: &str =
    "a7dac61994a15cd79af522c0b96495adec4956f8bc8477906980905d34887dce";

#[derive(Clone, Copy)]
enum CrashBoundary {
    Plan,
    Authorization,
    Result,
    ConfirmedObservation,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_after_plan_resumes_from_fresh_pixels() -> anyhow::Result<()> {
    run_crash_boundary(CrashBoundary::Plan).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_after_durable_authorization_resumes_without_replay() -> anyhow::Result<()> {
    run_crash_boundary(CrashBoundary::Authorization).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_after_mutation_result_resumes_without_replay() -> anyhow::Result<()> {
    run_crash_boundary(CrashBoundary::Result).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_after_confirmed_observation_resumes_from_fresh_pixels() -> anyhow::Result<()> {
    run_crash_boundary(CrashBoundary::ConfirmedObservation).await
}

async fn run_crash_boundary(boundary: CrashBoundary) -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_remote!(Ok(()), "the fake Unix socket is host-local");

    let server = responses::start_mock_server().await;
    let temp = TempDir::new()?;
    write_mock_config(temp.path(), &server.uri())?;
    let release = Arc::new(tokio::sync::Notify::new());
    let game = start(
        &temp,
        vec![
            ConnectionScript {
                generation: 1,
                calls: initial_calls(boundary, Arc::clone(&release)),
            },
            ConnectionScript {
                generation: 2,
                calls: vec![
                    capture(jpeg(2)),
                    click(240, 640, WINNING_ACTION_SHA256),
                    capture(jpeg(3)),
                ],
            },
        ],
    )?;
    let response_mock = mount_sse_sequence(
        &server,
        vec![
            exec_response(
                "initial-response",
                "initial-exec",
                initial_script(boundary),
            ),
            exec_response("resume-response", "resume-exec", resumed_script()),
        ],
    )
    .await;
    let runner_executable = codex_utils_cargo_bin::cargo_bin("codex-game-runner")?;
    let signal_path = temp.path().join("finish-crash-fixture");
    let mut child = spawn_child(
        temp.path(),
        &game.socket_path,
        &runner_executable,
        &signal_path,
    )?;

    let original = wait_for_checkpoint(temp.path(), |checkpoint| reached(boundary, checkpoint))
    .await
    .with_context(|| {
        let requests = response_mock.requests();
        format!("mock received {} requests before the boundary", requests.len())
    })?;
    std::fs::write(&signal_path, [])?;
    release.notify_one();
    let status = child.wait().await?;
    anyhow::ensure!(status.success(), "crash fixture failed to shut down cleanly");
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
    let normalized = read_checkpoint(temp.path())?;
    assert_eq!(
        normalized
            .unresolved_mutation
            .as_ref()
            .map(|mutation| mutation.result),
        normalized_mutation_result(boundary)
    );

    controller.command(CampaignCommand::Resume).await?;
    let report = controller.wait_for_report().await?;
    controller.shutdown().await?;
    let trace = game.task.await??;

    assert_eq!(report.thread_id, original.thread_id);
    assert_eq!(report.owner_lease.generation, original.owner_generation + 1);
    assert_eq!(report.terminal_state, CampaignTerminalState::Won);
    assert_eq!(trace.duplicate_operation_ids, Vec::<String>::new());
    assert_eq!(
        trace
            .calls
            .iter()
            .find(|call| call.generation == 2)
            .map(|call| call.method.as_str()),
        Some("get_app_state")
    );
    assert_eq!(
        trace
            .calls
            .iter()
            .filter_map(|call| call.action_sha256.as_deref())
            .filter(|hash| *hash == FIRST_ACTION_SHA256)
            .count(),
        usize::from(!matches!(boundary, CrashBoundary::Plan))
    );
    let resume_request = &response_mock.requests()[1];
    match boundary {
        CrashBoundary::Authorization => {
            assert!(resume_request.body_contains_text("result=indeterminate"));
            assert!(resume_request.body_contains_text(FIRST_ACTION_SHA256));
        }
        CrashBoundary::Result => {
            assert!(resume_request.body_contains_text("result=success"));
            assert!(resume_request.body_contains_text(FIRST_ACTION_SHA256));
        }
        CrashBoundary::Plan | CrashBoundary::ConfirmedObservation => {
            assert!(resume_request.body_contains_text("recovery context: none"));
        }
    }
    assert_eq!(std::fs::read_dir(game.spool_root)?.count(), 0);
    server.verify().await;
    Ok(())
}

fn initial_calls(boundary: CrashBoundary, release: Arc<tokio::sync::Notify>) -> Vec<ExpectedCall> {
    let mut calls = vec![capture(jpeg(1))];
    match boundary {
        CrashBoundary::Plan => {}
        CrashBoundary::Authorization => calls.push(ExpectedCall::Mutation {
            tool: "click".to_string(),
            arguments: serde_json::json!({"x": 180, "y": 640}),
            action_sha256: FIRST_ACTION_SHA256.to_string(),
            timing: ResponseTiming::Held(release),
        }),
        CrashBoundary::Result => calls.push(click(180, 640, FIRST_ACTION_SHA256)),
        CrashBoundary::ConfirmedObservation => {
            calls.push(click(180, 640, FIRST_ACTION_SHA256));
            calls.push(capture(jpeg(4)));
        }
    }
    calls
}

fn reached(boundary: CrashBoundary, checkpoint: &CampaignCheckpoint) -> bool {
    match boundary {
        CrashBoundary::Plan => {
            checkpoint.decision_audit.plans_accepted == 1 && checkpoint.summary.total_actions == 0
        }
        CrashBoundary::Authorization => checkpoint
            .unresolved_mutation
            .as_ref()
            .is_some_and(|mutation| mutation.result == DurableMutationResult::Pending),
        CrashBoundary::Result => checkpoint
            .unresolved_mutation
            .as_ref()
            .is_some_and(|mutation| mutation.result == DurableMutationResult::Success),
        CrashBoundary::ConfirmedObservation => {
            checkpoint.unresolved_mutation.is_none()
                && checkpoint
                    .latest_observation
                    .as_ref()
                    .and_then(|observation| observation.confirms_action_sequence)
                    == Some(1)
        }
    }
}

fn normalized_mutation_result(boundary: CrashBoundary) -> Option<DurableMutationResult> {
    match boundary {
        CrashBoundary::Plan | CrashBoundary::ConfirmedObservation => None,
        CrashBoundary::Authorization => Some(DurableMutationResult::Indeterminate),
        CrashBoundary::Result => Some(DurableMutationResult::Success),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_fixture_child() -> anyhow::Result<()> {
    let Some(codex_home) = std::env::var_os(CHILD_MODE).map(PathBuf::from) else {
        return Ok(());
    };
    let socket_path = PathBuf::from(
        std::env::var_os("CODEX_GAME_RUNNER_CRASH_SOCKET")
            .context("child crash fixture omitted socket")?,
    );
    let runner_executable = PathBuf::from(
        std::env::var_os("CODEX_GAME_RUNNER_CRASH_BINARY")
            .context("child crash fixture omitted runner executable")?,
    );
    let signal_path = PathBuf::from(
        std::env::var_os(CHILD_SIGNAL).context("child crash fixture omitted signal path")?,
    );
    let mut controller = CampaignController::open(controller_config(
        &codex_home,
        socket_path,
        runner_executable,
    ))
    .await?;
    controller.command(CampaignCommand::Start).await?;
    tokio::select! {
        result = controller.wait_for_report() => {
            anyhow::bail!("crash fixture ended before shutdown signal: {result:?}");
        }
        () = wait_for_signal(&signal_path) => {}
    }
    controller.command(CampaignCommand::Pause).await?;
    controller.shutdown().await?;
    Ok(())
}

fn spawn_child(
    codex_home: &Path,
    socket_path: &Path,
    runner_executable: &Path,
    signal_path: &Path,
) -> anyhow::Result<Child> {
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .arg("--exact")
        .arg("crash_fixture_child")
        .arg("--nocapture")
        .env(CHILD_MODE, codex_home)
        .env("CODEX_GAME_RUNNER_CRASH_SOCKET", socket_path)
        .env("CODEX_GAME_RUNNER_CRASH_BINARY", runner_executable)
        .env(CHILD_SIGNAL, signal_path)
        .kill_on_drop(true);
    Ok(command.spawn()?)
}

async fn wait_for_signal(path: &Path) {
    loop {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
name = "Mock provider for durable recovery"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#
        ),
    )
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
    .context("timed out waiting for crash checkpoint")
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

fn initial_script(boundary: CrashBoundary) -> String {
    let tail = match boundary {
        CrashBoundary::Plan => {
            "await new Promise(resolve => setTimeout(resolve, 60000));"
        }
        CrashBoundary::Authorization => "await tools.mcp__game__click({x: 180, y: 640});",
        CrashBoundary::Result => {
            "await tools.mcp__game__click({x: 180, y: 640});\nawait new Promise(resolve => setTimeout(resolve, 60000));"
        }
        CrashBoundary::ConfirmedObservation => {
            "await tools.mcp__game__click({x: 180, y: 640});\nawait tools.mcp__game__get_app_state({});\nawait new Promise(resolve => setTimeout(resolve, 60000));"
        }
    };
    format!(
        r#"
const before = await tools.mcp__game__get_app_state({{}});
await tools.game_runner__record_plan({{
  observation_reference: before.structuredContent.artifact_uri,
  objective: "dispatch the first action",
  visible_state_summary: "the first action is visible",
  candidates: [
    {{action: "Advance", predicted_visible_consequence: "the game advances"}},
    {{action: "Wait", predicted_visible_consequence: "the game remains"}}
  ],
  chosen_action: {{tool: "click", arguments: {{x: 180, y: 640}}}},
  reason: "advance the fixture",
  expected_visible_result: "the next state",
  invalidation_condition: "the visible state changes"
}});
{tail}
"#
    )
}

fn resumed_script() -> String {
    r#"
const before = await tools.mcp__game__get_app_state({});
await tools.game_runner__record_plan({
  observation_reference: before.structuredContent.artifact_uri,
  objective: "win after observing the crash result",
  visible_state_summary: "the winning action is visible",
  candidates: [
    {action: "Win", predicted_visible_consequence: "the victory screen appears"},
    {action: "Wait", predicted_visible_consequence: "the game remains"}
  ],
  chosen_action: {tool: "click", arguments: {x: 240, y: 640}},
  reason: "the fresh pixels show the winning action",
  expected_visible_result: "the full victory screen",
  invalidation_condition: "the visible state changes"
});
await tools.mcp__game__click({x: 240, y: 640});
const after = await tools.mcp__game__get_app_state({});
await tools.game_runner__report_outcome({
  outcome: "win",
  observation_reference: after.structuredContent.artifact_uri,
  visible_evidence_summary: "the full victory screen is visible",
  lesson: "observe indeterminate actions before continuing"
});
text("victory recorded");
"#
    .to_string()
}

fn jpeg(number: u8) -> Vec<u8> {
    vec![0xff, 0xd8, number, 0xd9]
}
