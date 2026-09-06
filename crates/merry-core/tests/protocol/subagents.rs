use crate::assert_json_round_trip;
use merry_core::{
    ErrorInfo, RuntimeEvent, RuntimeEventSource, RuntimeJournalEvent, RuntimeJournalPayload,
    SessionId, SubagentActivityPhase, SubagentActivitySnapshot, SubagentId, SubagentStatus,
    SubagentTaskId,
};
use serde_json::{Value, json};

#[test]
fn subagent_activity_phases_use_stable_snake_case_json() {
    let phases = [
        (SubagentActivityPhase::Starting, "starting"),
        (SubagentActivityPhase::Running, "running"),
        (SubagentActivityPhase::Waiting, "waiting"),
        (SubagentActivityPhase::Completed, "completed"),
        (SubagentActivityPhase::Failed, "failed"),
        (SubagentActivityPhase::Cancelled, "cancelled"),
        (SubagentActivityPhase::Blocked, "blocked"),
    ];

    for (phase, expected) in phases {
        assert_eq!(
            serde_json::to_value(phase).expect("activity phase serializes"),
            json!(expected)
        );
    }
}

#[test]
fn subagent_activity_snapshot_round_trips_as_json() {
    let snapshot = SubagentActivitySnapshot {
        subagent_id: SubagentId::new("subagent-1").expect("valid subagent id"),
        task_id: SubagentTaskId::new("task-1").expect("valid task id"),
        phase: SubagentActivityPhase::Running,
        summary: "Reading protocol tests".to_owned(),
        updated_at_ms: 1_725_000_000_000,
    };

    assert_eq!(
        serde_json::to_value(&snapshot).expect("activity snapshot serializes"),
        json!({
            "subagent_id": "subagent-1",
            "task_id": "task-1",
            "phase": "running",
            "summary": "Reading protocol tests",
            "updated_at_ms": 1_725_000_000_000_u64
        })
    );
    assert_json_round_trip(&snapshot);
}

#[test]
fn subagent_activity_snapshot_rejects_unknown_fields() {
    assert!(
        serde_json::from_value::<SubagentActivitySnapshot>(json!({
            "subagent_id": "subagent-1",
            "task_id": "task-1",
            "phase": "waiting",
            "summary": "Waiting for input",
            "updated_at_ms": 1_725_000_000_000_u64,
            "provider_status": "queued"
        }))
        .is_err()
    );
}

#[test]
fn public_subagent_status_changed_uses_typed_status() {
    let event = RuntimeEvent::SubagentStatusChanged {
        agent_id: SubagentId::new("agent-1").expect("valid subagent id"),
        task_id: SubagentTaskId::new("task-1").expect("valid task id"),
        status: SubagentStatus::Running,
        source: RuntimeEventSource::new(SessionId::new("session-1").expect("valid session id"), 6),
    };

    assert_eq!(
        serde_json::to_value(&event).expect("event serializes"),
        json!({
            "type": "subagent_status_changed",
            "agent_id": "agent-1",
            "task_id": "task-1",
            "status": "running",
            "source": {
                "session_id": "session-1",
                "sequence": 6
            }
        })
    );
    assert!(
        serde_json::from_value::<RuntimeEvent>(json!({
            "type": "subagent_status_changed",
            "agent_id": "agent-1",
            "task_id": "task-1",
            "status": "not_a_status",
            "source": {
                "session_id": "session-1",
                "sequence": 6
            }
        }))
        .is_err()
    );
    assert_json_round_trip(&event);
}

#[test]
fn subagent_spawned_event_uses_snake_case_and_round_trips() {
    let event = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        12,
        RuntimeJournalPayload::SubagentSpawned {
            agent_id: SubagentId::new("agent-1").expect("valid subagent id"),
            task_id: SubagentTaskId::new("task-1").expect("valid subagent task id"),
            task_anchor: "crates/merry-runtime/src/subagent.rs".to_owned(),
        },
    );

    assert_eq!(
        serde_json::to_value(&event).expect("event serializes"),
        json!({
            "session_id": "session-1",
            "sequence": 12,
            "payload": {
                "type": "subagent_spawned",
                "agent_id": "agent-1",
                "task_id": "task-1",
                "task_anchor": "crates/merry-runtime/src/subagent.rs"
            }
        })
    );
    assert_json_round_trip(&event);
}

