use std::sync::Arc;

use codex_core_api::DynamicToolCallOutputContentItem;
use codex_core_api::DynamicToolCallRequest;
use codex_core_api::DynamicToolNamespaceTool;
use codex_core_api::DynamicToolResponse;
use codex_core_api::DynamicToolSpec;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::ClickArguments;
use crate::DecisionGate;
use crate::MutationResult;
use crate::PlanCandidate;
use crate::PlanDraft;
use crate::PlannedAction;

use super::CampaignToolError;
use super::CampaignTools;

fn observed_gate() -> DecisionGate {
    let gate = DecisionGate::new(1);
    gate.begin_full_observation();
    gate.complete_full_observation(
        "capture-before".to_string(),
        "sha256:before".to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )
    .expect("valid observation fixture");
    gate
}

fn plan_draft(reference: &str) -> PlanDraft {
    PlanDraft {
        observation_reference: reference.to_string(),
        objective: "Open one safe non-gameplay menu".to_string(),
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
            x: 180,
            y: 640,
            button: None,
            count: None,
        }),
        reason: "Settings is reversible".to_string(),
        expected_visible_result: "A settings screen".to_string(),
        invalidation_condition: "The main menu changes".to_string(),
    }
}

fn request(tool: &str, arguments: serde_json::Value) -> DynamicToolCallRequest {
    DynamicToolCallRequest {
        call_id: format!("dynamic-{tool}-1"),
        turn_id: "turn-1".to_string(),
        started_at_ms: 0,
        namespace: Some("game_runner".to_string()),
        tool: tool.to_string(),
        arguments,
    }
}

#[test]
fn record_plan_returns_runner_owned_identity_and_hash() -> anyhow::Result<()> {
    let gate = Arc::new(observed_gate());
    let tools = CampaignTools::new(Arc::clone(&gate));

    assert_eq!(
        tools.handle(&request(
            "record_plan",
            serde_json::to_value(plan_draft("sha256:before"))?
        ))?,
        DynamicToolResponse {
            content_items: vec![DynamicToolCallOutputContentItem::InputText {
                text: concat!(
                    "{\"action_sha256\":\"",
                    "f709b60a7bbf91028aa10498db469d2a47fd96669d5167f6163200718809b3e1",
                    "\",\"observation_reference\":\"sha256:before\",",
                    "\"plan_id\":\"plan-1-1\"}"
                )
                .to_string(),
            }],
            success: true,
        }
    );
    assert_eq!(gate.snapshot().audit.plans_accepted, 1);
    Ok(())
}

#[test]
fn unexpected_namespace_or_tool_is_a_runner_error() {
    let tools = CampaignTools::new(Arc::new(observed_gate()));
    let mut wrong_namespace = request("record_plan", json!({}));
    wrong_namespace.namespace = Some("other".to_string());

    assert!(matches!(
        tools.handle(&wrong_namespace),
        Err(CampaignToolError::UnexpectedTool { .. })
    ));
    assert!(matches!(
        tools.handle(&request("other", json!({}))),
        Err(CampaignToolError::UnexpectedTool { .. })
    ));
}

#[test]
fn malformed_and_stale_plans_are_recoverable_rejections() -> anyhow::Result<()> {
    let gate = Arc::new(observed_gate());
    let tools = CampaignTools::new(Arc::clone(&gate));
    let mut unknown = serde_json::to_value(plan_draft("sha256:before"))?;
    unknown["extra"] = json!(true);
    let mut stale = plan_draft("sha256:stale");
    let one_candidate = stale.candidates.pop().expect("second candidate");
    let five_candidates = vec![one_candidate; 5];

    for arguments in [
        unknown,
        serde_json::to_value(&stale)?,
        serde_json::to_value(PlanDraft {
            candidates: five_candidates,
            ..plan_draft("sha256:before")
        })?,
    ] {
        let response = tools.handle(&request("record_plan", arguments))?;
        assert!(!response.success);
        assert_eq!(response.content_items.len(), 1);
    }
    assert_eq!(gate.snapshot().plan, None);
    Ok(())
}

