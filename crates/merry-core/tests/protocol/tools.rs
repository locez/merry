use crate::{assert_json_round_trip, json_schema};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, PendingToolCall, PendingToolCallBatch,
    RuntimeEvent, RuntimeEventSource, RuntimeJournalEvent, RuntimeJournalPayload, SessionId,
    ToolCallArguments, ToolCallBatchId, ToolCallId, ToolCallResult, ToolCallResultStatus,
    ToolInputSchema, ToolName, ToolOutput, ToolSpec,
};
use schemars::Schema;
use serde_json::json;

#[test]
fn tool_name_uses_provider_portable_validation() {
    for valid in ["tool", "tool_1", "_internal", "Tool-Name_99"] {
        let name = ToolName::new(valid).expect("valid portable tool name");
        assert_eq!(name.as_str(), valid);
        assert_json_round_trip(&name);
    }

    for invalid in [
        "",
        "-starts-with-dash",
        "1starts_with_digit",
        "contains.dot",
        "contains space",
        "contains/slash",
        "contains:colon",
        "tool\nname",
    ] {
        assert!(ToolName::new(invalid).is_err(), "{invalid:?} should reject");
        assert!(serde_json::from_value::<ToolName>(json!(invalid)).is_err());
    }

    let max_len = "a".repeat(64);
    assert!(ToolName::new(&max_len).is_ok());
    let overlong = "a".repeat(65);
    assert!(ToolName::new(&overlong).is_err());
}

#[test]
fn tool_name_schema_exposes_provider_portable_validation() {
    let schema = serde_json::to_value(schemars::schema_for!(ToolName))
        .expect("tool name schema should serialize");

    assert_eq!(schema["type"], "string");
    assert_eq!(schema["minLength"], 1);
    assert_eq!(schema["maxLength"], 64);
    assert_eq!(schema["pattern"], r"^[A-Za-z_][A-Za-z0-9_-]*$");
}

#[test]
fn tool_spec_validates_names_descriptions_and_object_schemas() {
    let schema = ToolInputSchema::new(json_schema(json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" }
        },
        "required": ["path"]
    })))
    .expect("object schema is valid");

    let spec = ToolSpec::new(
        ToolName::new("read_file").expect("valid name"),
        "Read a file from the workspace",
        schema,
    )
    .expect("valid tool spec");

    assert_eq!(spec.name().as_str(), "read_file");
    assert_eq!(spec.description(), "Read a file from the workspace");
    assert!(spec.input_schema().as_schema().as_object().is_some());
    assert_json_round_trip(&spec);

    assert!(ToolName::new("bad.name").is_err());
    assert!(
        serde_json::from_value::<ToolSpec>(json!({
            "name": "bad.name",
            "description": "Bad tool name",
            "input_schema": { "type": "object" }
        }))
        .is_err()
    );
    assert!(
        ToolSpec::new(
            ToolName::new("read_file").expect("valid name"),
            "  ",
            ToolInputSchema::new(json_schema(json!({ "type": "object" }))).expect("valid schema"),
        )
        .is_err()
    );
    assert!(ToolInputSchema::new(Schema::try_from(json!(true)).expect("boolean schema")).is_err());
    assert!(Schema::try_from(json!([])).is_err());

    assert!(
        serde_json::from_value::<ToolSpec>(json!({
            "name": "read_file",
            "description": "bad\ndescription",
            "input_schema": { "type": "object" }
        }))
        .is_err()
    );
}

#[test]
fn pending_tool_call_event_uses_provider_neutral_payload_shape() {
    let arguments = ToolCallArguments::try_from(json!({
        "city": "Shanghai",
        "options": {
            "units": "metric",
            "days": [1, 2, 3]
        }
    }))
    .expect("object arguments are valid");

    let call = PendingToolCall::new(
        ToolCallId::new("call.provider/opaque.id:42").expect("valid call id"),
        ToolName::new("lookup_weather").expect("valid tool name"),
        arguments,
    );

    assert_eq!(call.id().as_str(), "call.provider/opaque.id:42");
    assert_eq!(call.name().as_str(), "lookup_weather");
    assert_eq!(
        call.arguments().as_object().get("options"),
        Some(&json!({
            "units": "metric",
            "days": [1, 2, 3]
        }))
    );

    let event = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        9,
        RuntimeJournalPayload::ToolCallPending { call },
    );

    assert_eq!(
        serde_json::to_value(&event).expect("event serializes"),
        json!({
            "session_id": "session-1",
            "sequence": 9,
            "payload": {
                "type": "tool_call_pending",
                "call": {
                    "id": "call.provider/opaque.id:42",
                    "name": "lookup_weather",
                    "arguments": {
                        "city": "Shanghai",
                        "options": {
                            "units": "metric",
                            "days": [1, 2, 3]
                        }
                    }
                }
            }
        })
    );
    assert_json_round_trip(&event);
}

