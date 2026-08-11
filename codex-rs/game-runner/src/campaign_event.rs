use std::time::Instant;

use codex_core_api::CodexThread;
use codex_core_api::McpToolCallEndEvent;
use codex_core_api::Op;
use codex_core_api::SessionConfiguredEvent;

use super::CampaignDirective;
use super::CampaignExecutionContext;
use super::CampaignExit;
use super::CampaignProgress;
use super::CampaignStart;
use super::CampaignTerminalState;
use super::ContinuationReason;
use crate::AcceptedPlan;
use crate::AuthorizedMutation;
use crate::CampaignEvent;
use crate::CampaignReport;
use crate::CampaignSummary;
use crate::DecisionGate;
use crate::GAME_SERVER_NAME;
use crate::GameCallPolicy;
use crate::MutationResult;
use crate::ObservationEvidence;
use crate::ReportedOutcome;
use crate::RunnerError;
use crate::campaign_progress::CampaignProgressError;
use crate::campaign_report::CampaignReportContext;
use crate::campaign_prompt::ResumePromptContext;
use crate::campaign_prompt::initial_prompt;
use crate::campaign_prompt::resume_prompt;

impl CampaignExecutionContext {
    pub(super) fn start(&self) -> &CampaignStart {
        match self {
            Self::Ephemeral { start } | Self::Durable { start, .. } => start,
        }
    }

    pub(super) async fn record_progress(
        &self,
        summary: &CampaignSummary,
        gate: &DecisionGate,
        policy: &GameCallPolicy,
    ) -> Result<(), RunnerError> {
        let Self::Durable {
            persistence,
            events,
            commands: _,
            failures: _,
            start: _,
        } = self
        else {
            return Ok(());
        };
        persist_summary(persistence, summary, gate, policy).await?;
        let _ = events.send(CampaignEvent::Progress(summary.clone()));
        Ok(())
    }

    pub(super) async fn record_plan(
        &self,
        summary: &CampaignSummary,
        plan: &AcceptedPlan,
        gate: &DecisionGate,
        policy: &GameCallPolicy,
    ) -> Result<(), RunnerError> {
        let Self::Durable {
            persistence,
            events,
            commands: _,
            failures: _,
            start: _,
        } = self
        else {
            return Ok(());
        };
        persist_summary(persistence, summary, gate, policy).await?;
        let _ = events.send(CampaignEvent::Plan(plan.clone()));
        Ok(())
    }

    pub(super) async fn record_mutation(
        &self,
        authorization: &AuthorizedMutation,
        policy: &GameCallPolicy,
    ) -> Result<(), RunnerError> {
        let Self::Durable {
            persistence,
            events,
            commands: _,
            failures: _,
            start: _,
        } = self
        else {
            return Ok(());
        };
        let checkpoint = persistence
            .snapshot()
            .await
            .map_err(|error| durability_error(policy, error))?;
        let matches_authorization = checkpoint.unresolved_mutation.as_ref().is_some_and(
            |mutation| {
                mutation.operation_id == authorization.operation_id
                    && mutation.action_sha256 == authorization.action_sha256
                    && mutation.tool == authorization.tool
                    && mutation.action_sequence == checkpoint.summary.total_actions
            },
        );
        if !matches_authorization {
            return Err(durability_error(
                policy,
                "persisted mutation authority does not match the dispatched call",
            ));
        }
        let _ = events.send(CampaignEvent::Mutation(authorization.clone()));
        Ok(())
    }

    pub(super) async fn record_mutation_finished(
        &self,
        call_id: &str,
        result: MutationResult,
        policy: &GameCallPolicy,
    ) -> Result<(), RunnerError> {
        let Self::Durable {
            persistence,
            events,
            commands: _,
            failures: _,
            start: _,
        } = self
        else {
            return Ok(());
        };
        persistence
            .finish_mutation(call_id, result)
            .await
            .map_err(|error| durability_error(policy, error))?;
        let _ = events.send(CampaignEvent::MutationFinished(result));
        Ok(())
    }

    pub(super) async fn record_observation(
        &self,
        observation: &ObservationEvidence,
        policy: &GameCallPolicy,
    ) -> Result<(), RunnerError> {
        let Self::Durable {
            persistence,
            events,
            commands: _,
            failures: _,
            start: _,
        } = self
        else {
            return Ok(());
        };
        persistence
            .confirm_observation(observation)
            .await
            .map_err(|error| durability_error(policy, error))?;
        let _ = events.send(CampaignEvent::Observation(observation.clone()));
        Ok(())
    }

