use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use clap::Parser;
use codex_core_api::Arg0DispatchPaths;
use codex_core_api::arg0_dispatch_or_else;
use codex_core_api::find_codex_home;
use codex_core_api::set_default_originator;
use codex_game_runner::CampaignLimits;
use codex_game_runner::CampaignReport;
use codex_game_runner::CampaignRun;
use codex_game_runner::CampaignTools;
use codex_game_runner::DecisionGate;
use codex_game_runner::GENERATION;
use codex_game_runner::GameCallPolicy;
use codex_game_runner::HelperLauncher;
use codex_game_runner::ReadinessLimits;
use codex_game_runner::RunnerDeployment;
use codex_game_runner::RunnerError;
use codex_game_runner::RunnerRuntime;
use codex_game_runner::ShutdownMode;
use uuid::Uuid;

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
        let [_, socket] = raw_args.as_slice() else {
            bail!("{BRIDGE_MODE} requires exactly one socket path");
        };
        return codex_game_runner::run_image_bridge(Path::new(socket)).await;
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
    let config = codex_game_runner::load_runner_config(&deployment, &runner_executable).await?;

    HelperLauncher::new(ReadinessLimits {
        timeout: Duration::from_secs(15),
        poll_interval: Duration::from_millis(100),
    })
    .ensure_serving(&deployment)
    .await?;

    let gate = Arc::new(DecisionGate::new(GENERATION));
    let policy = Arc::new(GameCallPolicy::new(
        Uuid::new_v4().to_string(),
        GENERATION,
        Arc::clone(&gate),
    ));
    let runtime = RunnerRuntime::start(config, Arc::clone(&policy), CampaignTools::specs()).await?;
    let campaign = CampaignRun::new(CampaignLimits::stage_4a())
        .execute(
            &runtime.thread,
            &runtime.session_configured,
            policy.as_ref(),
            gate,
            &deployment.target_app,
        )
        .await;
    let shutdown_mode = if campaign.is_ok() {
        ShutdownMode::Completed
    } else {
        ShutdownMode::Interrupt
    };
    let cleanup_errors = runtime.shutdown(shutdown_mode).await;

    match (campaign, cleanup_errors.is_empty()) {
        (Ok(report), true) => Ok(report),
        (Ok(_), false) => bail!(
            "game campaign cleanup failed: {}",
            cleanup_errors.join("; ")
        ),
        (Err(primary), true) => Err(primary.into()),
        (Err(primary), false) => Err(RunnerError::RunAndCleanupFailed {
            primary: Box::new(primary),
            cleanup: cleanup_errors.join("; "),
        }
        .into()),
    }
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
