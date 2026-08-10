use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codex_core_api::CallToolResult;
use codex_core_api::EventMsg;
use codex_core_api::McpInvocation;
use codex_core_api::McpToolCallBeginEvent;
use codex_core_api::McpToolCallEndEvent;
use codex_core_api::McpToolCallPolicyContributor;
use codex_core_api::McpToolCallPolicyInput;
use codex_core_api::ModeKind;
use codex_core_api::TurnCompleteEvent;
use codex_core_api::TurnStartedEvent;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::ModelObservation;
use super::ObservationAccumulator;
use super::ObservationReport;
use crate::DecisionGate;
use crate::GameCallPolicy;
use crate::RunnerError;

fn policy() -> GameCallPolicy {
    GameCallPolicy::new(
        "test-epoch".to_string(),
        1,
        Arc::new(DecisionGate::new(1)),
    )
}

#[test]
fn newest_successful_observation_is_correlated_with_the_model_report() {
    let policy = policy();
    let mut accumulator = ObservationAccumulator::default();
    for event in [
        turn_started(),
        observation_end("observation-1", "obs-1", true),
        tool_end("game", "wait", "wait-1", true),
        observation_end("failed-observation", "ignored", false),
        tool_end("other", "get_app_state", "other-1", true),
        observation_end("observation-2", "obs-2", true),
    ] {
        accumulator.observe(&event);
    }

    let report = accumulator
        .finish(
            "thread-1",
            Some(PathBuf::from("/rollouts/thread-1.jsonl")),
            &policy,
            &turn_complete(valid_model_json()),
        )
        .expect("valid observation should produce a report");

    assert_eq!(
        report,
        ObservationReport {
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            observation_call_id: "observation-2".to_string(),
            observation_reference: Some("obs-2".to_string()),
            rollout_path: PathBuf::from("/rollouts/thread-1.jsonl"),
            epoch: "test-epoch".to_string(),
            generation: 1,
            mutation_attempts: 0,
            mutation_dispatches: 0,
            model: expected_model(),
        }
    );
}

#[test]
fn completion_without_successful_observation_is_rejected() {
    let policy = policy();
    let mut accumulator = ObservationAccumulator::default();
    accumulator.observe(&turn_started());
    accumulator.observe(&observation_end("failed", "ignored", false));

    let error = accumulator
        .finish(
            "thread-1",
            Some(PathBuf::from("/rollout.jsonl")),
            &policy,
            &turn_complete(valid_model_json()),
        )
        .expect_err("failed observations are not evidence");

    assert!(matches!(error, RunnerError::NoSuccessfulObservation));
}

#[test]
fn invalid_or_unbounded_model_reports_are_rejected() {
    let policy = policy();
    let oversized = "x".repeat(2_049);
    let too_many = vec!["object"; 33];
    for message in [
        "not-json".to_string(),
        json!({
            "visible_state_summary": "A board",
            "game_phase": "combat",
            "visible_objects": [],
            "resources_and_choices": [],
            "uncertainties": [],
            "extra": true,
        })
        .to_string(),
        json!({
            "visible_state_summary": oversized,
            "game_phase": "combat",
            "visible_objects": [],
            "resources_and_choices": [],
            "uncertainties": [],
        })
        .to_string(),
        json!({
            "visible_state_summary": "A board",
            "game_phase": "combat",
            "visible_objects": too_many,
            "resources_and_choices": [],
            "uncertainties": [],
        })
        .to_string(),
    ] {
        let mut accumulator = ObservationAccumulator::default();
        accumulator.observe(&turn_started());
        accumulator.observe(&observation_end("observation-1", "obs-1", true));
        assert!(matches!(
            accumulator.finish(
                "thread-1",
                Some(PathBuf::from("/rollout.jsonl")),
                &policy,
                &turn_complete(message),
            ),
            Err(RunnerError::InvalidModelReport { .. })
        ));
    }
}

#[tokio::test]
async fn pre_policy_mutation_begin_is_an_attempt_not_a_dispatch() {
    let policy = policy();
    let request_meta = serde_json::Map::new();
    let _decision = policy
        .evaluate(McpToolCallPolicyInput {
            server_name: "game",
            tool_name: "click",
            call_id: "mutation-1",
            arguments: None,
            request_meta: &request_meta,
        })
        .await;
    let mut accumulator = ObservationAccumulator::default();
    accumulator.observe(&turn_started());
    accumulator.observe(&EventMsg::McpToolCallBegin(McpToolCallBeginEvent {
        call_id: "mutation-1".to_string(),
        invocation: invocation("game", "click"),
        connector_id: None,
        mcp_app_resource_uri: None,
        link_id: None,
        app_name: None,
        action_name: None,
        plugin_id: None,
        read_only_hint: Some(false),
    }));
    accumulator.observe(&observation_end("observation-1", "obs-1", true));

    let error = accumulator
        .finish(
            "thread-1",
            Some(PathBuf::from("/rollout.jsonl")),
            &policy,
            &turn_complete(valid_model_json()),
        )
        .expect_err("mutation attempt must fail the observation run");

    assert!(matches!(error, RunnerError::MutationAttempted { count: 1 }));
}

fn turn_started() -> EventMsg {
    EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: "turn-1".to_string(),
        trace_id: None,
        started_at: None,
        model_context_window: None,
        collaboration_mode_kind: ModeKind::Default,
    })
}

fn observation_end(call_id: &str, observation_id: &str, success: bool) -> EventMsg {
    let mut event = tool_end("game", "get_app_state", call_id, success);
    if let EventMsg::McpToolCallEnd(event) = &mut event
        && let Ok(result) = &mut event.result
    {
        result.structured_content = Some(json!({ "observation_id": observation_id }));
    }
    event
}

fn tool_end(server: &str, tool: &str, call_id: &str, success: bool) -> EventMsg {
    EventMsg::McpToolCallEnd(McpToolCallEndEvent {
        call_id: call_id.to_string(),
        invocation: invocation(server, tool),
        connector_id: None,
        mcp_app_resource_uri: None,
        link_id: None,
        app_name: None,
        action_name: None,
        plugin_id: None,
        read_only_hint: Some(true),
        duration: Duration::from_millis(5),
        result: if success {
            Ok(CallToolResult {
                content: Vec::new(),
                structured_content: None,
                is_error: Some(false),
                meta: None,
            })
        } else {
            Err("capture failed".to_string())
        },
    })
}

fn invocation(server: &str, tool: &str) -> McpInvocation {
    McpInvocation {
        server: server.to_string(),
        tool: tool.to_string(),
        arguments: Some(json!({})),
    }
}

fn turn_complete(last_agent_message: String) -> TurnCompleteEvent {
    TurnCompleteEvent {
        turn_id: "turn-1".to_string(),
        last_agent_message: Some(last_agent_message),
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
        time_to_first_token_ms: None,
    }
}

fn valid_model_json() -> String {
    serde_json::to_string(&expected_model()).expect("serialize model fixture")
}

fn expected_model() -> ModelObservation {
    ModelObservation {
        visible_state_summary: "A combat board with a boss".to_string(),
        game_phase: "combat".to_string(),
        visible_objects: vec!["boss".to_string(), "player".to_string()],
        resources_and_choices: vec!["three energy".to_string()],
        uncertainties: vec!["boss intent icon is small".to_string()],
    }
}