#[test]
fn pending_tool_call_batch_validates_identity_order_and_unique_calls() {
    let call_a = PendingToolCall::new(
        ToolCallId::new("call-a").expect("valid call id"),
        ToolName::new("lookup_weather").expect("valid tool name"),
        ToolCallArguments::try_from(json!({ "city": "Shanghai" })).expect("valid arguments"),
    );
    let call_b = PendingToolCall::new(
        ToolCallId::new("call-b").expect("valid call id"),
        ToolName::new("lookup_weather").expect("valid tool name"),
        ToolCallArguments::try_from(json!({ "city": "Tokyo" })).expect("valid arguments"),
    );
    let batch_id = ToolCallBatchId::new("tool-batch-7").expect("valid batch id");

    let batch = PendingToolCallBatch::new(batch_id.clone(), vec![call_a.clone(), call_b])
        .expect("valid ordered batch");
    assert_eq!(batch.id(), &batch_id);
    assert_eq!(
        batch
            .calls()
            .iter()
            .map(|call| call.id().as_str())
            .collect::<Vec<_>>(),
        ["call-a", "call-b"]
    );
    assert_json_round_trip(&batch);

    assert!(PendingToolCallBatch::new(batch_id.clone(), Vec::new()).is_err());
    assert!(PendingToolCallBatch::new(batch_id, vec![call_a.clone(), call_a]).is_err());
}

