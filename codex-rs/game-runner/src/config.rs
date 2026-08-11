use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::ensure;
use codex_core_api::AppToolApproval;
use codex_core_api::AskForApproval;
use codex_core_api::Config;
use codex_core_api::ConfigBuilder;
use codex_core_api::ConfigOverrides;
use codex_core_api::Constrained;
use codex_core_api::Feature;
use codex_core_api::Features;
use codex_core_api::McpServerConfig;
use codex_core_api::PermissionProfile;
use codex_core_api::Permissions;
use codex_core_api::ReasoningEffort;
use codex_core_api::WebSearchMode;

pub const GAME_SERVER_NAME: &str = "game";
pub const MODEL: &str = "gpt-5.6-sol";
pub const GENERATION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeFocus {
    TargetApplication,
    PreserveCurrent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerDeployment {
    pub helper_app: PathBuf,
    pub socket_path: PathBuf,
    pub target_app: String,
    pub codex_home: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("codex-game-runner live execution requires macOS")]
    UnsupportedPlatform,
    #[error("helper app is not a readable app bundle: {path}", path = path.display())]
    InvalidHelperApp { path: PathBuf },
    #[error("LaunchServices could not start the signed helper")]
    LaunchServices {
        #[source]
        source: std::io::Error,
    },
    #[error("LaunchServices returned unsuccessful status {status}")]
    LaunchServicesExit { status: String },
    #[error("helper socket did not become ready: {path}", path = path.display())]
    SocketReadinessTimeout { path: PathBuf },
    #[error("failed to construct the fixed runner configuration")]
    Config {
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to start the observation thread")]
    ThreadStartup {
        #[source]
        source: anyhow::Error,
    },
    #[error("observation turn exceeded its deadline")]
    TurnTimeout,
    #[error("campaign execution failed: {message}")]
    CampaignFailed { message: String },
    #[error("observation turn failed: {message}")]
    TurnFailed { message: String },
    #[error("the turn completed without a successful game/get_app_state call")]
    NoSuccessfulObservation,
    #[error("the model attempted {count} mutating game calls")]
    MutationAttempted { count: u64 },
    #[error("the model attempted {count} unknown game tools")]
    UnknownGameToolAttempted { count: u64 },
    #[error("{count} mutating game calls reached MCP dispatch")]
    MutationDispatched { count: u64 },
    #[error("model observation report is invalid: {message}")]
    InvalidModelReport { message: String },
    #[error("non-ephemeral thread did not expose a rollout path")]
    MissingRolloutPath,
    #[error("campaign at {path} must be resumed explicitly", path = path.display())]
    CampaignRequiresResume { path: PathBuf },
    #[error("run failed and cleanup also failed: {cleanup}")]
    RunAndCleanupFailed {
        #[source]
        primary: Box<RunnerError>,
        cleanup: String,
    },
}

pub async fn load_runner_config(
    deployment: &RunnerDeployment,
    runner_executable: &Path,
) -> Result<Config, RunnerError> {
    load_runner_config_for_focus(
        deployment,
        runner_executable,
        BridgeFocus::TargetApplication,
    )
    .await
}

pub(crate) async fn load_runner_config_for_focus(
    deployment: &RunnerDeployment,
    runner_executable: &Path,
    bridge_focus: BridgeFocus,
) -> Result<Config, RunnerError> {
    load_runner_config_inner(deployment, runner_executable, bridge_focus)
        .await
        .map_err(|source| RunnerError::Config { source })
}

async fn load_runner_config_inner(
    deployment: &RunnerDeployment,
    runner_executable: &Path,
    bridge_focus: BridgeFocus,
) -> anyhow::Result<Config> {
    let mut config = ConfigBuilder::default()
        .codex_home(deployment.codex_home.clone())
        .harness_overrides(ConfigOverrides {
            cwd: Some(deployment.codex_home.clone()),
            codex_self_exe: Some(runner_executable.to_path_buf()),
            ..Default::default()
        })
        .build()
        .await
        .context("load Codex configuration")?;

    config.model = Some(MODEL.to_string());
    config.model_reasoning_effort = Some(ReasoningEffort::High);
    config.ephemeral = false;
    config.base_instructions = None;
    config.developer_instructions = None;
    config.compact_prompt = None;
    config.include_permissions_instructions = false;
    config.include_apps_instructions = false;
    config.include_collaboration_mode_instructions = false;
    config.include_skill_instructions = false;
    config.include_environment_context = false;
    config.orchestrator_skills_enabled = false;
    config.orchestrator_mcp_enabled = false;
    config.agents_enabled = false;
    config.agent_roles.clear();
    config.experimental_request_user_input_enabled = false;
    config.update_plan_enabled = false;
    config.project_doc_max_bytes = 0;
    config.project_doc_fallback_filenames.clear();
    config.web_search_config = None;
    config
        .web_search_mode
        .set(WebSearchMode::Disabled)
        .context("disable web search")?;
    ensure!(
        config.web_search_mode.value() == WebSearchMode::Disabled,
        "configuration requirements prevent disabling web search"
    );

    config.permissions = Permissions::from_approval_and_profile(
        Constrained::allow_any(AskForApproval::Never),
        Constrained::allow_any(PermissionProfile::read_only()),
    )
    .context("set read-only permissions")?;

    let mut features = Features::default();
    features.enable(Feature::CodeMode);
    features.enable(Feature::CodeModeHost);
    features.enable(Feature::CodeModeOnly);
    config
        .features
        .set(features.clone())
        .context("enable only code mode")?;
    ensure!(
        config.features.get() == &features,
        "configuration requirements enable unsupported runner features"
    );
    config.code_mode.excluded_tool_namespaces = vec!["functions".to_string()];
    config.code_mode.direct_only_tool_namespaces.clear();

    let mut bridge_args = vec![
        serde_json::json!("__stdio-to-uds"),
        serde_json::json!(deployment.socket_path),
    ];
    match bridge_focus {
        BridgeFocus::TargetApplication => {
            bridge_args.push(serde_json::json!(deployment.target_app));
        }
        BridgeFocus::PreserveCurrent => {}
    }
    let game_server = serde_json::from_value::<McpServerConfig>(serde_json::json!({
        "command": runner_executable,
        "args": bridge_args,
        "enabled": true,
        "required": true,
        "supports_parallel_tool_calls": false,
        "default_tools_approval_mode": "approve",
        "enabled_tools": ["get_app_state", "wait", "click", "drag", "focus_click"],
        "startup_timeout_sec": 15,
        "tool_timeout_sec": 30,
    }))
    .context("build game MCP server configuration")?;
    config
        .mcp_servers
        .set(HashMap::from([(GAME_SERVER_NAME.to_string(), game_server)]))
        .context("set game-only MCP server map")?;
    ensure!(
        config.mcp_servers.get().len() == 1
            && config.mcp_servers.get().contains_key(GAME_SERVER_NAME),
        "configuration requirements add unsupported MCP servers"
    );
    config.non_prefixed_mcp_tool_servers = None;

    ensure!(
        config
            .mcp_servers
            .get()
            .get(GAME_SERVER_NAME)
            .and_then(|server| server.default_tools_approval_mode)
            == Some(AppToolApproval::Approve),
        "game MCP server must be pre-approved"
    );

    Ok(config)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
