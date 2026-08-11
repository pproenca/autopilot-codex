use codex_core_api::CodexThread;
use codex_core_api::Op;

use super::CampaignProgress;
use super::ContinuationReason;
use super::campaign_submit::campaign_submit_error;
use super::campaign_submit::submit_continuation;
use crate::DecisionGate;
use crate::RunnerError;

#[derive(Default)]
pub(super) struct CampaignCompaction {
    requested: bool,
    continuation: Option<ContinuationReason>,
    applied: bool,
}

impl CampaignCompaction {
    pub(super) fn request(&mut self) {
        self.requested = true;
    }

    pub(super) fn is_active(&self) -> bool {
        self.continuation.is_some()
    }

    pub(super) fn record_applied(&mut self) {
        self.applied = true;
    }

    pub(super) async fn submit_at_boundary(
        &mut self,
        thread: &CodexThread,
        gate: &DecisionGate,
        progress: &CampaignProgress,
        reason: ContinuationReason,
    ) -> Result<(), RunnerError> {
        if std::mem::take(&mut self.requested) {
            self.continuation = Some(reason);
            thread
                .submit(Op::Compact)
                .await
                .map(|_| ())
                .map_err(campaign_submit_error)
        } else {
            submit_continuation(thread, gate, progress, reason).await
        }
    }

    pub(super) fn finish(
        &mut self,
        error: Option<String>,
    ) -> Result<Option<ContinuationReason>, String> {
        let Some(reason) = self.continuation.take() else {
            return Ok(None);
        };
        if let Some(error) = error {
            return Err(error);
        }
        if !std::mem::take(&mut self.applied) {
            return Err(
                "context compaction completed without replacement history".to_string(),
            );
        }
        Ok(Some(reason))
    }
}
