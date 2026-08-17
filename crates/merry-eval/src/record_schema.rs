use super::{
    ArtifactRecord, DenialRecord, EVALUATION_RECORD_SCHEMA_VERSION, EvaluationRun,
    EvaluationStatus, FailureKind, MAX_RECORD_ITEMS, TestResult,
};
use schemars::{Schema, SchemaGenerator, json_schema};

pub(super) fn evaluation_record_schema(generator: &mut SchemaGenerator) -> Schema {
    let run_schema = generator.subschema_for::<EvaluationRun>();
    let status_schema = generator.subschema_for::<EvaluationStatus>();
    let failure_kind_schema = generator.subschema_for::<Option<FailureKind>>();
    let counter_schema = json_schema!({
        "type": "number",
        "minimum": 0,
        "maximum": u32::MAX,
        "multipleOf": 1,
    });
    let optional_metric_schema = json_schema!({
        "type": ["number", "null"],
        "minimum": 0,
        "maximum": u64::MAX,
        "multipleOf": 1,
    });
    let tests_schema = json_schema!({
        "type": "array",
        "maxItems": MAX_RECORD_ITEMS,
        "items": generator.subschema_for::<TestResult>(),
    });
    let denials_schema = json_schema!({
        "type": "array",
        "maxItems": MAX_RECORD_ITEMS,
        "items": generator.subschema_for::<DenialRecord>(),
    });
    let artifacts_schema = json_schema!({
        "type": "array",
        "maxItems": MAX_RECORD_ITEMS,
        "items": generator.subschema_for::<ArtifactRecord>(),
    });
    let base_schema = json_schema!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "schema_version",
            "run",
            "status",
            "turns",
            "tool_calls",
            "retries",
            "tests",
            "denials",
            "artifacts"
        ],
        "properties": {
            "schema_version": {
                "type": "number",
                "const": EVALUATION_RECORD_SCHEMA_VERSION,
                "multipleOf": 1,
            },
            "run": run_schema,
            "status": status_schema,
            "failure_kind": failure_kind_schema,
            "turns": counter_schema,
            "tool_calls": counter_schema,
            "retries": counter_schema,
            "latency_ms": optional_metric_schema,
            "cost_micros": optional_metric_schema,
            "tests": tests_schema,
            "denials": denials_schema,
            "artifacts": artifacts_schema,
        }
    });
    json_schema!({
        "allOf": [
            base_schema,
            {
                "oneOf": [
                    {
                        "properties": {
                            "status": {"const": "passed"},
                            "failure_kind": {"type": "null"},
                            "tests": {
                                "items": {
                                    "required": ["passed"],
                                    "properties": {"passed": {"const": true}}
                                }
                            }
                        }
                    },
                    {
                        "required": ["failure_kind"],
                        "properties": {
                            "status": {"const": "failed"},
                            "failure_kind": {"type": "string"}
                        }
                    },
                    {
                        "required": ["failure_kind"],
                        "properties": {
                            "status": {"const": "blocked"},
                            "failure_kind": {"const": "permission"}
                        }
                    },
                    {
                        "required": ["failure_kind"],
                        "properties": {
                            "status": {"const": "cancelled"},
                            "failure_kind": {"const": "cancelled"}
                        }
                    },
                    {
                        "required": ["failure_kind"],
                        "properties": {
                            "status": {"const": "error"},
                            "failure_kind": {"enum": ["runtime", "timeout"]}
                        }
                    }
                ]
            }
        ]
    })
}
