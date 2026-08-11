use std::collections::VecDeque;
use std::time::Instant;

use serde::Deserialize;
use serde::Serialize;

use crate::CampaignTerminalState;
use crate::DecisionAudit;
use crate::DecisionSnapshot;
use crate::OutcomeDraft;
use crate::ReportedOutcome;
use crate::StrategyRecord;

const MAX_RECENT_TURNS: usize = 64;
const MAX_TURN_ID_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CampaignLimits {
    pub turn_timeout: std::time::Duration,
    pub post_mutation_timeout: std::time::Duration,
    pub interrupt_timeout: std::time::Duration,
}

impl CampaignLimits {
    pub fn stage_4b1() -> Self {
        Self {
            turn_timeout: std::time::Duration::from_secs(15 * 60),
            post_mutation_timeout: std::time::Duration::from_secs(5 * 60),
            interrupt_timeout: std::time::Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CampaignSummary {
    pub attempt_number: u64,
    pub total_turns: u64,
    pub total_actions: u64,
    pub losses: u64,
    pub strategy: Option<StrategyRecord>,
    pub recent_turn_ids: Vec<String>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub(crate) enum CampaignProgressError {
    #[error("campaign counter {counter} overflowed")]
    CounterOverflow { counter: &'static str },
    #[error("mutation authorization audit regressed from {previous} to {actual}")]
    ActionAuditRegressed { previous: u64, actual: u64 },
    #[error("campaign outcome was applied more than once")]
    OutcomeAlreadyApplied,
    #[error("safe interruption is already pending")]
    InterruptAlreadyPending,
    #[error("no safe interruption is pending")]
    MissingPendingInterrupt,
    #[error("turn id exceeds the 2048-byte limit")]
    TurnIdTooLarge,
    #[error("restored campaign summary is inconsistent")]
    InvalidRestoredSummary,
    #[error("restored campaign strategy is invalid")]
    InvalidRestoredStrategy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuationReason {
    Ordinary,
    NewAttempt,
    TurnTimeout,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CampaignDirective {
    SubmitContinuation(ContinuationReason),
    InterruptThenContinue(ContinuationReason),
    Complete(CampaignTerminalState),
    Block(String),
}

pub(crate) struct CampaignProgress {
    limits: CampaignLimits,
    attempt_number: u64,
    total_turns: u64,
    total_actions: u64,
    losses: u64,
    strategy: Option<StrategyRecord>,
    recent_turn_ids: VecDeque<String>,
    last_action_audit: u64,
    outcome_applied: bool,
    turn_deadline: Instant,
    post_mutation_deadline: Option<Instant>,
    pending_interrupt: Option<(ContinuationReason, Instant)>,
}

impl CampaignProgress {
    pub(crate) fn new(limits: CampaignLimits) -> Self {
        Self {
            limits,
            attempt_number: 1,
            total_turns: 0,
            total_actions: 0,
            losses: 0,
            strategy: None,
            recent_turn_ids: VecDeque::with_capacity(MAX_RECENT_TURNS),
            last_action_audit: 0,
            outcome_applied: false,
            turn_deadline: Instant::now() + limits.turn_timeout,
            post_mutation_deadline: None,
            pending_interrupt: None,
        }
    }

    pub(crate) fn restore(
        limits: CampaignLimits,
        summary: CampaignSummary,
        decision_audit: DecisionAudit,
    ) -> Result<Self, CampaignProgressError> {
        if summary.attempt_number == 0
            || summary.losses.checked_add(1) != Some(summary.attempt_number)
            || summary.losses > summary.total_turns
            || summary.total_actions != decision_audit.mutation_authorizations
            || summary.recent_turn_ids.len() > MAX_RECENT_TURNS
            || summary.recent_turn_ids.len() as u64 > summary.total_turns
            || summary
                .recent_turn_ids
                .iter()
                .any(|turn_id| turn_id.len() > MAX_TURN_ID_BYTES)
        {
            return Err(CampaignProgressError::InvalidRestoredSummary);
        }
        if summary
            .strategy
            .as_ref()
            .is_some_and(|strategy| strategy.validate().is_err())
        {
            return Err(CampaignProgressError::InvalidRestoredStrategy);
        }
        Ok(Self {
            limits,
            attempt_number: summary.attempt_number,
            total_turns: summary.total_turns,
            total_actions: summary.total_actions,
            losses: summary.losses,
            strategy: summary.strategy,
            recent_turn_ids: summary.recent_turn_ids.into(),
            last_action_audit: decision_audit.mutation_authorizations,
            outcome_applied: false,
            turn_deadline: Instant::now() + limits.turn_timeout,
            post_mutation_deadline: None,
            pending_interrupt: None,
        })
    }

    pub(crate) fn on_turn_started(&mut self, turn_id: String) -> Result<(), CampaignProgressError> {
        if turn_id.len() > MAX_TURN_ID_BYTES {
            return Err(CampaignProgressError::TurnIdTooLarge);
        }
        let total_turns = checked_increment(self.total_turns, "total_turns")?;
        if self.recent_turn_ids.len() == MAX_RECENT_TURNS {
            self.recent_turn_ids.pop_front();
        }
        self.recent_turn_ids.push_back(turn_id);
        self.total_turns = total_turns;
        self.outcome_applied = false;
        self.turn_deadline = Instant::now() + self.limits.turn_timeout;
        Ok(())
    }

    pub(crate) fn accept_outcome(
        &mut self,
        outcome: &ReportedOutcome,
    ) -> Result<CampaignDirective, CampaignProgressError> {
        if self.outcome_applied {
            return Err(CampaignProgressError::OutcomeAlreadyApplied);
        }
        let directive = match &outcome.draft {
            OutcomeDraft::Loss { strategy, .. } => {
                let losses = checked_increment(self.losses, "losses")?;
                let attempt_number = checked_increment(self.attempt_number, "attempt_number")?;
                self.losses = losses;
                self.attempt_number = attempt_number;
                self.strategy = Some(strategy.clone());
                CampaignDirective::InterruptThenContinue(ContinuationReason::NewAttempt)
            }
            OutcomeDraft::Win { .. } => CampaignDirective::Complete(CampaignTerminalState::Won),
            OutcomeDraft::TerminalBlock {
                visible_evidence_summary,
                ..
            } => CampaignDirective::Block(visible_evidence_summary.clone()),
        };
        self.outcome_applied = true;
        Ok(directive)
    }

    pub(crate) fn on_turn_complete(
        &mut self,
        snapshot: &DecisionSnapshot,
    ) -> Result<CampaignDirective, CampaignProgressError> {
        self.observe_snapshot(snapshot, Instant::now())?;
        if let Some(outcome) = &snapshot.outcome {
            return self.accept_outcome(outcome);
        }
        self.turn_deadline = Instant::now() + self.limits.turn_timeout;
        Ok(CampaignDirective::SubmitContinuation(
            ContinuationReason::Ordinary,
        ))
    }

    pub(crate) fn observe_snapshot(
        &mut self,
        snapshot: &DecisionSnapshot,
        now: Instant,
    ) -> Result<(), CampaignProgressError> {
        let actual = snapshot.audit.mutation_authorizations;
        if actual < self.last_action_audit {
            return Err(CampaignProgressError::ActionAuditRegressed {
                previous: self.last_action_audit,
                actual,
            });
        }
        let additional_actions = actual - self.last_action_audit;
        self.total_actions = self.total_actions.checked_add(additional_actions).ok_or(
            CampaignProgressError::CounterOverflow {
                counter: "total_actions",
            },
        )?;
        self.last_action_audit = actual;

        if snapshot.requires_post_mutation_observation {
            self.post_mutation_deadline
                .get_or_insert(now + self.limits.post_mutation_timeout);
        } else {
            self.post_mutation_deadline = None;
        }
        Ok(())
    }

    pub(crate) fn begin_interrupt(
        &mut self,
        reason: ContinuationReason,
        now: Instant,
    ) -> Result<(), CampaignProgressError> {
        if self.pending_interrupt.is_some() {
            return Err(CampaignProgressError::InterruptAlreadyPending);
        }
        self.pending_interrupt = Some((reason, now + self.limits.interrupt_timeout));
        Ok(())
    }

    pub(crate) fn complete_expected_interrupt(
        &mut self,
    ) -> Result<CampaignDirective, CampaignProgressError> {
        let (reason, _) = self
            .pending_interrupt
            .take()
            .ok_or(CampaignProgressError::MissingPendingInterrupt)?;
        self.turn_deadline = Instant::now() + self.limits.turn_timeout;
        Ok(CampaignDirective::SubmitContinuation(reason))
    }

    pub(crate) fn deadline_directive(
        &self,
        snapshot: &DecisionSnapshot,
        now: Instant,
    ) -> Option<CampaignDirective> {
        if self
            .post_mutation_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            return Some(CampaignDirective::Block(
                "fresh post-mutation evidence did not arrive before its deadline".to_string(),
            ));
        }
        if self
            .pending_interrupt
            .is_some_and(|(_, deadline)| now >= deadline)
        {
            return Some(CampaignDirective::Block(
                "safe interruption did not complete before its deadline".to_string(),
            ));
        }
        if self.pending_interrupt.is_none() && now >= self.turn_deadline {
            return Some(if snapshot.requires_post_mutation_observation {
                CampaignDirective::Block(
                    "turn deadline elapsed with unresolved physical state".to_string(),
                )
            } else {
                CampaignDirective::InterruptThenContinue(ContinuationReason::TurnTimeout)
            });
        }
        None
    }

    pub(crate) fn next_deadline(&self) -> Instant {
        let mut deadline = self
            .pending_interrupt
            .map_or(self.turn_deadline, |(_, deadline)| deadline);
        if let Some(post_mutation) = self.post_mutation_deadline {
            deadline = deadline.min(post_mutation);
        }
        deadline
    }

    pub(crate) fn summary(&self) -> CampaignSummary {
        CampaignSummary {
            attempt_number: self.attempt_number,
            total_turns: self.total_turns,
            total_actions: self.total_actions,
            losses: self.losses,
            strategy: self.strategy.clone(),
            recent_turn_ids: self.recent_turn_ids.iter().cloned().collect(),
        }
    }
}

pub(crate) fn checked_increment(
    value: u64,
    counter: &'static str,
) -> Result<u64, CampaignProgressError> {
    value
        .checked_add(1)
        .ok_or(CampaignProgressError::CounterOverflow { counter })
}

#[cfg(test)]
#[path = "campaign_progress_tests.rs"]
mod tests;
