use std::sync::Arc;

use codex_core_api::McpToolCallPolicyContributor;
use codex_core_api::McpToolCallPolicyDecision;
use codex_core_api::McpToolCallPolicyInput;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

use crate::CampaignPersistence;
use crate::ClickArguments;
use crate::DecisionGate;
use crate::MAX_ACTIONS_PER_TURN;
use crate::MutationResult;
use crate::OwnerLeaseState;
use crate::PlanCandidate;
use crate::PlanDraft;
use crate::PlannedAction;

use super::GameCallPolicy;
use super::PolicyAudit;

use crate::campaign_persistence::tests::checkpoint;
use crate::campaign_persistence::tests::store;

fn install_click_plan(gate: &DecisionGate, reference: &str, x: i64) -> anyhow::Result<()> {
    gate.begin_full_observation();
    gate.complete_full_observation(
        format!("capture-{reference}"),
        reference.to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;
    gate.record_plan(PlanDraft {
        observation_reference: reference.to_string(),
        objective: "Open a safe menu".to_string(),
        visible_state_summary: "The main menu is visible".to_string(),
        candidates: vec![
            PlanCandidate {
                action: "Open Settings".to_string(),
                predicted_visible_consequence: "Settings appears".to_string(),
            },
            PlanCandidate {
                action: "Open Credits".to_string(),
                predicted_visible_consequence: "Credits appears".to_string(),
            },
        ],
        chosen_action: PlannedAction::Click(ClickArguments {
            x,
            y: 640,
            button: None,
            count: None,
        }),
        reason: "The action is reversible".to_string(),
        expected_visible_result: "A settings screen".to_string(),
        invalidation_condition: "The menu changes".to_string(),
    })?;
    Ok(())
}

async fn evaluate(
    policy: &GameCallPolicy,
    tool_name: &str,
    call_id: &str,
    arguments: Option<&Value>,
) -> McpToolCallPolicyDecision {
    let request_meta = serde_json::Map::new();
    policy
        .evaluate(McpToolCallPolicyInput {
            server_name: "game",
            tool_name,
            call_id,
            arguments,
            request_meta: &request_meta,
        })
        .await
}

#[tokio::test]
async fn exact_planned_mutation_receives_owner_and_operation_metadata() -> anyhow::Result<()> {
    let gate = Arc::new(DecisionGate::new(1));
    install_click_plan(&gate, "sha256:before", 180)?;
    let policy = GameCallPolicy::new("epoch-1".to_string(), 1, Arc::clone(&gate));
    let arguments = json!({"x": 180, "y": 640});

    assert_eq!(
        evaluate(&policy, "click", "mutation-1", Some(&arguments)).await,
        McpToolCallPolicyDecision::Allow {
            additional_request_meta: json!({
                "action_sha256": "f709b60a7bbf91028aa10498db469d2a47fd96669d5167f6163200718809b3e1",
                "call_id": "mutation-1",
                "epoch": "epoch-1",
                "generation": 1,
                "operation_id": "mutation-1",
            })
            .as_object()
            .expect("metadata fixture must be an object")
            .clone(),
        }
    );
    assert_eq!(
        policy.audit(),
        PolicyAudit {
            mutation_attempts: 1,
            unknown_tool_attempts: 0,
            mutation_authorizations: 1,
        }
    );
    Ok(())
}

#[tokio::test]
async fn durable_policy_persists_pending_before_allowing_dispatch() -> anyhow::Result<()> {
    let (_codex_home, store, _guard) = store()?;
    let persistence = Arc::new(CampaignPersistence::empty(store));
    let checkpoint = checkpoint();
    persistence.install(checkpoint.clone()).await?;
    let gate = Arc::new(DecisionGate::new(1));
    install_click_plan(&gate, "sha256:before", 180)?;
    let lease = Arc::new(OwnerLeaseState::new(checkpoint.epoch.clone(), 1));
    let policy = GameCallPolicy::durable(lease, Arc::clone(&gate), Arc::clone(&persistence));
    let arguments = json!({"x": 180, "y": 640});

    assert!(matches!(
        evaluate(&policy, "click", "mutation-1", Some(&arguments)).await,
        McpToolCallPolicyDecision::Allow { .. }
    ));
    let persisted = persistence.snapshot().await?;
    assert_eq!(persisted.summary.total_actions, 1);
    assert_eq!(
        persisted
            .unresolved_mutation
            .expect("pending mutation")
            .result,
        crate::DurableMutationResult::Pending
    );
    assert_eq!(policy.mutation_lane_is_open(), true);
    Ok(())
}

#[tokio::test]
async fn checkpoint_failure_denies_dispatch_and_closes_the_mutation_lane() -> anyhow::Result<()> {
    let (_codex_home, store, _guard) = store()?;
    let persistence = Arc::new(CampaignPersistence::empty(Arc::clone(&store)));
    let checkpoint = checkpoint();
    persistence.install(checkpoint.clone()).await?;
    std::fs::remove_file(store.path())?;
    std::fs::create_dir(store.path())?;
    let gate = Arc::new(DecisionGate::new(1));
    install_click_plan(&gate, "sha256:before", 180)?;
    let lease = Arc::new(OwnerLeaseState::new(checkpoint.epoch, 1));
    let policy = GameCallPolicy::durable(lease, gate, persistence.clone());
    let arguments = json!({"x": 180, "y": 640});

    assert_eq!(
        evaluate(&policy, "click", "mutation-1", Some(&arguments)).await,
        McpToolCallPolicyDecision::Deny {
            reason: "campaign checkpoint write failed before mutation dispatch".to_string(),
        }
    );
    assert_eq!(persistence.snapshot().await?.unresolved_mutation, None);
    assert_eq!(policy.mutation_lane_is_open(), false);
    Ok(())
}

#[tokio::test]
async fn mismatched_and_argumentless_mutations_consume_the_plan() -> anyhow::Result<()> {
    for arguments in [Some(json!({"x": 181, "y": 640})), None] {
        let gate = Arc::new(DecisionGate::new(1));
        install_click_plan(&gate, "sha256:before", 180)?;
        let policy = GameCallPolicy::new("epoch-1".to_string(), 1, Arc::clone(&gate));
        let decision = evaluate(&policy, "click", "mutation-1", arguments.as_ref()).await;

        assert!(matches!(decision, McpToolCallPolicyDecision::Deny { .. }));
        assert_eq!(gate.snapshot().plan, None);
        assert_eq!(gate.snapshot().audit.mutation_attempts, 1);
    }
    Ok(())
}

#[tokio::test]
async fn ninth_planned_mutation_is_denied_without_helper_metadata() -> anyhow::Result<()> {
    let gate = Arc::new(DecisionGate::new(1));
    let policy = GameCallPolicy::new("epoch-1".to_string(), 1, Arc::clone(&gate));
    let arguments = json!({"x": 180, "y": 640});
    for action_number in 1..=MAX_ACTIONS_PER_TURN {
        install_click_plan(
            &gate,
            &format!("sha256:before-{action_number}"),
            /*x*/ 180,
        )?;
        let call_id = format!("mutation-{action_number}");
        assert!(matches!(
            evaluate(&policy, "click", &call_id, Some(&arguments)).await,
            McpToolCallPolicyDecision::Allow { .. }
        ));
        gate.record_mutation_result(&call_id, MutationResult::Success)?;
    }
    install_click_plan(&gate, "sha256:after-8", /*x*/ 180)?;

    assert_eq!(
        evaluate(&policy, "click", "mutation-9", Some(&arguments)).await,
        McpToolCallPolicyDecision::Deny {
            reason: "the eight-action turn batch is exhausted; verify the latest action and finish this turn"
                .to_string(),
        }
    );
    let snapshot = gate.snapshot();
    assert_eq!(snapshot.batch_actions, MAX_ACTIONS_PER_TURN);
    assert_eq!(
        snapshot
            .mutation
            .as_ref()
            .map(|mutation| mutation.authorization.operation_id.as_str()),
        Some("mutation-8")
    );
    Ok(())
}

#[tokio::test]
async fn capture_and_positive_wait_invalidate_before_dispatch() -> anyhow::Result<()> {
    let gate = Arc::new(DecisionGate::new(1));
    install_click_plan(&gate, "sha256:before", 180)?;
    let policy = GameCallPolicy::new("epoch-1".to_string(), 1, Arc::clone(&gate));

    assert!(matches!(
        evaluate(&policy, "wait", "wait-0", Some(&json!({"seconds": 0}))).await,
        McpToolCallPolicyDecision::Allow { .. }
    ));
    assert!(gate.snapshot().plan.is_some());
    evaluate(&policy, "wait", "wait-1", Some(&json!({"seconds": 1}))).await;
    assert_eq!(
        (gate.snapshot().observation, gate.snapshot().plan),
        (None, None)
    );

    install_click_plan(&gate, "sha256:again", 180)?;
    evaluate(&policy, "get_app_state", "capture-2", Some(&json!({}))).await;
    assert_eq!(
        (gate.snapshot().observation, gate.snapshot().plan),
        (None, None)
    );
    Ok(())
}

#[tokio::test]
async fn zoom_and_unknown_calls_are_denied_and_audited() {
    let gate = Arc::new(DecisionGate::new(1));
    let policy = GameCallPolicy::new("epoch-1".to_string(), 1, gate);
    for tool_name in ["zoom", "unexpected_tool"] {
        assert!(matches!(
            evaluate(&policy, tool_name, "unknown-1", None).await,
            McpToolCallPolicyDecision::Deny { .. }
        ));
    }
    assert_eq!(
        policy.audit(),
        PolicyAudit {
            mutation_attempts: 0,
            unknown_tool_attempts: 2,
            mutation_authorizations: 0,
        }
    );
}

#[tokio::test]
async fn non_game_server_is_not_changed() {
    let gate = Arc::new(DecisionGate::new(1));
    let policy = GameCallPolicy::new("epoch-1".to_string(), 1, gate);
    let request_meta = serde_json::Map::new();

    assert_eq!(
        policy
            .evaluate(McpToolCallPolicyInput {
                server_name: "other",
                tool_name: "click",
                call_id: "call-other",
                arguments: None,
                request_meta: &request_meta,
            })
            .await,
        McpToolCallPolicyDecision::Allow {
            additional_request_meta: serde_json::Map::new(),
        }
    );
}