    pub(super) async fn record_outcome(
        &self,
        summary: &CampaignSummary,
        outcome: &ReportedOutcome,
        directive: &CampaignDirective,
        gate: &DecisionGate,
        policy: &GameCallPolicy,
    ) -> Result<(), RunnerError> {
        let Self::Durable {
            persistence,
            events,
            commands: _,
            failures: _,
            start: _,
        } = self
        else {
            return Ok(());
        };
        match directive {
            CampaignDirective::InterruptThenContinue(ContinuationReason::NewAttempt) => {
                persist_summary(persistence, summary, gate, policy).await?;
                let _ = events.send(CampaignEvent::Outcome(outcome.clone()));
                let _ = events.send(CampaignEvent::Progress(summary.clone()));
                Ok(())
            }
            CampaignDirective::Complete(CampaignTerminalState::Won) => Ok(()),
            CampaignDirective::Block(_) => {
                persist_summary(persistence, summary, gate, policy).await?;
                let _ = events.send(CampaignEvent::Outcome(outcome.clone()));
                Ok(())
            }
            CampaignDirective::SubmitContinuation(_)
            | CampaignDirective::InterruptThenContinue(
                ContinuationReason::Ordinary | ContinuationReason::TurnTimeout,
            )
            | CampaignDirective::Complete(CampaignTerminalState::TerminalBlock) => Err(
                campaign_event_error("outcome produced an invalid campaign transition"),
            ),
        }
    }
}

async fn persist_summary(
    persistence: &crate::CampaignPersistence,
    summary: &CampaignSummary,
    gate: &DecisionGate,
    policy: &GameCallPolicy,
) -> Result<(), RunnerError> {
    persistence
        .persist_summary(summary.clone(), gate.snapshot().audit, policy.audit())
        .await
        .map_err(|error| durability_error(policy, error))
}

fn durability_error(policy: &GameCallPolicy, error: impl std::fmt::Display) -> RunnerError {
    policy.close_mutation_lane();
    campaign_event_error(error)
}

fn campaign_event_error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::CampaignFailed {
        message: error.to_string(),
    }
}

pub(super) fn initialize_campaign_start(
    start: &CampaignStart,
    limits: crate::CampaignLimits,
) -> Result<(CampaignProgress, String), RunnerError> {
    match start {
        CampaignStart::Fresh { target_app } => {
            Ok((CampaignProgress::new(limits), initial_prompt(target_app)))
        }
        CampaignStart::Resumed { checkpoint } => {
            let progress = CampaignProgress::restore(
                limits,
                checkpoint.summary.clone(),
                checkpoint.decision_audit,
            )
            .map_err(campaign_event_error)?;
            let prompt = resume_prompt(ResumePromptContext {
                attempt_number: checkpoint.summary.attempt_number,
                strategy: checkpoint.summary.strategy.as_ref(),
                unresolved_mutation: checkpoint.unresolved_mutation.as_ref(),
            })?;
            Ok((progress, prompt))
        }
    }
}

pub(super) fn block_exit(
    session: &SessionConfiguredEvent,
    progress: &CampaignProgress,
    policy: &GameCallPolicy,
    gate: &DecisionGate,
    reason: String,
) -> Result<CampaignExit, RunnerError> {
    block_report(session, progress, policy, gate, reason).map(CampaignExit::Blocked)
}

pub(super) fn build_exit(
    session: &SessionConfiguredEvent,
    progress: &CampaignProgress,
    policy: &GameCallPolicy,
    gate: &DecisionGate,
    terminal_state: CampaignTerminalState,
    terminal_failure: Option<String>,
) -> Result<CampaignExit, RunnerError> {
    let report = build_report(
        session,
        progress,
        policy,
        gate,
        terminal_state,
        terminal_failure,
    )?;
    match terminal_state {
        CampaignTerminalState::Won => Ok(CampaignExit::VerifiedWin(report)),
        CampaignTerminalState::TerminalBlock => Ok(CampaignExit::Blocked(report)),
    }
}

pub(super) async fn begin_safe_interrupt(
    thread: &CodexThread,
    progress: &mut CampaignProgress,
    reason: ContinuationReason,
) -> Result<(), RunnerError> {
    progress
        .begin_interrupt(reason, Instant::now())
        .map_err(campaign_progress_error)?;
    thread
        .submit(Op::Interrupt)
        .await
        .map(|_| ())
        .map_err(campaign_progress_error)
}

