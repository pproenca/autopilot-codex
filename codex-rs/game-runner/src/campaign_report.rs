use std::path::PathBuf;

use serde::Serialize;

use crate::AcceptedPlan;
use crate::AuthorizedMutation;
use crate::DecisionAudit;
use crate::DecisionSnapshot;
use crate::MutationResult;
use crate::ObservationEvidence;
use crate::OwnerLease;
use crate::PolicyAudit;
use crate::ReportedOutcome;
use crate::campaign::CampaignTerminalState;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CampaignReport {
    pub terminal_state: CampaignTerminalState,
    pub thread_id: String,
    pub turn_ids: Vec<String>,
    pub rollout_path: PathBuf,
    pub before: Option<ObservationEvidence>,
    pub after: Option<ObservationEvidence>,
    pub accepted_plan: Option<AcceptedPlan>,
    pub mutation: Option<AuthorizedMutation>,
    pub mutation_result: Option<MutationResult>,
    pub outcome: Option<ReportedOutcome>,
    pub owner_lease: OwnerLease,
    pub decision_audit: DecisionAudit,
    pub policy_audit: PolicyAudit,
    pub terminal_failure: Option<String>,
}

pub(crate) struct CampaignReportContext {
    pub terminal_state: CampaignTerminalState,
    pub thread_id: String,
    pub turn_ids: Vec<String>,
    pub rollout_path: PathBuf,
    pub owner_lease: OwnerLease,
    pub policy_audit: PolicyAudit,
    pub terminal_failure: Option<String>,
}

impl CampaignReport {
    pub(crate) fn from_snapshot(
        context: CampaignReportContext,
        snapshot: DecisionSnapshot,
    ) -> Self {
        let before = snapshot
            .mutation
            .as_ref()
            .map(|mutation| mutation.plan.observation.clone());
        let after = snapshot.observation.clone().filter(|observation| {
            before
                .as_ref()
                .is_some_and(|before| observation.generation > before.generation)
        });
        let accepted_plan = snapshot
            .mutation
            .as_ref()
            .map(|mutation| mutation.plan.clone())
            .or(snapshot.plan);
        let mutation = snapshot
            .mutation
            .as_ref()
            .map(|mutation| mutation.authorization.clone());
        let mutation_result = snapshot
            .mutation
            .as_ref()
            .and_then(|mutation| mutation.result);

        Self {
            terminal_state: context.terminal_state,
            thread_id: context.thread_id,
            turn_ids: context.turn_ids,
            rollout_path: context.rollout_path,
            before,
            after,
            accepted_plan,
            mutation,
            mutation_result,
            outcome: snapshot.outcome,
            owner_lease: context.owner_lease,
            decision_audit: snapshot.audit,
            policy_audit: context.policy_audit,
            terminal_failure: context.terminal_failure,
        }
    }
}
