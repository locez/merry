use crate::{assert_json_round_trip, sample_plan_snapshot};
use merry_core::{
    PlanExecutorPolicy, PlanNodeStatus, PlanPhase, PlanRevisionSummary, PlanSchedulerStatus,
    RuntimeEvent, RuntimeEventSource, RuntimeJournalEvent, RuntimeJournalPayload, SessionId,
};
use serde_json::json;

#[test]
fn plan_phase_and_status_use_stable_snake_case_json() {
    assert_eq!(
        serde_json::to_value(PlanPhase::AwaitingApproval).expect("phase serializes"),
        json!("awaiting_approval")
    );
    assert_eq!(
        serde_json::to_value(PlanNodeStatus::InProgress).expect("status serializes"),
        json!("in_progress")
    );
    assert_eq!(
        serde_json::to_value(PlanExecutorPolicy::Delegate).expect("executor serializes"),
        json!("delegate")
    );
    assert_eq!(
        serde_json::to_value(PlanSchedulerStatus::Draining).expect("scheduler serializes"),
        json!("draining")
    );
}

#[test]
fn plan_updated_event_round_trips_with_bounded_snapshot() {
    let snapshot = sample_plan_snapshot();
    let summary =
        PlanRevisionSummary::new(2, "root execution started").expect("valid revision summary");
    let event = RuntimeEvent::PlanUpdated {
        snapshot: snapshot.clone(),
        summary: summary.clone(),
        source: RuntimeEventSource::new(SessionId::new("session-1").expect("valid session id"), 7),
    };

    let json = serde_json::to_value(&event).expect("plan event serializes");
    assert_eq!(json["type"], "plan_updated");
    assert_eq!(json["snapshot"]["plan_id"], "plan-1");
    assert_eq!(json["snapshot"]["nodes"][0]["id"], "node-root");
    assert_eq!(json["summary"]["revision"], 2);
    assert_json_round_trip(&event);

    let journal = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        7,
        RuntimeJournalPayload::PlanUpdated { snapshot, summary },
    );
    assert_json_round_trip(&journal);
}
