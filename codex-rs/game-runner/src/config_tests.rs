use codex_core_api::AppToolApproval;
use codex_core_api::Config;
use codex_core_api::Feature;
use codex_core_api::McpServerTransportConfig;
use codex_core_api::ReasoningEffort;
use codex_core_api::WebSearchMode;
use pretty_assertions::assert_eq;

use super::RunnerDeployment;
use super::load_runner_config;

#[derive(Debug, PartialEq, Eq)]
struct ConfigProjection {
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    ephemeral: bool,
    include_permissions_instructions: bool,
    include_apps_instructions: bool,
    include_collaboration_mode_instructions: bool,
    include_skill_instructions: bool,
    include_environment_context: bool,
    orchestrator_skills_enabled: bool,
    orchestrator_mcp_enabled: bool,
    agents_enabled: bool,
    request_user_input_enabled: bool,
    update_plan_enabled: bool,
    project_doc_max_bytes: usize,
    web_search_mode: WebSearchMode,
    code_mode_enabled: bool,
    code_mode_host_enabled: bool,
    code_mode_only_enabled: bool,
    excluded_code_mode_namespaces: Vec<String>,
    mcp_server_names: Vec<String>,
    game_tools: Option<Vec<String>>,
    game_required: bool,
    game_approval: Option<AppToolApproval>,
    game_command: String,
    game_args: Vec<String>,
}

#[tokio::test]
async fn runner_config_is_fixed_to_read_only_sol() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let deployment = RunnerDeployment {
        helper_app: temp.path().join("AutoPilotHelper.app"),
        socket_path: temp.path().join("game.sock"),
        target_app: "Gambonanza".to_string(),
        codex_home: temp.path().to_path_buf(),
    };
    let runner_executable = temp.path().join("codex-game-runner");

    let config = load_runner_config(&deployment, &runner_executable).await?;

    assert_eq!(
        project(&config),
        ConfigProjection {
            model: Some("gpt-5.6-sol".to_string()),
            reasoning_effort: Some(ReasoningEffort::High),
            ephemeral: false,
            include_permissions_instructions: false,
            include_apps_instructions: false,
            include_collaboration_mode_instructions: false,
            include_skill_instructions: false,
            include_environment_context: false,
            orchestrator_skills_enabled: false,
            orchestrator_mcp_enabled: false,
            agents_enabled: false,
            request_user_input_enabled: false,
            update_plan_enabled: false,
            project_doc_max_bytes: 0,
            web_search_mode: WebSearchMode::Disabled,
            code_mode_enabled: true,
            code_mode_host_enabled: true,
            code_mode_only_enabled: true,
            excluded_code_mode_namespaces: vec!["functions".to_string()],
            mcp_server_names: vec!["game".to_string()],
            game_tools: Some(vec![
                "get_app_state".to_string(),
                "wait".to_string(),
                "zoom".to_string(),
            ]),
            game_required: true,
            game_approval: Some(AppToolApproval::Approve),
            game_command: runner_executable.display().to_string(),
            game_args: vec![
                "__stdio-to-uds".to_string(),
                temp.path().join("game.sock").display().to_string(),
            ],
        }
    );
    Ok(())
}

fn project(config: &Config) -> ConfigProjection {
    let game = config
        .mcp_servers
        .get()
        .get("game")
        .expect("fixed runner config must contain the game MCP server");
    let (game_command, game_args) = match &game.transport {
        McpServerTransportConfig::Stdio { command, args, .. } => {
            (command.clone(), args.clone())
        }
        McpServerTransportConfig::StreamableHttp { .. } => {
            panic!("game MCP server must use the stdio bridge")
        }
    };
    let mut mcp_server_names = config.mcp_servers.get().keys().cloned().collect::<Vec<_>>();
    mcp_server_names.sort();

    ConfigProjection {
        model: config.model.clone(),
        reasoning_effort: config.model_reasoning_effort.clone(),
        ephemeral: config.ephemeral,
        include_permissions_instructions: config.include_permissions_instructions,
        include_apps_instructions: config.include_apps_instructions,
        include_collaboration_mode_instructions: config.include_collaboration_mode_instructions,
        include_skill_instructions: config.include_skill_instructions,
        include_environment_context: config.include_environment_context,
        orchestrator_skills_enabled: config.orchestrator_skills_enabled,
        orchestrator_mcp_enabled: config.orchestrator_mcp_enabled,
        agents_enabled: config.agents_enabled,
        request_user_input_enabled: config.experimental_request_user_input_enabled,
        update_plan_enabled: config.update_plan_enabled,
        project_doc_max_bytes: config.project_doc_max_bytes,
        web_search_mode: config.web_search_mode.value(),
        code_mode_enabled: config.features.get().enabled(Feature::CodeMode),
        code_mode_host_enabled: config.features.get().enabled(Feature::CodeModeHost),
        code_mode_only_enabled: config.features.get().enabled(Feature::CodeModeOnly),
        excluded_code_mode_namespaces: config.code_mode.excluded_tool_namespaces.clone(),
        mcp_server_names,
        game_tools: game.enabled_tools.clone(),
        game_required: game.required,
        game_approval: game.default_tools_approval_mode,
        game_command,
        game_args,
    }
}
