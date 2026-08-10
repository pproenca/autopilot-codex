use serde::Serialize;

pub(crate) use crate::campaign_progress::CampaignDirective;
pub(crate) use crate::campaign_progress::CampaignProgress;
pub(crate) use crate::campaign_progress::ContinuationReason;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CampaignTerminalState {
    Won,
    TerminalBlock,
}

impl CampaignTerminalState {
    pub fn is_success(self) -> bool {
        match self {
            Self::Won => true,
            Self::TerminalBlock => false,
        }
    }
}

pub struct CampaignRun {
    limits: crate::campaign_progress::CampaignLimits,
}

impl CampaignRun {
    pub fn new(limits: crate::campaign_progress::CampaignLimits) -> Self {
        Self { limits }
    }
}

#[path = "campaign_loop.rs"]
mod campaign_loop;

#[cfg(test)]
#[path = "campaign_tests.rs"]
mod tests;
