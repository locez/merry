use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, CompactionUsageWindow, ContextWindowSource, CoreError,
    ErrorInfo, EvidenceLocator, EvidenceRef, MerryErrorDomain, MerryErrorInfo, MerryRetryability,
    ModelUsage, PendingToolCall, PendingToolCallBatch, ProviderName, QueuedInputLane,
    QueuedInputView, QueuedInputsView, RuntimeEvent, RuntimeEventSource, RuntimeJournalEvent,
    RuntimeJournalPayload, SessionId, SessionUsage, SkillId, SubagentId, SubagentStatus,
    SubagentTaskId, ToolCallArguments, ToolCallBatchId, ToolCallId, ToolCallResult,
    ToolCallResultStatus, ToolInputSchema, ToolName, ToolOutput, ToolSpec, UsageContextWindow,
};
use schemars::{JsonSchema, Schema};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::str::FromStr;

fn assert_json_round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_string(value).expect("value should serialize");
    let decoded = serde_json::from_str::<T>(&encoded).expect("value should deserialize");
    assert_eq!(&decoded, value);
}

fn assert_schema_compiles<T: JsonSchema>() {
    let _schema = schemars::schema_for!(T);
}

fn json_schema(value: Value) -> Schema {
    Schema::try_from(value).expect("test schema should be JSON schema")
}

fn assert_uuid_v4_string(value: &str) {
    assert_eq!(value.len(), 36);
    for (index, byte) in value.bytes().enumerate() {
        match index {
            8 | 13 | 18 | 23 => assert_eq!(byte, b'-'),
            14 => assert_eq!(byte, b'4'),
            19 => assert!(
                matches!(byte, b'8' | b'9' | b'a' | b'b'),
                "uuid variant nibble should be RFC 4122"
            ),
            _ => assert!(byte.is_ascii_hexdigit()),
        }
    }
}

#[test]
fn ids_validate_and_round_trip_as_json_strings() {
    let session = SessionId::new("session-1").expect("valid session id");
    let uuid_session = SessionId::new("550e8400-e29b-41d4-a716-446655440000")
        .expect("uuid session id should be valid");
    let artifact = ArtifactId::from_str("artifact_1").expect("valid artifact id");
    let skill = SkillId::try_from("skill.alpha").expect("valid skill id");
    let provider =
        ProviderName::try_from(String::from("openai-compatible")).expect("valid provider name");

    assert_eq!(session.as_str(), "session-1");
    assert_eq!(
        uuid_session.as_str(),
        "550e8400-e29b-41d4-a716-446655440000"
    );
    assert_eq!(artifact.to_string(), "artifact_1");
    assert_eq!(skill.as_str(), "skill.alpha");
    assert_eq!(provider.as_str(), "openai-compatible");

    assert_eq!(
        serde_json::to_value(&session).expect("session serializes"),
        json!("session-1")
    );

    assert_json_round_trip(&session);
    assert_json_round_trip(&uuid_session);
    assert_json_round_trip(&artifact);
    assert_json_round_trip(&skill);
    assert_json_round_trip(&provider);

    for invalid in ["", "   ", " has-leading", "has-trailing ", "has\nnewline"] {
        assert!(
            SessionId::new(invalid).is_err(),
            "{invalid:?} should reject"
        );
        assert!(serde_json::from_value::<SessionId>(json!(invalid)).is_err());
        assert!(serde_json::from_value::<SkillId>(json!(invalid)).is_err());
        assert!(serde_json::from_value::<ProviderName>(json!(invalid)).is_err());
    }

    for invalid in [
        "bad/session",
        "bad\\session",
        "bad:session",
        "bad space",
        ".",
        "..",
    ] {
        assert!(
            SessionId::new(invalid).is_err(),
            "{invalid:?} should reject as a filesystem-safe session id"
        );
        assert!(serde_json::from_value::<SessionId>(json!(invalid)).is_err());
    }

    let overlong = "a".repeat(129);
    assert!(ArtifactId::new(&overlong).is_err());
    assert!(serde_json::from_value::<ArtifactId>(json!(overlong)).is_err());
}

#[test]
fn session_id_random_generates_distinct_uuid_v4_ids() {
    let first = SessionId::random();
    let second = SessionId::random();

    assert_ne!(first, second);
    assert_uuid_v4_string(first.as_str());
    assert_uuid_v4_string(second.as_str());
    assert_json_round_trip(&first);
    assert_json_round_trip(&second);
}

