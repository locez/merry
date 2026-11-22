use crate::{assert_json_round_trip, sample_plan_document};
use merry_core::{
    PlanDocumentStatus, PlanModeState, PlanReviewDecision, PlanSessionSnapshot, RuntimeEvent,
    RuntimeEventSource, RuntimeJournalEvent, RuntimeJournalPayload, SessionId, TodoItem,
    TodoStatus,
};
use serde_json::json;

#[test]
fn plan_mode_and_todo_enums_use_stable_snake_case_json() {
    assert_eq!(
        serde_json::to_value(PlanModeState::ExitPending).expect("mode serializes"),
        json!("exit_pending")
    );
    assert_eq!(
        serde_json::to_value(PlanDocumentStatus::AwaitingApproval).expect("status serializes"),
        json!("awaiting_approval")
    );
    assert_eq!(
        serde_json::to_value(TodoStatus::InProgress).expect("todo status serializes"),
        json!("in_progress")
    );
}

#[test]
fn plan_and_todo_events_round_trip() {
    let plan = sample_plan_document();
    let source = RuntimeEventSource::new(SessionId::new("session-1").expect("valid session id"), 7);

    let mode_event = RuntimeEvent::PlanModeChanged {
        mode: PlanModeState::Active,
        source: source.clone(),
    };
    assert_eq!(
        serde_json::to_value(&mode_event).expect("mode event serializes")["type"],
        "plan_mode_changed"
    );
    assert_json_round_trip(&mode_event);

    let saved_event = RuntimeEvent::PlanSaved {
        plan: plan.clone(),
        source: source.clone(),
    };
    assert_eq!(
        serde_json::to_value(&saved_event).expect("saved event serializes")["type"],
        "plan_saved"
    );
    assert_json_round_trip(&saved_event);

    let approval_event = RuntimeEvent::PlanApprovalRequested {
        plan: plan.clone(),
        source: source.clone(),
    };
    assert_eq!(
        serde_json::to_value(&approval_event).expect("approval event serializes")["type"],
        "plan_approval_requested"
    );
    assert_json_round_trip(&approval_event);

    let reviewed_event = RuntimeEvent::PlanReviewed {
        plan_id: plan.summary.id.clone(),
        decision: PlanReviewDecision::Approved {
            note: Some("Looks good, proceed.".to_owned()),
        },
        source: source.clone(),
    };
    assert_eq!(
        serde_json::to_value(&reviewed_event).expect("reviewed event serializes")["type"],
        "plan_reviewed"
    );
    assert_json_round_trip(&reviewed_event);

    let todo_items = vec![
        TodoItem {
            id: "1".to_owned(),
            content: "Implement Plan Mode gate".to_owned(),
            status: TodoStatus::InProgress,
        },
        TodoItem {
            id: "2".to_owned(),
            content: "Add TUI modal".to_owned(),
            status: TodoStatus::Pending,
        },
    ];
    let todo_event = RuntimeEvent::TodoListUpdated {
        items: todo_items.clone(),
        source,
    };
    assert_eq!(
        serde_json::to_value(&todo_event).expect("todo event serializes")["type"],
        "todo_list_updated"
    );
    assert_json_round_trip(&todo_event);

    let journal = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        7,
        RuntimeJournalPayload::TodoListUpdated { items: todo_items },
    );
    assert_json_round_trip(&journal);

    let session_snapshot = PlanSessionSnapshot {
        mode: PlanModeState::Active,
        active_plan_id: Some(plan.summary.id.clone()),
        awaiting_approval_plan_id: Some(plan.summary.id.clone()),
        plans: vec![plan.summary],
    };
    assert_json_round_trip(&session_snapshot);
}
