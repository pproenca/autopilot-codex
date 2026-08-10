use std::time::Duration;
use std::time::Instant;

use serde::Serialize;

use crate::DecisionSnapshot;
use crate::OutcomeKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CampaignLimits {
    pub max_turns: usize,
    pub total_timeout: Duration,
    pub post_mutation_timeout: Duration,
}

impl CampaignLimits {
    pub fn stage_4a() -> Self {
        Self {
            max_turns: 6,
            total_timeout: Duration::from_secs(15 * 60),
            post_mutation_timeout: Duration::from_secs(5 * 60),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CampaignTerminalState {
    CanaryComplete,
    Won,
    LossObserved,
    TerminalBlock,
}

impl CampaignTerminalState {
    pub fn is_success(self) -> bool {
        match self {
            Self::CanaryComplete | Self::Won => true,
            Self::LossObserved | Self::TerminalBlock => false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CampaignDirective {
    Continue,
    Complete(CampaignTerminalState),
    Block(String),
}

struct CampaignProgress {
    limits: CampaignLimits,
    started_at: Instant,
    turn_ids: Vec<String>,
    post_mutation_deadline: Option<Instant>,
}

impl CampaignProgress {
    fn new(limits: CampaignLimits) -> Self {
        Self {
            limits,
            started_at: Instant::now(),
            turn_ids: Vec::new(),
            post_mutation_deadline: None,
        }
    }

    fn on_turn_started(&mut self, turn_id: String) {
        self.turn_ids.push(turn_id);
    }

    fn on_turn_complete(&mut self, snapshot: &DecisionSnapshot) -> CampaignDirective {
        if let Some(outcome) = &snapshot.outcome {
            return CampaignDirective::Complete(match outcome.draft.outcome {
                OutcomeKind::Win => CampaignTerminalState::Won,
                OutcomeKind::Loss => CampaignTerminalState::LossObserved,
                OutcomeKind::TerminalBlock => CampaignTerminalState::TerminalBlock,
            });
        }
        if let (Some(mutation), Some(observation)) = (&snapshot.mutation, &snapshot.observation)
            && !snapshot.requires_post_mutation_observation
            && observation.generation > mutation.plan.observation.generation
        {
            return CampaignDirective::Complete(CampaignTerminalState::CanaryComplete);
        }
        self.observe_snapshot(snapshot);
        if self.turn_ids.len() >= self.limits.max_turns {
            return CampaignDirective::Block(format!(
                "campaign reached its {}-turn limit",
                self.limits.max_turns
            ));
        }
        CampaignDirective::Continue
    }

    fn observe_snapshot(&mut self, snapshot: &DecisionSnapshot) {
        if snapshot.mutation.is_some()
            && snapshot.requires_post_mutation_observation
            && self.post_mutation_deadline.is_none()
        {
            self.post_mutation_deadline = Some(Instant::now() + self.limits.post_mutation_timeout);
        }
    }

    fn deadline_directive(
        &self,
        snapshot: &DecisionSnapshot,
        now: Instant,
    ) -> Option<CampaignDirective> {
        if now >= self.started_at + self.limits.total_timeout {
            return Some(CampaignDirective::Block(
                "campaign exceeded its total deadline".to_string(),
            ));
        }
        if snapshot.requires_post_mutation_observation
            && self
                .post_mutation_deadline
                .is_some_and(|deadline| now >= deadline)
        {
            return Some(CampaignDirective::Block(
                "fresh post-mutation evidence did not arrive before its deadline".to_string(),
            ));
        }
        None
    }

    fn turn_ids(&self) -> &[String] {
        &self.turn_ids
    }

    fn next_deadline(&self) -> Instant {
        let total = self.started_at + self.limits.total_timeout;
        self.post_mutation_deadline
            .map_or(total, |post_mutation| total.min(post_mutation))
    }
}

pub struct CampaignRun {
    limits: CampaignLimits,
}

impl CampaignRun {
    pub fn new(limits: CampaignLimits) -> Self {
        Self { limits }
    }
}

#[path = "campaign_loop.rs"]
mod campaign_loop;

#[cfg(test)]
#[path = "campaign_tests.rs"]
mod tests;
