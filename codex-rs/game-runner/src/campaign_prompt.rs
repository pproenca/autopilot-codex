pub(crate) fn initial_prompt(target_app: &str) -> String {
    format!(
        "Play the currently visible {target_app} game until a full-game victory is visibly verified. Use only fresh full-frame pixels from mcp__game__get_app_state. Before every click, drag, or focus-click, call game_runner.record_plan with two to four candidates and one exact action. After every dispatched mutation, capture fresh pixels before planning again. A turn permits at most eight mutations; finish the turn when the batch is exhausted and the runner will continue. Call game_runner.report_outcome only for a visible loss, full-game win, or terminal infrastructure block. A loss must include a complete replacement strategy and ends only the attempt. Use visible restart controls on the next attempt. Difficulty, an unchanged screen, or a failed strategy is not a stopping condition. Never retry an indeterminate mutation without observing first."
    )
}

pub(crate) fn continuation_prompt(attempt_number: u64) -> String {
    format!(
        "Continue attempt {attempt_number} of the same campaign. Capture fresh full-frame pixels before planning. Keep using one recorded plan per mutation and fresh pixels after every mutation. Report only a visible loss, full-game win, or terminal infrastructure block."
    )
}

pub(crate) fn new_attempt_prompt(attempt_number: u64) -> String {
    format!(
        "Start attempt {attempt_number} of the same campaign. Use the visible restart controls, guided by the replacement strategy you just recorded. Capture fresh full-frame pixels before planning, and keep one recorded plan plus one fresh post-mutation observation for every action."
    )
}

pub(crate) struct ResumePromptContext<'a> {
    pub attempt_number: u64,
    pub strategy: Option<&'a StrategyRecord>,
    pub unresolved_mutation: Option<&'a DurableMutation>,
}

pub(crate) fn resume_prompt(context: ResumePromptContext<'_>) -> Result<String, RunnerError> {
    let strategy = match context.strategy {
        Some(strategy) => {
            strategy
                .validate()
                .map_err(|error| RunnerError::CampaignFailed {
                    message: format!("invalid retained strategy: {error}"),
                })?;
            serde_json::to_string(strategy).map_err(|error| RunnerError::CampaignFailed {
                message: format!("failed to encode retained strategy: {error}"),
            })?
        }
        None => "none".to_string(),
    };
    let unresolved_mutation = match context.unresolved_mutation {
        Some(mutation) => {
            let result = match mutation.result {
                DurableMutationResult::Pending => "pending",
                DurableMutationResult::Success => "success",
                DurableMutationResult::CleanFailure => "cleanFailure",
                DurableMutationResult::Indeterminate => "indeterminate",
            };
            format!(
                "operation_id={}, action_sha256={}, tool={}, result={result}",
                mutation.operation_id, mutation.action_sha256, mutation.tool
            )
        }
        None => "none".to_string(),
    };
    Ok(format!(
        "Resume attempt {} of the same campaign. Retained replacement strategy: {strategy}. Unresolved mutation recovery context: {unresolved_mutation}. Capture fresh full-frame pixels before planning or mutating. Never retry the unresolved operation; use the new pixels to decide what visibly happened. Continue until a full-game victory is visibly verified.",
        context.attempt_number
    ))
}

#[cfg(test)]
#[path = "campaign_prompt_tests.rs"]
mod tests;
use crate::DurableMutation;
use crate::DurableMutationResult;
use crate::RunnerError;
use crate::StrategyRecord;