#[test]
fn subagent_nonterminal_events_use_snake_case_and_round_trip() {
    let started = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        13,
        RuntimeJournalPayload::SubagentStarted {
            agent_id: SubagentId::new("agent-1").expect("valid subagent id"),
            task_id: SubagentTaskId::new("task-1").expect("valid subagent task id"),
        },
    );
    assert_eq!(
        serde_json::to_value(&started).expect("started event serializes"),
        json!({
            "session_id": "session-1",
            "sequence": 13,
            "payload": {
                "type": "subagent_started",
                "agent_id": "agent-1",
                "task_id": "task-1"
            }
        })
    );
    assert_json_round_trip(&started);

    let status_changed = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        14,
        RuntimeJournalPayload::SubagentStatusChanged {
            agent_id: SubagentId::new("agent-1").expect("valid subagent id"),
            task_id: SubagentTaskId::new("task-1").expect("valid subagent task id"),
            status: SubagentStatus::Running,
        },
    );
    assert_eq!(
        serde_json::to_value(&status_changed).expect("status event serializes"),
        json!({
            "session_id": "session-1",
            "sequence": 14,
            "payload": {
                "type": "subagent_status_changed",
                "agent_id": "agent-1",
                "task_id": "task-1",
                "status": "running"
            }
        })
    );
    assert_json_round_trip(&status_changed);
}

#[test]
fn subagent_terminal_events_do_not_embed_large_payloads() {
    let completed = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        13,
        RuntimeJournalPayload::SubagentCompleted {
            agent_id: SubagentId::new("agent-1").expect("valid subagent id"),
            task_id: SubagentTaskId::new("task-1").expect("valid subagent task id"),
            summary: "Updated protocol tests and core event vocabulary.".to_owned(),
            output_paths: vec!["artifacts/subagents/task-1/report.md".to_owned()],
            changed_paths: vec!["crates/merry-core/src/event.rs".to_owned()],
        },
    );
    let completed_json = serde_json::to_value(&completed).expect("completed event serializes");
    assert_eq!(
        completed_json,
        json!({
            "session_id": "session-1",
            "sequence": 13,
            "payload": {
                "type": "subagent_completed",
                "agent_id": "agent-1",
                "task_id": "task-1",
                "summary": "Updated protocol tests and core event vocabulary.",
                "output_paths": ["artifacts/subagents/task-1/report.md"],
                "changed_paths": ["crates/merry-core/src/event.rs"]
            }
        })
    );
    let completed_kind = completed_json
        .get("payload")
        .and_then(Value::as_object)
        .expect("kind should be an object");
    assert!(completed_kind.get("output").is_none());
    assert!(completed_kind.get("payload").is_none());
    assert!(completed_kind.get("artifact").is_none());
    assert_json_round_trip(&completed);

    let failed = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        14,
        RuntimeJournalPayload::SubagentFailed {
            agent_id: SubagentId::new("agent-1").expect("valid subagent id"),
            task_id: SubagentTaskId::new("task-1").expect("valid subagent task id"),
            diagnostic: ErrorInfo::new("subagent_failed", "Subagent exited with status 1")
                .expect("valid diagnostic"),
        },
    );
    let failed_json = serde_json::to_value(&failed).expect("failed event serializes");
    assert_eq!(
        failed_json,
        json!({
            "session_id": "session-1",
            "sequence": 14,
            "payload": {
                "type": "subagent_failed",
                "agent_id": "agent-1",
                "task_id": "task-1",
                "diagnostic": {
                    "code": "subagent_failed",
                    "message": "Subagent exited with status 1"
                }
            }
        })
    );
    let failed_kind = failed_json
        .get("payload")
        .and_then(Value::as_object)
        .expect("kind should be an object");
    assert!(failed_kind.get("output").is_none());
    assert!(failed_kind.get("payload").is_none());
    assert!(failed_kind.get("artifact").is_none());
    assert_json_round_trip(&failed);

    let cancelled = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        15,
        RuntimeJournalPayload::SubagentCancelled {
            agent_id: SubagentId::new("agent-1").expect("valid subagent id"),
            task_id: SubagentTaskId::new("task-1").expect("valid subagent task id"),
            diagnostic: ErrorInfo::new("subagent_cancelled", "Cancellation token was dropped")
                .expect("valid diagnostic"),
        },
    );
    let cancelled_json = serde_json::to_value(&cancelled).expect("cancelled event serializes");
    assert_eq!(
        cancelled_json,
        json!({
            "session_id": "session-1",
            "sequence": 15,
            "payload": {
                "type": "subagent_cancelled",
                "agent_id": "agent-1",
                "task_id": "task-1",
                "diagnostic": {
                    "code": "subagent_cancelled",
                    "message": "Cancellation token was dropped"
                }
            }
        })
    );
    let cancelled_kind = cancelled_json
        .get("payload")
        .and_then(Value::as_object)
        .expect("kind should be an object");
    assert!(cancelled_kind.get("output").is_none());
    assert!(cancelled_kind.get("payload").is_none());
    assert!(cancelled_kind.get("artifact").is_none());
    assert_json_round_trip(&cancelled);
}
