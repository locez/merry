use super::*;

#[test]
fn trajectory_record_serialization_keeps_unknown_timing_explicit() {
    let id = TrajectoryRecordId::new("record-1").expect("valid record id");
    let record = TrajectoryRecord::new(
        id,
        TrajectoryLane::Model,
        TrajectoryRecordKind::AssistantMessage,
        "assistant".to_owned(),
        TrajectoryRecordStatus::Running,
        4,
    );
    let json = serde_json::to_value(record).expect("record serializes");
    assert_eq!(json["start_sequence"], "4");
    assert_eq!(json["started_at_ms"], serde_json::Value::Null);
    assert_eq!(json["finished_at_ms"], serde_json::Value::Null);
    assert_eq!(json["sequence_order"], 0);
    assert_eq!(json["turn_id"], serde_json::Value::Null);
}

#[test]
fn trajectory_wire_counters_round_trip_string_and_legacy_number_forms() {
    let id = TrajectoryRecordId::new("record-1").expect("valid record id");
    let mut record = TrajectoryRecord::new(
        id,
        TrajectoryLane::Model,
        TrajectoryRecordKind::AssistantMessage,
        "assistant".to_owned(),
        TrajectoryRecordStatus::Running,
        4,
    );
    record.set_turn_id(Some(TrajectoryTurnId::new(7).expect("valid turn id")));
    let mut json = serde_json::to_value(&record).expect("record serializes");
    assert_eq!(json["turn_id"], "7");
    json["start_sequence"] = serde_json::json!(9);
    json["turn_id"] = serde_json::json!(8);
    let restored: TrajectoryRecord =
        serde_json::from_value(json).expect("legacy numeric counters remain readable");
    assert_eq!(restored.start_sequence(), 9);
    assert_eq!(restored.turn_id().map(TrajectoryTurnId::value), Some(8));
}

#[test]
fn trajectory_turn_id_rejects_zero_on_construction_and_deserialization() {
    assert!(TrajectoryTurnId::new(0).is_err());
    assert!(serde_json::from_str::<TrajectoryTurnId>("0").is_err());
}

#[test]
fn snapshot_orders_records_by_sequence_then_explicit_order() {
    let session = SessionId::new("trajectory-test").expect("valid session id");
    let mut snapshot = TrajectorySnapshot::new(session);
    for (id, order) in [("record-late", 2), ("record-early", 1)] {
        let mut record = TrajectoryRecord::new(
            TrajectoryRecordId::new(id).expect("valid record id"),
            TrajectoryLane::Model,
            TrajectoryRecordKind::AssistantMessage,
            id.to_owned(),
            TrajectoryRecordStatus::Succeeded,
            4,
        );
        record.set_sequence_order(order);
        assert!(snapshot.upsert_record(record));
    }
    assert_eq!(snapshot.records()[0].id().as_str(), "record-early");
}

#[test]
fn snapshot_retains_all_records_without_a_history_cap() {
    let session = SessionId::new("trajectory-test").expect("valid session id");
    let mut snapshot = TrajectorySnapshot::new(session);
    for sequence in 1..=4_097 {
        let mut record = TrajectoryRecord::new(
            TrajectoryRecordId::new(&format!("record-{sequence}")).expect("valid record id"),
            TrajectoryLane::Model,
            TrajectoryRecordKind::AssistantMessage,
            "assistant".to_owned(),
            TrajectoryRecordStatus::Succeeded,
            sequence,
        );
        record.set_sequence_order(sequence as u32);
        assert!(snapshot.upsert_record(record));
    }

    assert_eq!(snapshot.records().len(), 4_097);
    assert_eq!(snapshot.history_truncated_before(), None);
}

#[test]
fn persisted_closed_snapshot_can_be_reopened_for_resume() {
    let session = SessionId::new("trajectory-test").expect("valid session id");
    let mut snapshot = TrajectorySnapshot::new(session);

    snapshot.mark_closed();
    assert!(snapshot.is_closed());

    snapshot.reopen();
    assert!(!snapshot.is_closed());
}