#[test]
fn pending_tool_call_validates_call_id_tool_name_and_object_arguments() {
    for valid in ["call-1", "call.provider/opaque.id:42", "openai_call_123"] {
        let id = ToolCallId::new(valid).expect("valid provider-originated call id");
        assert_eq!(id.as_str(), valid);
        assert_json_round_trip(&id);
    }

    for invalid in ["", "   ", " leading", "trailing ", "has\nnewline"] {
        assert!(
            ToolCallId::new(invalid).is_err(),
            "{invalid:?} should reject"
        );
        assert!(serde_json::from_value::<ToolCallId>(json!(invalid)).is_err());
    }

    let max_len = "c".repeat(256);
    assert!(ToolCallId::new(&max_len).is_ok());
    let overlong = "c".repeat(257);
    assert!(ToolCallId::new(&overlong).is_err());

    assert!(ToolCallArguments::try_from(json!({ "path": "README.md" })).is_ok());
    for invalid in [json!(null), json!(true), json!("text"), json!([["path"]])] {
        assert!(
            ToolCallArguments::try_from(invalid).is_err(),
            "non-object arguments should reject"
        );
    }

    assert!(
        serde_json::from_value::<PendingToolCall>(json!({
            "id": "call-1",
            "name": "bad.name",
            "arguments": {}
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PendingToolCall>(json!({
            "id": "call-1",
            "name": "lookup_weather",
            "arguments": "not an object"
        }))
        .is_err()
    );
}

#[test]
fn tool_call_result_uses_status_constraints_and_artifact_reference_only() {
    let call_id = ToolCallId::new("call-1").expect("valid call id");
    let artifact = ArtifactRef::new(
        ArtifactId::new("tool-result-1").expect("valid artifact id"),
        ArtifactKind::Json,
    );
    let success = ToolCallResult::succeeded(call_id.clone(), artifact.clone());

    assert_eq!(success.call_id(), &call_id);
    assert_eq!(success.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(success.artifact(), &artifact);
    assert!(success.diagnostic().is_none());
    assert_eq!(
        serde_json::to_value(&success).expect("success result serializes"),
        json!({
            "call_id": "call-1",
            "status": "succeeded",
            "artifact": {
                "id": "tool-result-1",
                "kind": "json",
                "label": null
            },
            "diagnostic": null
        })
    );
    assert_json_round_trip(&success);

    let diagnostic =
        ErrorInfo::new("tool_failed", "Tool exited with status 2").expect("valid diagnostic");
    let failed = ToolCallResult::failed(call_id.clone(), artifact.clone(), diagnostic.clone());
    assert_eq!(failed.status(), ToolCallResultStatus::Failed);
    assert_eq!(failed.diagnostic(), Some(&diagnostic));
    assert_eq!(
        serde_json::to_value(&failed).expect("failed result serializes"),
        json!({
            "call_id": "call-1",
            "status": "failed",
            "artifact": {
                "id": "tool-result-1",
                "kind": "json",
                "label": null
            },
            "diagnostic": {
                "code": "tool_failed",
                "message": "Tool exited with status 2"
            }
        })
    );
    assert_json_round_trip(&failed);

    assert!(
        ToolCallResult::new(
            call_id.clone(),
            ToolCallResultStatus::Succeeded,
            artifact.clone(),
            Some(diagnostic.clone())
        )
        .is_err()
    );
    assert!(ToolCallResult::new(call_id, ToolCallResultStatus::Failed, artifact, None).is_err());
    assert!(
        serde_json::from_value::<ToolCallResult>(json!({
            "call_id": "call-1",
            "status": "succeeded",
            "artifact": {
                "id": "tool-result-1",
                "kind": "json",
                "label": null
            },
            "diagnostic": {
                "code": "unexpected",
                "message": "success must not carry diagnostics"
            }
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ToolCallResult>(json!({
            "call_id": "call-1",
            "status": "failed",
            "artifact": {
                "id": "tool-result-1",
                "kind": "json",
                "label": null
            },
            "diagnostic": null
        }))
        .is_err()
    );
}

#[test]
fn tool_call_resolved_event_uses_snake_case_and_no_inline_payload() {
    let result = ToolCallResult::succeeded(
        ToolCallId::new("call-1").expect("valid call id"),
        ArtifactRef::new(
            ArtifactId::new("tool-result-1").expect("valid artifact id"),
            ArtifactKind::Text,
        ),
    );
    let event = RuntimeJournalEvent::new(
        SessionId::new("session-1").expect("valid session id"),
        10,
        RuntimeJournalPayload::ToolCallResolved { result },
    );

    assert_eq!(
        serde_json::to_value(&event).expect("event serializes"),
        json!({
            "session_id": "session-1",
            "sequence": 10,
            "payload": {
                "type": "tool_call_resolved",
                "result": {
                    "call_id": "call-1",
                    "status": "succeeded",
                    "artifact": {
                        "id": "tool-result-1",
                        "kind": "text",
                        "label": null
                    },
                    "diagnostic": null
                }
            }
        })
    );
    assert_json_round_trip(&event);
}

#[test]
fn public_tool_call_started_does_not_expose_bridge_runner() {
    let call = PendingToolCall::new(
        ToolCallId::new("call-bridge").expect("valid call id"),
        ToolName::new("python_tool").expect("valid tool name"),
        ToolCallArguments::try_from(json!({ "value": 1 })).expect("valid arguments"),
    );
    let event = RuntimeEvent::ToolCallStarted {
        call,
        source: RuntimeEventSource::new(SessionId::new("session-1").expect("valid session id"), 5),
    };
    let value = serde_json::to_value(&event).expect("event serializes");

    assert_eq!(value["type"], json!("tool_call_started"));
    assert_eq!(value["call"]["id"], json!("call-bridge"));
    assert!(value.get("runner").is_none());
    assert!(value.get("bridge").is_none());
    assert_json_round_trip(&event);
}

#[test]
fn public_tool_call_finished_carries_complete_text_output() {
    let result = ToolCallResult::succeeded(
        ToolCallId::new("call-1").expect("valid call id"),
        ArtifactRef::new(
            ArtifactId::new("tool-result-1").expect("valid artifact id"),
            ArtifactKind::Text,
        ),
    );
    let event = RuntimeEvent::ToolCallFinished {
        result,
        output: Some(ToolOutput::Text {
            text: "complete tool output".to_owned(),
        }),
        source: RuntimeEventSource::new(SessionId::new("session-1").expect("valid session id"), 6),
    };
    let value = serde_json::to_value(&event).expect("event serializes");

    assert_eq!(value["type"], json!("tool_call_finished"));
    assert_eq!(value["output"]["kind"], json!("text"));
    assert_eq!(value["output"]["text"], json!("complete tool output"));
    assert!(value.get("output_preview").is_none());
    assert!(value["output"].get("truncated").is_none());
    assert_json_round_trip(&event);
}