#[test]
fn oversized_plan_is_rejected_without_changing_authority() -> anyhow::Result<()> {
    let gate = Arc::new(observed_gate());
    let before = gate.snapshot();
    let tools = CampaignTools::new(Arc::clone(&gate));
    let large = "x".repeat(1900);
    let mut draft = plan_draft("sha256:before");
    draft.objective = large.clone();
    draft.visible_state_summary = large.clone();
    draft.reason = large.clone();
    draft.expected_visible_result = large.clone();
    draft.invalidation_condition = large.clone();
    for candidate in &mut draft.candidates {
        candidate.action = large.clone();
        candidate.predicted_visible_consequence = large.clone();
    }

    let response = tools.handle(&request("record_plan", serde_json::to_value(draft)?))?;

    assert!(!response.success);
    assert_eq!(gate.snapshot().observation, before.observation);
    assert_eq!(gate.snapshot().plan, None);
    Ok(())
}

#[test]
fn outcome_requires_after_evidence_and_accepts_a_visible_win() -> anyhow::Result<()> {
    let gate = Arc::new(observed_gate());
    let tools = CampaignTools::new(Arc::clone(&gate));
    let arguments = json!({
        "outcome": "win",
        "observation_reference": "sha256:before",
        "visible_evidence_summary": "The victory screen is visible",
        "lesson": "The selected action won"
    });
    assert!(
        !tools
            .handle(&request("report_outcome", arguments.clone()))?
            .success
    );

    gate.record_plan(plan_draft("sha256:before"))?;
    gate.prepare_mutation("click", &json!({"x": 180, "y": 640}), "mutation-1")?;
    gate.record_mutation_result("mutation-1", MutationResult::Success)?;
    gate.begin_full_observation();
    gate.complete_full_observation(
        "capture-after".to_string(),
        "sha256:after".to_string(),
        /*width*/ 1051,
        /*height*/ 820,
    )?;
    let mut after = arguments;
    after["observation_reference"] = json!("sha256:after");

    assert!(tools.handle(&request("report_outcome", after))?.success);
    assert!(gate.snapshot().outcome.is_some());
    Ok(())
}

#[test]
fn oversized_outcome_is_a_recoverable_rejection() -> anyhow::Result<()> {
    let tools = CampaignTools::new(Arc::new(observed_gate()));
    let response = tools.handle(&request(
        "report_outcome",
        json!({
            "outcome": "terminal_block",
            "observation_reference": "sha256:before",
            "visible_evidence_summary": "x".repeat(2049),
            "lesson": "bounded"
        }),
    ))?;
    assert!(!response.success);
    Ok(())
}

#[test]
fn specs_expose_only_two_strict_direct_tools() {
    let specs = CampaignTools::specs();
    let [DynamicToolSpec::Namespace(namespace)] = specs.as_slice() else {
        panic!("campaign tools must expose exactly one namespace");
    };
    assert_eq!(namespace.name, "game_runner");
    assert_eq!(namespace.tools.len(), 2);
    for tool in &namespace.tools {
        let DynamicToolNamespaceTool::Function(function) = tool;
        assert!(!function.defer_loading);
    }
    let DynamicToolNamespaceTool::Function(record_plan) = &namespace.tools[0];
    assert_eq!(record_plan.input_schema["additionalProperties"], false);
    let DynamicToolNamespaceTool::Function(report_outcome) = &namespace.tools[1];
    let branches = report_outcome.input_schema["oneOf"]
        .as_array()
        .expect("outcomes use exhaustive schema branches");
    assert_eq!(
        branches
            .iter()
            .map(|branch| branch["properties"]["outcome"]["const"].clone())
            .collect::<Vec<_>>(),
        vec![json!("loss"), json!("win"), json!("terminal_block")]
    );
    assert!(branches.iter().all(|branch| {
        branch["additionalProperties"] == false
            && branch["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|field| field == "outcome"))
    }));
    assert!(
        branches[0]["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|field| field == "strategy"))
    );
    assert!(branches[1..].iter().all(|branch| {
        branch["required"]
            .as_array()
            .is_some_and(|required| required.iter().all(|field| field != "strategy"))
    }));
}
