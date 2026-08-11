use std::sync::Arc;

use serde::Serialize;

use crate::CampaignCheckpoint;
use crate::CampaignEvent;
use crate::CampaignPersistence;
use crate::CampaignReport;

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

pub(crate) enum CampaignStart {
    Fresh { target_app: String },
    Resumed { checkpoint: CampaignCheckpoint },
}

pub(crate) enum CampaignExecutionContext {
    Ephemeral {
        start: CampaignStart,
    },
    Durable {
        persistence: Arc<CampaignPersistence>,
        events: tokio::sync::broadcast::Sender<CampaignEvent>,
        start: CampaignStart,
    },
}

pub(crate) enum CampaignExit {
    VerifiedWin(CampaignReport),
    Paused,
    Stopped,
    Blocked(CampaignReport),
}

impl CampaignRun {
    pub fn new(limits: crate::campaign_progress::CampaignLimits) -> Self {
        Self { limits }
    }
}

#[path = "campaign_loop.rs"]
mod campaign_loop;

#[path = "campaign_event.rs"]
mod campaign_event;

#[cfg(test)]
#[path = "campaign_tests.rs"]
mod tests;
