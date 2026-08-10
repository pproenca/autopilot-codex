use std::sync::Arc;

use anyhow::Context;
use codex_core_api::AuthManager;
use codex_core_api::CodexAppsToolsCache;
use codex_core_api::CodexThread;
use codex_core_api::Config;
use codex_core_api::DynamicToolSpec;
use codex_core_api::EnvironmentManager;
use codex_core_api::ExecServerRuntimePaths;
use codex_core_api::ExtensionRegistryBuilder;
use codex_core_api::LoadUserInstructionsFuture;
use codex_core_api::LoadedUserInstructions;
use codex_core_api::NewThread;
use codex_core_api::Op;
use codex_core_api::SessionConfiguredEvent;
use codex_core_api::SessionSource;
use codex_core_api::StartThreadOptions;
use codex_core_api::ThreadId;
use codex_core_api::ThreadManager;
use codex_core_api::UserInstructionsProvider;
use codex_core_api::build_models_manager;
use codex_core_api::init_state_db;
use codex_core_api::local_agent_graph_store_from_state_db;
use codex_core_api::resolve_installation_id;
use codex_core_api::thread_store_from_config;

use crate::GameCallPolicy;
use crate::RunnerError;

struct NoUserInstructions;

impl UserInstructionsProvider for NoUserInstructions {
    fn load_user_instructions(&self) -> LoadUserInstructionsFuture<'_> {
        Box::pin(async { LoadedUserInstructions::default() })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownMode {
    Completed,
    Interrupt,
}

pub struct RunnerRuntime {
    thread_manager: ThreadManager,
    pub thread_id: ThreadId,
    pub thread: Arc<CodexThread>,
    pub session_configured: SessionConfiguredEvent,
}

impl RunnerRuntime {
    pub async fn start(
        config: Config,
        policy: Arc<GameCallPolicy>,
        dynamic_tools: Vec<DynamicToolSpec>,
    ) -> Result<Self, RunnerError> {
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
        let mut extensions = ExtensionRegistryBuilder::<Config>::new();
        extensions.mcp_tool_call_policy_contributor(policy);
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
            .start_thread(StartThreadOptions {
                dynamic_tools,
                ..StartThreadOptions::new(config)
            })
            .await
            .context("start game campaign thread")
            .map_err(thread_startup_error)?;
        Ok(Self {
            thread_manager,
            thread_id,
            thread,
            session_configured,
        })
    }

    pub async fn shutdown(self, mode: ShutdownMode) -> Vec<String> {
        let mut errors = Vec::new();
        if mode == ShutdownMode::Interrupt
            && let Err(error) = self.thread.submit(Op::Interrupt).await
        {
            errors.push(format!("interrupt failed: {error}"));
        }
        if let Err(error) = self.thread.shutdown_and_wait().await {
            errors.push(format!("shutdown failed: {error}"));
        }
        let _ = self.thread_manager.remove_thread(&self.thread_id).await;
        errors
    }
}

fn thread_startup_error(source: anyhow::Error) -> RunnerError {
    RunnerError::ThreadStartup { source }
}
