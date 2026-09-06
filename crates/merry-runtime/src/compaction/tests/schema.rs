use super::{evidence, test_window};
use crate::compaction::{
    CitationCompactionInput, CitationCompactionInputParts, CitationCompactionInputPolicy,
    CitationCompactionPolicy, CitationCompactionWindowBundle,
};
use crate::{
    checkpoint::{
        CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest,
        CheckpointSequenceRange, CheckpointSourceKind,
    },
    compaction::{compile_citation_compaction_model_request, schema},
};
use merry_llm::ModelName;

#[test]
fn compaction_schema_has_exact_eight_sections_and_handoffs() {
    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 2).expect("valid policy");
    let checkpoint_id = CheckpointId::new("checkpoint-1").expect("valid checkpoint id");
    let manifest = CheckpointRefManifest::new(
        checkpoint_id,
        vec![CheckpointRef::new(
            CheckpointRefId::new("r1").expect("valid ref id"),
            CheckpointSourceKind::UserMessage,
            CheckpointSequenceRange::new(1, 1).expect("valid range"),
            evidence("user-message-1"),
        )],
    )
    .expect("valid manifest");
    let (window, plan) = test_window(1, "r1", "Need strict checkpoint JSON.");
    let input = CitationCompactionInput::new(
        CitationCompactionInputParts {
            input_policy: CitationCompactionInputPolicy::new(
                policy,
                policy.resolve(64_000).expect("test budget resolves"),
            ),
            task_anchor_snapshot: None,
            manifest,
            previous_checkpoint: None,
            previous_checkpoint_snapshot: None,
        },
        CitationCompactionWindowBundle {
            covered_history_ids: [1].into_iter().collect(),
            window,
            window_plan: plan,
            archived_refs: Vec::new(),
        },
    );

    let request = compile_citation_compaction_model_request(
        &input,
        &ModelName::new("compaction-model").expect("valid model"),
    )
    .expect("compaction request compiles");
    let format = request
        .response_format()
        .expect("compaction must request structured output");
    let json = serde_json::to_value(format).expect("format serializes");

    assert_eq!(json["type"], "structured_output");
    assert_eq!(json["name"], "compacted_checkpoint_candidate");
    assert_eq!(json["strict"], true);
    let schema = &json["schema"];
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["required"],
        serde_json::json!([
            "confirmed_decisions",
            "rejected_approaches",
            "constraints_preferences_boundaries",
            "corrected_misunderstandings",
            "durable_conclusions",
            "open_questions",
            "current_progress_and_next_steps",
            "exact_details",
            "handoffs"
        ])
    );
    assert_eq!(
        schema["properties"]["handoffs"]["items"]["properties"]["action"]["enum"],
        serde_json::json!(["keep", "replace"])
    );
    assert_eq!(
        schema["$defs"]["CheckpointEntryWire"]["properties"]["refs"]["minItems"],
        serde_json::json!(1)
    );
    assert_eq!(
        schema["$defs"]["CheckpointEntryWire"]["properties"]["refs"]["items"]["enum"],
        serde_json::json!(["r1"])
    );

    fn schema_allows_null(value: &serde_json::Value) -> bool {
        value.get("type").is_some_and(|type_value| {
            type_value.as_str() == Some("null")
                || type_value
                    .as_array()
                    .is_some_and(|types| types.iter().any(|item| item.as_str() == Some("null")))
        }) || value
            .get("anyOf")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|schemas| schemas.iter().any(schema_allows_null))
    }

    fn assert_strict_objects(value: &serde_json::Value, path: &str, saw_rationale: &mut bool) {
        match value {
            serde_json::Value::Object(object) => {
                if object.get("type") == Some(&serde_json::Value::String("object".to_owned())) {
                    assert_eq!(
                        object.get("additionalProperties"),
                        Some(&serde_json::Value::Bool(false)),
                        "object schema at {path} must reject unknown fields"
                    );
                    let properties = object
                        .get("properties")
                        .and_then(serde_json::Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let required = object
                        .get("required")
                        .and_then(serde_json::Value::as_array)
                        .expect("strict object schemas must declare required");
                    assert_eq!(
                        required.len(),
                        properties.len(),
                        "object schema at {path} must require exactly its properties"
                    );
                    for property in properties.keys() {
                        assert!(
                            required.iter().any(|item| item.as_str() == Some(property)),
                            "object schema at {path} must require {property}"
                        );
                        if property == "rationale" {
                            *saw_rationale = true;
                            assert!(
                                schema_allows_null(&properties[property]),
                                "rationale at {path} must remain nullable"
                            );
                        }
                    }
                }
                for (key, child) in object {
                    assert_strict_objects(child, &format!("{path}.{key}"), saw_rationale);
                }
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    assert_strict_objects(child, &format!("{path}[{index}]"), saw_rationale);
                }
            }
            _ => {}
        }
    }
    let mut saw_rationale = false;
    assert_strict_objects(schema, "schema", &mut saw_rationale);
    assert!(
        saw_rationale,
        "checkpoint entry schema must include rationale"
    );

    fn assert_no_one_of(value: &serde_json::Value, path: &str) {
        match value {
            serde_json::Value::Object(object) => {
                assert!(
                    !object.contains_key("oneOf"),
                    "schema at {path} must not use oneOf"
                );
                for (key, child) in object {
                    assert_no_one_of(child, &format!("{path}.{key}"));
                }
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    assert_no_one_of(child, &format!("{path}[{index}]"));
                }
            }
            _ => {}
        }
    }
    assert_no_one_of(schema, "schema");
}

#[test]
fn compaction_schema_allows_only_exact_available_ref_ids() {
    let schema = schema::citation_compaction_response_schema_for_refs(&["h290", "h291"])
        .expect("citation ref schema compiles");
    let schema = serde_json::to_value(schema).expect("schema serializes");
    let allowed = &schema["$defs"]["CheckpointEntryWire"]["properties"]["refs"]["items"]["enum"];

    assert_eq!(allowed, &serde_json::json!(["h290", "h291"]));
    assert!(
        !allowed
            .as_array()
            .expect("allowed refs are an array")
            .iter()
            .any(|ref_id| ref_id == "h1506")
    );

    let validator = jsonschema::validator_for(&schema).expect("schema validates");
    let valid_candidate = serde_json::json!({
        "confirmed_decisions": [],
        "rejected_approaches": [],
        "constraints_preferences_boundaries": [],
        "corrected_misunderstandings": [],
        "durable_conclusions": [{
            "id": "entry-1",
            "text": "A cited conclusion.",
            "rationale": null,
            "refs": ["h290"]
        }],
        "open_questions": [],
        "current_progress_and_next_steps": [],
        "exact_details": [],
        "handoffs": []
    });
    assert!(validator.is_valid(&valid_candidate));

    let mut invalid_candidate = valid_candidate.clone();
    invalid_candidate["durable_conclusions"][0]["refs"] = serde_json::json!(["h1506"]);
    assert!(!validator.is_valid(&invalid_candidate));
}
