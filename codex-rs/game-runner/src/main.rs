use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use clap::Parser;
use codex_core_api::Arg0DispatchPaths;
use codex_core_api::AuthManager;
use codex_core_api::CodexAppsToolsCache;
use codex_core_api::Config;
use codex_core_api::EnvironmentManager;
use codex_core_api::ExecServerRuntimePaths;
use codex_core_api::ExtensionRegistryBuilder;
use codex_core_api::LoadUserInstructionsFuture;
use codex_core_api::LoadedUserInstructions;
use codex_core_api::NewThread;
use codex_core_api::Op;
use codex_core_api::SessionSource;
use codex_core_api::StartThreadOptions;
use codex_core_api::ThreadManager;
use codex_core_api::UserInstructionsProvider;
use codex_core_api::arg0_dispatch_or_else;
use codex_core_api::build_models_manager;
use codex_core_api::find_codex_home;
use codex_core_api::init_state_db;
use codex_core_api::local_agent_graph_store_from_state_db;
use codex_core_api::resolve_installation_id;
use codex_core_api::set_default_originator;
use codex_core_api::thread_store_from_config;
use codex_game_runner::GENERATION;
use codex_game_runner::GameCallPolicy;
use codex_game_runner::HelperLauncher;
use codex_game_runner::ObservationLimits;
use codex_game_runner::ObservationReport;
use codex_game_runner::ObservationRun;
use codex_game_runner::ReadinessLimits;
use codex_game_runner::RunnerDeployment;
use codex_game_runner::RunnerError;
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

struct NoUserInstructions;

impl UserInstructionsProvider for NoUserInstructions {
    fn load_user_instructions(&self) -> LoadUserInstructionsFuture<'_> {
        Box::pin(async { LoadedUserInstructions::default() })
    }
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
        return codex_stdio_to_uds::run(Path::new(socket)).await;
    }

    if let Err(err) = set_default_originator("codex_game_runner".to_string()) {
        tracing::warn!("failed to set originator: {err:?}");
    }

    let runner_executable = arg0_paths
        .codex_self_exe
        .context("resolve game runner executable")?;
    let report = run(Args::parse(), runner_executable).await?;
    serde_json::to_writer(std::io::stdout().lock(), &report)
        .context("serialize observation report")?;
    Ok(())
}

async fn run(args: Args, runner_executable: PathBuf) -> anyhow::Result<ObservationReport> {
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

    let state_db = init_state_db(&config).await;
    let auth_manager =
        AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false).await;
    let local_runtime_paths = ExecServerRuntimePaths::from_optional_paths(
        config.codex_self_exe.clone(),
        config.codex_linux_sandbox_exe.clone(),
    )
    .context("resolve local execution runtime")
    .map_err(thread_startup_error)?;
    let thread_store = thread_store_from_config(&config, state_db.clone());
    let environment_manager = Arc::new(
        EnvironmentManager::from_codex_home(
            config.codex_home.clone(),
            Some(local_runtime_paths),
            config.http_client_factory(),
        )
        .await
        .context("initialize environment manager")
        .map_err(thread_startup_error)?,
    );
    let installation_id = resolve_installation_id(&config.codex_home)
        .await
        .context("resolve installation identity")
        .map_err(thread_startup_error)?;

    let policy = Arc::new(GameCallPolicy::new(Uuid::new_v4().to_string(), GENERATION));
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.mcp_tool_call_policy_contributor(policy.clone());
    let thread_manager = ThreadManager::new(
        &config,
        Arc::clone(&auth_manager),
        build_models_manager(&config, auth_manager),
        CodexAppsToolsCache::default(),
        SessionSource::Custom("game_runner".to_string()),
        environment_manager,
        Arc::new(extensions.build()),
        Arc::new(NoUserInstructions),
        /*analytics_events_client*/ None,
        Arc::clone(&thread_store),
        local_agent_graph_store_from_state_db(state_db.as_ref()),
        installation_id,
        /*attestation_provider*/ None,
        /*external_time_provider*/ None,
    );

    let NewThread {
        thread_id,
        thread,
        session_configured,
    } = thread_manager
        .start_thread(StartThreadOptions::new(config))
        .await
        .context("start game observation thread")
        .map_err(thread_startup_error)?;

    let observation = ObservationRun::new(ObservationLimits {
        turn_timeout: Duration::from_secs(5 * 60),
    })
    .execute(
        &thread,
        &session_configured,
        policy.as_ref(),
        &deployment.target_app,
    )
    .await;

    let mut cleanup_errors = Vec::new();
    if observation.is_err()
        && let Err(error) = thread.submit(Op::Interrupt).await
    {
        cleanup_errors.push(format!("interrupt failed: {error}"));
    }
    if let Err(error) = thread.shutdown_and_wait().await {
        cleanup_errors.push(format!("shutdown failed: {error}"));
    }
    let _ = thread_manager.remove_thread(&thread_id).await;

    match (observation, cleanup_errors.is_empty()) {
        (Ok(report), true) => Ok(report),
        (Ok(_), false) => bail!(
            "game observation cleanup failed: {}",
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

fn thread_startup_error(source: anyhow::Error) -> RunnerError {
    RunnerError::ThreadStartup { source }
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
