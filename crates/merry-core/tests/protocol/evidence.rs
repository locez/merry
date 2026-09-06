use crate::assert_json_round_trip;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, RuntimeJournalEvent,
    RuntimeJournalPayload, SessionId, ToolCallId,
};
use serde_json::json;

#[test]
fn artifact_and_evidence_references_round_trip_with_stable_json_shapes() {
    let artifact = ArtifactRef::new(
        ArtifactId::new("artifact-1").expect("valid artifact id"),
        ArtifactKind::Json,
    )
    .with_label("Result payload")
    .expect("valid artifact label");

    let artifact_json = serde_json::to_value(&artifact).expect("artifact serializes");
    assert_eq!(
        artifact_json,
        json!({
            "id": "artifact-1",
            "kind": "json",
            "label": "Result payload"
        })
    );
    assert_eq!(artifact.id().as_str(), "artifact-1");
    assert_eq!(artifact.kind(), &ArtifactKind::Json);
    assert_eq!(artifact.label(), Some("Result payload"));
    assert_json_round_trip(&artifact);
    assert!(
        serde_json::from_value::<ArtifactRef>(json!({
            "id": "artifact-1",
            "kind": "json",
            "label": " invalid label "
        }))
        .is_err()
    );

    let whole = EvidenceRef::new(
        ArtifactId::new("artifact-1").expect("valid artifact id"),
        EvidenceLocator::whole_artifact(),
    );
    assert_eq!(
        serde_json::to_value(&whole).expect("evidence serializes"),
        json!({
            "artifact_id": "artifact-1",
            "locator": { "type": "whole_artifact" }
        })
    );
    assert_json_round_trip(&whole);

    let locators = [
        EvidenceLocator::line_range(3, 8).expect("valid line range"),
        EvidenceLocator::byte_range(10, 42).expect("valid byte range"),
        EvidenceLocator::json_pointer("/items/0/name").expect("valid json pointer"),
        EvidenceLocator::named_section("Findings").expect("valid named section"),
    ];

    for locator in locators {
        assert_json_round_trip(&locator);
    }

    let line_range = EvidenceLocator::line_range(3, 8).expect("valid line range");
    assert_eq!(line_range.as_line_range(), Some((3, 8)));
    assert_eq!(line_range.as_byte_range(), None);
    assert_eq!(line_range.as_json_pointer(), None);
    assert_eq!(line_range.as_named_section(), None);
    assert!(!line_range.is_whole_artifact());

    let byte_range = EvidenceLocator::byte_range(10, 42).expect("valid byte range");
    assert_eq!(byte_range.as_byte_range(), Some((10, 42)));

    let pointer = EvidenceLocator::json_pointer("/items/0/name").expect("valid json pointer");
    assert_eq!(pointer.as_json_pointer(), Some("/items/0/name"));

    let section = EvidenceLocator::named_section("Findings").expect("valid named section");
    assert_eq!(section.as_named_section(), Some("Findings"));

    assert!(EvidenceLocator::whole_artifact().is_whole_artifact());
    assert_eq!(EvidenceLocator::whole_artifact().as_line_range(), None);
}

#[test]
fn evidence_locators_reject_invalid_ranges_and_json_pointers() {
    assert!(EvidenceLocator::line_range(0, 1).is_err());
    assert!(EvidenceLocator::line_range(4, 3).is_err());
    assert!(EvidenceLocator::byte_range(7, 7).is_err());
    assert!(EvidenceLocator::byte_range(8, 7).is_err());

    for invalid in [
        "items/0",
        "/bad~escape",
        "/bad~2escape",
        "/has/control\nchar",
    ] {
        assert!(
            EvidenceLocator::json_pointer(invalid).is_err(),
            "{invalid:?} should reject"
        );
    }

    assert!(EvidenceLocator::named_section("").is_err());
    assert!(EvidenceLocator::named_section(" section ").is_err());

    assert!(
        serde_json::from_value::<EvidenceLocator>(json!({
            "type": "line_range",
            "start": 0,
            "end": 1
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<EvidenceLocator>(json!({
            "type": "byte_range",
            "start": 8,
            "end": 7
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<EvidenceLocator>(json!({
            "type": "named_section",
            "name": " section "
        }))
        .is_err()
    );
}

#[test]
fn empty_json_pointer_policy_uses_whole_artifact_locator() {
    assert!(
        EvidenceLocator::json_pointer("").is_err(),
        "empty JSON Pointer is reserved for EvidenceLocator::WholeArtifact"
    );
    assert!(
        serde_json::from_value::<EvidenceLocator>(json!({
            "type": "json_pointer",
            "pointer": ""
        }))
        .is_err()
    );

    assert_json_round_trip(&EvidenceLocator::whole_artifact());
}

#[test]
fn final_output_recorded_event_uses_artifact_ref_without_payload() {
    let event = RuntimeJournalEvent::new(
        SessionId::new("final-output-session").expect("valid session id"),
        3,
        RuntimeJournalPayload::FinalOutputRecorded {
            call_id: ToolCallId::new("call-final").expect("valid call id"),
            artifact: ArtifactRef::new(
                ArtifactId::new("final-output-3").expect("valid artifact id"),
                ArtifactKind::Json,
            ),
        },
    );

    let value = serde_json::to_value(&event).expect("event serializes");

    assert_eq!(value["payload"]["type"], json!("final_output_recorded"));
    assert_eq!(value["payload"]["call_id"], json!("call-final"));
    assert_eq!(value["payload"]["artifact"]["id"], json!("final-output-3"));
    assert_eq!(value["payload"]["artifact"]["kind"], json!("json"));
    assert!(value["payload"].get("content").is_none());
    assert_json_round_trip(&event);
}
