use merry_core::{
    ArtifactKind, ArtifactRef, SessionId, ToolCallArguments, ToolCallId, ToolInputSchema, ToolName,
    ToolSpec, TrajectoryEvent, TrajectoryLane, TrajectoryPayload, TrajectoryPayloadKind,
    TrajectoryPromptBlock, TrajectoryRecord, TrajectoryRecordId, TrajectoryRecordKind,
    TrajectoryRecordStatus, TrajectorySnapshot, TrajectoryTurnId,
};
use schemars::Schema;
use serde_json::json;

fn canonical_fixture() -> serde_json::Value {
    let session_id = SessionId::new("trajectory-contract").expect("valid session id");
    let turn_id = TrajectoryTurnId::new(7).expect("valid turn id");
    let input_id = TrajectoryRecordId::new("input-1").expect("valid input record id");

    let mut input = TrajectoryRecord::new(
        input_id.clone(),
        TrajectoryLane::Input,
        TrajectoryRecordKind::UserInput,
        "User input".to_owned(),
        TrajectoryRecordStatus::Completed,
        42,
    );
    input.set_summary(Some("Inspect the repository".to_owned()));
    input.set_turn_id(Some(turn_id));
    input.set_message_details("Inspect the repository".to_owned(), false);
    input.finish(TrajectoryRecordStatus::Completed, 42);

    let call_id = ToolCallId::new("call-1").expect("valid tool call id");
    let mut tool = TrajectoryRecord::new(
        TrajectoryRecordId::new("tool-1").expect("valid tool record id"),
        TrajectoryLane::Tools,
        TrajectoryRecordKind::ToolCall,
        "Read file".to_owned(),
        TrajectoryRecordStatus::Succeeded,
        43,
    );
    tool.set_summary(Some("README.md".to_owned()));
    tool.set_turn_id(Some(turn_id));
    tool.set_relationship(Some(input_id), Some(call_id));
    tool.set_tool_details(
        Some(ToolName::new("read_file").expect("valid tool name")),
        ToolCallArguments::try_from(json!({"path": "README.md", "mode": "text"}))
            .expect("valid tool arguments"),
    );
    tool.set_tool_output(Some(TrajectoryPayload::new(
        TrajectoryPayloadKind::Text,
        "file contents".to_owned(),
        true,
    )));
    tool.add_artifact(
        ArtifactRef::new(
            merry_core::ArtifactId::new("artifact-1").expect("valid artifact id"),
            ArtifactKind::Text,
        )
        .with_label("README.md")
        .expect("valid artifact label"),
    );
    tool.finish(TrajectoryRecordStatus::Succeeded, 43);

    let mut snapshot = TrajectorySnapshot::new(session_id);
    snapshot.set_tool_specs(vec![
        ToolSpec::new(
            ToolName::new("read_file").expect("valid tool name"),
            "Read a UTF-8 file.",
            ToolInputSchema::new(
                Schema::try_from(json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }))
                .expect("valid tool schema"),
            )
            .expect("valid input schema"),
        )
        .expect("valid tool spec"),
    ]);
    snapshot.upsert_prompt_block(TrajectoryPromptBlock::new(
        TrajectoryRecordId::new("prompt-1").expect("valid prompt id"),
        0,
        "Stable runtime instructions".to_owned(),
        false,
    ));
    snapshot.add_dynamic_context(2, 41);
    snapshot.upsert_record(input);
    snapshot.upsert_record(tool.clone());
    snapshot.advance_latest_sequence(43);
    snapshot.advance_revision();

    serde_json::to_value(json!({
        "snapshot": snapshot.clone(),
        "events": [
            TrajectoryEvent::Snapshot {
                snapshot: snapshot.clone(),
            },
            TrajectoryEvent::RecordUpsert {
                revision: 2,
                latest_sequence: 43,
                record: Box::new(tool),
            },
            TrajectoryEvent::PromptUpdated {
                revision: 3,
                latest_sequence: 43,
                prompt: snapshot.prompt().clone(),
            },
            TrajectoryEvent::SessionClosed {
                revision: 4,
                latest_sequence: 43,
            },
        ],
    }))
    .expect("trajectory fixture serializes")
}

#[test]
fn canonical_trajectory_fixture_matches_rust_serialization() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/trajectory-contract.json"))
            .expect("canonical trajectory fixture is valid JSON");

    assert_eq!(canonical_fixture(), expected);
}

#[test]
fn checked_in_trajectory_schema_matches_rust_contract() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../schema/trajectory-event.json"))
            .expect("checked-in trajectory schema is valid JSON");
    let actual = serde_json::to_value(schemars::schema_for!(TrajectoryEvent))
        .expect("trajectory schema serializes");

    assert_eq!(actual, expected);
}
