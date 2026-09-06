pub(super) use attempts::{
    deliver_directives, heartbeat, issue_directive, record_runtime_effect, report_attempt,
    report_progress, start_attempt, start_local_attempt,
};
pub(super) use control::{cancel_attempt, control_plan, recover_attempts, review_progress};
pub(super) use editing::{authorize_execution, begin_plan, begin_user_plan, update_plan};
use merry_core::{PlanSnapshot, RuntimeJournalPayload};
pub(super) use subagents::{bind_subagent, update_subagent, update_subagent_link};

mod attempts;

mod editing;

mod persistence;

mod subagents;

mod control;

pub(super) fn plan_updated_payload(snapshot: &PlanSnapshot) -> RuntimeJournalPayload {
    let summary = snapshot
        .revision_summaries
        .last()
        .cloned()
        .expect("plan mutation records a revision summary");
    RuntimeJournalPayload::PlanUpdated {
        snapshot: snapshot.clone(),
        summary,
    }
}
