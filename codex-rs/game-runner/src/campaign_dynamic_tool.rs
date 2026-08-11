use codex_core_api::DynamicToolCallRequest;
use codex_core_api::DynamicToolResponse;

use super::CampaignDirective;
use super::CampaignExecutionContext;
use super::CampaignProgress;
use super::CampaignTerminalState;
use super::campaign_event::reduce_accepted_outcome;
use crate::CampaignTools;
use crate::DecisionGate;
use crate::GameCallPolicy;
use crate::RunnerError;

pub(super) struct PreparedDynamicToolResponse {
    pub(super) call_id: String,
    pub(super) response: DynamicToolResponse,
    pub(super) outcome_directive: Option<CampaignDirective>,
}

pub(super) async fn prepare_dynamic_tool_response(
    request: DynamicToolCallRequest,
    tools: &CampaignTools,
    context: &CampaignExecutionContext,
    progress: &mut CampaignProgress,
    policy: &GameCallPolicy,
    gate: &DecisionGate,
) -> Result<PreparedDynamicToolResponse, RunnerError> {
    let response = tools.handle(&request).map_err(dynamic_tool_error)?;
    let accepted_plan = response.success && request.tool == "record_plan";
    let accepted_outcome = response.success && request.tool == "report_outcome";
    if accepted_plan {
        let snapshot = gate.snapshot();
        let plan = snapshot
            .plan
            .as_ref()
            .ok_or_else(|| dynamic_tool_error("accepted plan response did not retain its plan"))?;
        context
            .record_plan(&progress.summary(), plan, gate, policy)
            .await?;
    }
    let outcome_directive = if accepted_outcome {
        let snapshot = gate.snapshot();
        let outcome = snapshot.outcome.ok_or_else(|| {
            dynamic_tool_error("accepted outcome response did not retain evidence")
        })?;
        let directive = reduce_accepted_outcome(progress, &outcome).map_err(dynamic_tool_error)?;
        context
            .record_outcome(&progress.summary(), &outcome, &directive, gate, policy)
            .await?;
        if matches!(
            directive,
            CampaignDirective::Complete(CampaignTerminalState::Won)
        ) {
            policy.close_mutation_lane();
        }
        Some(directive)
    } else {
        None
    };
    Ok(PreparedDynamicToolResponse {
        call_id: request.call_id,
        response,
        outcome_directive,
    })
}

fn dynamic_tool_error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::CampaignFailed {
        message: error.to_string(),
    }
}
