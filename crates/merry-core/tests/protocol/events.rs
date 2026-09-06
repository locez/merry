use crate::assert_json_round_trip;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, QueuedInputLane, QueuedInputView,
    QueuedInputsView, RuntimeEvent, RuntimeEventSource, RuntimeJournalEvent, RuntimeJournalPayload,
    SessionId, ToolCallId,
};
use serde_json::json;

#[test]
fn public_runtime_event_assistant_message_uses_top_level_type() {
    let source = RuntimeEventSource::new(SessionId::new("session-1").expect("valid session id"), 4);
    let event = RuntimeEvent::AssistantMessage {
        text: "hello from the model".to_owned(),
        artifact: ArtifactRef::new(
            ArtifactId::new("assistant-output-4").expect("valid artifact id"),
            ArtifactKind::Text,
        ),
        source,
    };

    assert_eq!(
        serde_json::to_value(&event).expect("event serializes"),
        json!({
            "type": "assistant_message",
            "text": "hello from the model",
            "artifact": {
                "id": "assistant-output-4",
                "kind": "text",
                "label": null
            },
            "source": {
                "session_id": "session-1",
                "sequence": 4
            }
        })
    );
    assert_json_round_trip(&event);
}

#[test]
fn public_runtime_event_assistant_message_delta_uses_top_level_type() {
    let source = RuntimeEventSource::new(SessionId::new("session-1").expect("valid session id"), 5);
    let event = RuntimeEvent::AssistantMessageDelta {
        delta: "hel".to_owned(),
        source,
    };

    assert_eq!(
        serde_json::to_value(&event).expect("event serializes"),
        json!({
            "type": "assistant_message_delta",
            "delta": "hel",
            "source": {
                "session_id": "session-1",
                "sequence": 5
            }
        })
    );
    assert_json_round_trip(&event);
}

#[test]
fn public_queued_inputs_changed_uses_inputs_view() {
    let event = RuntimeEvent::QueuedInputsChanged {
        inputs: QueuedInputsView {
            next: vec![QueuedInputView {
                text: "use the other approach".to_owned(),
                lane: QueuedInputLane::Next,
                position: 0,
            }],
            suspended: Vec::new(),
            backlog: vec![QueuedInputView {
                text: "run tests after that".to_owned(),
                lane: QueuedInputLane::Backlog,
                position: 0,
            }],
        },
    };

    assert_eq!(
        serde_json::to_value(&event).expect("event serializes"),
        json!({
            "type": "queued_inputs_changed",
            "inputs": {
                "next": [{
                    "text": "use the other approach",
                    "lane": "next",
                    "position": 0
                }],
                "suspended": [],
                "backlog": [{
                    "text": "run tests after that",
                    "lane": "backlog",
                    "position": 0
                }]
            }
        })
    );
    assert_json_round_trip(&event);
}

#[test]
fn skill_used_event_records_catalog_skill_read() {
    let event = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        11,
        RuntimeJournalPayload::SkillUsed {
            skill_name: "demo-skill".to_owned(),
            skill_md_path: "demo/SKILL.md".to_owned(),
            tool_call_id: ToolCallId::new("call-read-skill").expect("valid call id"),
            artifact: ArtifactRef::new(
                ArtifactId::new("tool-result-1").expect("valid artifact id"),
                ArtifactKind::Json,
            ),
        },
    );

    assert_eq!(
        serde_json::to_value(&event).expect("event serializes"),
        json!({
            "session_id": "session-1",
            "sequence": 11,
            "payload": {
                "type": "skill_used",
                "skill_name": "demo-skill",
                "skill_md_path": "demo/SKILL.md",
                "tool_call_id": "call-read-skill",
                "artifact": {
                    "id": "tool-result-1",
                    "kind": "json",
                    "label": null
                }
            }
        })
    );
    assert_json_round_trip(&event);
}

#[test]
fn runtime_event_uses_stable_snake_case_tags_and_round_trips() {
    let event = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        7,
        RuntimeJournalPayload::ArtifactRecorded {
            artifact: ArtifactRef::new(
                ArtifactId::new("artifact-1").expect("valid artifact id"),
                ArtifactKind::Text,
            ),
        },
    );

    assert_eq!(
        serde_json::to_value(&event).expect("event serializes"),
        json!({
            "session_id": "session-1",
            "sequence": 7,
            "payload": {
                "type": "artifact_recorded",
                "artifact": {
                    "id": "artifact-1",
                    "kind": "text",
                    "label": null
                }
            }
        })
    );
    assert_json_round_trip(&event);

    let failed = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        8,
        RuntimeJournalPayload::Failed {
            diagnostic: ErrorInfo::new("validation", "Tool spec was invalid")
                .expect("valid diagnostic"),
        },
    );
    let failed_json = serde_json::to_value(&failed).expect("failed event serializes");
    assert!(failed_json.get("provider").is_none());
    assert_json_round_trip(&failed);

    let diagnostic =
        ErrorInfo::new("validation", "Tool spec was invalid").expect("valid diagnostic");
    assert_eq!(diagnostic.code(), "validation");
    assert_eq!(diagnostic.message(), "Tool spec was invalid");

    assert!(ErrorInfo::new("", "message").is_err());
    assert!(ErrorInfo::new("kind", " ").is_err());
    assert!(
        serde_json::from_value::<ErrorInfo>(json!({
            "code": " validation",
            "message": "Tool spec was invalid"
        }))
        .is_err()
    );
}