fn campaign_progress_error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::CampaignFailed {
        message: error.to_string(),
    }
}

pub(super) fn block_report(
    session: &SessionConfiguredEvent,
    progress: &CampaignProgress,
    policy: &GameCallPolicy,
    gate: &DecisionGate,
    reason: String,
) -> Result<CampaignReport, RunnerError> {
    build_report(
        session,
        progress,
        policy,
        gate,
        CampaignTerminalState::TerminalBlock,
        Some(reason),
    )
}

pub(super) fn build_report(
    session: &SessionConfiguredEvent,
    progress: &CampaignProgress,
    policy: &GameCallPolicy,
    gate: &DecisionGate,
    terminal_state: CampaignTerminalState,
    terminal_failure: Option<String>,
) -> Result<CampaignReport, RunnerError> {
    let rollout_path = session
        .rollout_path
        .clone()
        .ok_or(RunnerError::MissingRolloutPath)?;
    Ok(CampaignReport::from_snapshot(
        CampaignReportContext {
            terminal_state,
            thread_id: session.thread_id.to_string(),
            summary: progress.summary(),
            rollout_path,
            owner_lease: policy.lease(),
            policy_audit: policy.audit(),
            terminal_failure,
        },
        gate.snapshot(),
    ))
}

pub(super) fn observe_game_call_end(
    gate: &DecisionGate,
    event: &McpToolCallEndEvent,
) -> Result<(), RunnerError> {
    if event.invocation.server != GAME_SERVER_NAME {
        return Ok(());
    }
    match event.invocation.tool.as_str() {
        "get_app_state" => {
            let Some((reference, width, height)) = event
                .result
                .as_ref()
                .ok()
                .filter(|result| !result.is_error.unwrap_or(false))
                .and_then(|result| result.structured_content.as_ref())
                .and_then(full_frame_metadata)
            else {
                return Ok(());
            };
            gate.complete_full_observation(event.call_id.clone(), reference, width, height)
                .map_err(|error| RunnerError::CampaignFailed {
                    message: error.to_string(),
                })?;
        }
        "click" | "drag" | "focus_click" => {
            let is_authorized_call = gate
                .snapshot()
                .mutation
                .is_some_and(|mutation| mutation.authorization.call_id == event.call_id);
            if is_authorized_call {
                let result = match &event.result {
                    Ok(result) if result.is_error.unwrap_or(false) => MutationResult::CleanFailure,
                    Ok(_) => MutationResult::Success,
                    Err(_) => MutationResult::Indeterminate,
                };
                gate.record_mutation_result(&event.call_id, result)
                    .map_err(|error| RunnerError::CampaignFailed {
                        message: error.to_string(),
                    })?;
            }
        }
        "wait" | "zoom" => {}
        _ => {}
    }
    Ok(())
}

fn full_frame_metadata(content: &serde_json::Value) -> Option<(String, u32, u32)> {
    let reference = content.get("artifact_uri")?.as_str()?;
    if reference.len() > 2 * 1024 {
        return None;
    }
    let width = u32::try_from(content.get("width")?.as_u64()?).ok()?;
    let height = u32::try_from(content.get("height")?.as_u64()?).ok()?;
    (width > 0 && height > 0).then(|| (reference.to_string(), width, height))
}

pub(super) fn reduce_accepted_outcome(
    progress: &mut CampaignProgress,
    outcome: &ReportedOutcome,
) -> Result<CampaignDirective, CampaignProgressError> {
    progress.accept_outcome(outcome)
}

pub(super) fn reduce_turn_complete(
    progress: &mut CampaignProgress,
    snapshot: &crate::DecisionSnapshot,
) -> Result<CampaignDirective, CampaignProgressError> {
    match progress.complete_expected_interrupt() {
        Ok(directive) => Ok(directive),
        Err(CampaignProgressError::MissingPendingInterrupt) => progress.on_turn_complete(snapshot),
        Err(error) => Err(error),
    }
}

pub(super) fn reduce_turn_aborted(
    progress: &mut CampaignProgress,
) -> Result<CampaignDirective, CampaignProgressError> {
    match progress.complete_expected_interrupt() {
        Ok(directive) => Ok(directive),
        Err(CampaignProgressError::MissingPendingInterrupt) => Ok(CampaignDirective::Block(
            "campaign turn aborted unexpectedly".to_string(),
        )),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "campaign_event_tests.rs"]
mod tests;