#[test]
fn subagent_ids_validate_and_round_trip_as_json_strings() {
    let agent = SubagentId::new("subagent-1").expect("valid subagent id");
    let task = SubagentTaskId::from_str("subagent-task_1").expect("valid subagent task id");

    assert_eq!(agent.as_str(), "subagent-1");
    assert_eq!(task.to_string(), "subagent-task_1");
    assert_eq!(
        serde_json::to_value(&agent).expect("subagent id serializes"),
        json!("subagent-1")
    );
    assert_eq!(
        serde_json::to_value(&task).expect("subagent task id serializes"),
        json!("subagent-task_1")
    );

    assert_json_round_trip(&agent);
    assert_json_round_trip(&task);

    for invalid in ["", "   ", " has-leading", "has-trailing ", "has\nnewline"] {
        assert!(
            SubagentId::new(invalid).is_err(),
            "{invalid:?} should reject"
        );
        assert!(
            serde_json::from_value::<SubagentTaskId>(json!(invalid)).is_err(),
            "{invalid:?} should reject during deserialize"
        );
    }

    let overlong = "a".repeat(129);
    assert!(SubagentId::new(&overlong).is_err());
    assert!(serde_json::from_value::<SubagentTaskId>(json!(overlong)).is_err());
}

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
fn merry_error_info_serializes_stable_sdk_shape() {
    let diagnostic = MerryErrorInfo::builder(
        "tool.executor_exception",
        MerryErrorDomain::Tool,
        "Tool `lookup_order` raised an unexpected exception.",
        MerryRetryability::NotRetryable,
    )
    .hint("Handle expected business failures inside the tool.")
    .context("tool_name", "lookup_order")
    .context("call_id", "call_123")
    .build()
    .expect("valid SDK error info");

    assert_eq!(
        serde_json::to_value(&diagnostic).expect("serializes"),
        json!({
            "code": "tool.executor_exception",
            "domain": "tool",
            "message": "Tool `lookup_order` raised an unexpected exception.",
            "hint": "Handle expected business failures inside the tool.",
            "retryability": "not_retryable",
            "context": {
                "call_id": "call_123",
                "tool_name": "lookup_order"
            }
        })
    );
}

