use std::sync::Arc;

use codex_core_api::EventMsg;
use codex_core_api::Op;
use codex_core_api::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::CampaignTools;
use crate::DecisionGate;
use crate::GameCallPolicy;

use super::RunnerRuntime;
use super::ShutdownMode;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_resume_keeps_thread_identity_and_dynamic_campaign_tools() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_remote!(Ok(()), "rollout resume is local to the test filesystem");

    let server = responses::start_mock_server().await;
    let response = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("response-1"),
                responses::ev_completed("response-1"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("response-2"),
                responses::ev_completed("response-2"),
            ]),
        ],
    )
    .await;
    let mut base = test_codex();
    let base = base.build_with_auto_env(&server).await?;
    base.codex.shutdown_and_wait().await?;
    let mut config = base.config.clone();
    config.ephemeral = false;
    let first_policy = Arc::new(GameCallPolicy::new(
        "11111111-1111-4111-8111-111111111111".to_string(),
        1,
        Arc::new(DecisionGate::new(1)),
    ));
    let runtime = RunnerRuntime::start(
        config.clone(),
        first_policy,
        CampaignTools::specs(),
    )
    .await?;
    let thread_id = runtime.thread_id;
    submit_text(&runtime, "materialize this campaign rollout").await?;
    let rollout_path = runtime
        .session_configured
        .rollout_path
        .clone()
        .expect("persistent runner rollout");
    runtime.thread.flush_rollout().await?;
    assert_eq!(runtime.shutdown(ShutdownMode::Completed).await, Vec::<String>::new());

    let resumed_policy = Arc::new(GameCallPolicy::new(
        "11111111-1111-4111-8111-111111111111".to_string(),
        2,
        Arc::new(DecisionGate::new(2)),
    ));
    let resumed = RunnerRuntime::resume(config, resumed_policy, rollout_path, thread_id).await?;
    assert_eq!(resumed.thread_id, thread_id);
    submit_text(&resumed, "continue the same campaign").await?;
    assert_eq!(resumed.shutdown(ShutdownMode::Completed).await, Vec::<String>::new());

    let requests = response.requests();
    assert_eq!(requests.len(), 2);
    let resumed_body = requests[1].body_json();
    let tools = resumed_body["tools"].as_array().expect("resumed tools");
    let campaign_namespace = tools
        .iter()
        .find(|tool| tool.get("name") == Some(&json!("game_runner")))
        .expect("restored game runner namespace");
    let campaign_tool_names = campaign_namespace["tools"]
        .as_array()
        .expect("campaign namespace tools")
        .iter()
        .map(|tool| tool["name"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        campaign_tool_names,
        vec![json!("record_plan"), json!("report_outcome")]
    );
    Ok(())
}

async fn submit_text(runtime: &RunnerRuntime, prompt: &str) -> anyhow::Result<()> {
    runtime
        .thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: prompt.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(&runtime.thread, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    Ok(())
}
