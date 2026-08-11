use std::future::Future;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::bail;
use clap::Parser;
use codex_core_api::Arg0DispatchPaths;
use codex_core_api::arg0_dispatch_or_else;
use codex_core_api::find_codex_home;
use codex_core_api::set_default_originator;
use codex_game_runner::BridgeFocus;
use codex_game_runner::CampaignCommand;
use codex_game_runner::CampaignController;
use codex_game_runner::CampaignLimits;
use codex_game_runner::CampaignReport;
use codex_game_runner::ControllerConfig;
use codex_game_runner::ControllerError;
use codex_game_runner::RunnerDeployment;
use codex_game_runner::RunnerError;

const BRIDGE_MODE: &str = "__stdio-to-uds";

#[derive(Debug, Parser, PartialEq, Eq)]
#[command(name = "codex-game-runner")]
struct Args {
    #[arg(long, value_name = "APP_BUNDLE")]
    helper_app: PathBuf,

    #[arg(long, value_name = "SOCKET")]
    socket: PathBuf,

    #[arg(long, value_name = "APP_NAME")]
    target_app: String,
}

fn main() -> anyhow::Result<()> {
    dispatch_main(run_main)
}

fn dispatch_main<F, Fut>(main_fn: F) -> anyhow::Result<()>
where
    F: FnOnce(Arg0DispatchPaths) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>>,
{
    arg0_dispatch_or_else(main_fn)
}

async fn run_main(arg0_paths: Arg0DispatchPaths) -> anyhow::Result<()> {
    let raw_args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if raw_args.first().and_then(|arg| arg.to_str()) == Some(BRIDGE_MODE) {
        return match raw_args.as_slice() {
            [_, socket, target_app] => {
                codex_game_runner::run_image_bridge(
                    Path::new(socket),
                    target_app.to_string_lossy().as_ref(),
                )
                .await
            }
            [_, socket] => {
                codex_game_runner::run_image_bridge_without_focus(Path::new(socket)).await
            }
            _ => bail!("{BRIDGE_MODE} requires one socket path and an optional target app"),
        };
    }

    if let Err(err) = set_default_originator("codex_game_runner".to_string()) {
        tracing::warn!("failed to set originator: {err:?}");
    }

    let runner_executable = arg0_paths
        .codex_self_exe
        .context("resolve game runner executable")?;
    let report = run(Args::parse(), runner_executable).await?;
    serde_json::to_writer(std::io::stdout().lock(), &report)
        .context("serialize campaign report")?;
    if !report.terminal_state.is_success() {
        bail!("game campaign ended in {:?}", report.terminal_state);
    }
    Ok(())
}

async fn run(args: Args, runner_executable: PathBuf) -> anyhow::Result<CampaignReport> {
    let codex_home = find_codex_home()
        .context("find Codex home")
        .map_err(|source| RunnerError::Config { source })?;
    let deployment = RunnerDeployment {
        helper_app: args.helper_app,
        socket_path: args.socket,
        target_app: args.target_app,
        codex_home: codex_home.to_path_buf(),
    };
    let mut controller = CampaignController::open(ControllerConfig {
        deployment,
        runner_executable,
        bridge_focus: BridgeFocus::TargetApplication,
        limits: CampaignLimits::stage_4b1(),
    })
    .await
    .map_err(map_controller_error)?;
    if let Err(error) = controller.command(CampaignCommand::Start).await {
        controller.shutdown().await.map_err(map_controller_error)?;
        return Err(map_controller_error(error));
    }
    let report = controller.wait_for_report().await;
    let cleanup = controller.shutdown().await;
    match (report, cleanup) {
        (Ok(report), Ok(())) => Ok(report),
        (Ok(_), Err(error)) => Err(map_controller_error(error)),
        (Err(error), Ok(())) => Err(map_controller_error(error)),
        (Err(primary), Err(cleanup)) => {
            bail!("game campaign failed: {primary}; controller cleanup failed: {cleanup}")
        }
    }
}

fn map_controller_error(error: ControllerError) -> anyhow::Error {
    match error {
        ControllerError::CampaignRequiresResume { path } => {
            RunnerError::CampaignRequiresResume { path }.into()
        }
        error => error.into(),
    }
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