#[test]
fn merry_error_info_rejects_unbounded_or_sensitive_context_keys() {
    let error = MerryErrorInfo::builder(
        "provider.stream_failed",
        MerryErrorDomain::Provider,
        "Provider stream failed.",
        MerryRetryability::Retryable,
    )
    .context("authorization", "Bearer secret")
    .build()
    .expect_err("authorization context must be rejected");

    assert!(error.to_string().contains("context key is not allowed"));
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
fn usage_protocol_round_trips_and_preserves_unknown_subcounts() {
    let last = ModelUsage::with_details(2400, Some(2200), 260, None, 2660);
    let total = ModelUsage::with_details(12000, Some(8000), 1400, None, 13400);
    let usage = SessionUsage {
        total,
        last,
        context: Some(UsageContextWindow {
            resolved_model_window_tokens: 128000,
            effective_window_tokens: 121600,
            source: ContextWindowSource::ProviderCapabilities,
        }),
        compaction: Some(CompactionUsageWindow {
            auto_compaction_enabled: true,
            body_budget_tokens: 90000,
            soft_water_tokens: 70000,
            hard_water_tokens: 82000,
        }),
    };

    assert_json_round_trip(&last);
    assert_json_round_trip(&usage);
    assert_eq!(last.cached_input_tokens, Some(2200));
    assert_eq!(last.reasoning_output_tokens, None);

    assert_eq!(
        serde_json::to_value(&usage).expect("usage serializes"),
        json!({
            "total": {
                "input_tokens": 12000,
                "cached_input_tokens": 8000,
                "output_tokens": 1400,
                "reasoning_output_tokens": null,
                "total_tokens": 13400
            },
            "last": {
                "input_tokens": 2400,
                "cached_input_tokens": 2200,
                "output_tokens": 260,
                "reasoning_output_tokens": null,
                "total_tokens": 2660
            },
            "context": {
                "resolved_model_window_tokens": 128000,
                "effective_window_tokens": 121600,
                "source": "provider_capabilities"
            },
            "compaction": {
                "auto_compaction_enabled": true,
                "body_budget_tokens": 90000,
                "soft_water_tokens": 70000,
                "hard_water_tokens": 82000
            }
        })
    );
}

#[test]
fn usage_updated_events_round_trip_as_full_snapshots() {
    let usage = SessionUsage {
        total: ModelUsage::new(10, 4),
        last: ModelUsage::new(10, 4),
        context: None,
        compaction: None,
    };
    let source = RuntimeEventSource::new(
        SessionId::new("usage-event-session").expect("valid session id"),
        7,
    );

    assert_json_round_trip(&RuntimeJournalPayload::SessionUsageUpdated {
        usage: usage.clone(),
    });
    assert_json_round_trip(&RuntimeEvent::UsageUpdated {
        usage: usage.clone(),
        source,
    });
}

#[test]
fn compaction_lifecycle_events_round_trip_as_low_noise_public_events() {
    let source = RuntimeEventSource::new(
        SessionId::new("compaction-event-session").expect("valid session id"),
        8,
    );

    assert_json_round_trip(&RuntimeJournalPayload::CompactionStarted);
    assert_json_round_trip(&RuntimeJournalPayload::CompactionCompleted {
        checkpoint_id: "checkpoint-session-8".to_owned(),
        covered_history_item_count: 6,
    });

    assert_eq!(
        serde_json::to_value(&RuntimeEvent::CompactionStarted {
            source: source.clone(),
        })
        .expect("event serializes"),
        json!({
            "type": "compaction_started",
            "source": {
                "session_id": "compaction-event-session",
                "sequence": 8
            }
        })
    );
    assert_eq!(
        serde_json::to_value(&RuntimeEvent::CompactionCompleted {
            checkpoint_id: "checkpoint-session-8".to_owned(),
            covered_history_item_count: 6,
            source,
        })
        .expect("event serializes"),
        json!({
            "type": "compaction_completed",
            "checkpoint_id": "checkpoint-session-8",
            "covered_history_item_count": 6,
            "source": {
                "session_id": "compaction-event-session",
                "sequence": 8
            }
        })
    );
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

#[test]
fn core_error_display_messages_include_actionable_context() {
    let id_error = SessionId::new("bad\nid").expect_err("control character should reject");
    assert!(matches!(id_error, CoreError::InvalidIdentifier { .. }));
    assert!(
        id_error
            .to_string()
            .contains("SessionId must not contain control characters")
    );

    let schema_error = ToolInputSchema::new(Schema::try_from(json!(true)).expect("boolean schema"))
        .expect_err("boolean schema should reject");
    assert!(matches!(schema_error, CoreError::InvalidSchema { .. }));
    assert!(
        schema_error
            .to_string()
            .contains("ToolInputSchema must be a JSON object")
    );

    let evidence_error =
        EvidenceLocator::line_range(9, 2).expect_err("descending line range should reject");
    assert!(matches!(
        evidence_error,
        CoreError::InvalidEvidenceLocator { .. }
    ));
    assert!(
        evidence_error
            .to_string()
            .contains("line range start must be less than or equal to end")
    );

    let tool_error = ToolSpec::new(
        ToolName::new("valid_tool").expect("valid name"),
        "",
        ToolInputSchema::new(json_schema(json!({}))).expect("valid schema"),
    )
    .expect_err("blank description should reject");
    assert!(matches!(tool_error, CoreError::InvalidToolSpec { .. }));
    assert!(
        tool_error
            .to_string()
            .contains("ToolSpec description must not be blank")
    );
}

#[test]
fn schemars_generation_compiles_for_public_protocol_types() {
    assert_schema_compiles::<SessionId>();
    assert_schema_compiles::<ArtifactId>();
    assert_schema_compiles::<ToolName>();
    assert_schema_compiles::<SkillId>();
    assert_schema_compiles::<ProviderName>();
    assert_schema_compiles::<ArtifactKind>();
    assert_schema_compiles::<ArtifactRef>();
    assert_schema_compiles::<EvidenceLocator>();
    assert_schema_compiles::<EvidenceRef>();
    assert_schema_compiles::<ToolCallId>();
    assert_schema_compiles::<ToolCallArguments>();
    assert_schema_compiles::<PendingToolCall>();
    assert_schema_compiles::<ToolCallResultStatus>();
    assert_schema_compiles::<ToolCallResult>();
    assert_schema_compiles::<ToolInputSchema>();
    assert_schema_compiles::<ToolSpec>();
    assert_schema_compiles::<ErrorInfo>();
    assert_schema_compiles::<RuntimeJournalEvent>();
    assert_schema_compiles::<RuntimeJournalPayload>();
    assert_schema_compiles::<RuntimeEvent>();
    assert_schema_compiles::<ModelUsage>();
    assert_schema_compiles::<SessionUsage>();
    assert_schema_compiles::<UsageContextWindow>();
    assert_schema_compiles::<CompactionUsageWindow>();
    assert_schema_compiles::<ContextWindowSource>();
}
