//! Provider-neutral evaluation manifests and deterministic run records.
//!
//! This crate is an upper-layer protocol. It describes what a harness should
//! evaluate and records bounded metadata about the result; it does not own
//! runtime state, provider wire payloads, secrets, or artifact contents.

mod error;
mod manifest;
mod numeric;
mod record;

pub use error::EvalError;
pub use manifest::{
    ArtifactKind, CommandSpec, ExpectedArtifact, RepositorySpec, ResourceLimits, RiskPolicy,
    SuccessCriterion, TASK_SCHEMA_VERSION, TaskSpec,
};
pub use record::{
    ArtifactRecord, DenialRecord, EVALUATION_RECORD_SCHEMA_VERSION, EVALUATION_RUN_SCHEMA_VERSION,
    EvaluationRecord, EvaluationRun, EvaluationStatus, FailureKind, TestResult,
};

#[cfg(test)]
mod tests {
    use super::{
        ArtifactKind, ArtifactRecord, CommandSpec, DenialRecord, EvaluationRecord, EvaluationRun,
        EvaluationStatus, ExpectedArtifact, FailureKind, RepositorySpec, ResourceLimits,
        SuccessCriterion, TaskSpec, TestResult,
    };
    use schemars::schema_for;
    use serde_json::{Value, json};

    const VALID_MANIFEST: &str = r#"
        schema_version = 1
        task_id = "rust-fix-001"
        task_version = "2026-08-17"
        description = "Fix the parser regression."
        write_scope = ["src/**"]
        timeout_seconds = 300

        [repository]
        path = "fixtures/parser"

        [[success_criteria]]
        kind = "file_exists"
        path = "src/parser.rs"
    "#;

    const NUMERIC_MANIFEST: &str = r#"
        schema_version = 1.0
        task_id = "numeric-001"
        task_version = "v1"
        description = "Exercise numeric deserializers."
        write_scope = ["src/**"]
        timeout_seconds = 300.0

        [repository]
        path = "fixtures/parser"

        [[setup]]
        program = "cargo"
        timeout_seconds = 5.0

        [[tests]]
        program = "cargo"
        timeout_seconds = 6.0

        [resource_limits]
        max_output_bytes = 1024.0
        max_file_changes = 2.0
        max_processes = 1.0

        [[success_criteria]]
        kind = "command_passes"
        program = "cargo"
        timeout_seconds = 7.0
    "#;

    #[test]
    fn parses_a_versioned_task_manifest() {
        let task = TaskSpec::from_toml(VALID_MANIFEST).expect("valid task manifest");
        assert_eq!(task.task_id(), "rust-fix-001");
        assert_eq!(task.schema_version(), 1);
        assert_eq!(task.write_scope(), ["src/**"]);
        assert_eq!(task.success_criteria().len(), 1);
    }

    #[test]
    fn toml_numeric_wire_fields_accept_integral_floats() {
        let task = TaskSpec::from_toml(NUMERIC_MANIFEST).expect("integral TOML numbers parse");
        assert_eq!(task.schema_version(), 1);
        assert_eq!(task.timeout_seconds(), 300);
        assert_eq!(task.setup()[0].timeout_seconds(), Some(5));
        assert_eq!(task.tests()[0].timeout_seconds(), Some(6));
        assert_eq!(task.resource_limits().max_output_bytes(), Some(1024));
        assert_eq!(task.resource_limits().max_file_changes(), Some(2));
        assert_eq!(task.resource_limits().max_processes(), Some(1));
        assert!(matches!(
            task.success_criteria()[0],
            SuccessCriterion::CommandPasses {
                timeout_seconds: Some(7),
                ..
            }
        ));
    }

    #[test]
    fn toml_numeric_wire_fields_reject_fractional_floats() {
        for (needle, replacement) in [
            ("schema_version = 1.0", "schema_version = 1.5"),
            ("timeout_seconds = 300.0", "timeout_seconds = 300.5"),
            ("max_output_bytes = 1024.0", "max_output_bytes = 1024.5"),
            ("max_file_changes = 2.0", "max_file_changes = 2.5"),
            ("max_processes = 1.0", "max_processes = 1.5"),
            ("timeout_seconds = 5.0", "timeout_seconds = 5.5"),
            ("timeout_seconds = 6.0", "timeout_seconds = 6.5"),
            ("timeout_seconds = 7.0", "timeout_seconds = 7.5"),
        ] {
            let manifest = NUMERIC_MANIFEST.replace(needle, replacement);
            assert!(TaskSpec::from_toml(&manifest).is_err(), "field: {needle}");
        }

        let object_number = NUMERIC_MANIFEST.replace(
            "timeout_seconds = 300.0",
            "timeout_seconds = { anything = \"5\" }",
        );
        assert!(TaskSpec::from_toml(&object_number).is_err());
    }

    #[test]
    fn reports_expected_artifact_index_in_manifest_errors() {
        let manifest = format!(
            "{VALID_MANIFEST}\n\
             [[expected_artifacts]]\n\
             path = \"out.json\"\n\
             kind = \"file\"\n\
             [[expected_artifacts]]\n\
             path = \"../outside\"\n\
             kind = \"file\"\n"
        );
        let error = TaskSpec::from_toml(&manifest).expect_err("invalid artifact path must fail");
        assert!(error.to_string().contains("expected_artifacts[1].path"));
    }

    #[test]
    fn rejects_unknown_manifest_fields() {
        let manifest = format!("{VALID_MANIFEST}\nunknown = true\n");
        let error = TaskSpec::from_toml(&manifest).expect_err("unknown fields must be rejected");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_unsupported_manifest_versions() {
        let manifest = VALID_MANIFEST.replace("schema_version = 1", "schema_version = 2");
        let error = TaskSpec::from_toml(&manifest).expect_err("unsupported versions must fail");
        assert!(
            error
                .to_string()
                .contains("unsupported task manifest version 2")
        );
    }

    #[test]
    fn rejects_workspace_escape_in_write_scope() {
        let manifest = VALID_MANIFEST.replace(
            "write_scope = [\"src/**\"]",
            "write_scope = [\"../outside\"]",
        );
        let error = TaskSpec::from_toml(&manifest).expect_err("workspace escapes must fail");
        assert!(
            error
                .to_string()
                .contains("must not escape the task workspace")
        );
    }

    #[test]
    fn task_manifest_round_trips_without_losing_contract_fields() {
        let task = TaskSpec::from_toml(VALID_MANIFEST).expect("valid task manifest");
        let encoded = task.to_toml().expect("task serializes");
        let decoded = TaskSpec::from_toml(&encoded).expect("serialized task parses");
        assert_eq!(decoded, task);
    }

    #[test]
    fn direct_nested_manifest_deserialization_keeps_validation_boundary() {
        let empty_repository = serde_json::from_str::<super::RepositorySpec>(r#"{}"#);
        assert!(empty_repository.is_err());
        let ambiguous_repository = serde_json::from_str::<super::RepositorySpec>(
            r#"{"path":"fixture","image":"image:latest","commit":null}"#,
        );
        assert!(ambiguous_repository.is_err());

        let invalid_command = serde_json::from_str::<super::CommandSpec>(
            r#"{"program":"","args":[],"working_dir":null,"timeout_seconds":0}"#,
        );
        assert!(invalid_command.is_err());

        let invalid_limits = serde_json::from_str::<super::ResourceLimits>(
            r#"{"max_output_bytes":0,"max_file_changes":null,"max_processes":null}"#,
        );
        assert!(invalid_limits.is_err());

        let invalid_criterion = serde_json::from_str::<super::SuccessCriterion>(
            r#"{"kind":"file_exists","path":"../outside"}"#,
        );
        assert!(invalid_criterion.is_err());
        let unknown_criterion_field = serde_json::from_str::<super::SuccessCriterion>(
            r#"{"kind":"file_exists","path":"src/lib.rs","extra":true}"#,
        );
        assert!(unknown_criterion_field.is_err());

        let invalid_artifact = serde_json::from_str::<super::ExpectedArtifact>(
            r#"{"path":"out.json","kind":"json","sha256":"bad"}"#,
        );
        assert!(invalid_artifact.is_err());
    }

    #[test]
    fn public_manifest_constructors_validate_and_round_trip() {
        let repository = RepositorySpec::from_path("fixtures/parser")
            .expect("valid repository path")
            .with_commit("abc123")
            .expect("valid repository commit");
        assert_eq!(repository.path(), Some("fixtures/parser"));
        assert_eq!(repository.commit(), Some("abc123"));

        let command = CommandSpec::new("cargo", &["test"])
            .expect("valid command")
            .with_working_dir("crates/merry-eval")
            .expect("valid command directory")
            .with_timeout_seconds(60)
            .expect("valid command timeout");
        assert_eq!(command.program(), "cargo");
        assert_eq!(command.timeout_seconds(), Some(60));

        let limits = ResourceLimits::new(Some(1024), Some(2), Some(1)).expect("valid limits");
        assert_eq!(limits.max_file_changes(), Some(2));

        let criterion = SuccessCriterion::FileExists {
            path: "src/lib.rs".to_owned(),
        };
        assert!(criterion.validate().is_ok());
        let artifact = ExpectedArtifact::new("target/report.json", ArtifactKind::Json, None)
            .expect("valid expected artifact");
        assert_eq!(artifact.kind(), ArtifactKind::Json);

        let criterion_json = serde_json::to_string(&criterion).expect("criterion serializes");
        let decoded_criterion: SuccessCriterion =
            serde_json::from_str(&criterion_json).expect("criterion parses");
        assert_eq!(decoded_criterion, criterion);
    }

    #[test]
    fn generated_task_schema_preserves_manifest_bounds() {
        let schema = serde_json::to_value(schema_for!(TaskSpec)).expect("schema serializes");
        assert_eq!(schema["properties"]["task_id"]["minLength"], json!(1));
        assert_eq!(schema["properties"]["write_scope"]["minItems"], json!(1));
        assert_eq!(
            schema["properties"]["success_criteria"]["minItems"],
            json!(1)
        );
        assert_eq!(schema["properties"]["timeout_seconds"]["minimum"], json!(1));
        assert_eq!(
            schema["properties"]["timeout_seconds"]["maximum"],
            json!(604800)
        );
        assert!(!schema["required"].as_array().unwrap().iter().any(|field| {
            matches!(
                field.as_str(),
                Some("setup" | "tests" | "resource_limits" | "risk_policy" | "expected_artifacts")
            )
        }));

        let validator = jsonschema::validator_for(&schema).expect("TaskSpec schema compiles");
        let valid: Value = serde_json::from_str(
            r#"{
                "schema_version": 1,
                "task_id": "task-1",
                "task_version": "v1",
                "description": "Read and fix the fixture.",
                "repository": {"path": "fixtures/ab:c"},
                "write_scope": ["src/**"],
                "timeout_seconds": 300,
                "success_criteria": [{"kind": "file_exists", "path": "src/lib.rs"}]
            }"#,
        )
        .expect("valid schema fixture parses");
        assert!(validator.is_valid(&valid));

        let mut integral_timeout = valid.clone();
        integral_timeout["timeout_seconds"] = json!(300.0);
        assert!(validator.is_valid(&integral_timeout));
        assert!(serde_json::from_value::<TaskSpec>(integral_timeout).is_ok());

        let mut drive_like_path = valid.clone();
        drive_like_path["repository"] = json!({"path": "fixtures/a:b"});
        assert!(!validator.is_valid(&drive_like_path));

        let mut whitespace_identifier = valid.clone();
        whitespace_identifier["task_id"] = json!(" task-1");
        assert!(!validator.is_valid(&whitespace_identifier));

        let mut unknown_criterion_field = valid.clone();
        unknown_criterion_field["success_criteria"][0]["extra"] = json!(true);
        assert!(!validator.is_valid(&unknown_criterion_field));

        let mut blank_command_program = valid.clone();
        blank_command_program["success_criteria"] = json!([{
            "kind": "command_passes",
            "program": " ",
            "args": [],
            "working_dir": null,
            "timeout_seconds": null
        }]);
        assert!(!validator.is_valid(&blank_command_program));
        assert!(serde_json::from_value::<TaskSpec>(blank_command_program).is_err());

        let invalid: Value = serde_json::from_str(
            r#"{
                "schema_version": 1,
                "task_id": "task-1",
                "task_version": "v1",
                "description": "Read and fix the fixture.",
                "repository": {},
                "write_scope": ["../outside"],
                "timeout_seconds": 0,
                "success_criteria": [{"kind": "file_exists", "path": "src/lib.rs"}]
            }"#,
        )
        .expect("invalid schema fixture parses");
        assert!(!validator.is_valid(&invalid));
    }

    #[test]
    fn generated_record_schema_preserves_nested_bounds() {
        let schema =
            serde_json::to_value(schema_for!(EvaluationRecord)).expect("schema serializes");
        let base = &schema["allOf"][0];
        assert_eq!(base["properties"]["schema_version"]["const"], json!(1));
        assert_eq!(
            schema["$defs"]["DenialRecord"]["properties"]["count"]["minimum"],
            json!(1)
        );
        assert_eq!(
            schema["$defs"]["ArtifactRecord"]["properties"]["sha256"]["pattern"],
            json!("^[0-9A-Fa-f]{64}$")
        );
        assert_eq!(base["properties"]["tests"]["maxItems"], json!(1024));
        assert_eq!(base["properties"]["turns"]["maximum"], json!(u32::MAX));
        assert_eq!(base["properties"]["latency_ms"]["maximum"], json!(u64::MAX));
        assert_eq!(
            schema["$defs"]["EvaluationRun"]["properties"]["started_at_ms"]["maximum"],
            json!(u64::MAX)
        );
        assert_eq!(
            schema["$defs"]["TestResult"]["properties"]["duration_ms"]["maximum"],
            json!(u64::MAX)
        );
        assert_eq!(
            schema["$defs"]["DenialRecord"]["properties"]["count"]["maximum"],
            json!(u32::MAX)
        );

        let validator = jsonschema::validator_for(&schema).expect("record schema compiles");
        let run = EvaluationRun::new("run-001", "task-001", "v1", 1000).expect("valid run");
        for (status, failure_kind) in [
            (EvaluationStatus::Passed, None),
            (EvaluationStatus::Failed, Some(FailureKind::Test)),
            (EvaluationStatus::Blocked, Some(FailureKind::Permission)),
            (EvaluationStatus::Cancelled, Some(FailureKind::Cancelled)),
            (EvaluationStatus::Error, Some(FailureKind::Runtime)),
            (EvaluationStatus::Error, Some(FailureKind::Timeout)),
        ] {
            let record = EvaluationRecord::try_new(run.clone(), status, failure_kind)
                .expect("valid status combination");
            let value = serde_json::to_value(record).expect("valid record serializes");
            assert!(
                validator.is_valid(&value),
                "schema rejected valid record: {value}"
            );
        }

        let valid = serde_json::to_value(EvaluationRecord::new(run, EvaluationStatus::Passed))
            .expect("valid record serializes");
        assert!(validator.is_valid(&valid));

        let mut missing_failure = valid.clone();
        missing_failure["status"] = json!("failed");
        assert!(!validator.is_valid(&missing_failure));

        let mut blocked_with_test_failure = valid.clone();
        blocked_with_test_failure["status"] = json!("blocked");
        blocked_with_test_failure["failure_kind"] = json!("test");
        assert!(!validator.is_valid(&blocked_with_test_failure));

        let mut unknown_failure_kind = valid.clone();
        unknown_failure_kind["status"] = json!("failed");
        unknown_failure_kind["failure_kind"] = json!("future_kind");
        assert!(!validator.is_valid(&unknown_failure_kind));

        let mut unknown_test_field = valid.clone();
        unknown_test_field["tests"] = json!([{
            "name": "unit",
            "passed": true,
            "duration_ms": null,
            "extra": true
        }]);
        assert!(!validator.is_valid(&unknown_test_field));

        let mut passed_with_failed_test = valid;
        passed_with_failed_test["tests"] = json!([{
            "name": "unit",
            "passed": false,
            "duration_ms": null
        }]);
        assert!(!validator.is_valid(&passed_with_failed_test));
    }

    #[test]
    fn schema_integer_numbers_deserialize_through_record_wire() {
        let run = EvaluationRun::new("run-001", "task-001", "v1", 1000).expect("valid run");
        let record = EvaluationRecord::new(run, EvaluationStatus::Passed);
        let mut value: Value = serde_json::to_value(record).expect("record encodes");
        value["turns"] = json!(1.0);
        value["tool_calls"] = json!(1e2);
        value["retries"] = json!(0.0);
        value["latency_ms"] = json!(1.0);
        value["cost_micros"] = json!(2e2);

        let schema =
            serde_json::to_value(schema_for!(EvaluationRecord)).expect("schema serializes");
        let validator = jsonschema::validator_for(&schema).expect("record schema compiles");
        assert!(
            validator.is_valid(&value),
            "schema errors: {:?}",
            validator.iter_errors(&value).collect::<Vec<_>>()
        );
        let decoded =
            serde_json::from_value::<EvaluationRecord>(value).expect("integral numbers parse");
        assert_eq!(decoded.turns(), 1);
        assert_eq!(decoded.tool_calls(), 100);
        assert_eq!(decoded.retries(), 0);
        assert_eq!(decoded.latency_ms(), Some(1));
        assert_eq!(decoded.cost_micros(), Some(200));

        let mut fractional: Value = serde_json::to_value(EvaluationRecord::new(
            EvaluationRun::new("run-002", "task-001", "v1", 1000).expect("valid run"),
            EvaluationStatus::Passed,
        ))
        .expect("record encodes");
        fractional["turns"] = json!(1.5);
        assert!(!validator.is_valid(&fractional));
        assert!(serde_json::from_value::<EvaluationRecord>(fractional).is_err());

        let mut negative = serde_json::to_value(EvaluationRecord::new(
            EvaluationRun::new("run-003", "task-001", "v1", 1000).expect("valid run"),
            EvaluationStatus::Passed,
        ))
        .expect("record encodes");
        negative["turns"] = json!(-1);
        assert!(!validator.is_valid(&negative));
        assert!(serde_json::from_value::<EvaluationRecord>(negative).is_err());

        let mut overflow = serde_json::to_value(EvaluationRecord::new(
            EvaluationRun::new("run-004", "task-001", "v1", 1000).expect("valid run"),
            EvaluationStatus::Passed,
        ))
        .expect("record encodes");
        overflow["turns"] = json!(u64::from(u32::MAX) + 1);
        assert!(!validator.is_valid(&overflow));
        assert!(serde_json::from_value::<EvaluationRecord>(overflow).is_err());
    }

    #[test]
    fn json_integral_float_preserves_large_integer_digits() {
        let run = EvaluationRun::new("run-001", "task-001", "v1", 1000).expect("valid run");
        let record = EvaluationRecord::new(run, EvaluationStatus::Passed);
        let line = record
            .to_jsonl()
            .expect("record serializes")
            .replace("\"latency_ms\":null", "\"latency_ms\":9007199254740993.0");
        let payload: Value = serde_json::from_str(line.trim_end()).expect("payload parses");
        let schema =
            serde_json::to_value(schema_for!(EvaluationRecord)).expect("schema serializes");
        let validator = jsonschema::validator_for(&schema).expect("record schema compiles");
        assert!(validator.is_valid(&payload));
        let decoded = EvaluationRecord::from_jsonl(&line).expect("large integral float parses");
        assert_eq!(decoded.latency_ms(), Some(9_007_199_254_740_993));

        let fractional = line.replace("9007199254740993.0", "9007199254740993.5");
        let fractional_payload: Value =
            serde_json::from_str(fractional.trim_end()).expect("payload parses");
        // The jsonschema crate currently rounds very large JSON numbers while validating;
        // the protocol deserializer retains the raw number and rejects the fraction.
        assert!(fractional_payload["latency_ms"].is_number());
        assert!(EvaluationRecord::from_jsonl(&fractional).is_err());

        let object_number = line.replace(
            "\"latency_ms\":9007199254740993.0",
            "\"latency_ms\":{\"anything\":\"5\"}",
        );
        assert!(EvaluationRecord::from_jsonl(&object_number).is_err());
    }

    #[test]
    fn direct_record_deserialization_keeps_validation_boundary() {
        let run = EvaluationRun::new("run-001", "task-001", "v1", 1000).expect("valid run");
        let record = EvaluationRecord::new(run, EvaluationStatus::Passed);
        let mut value: Value = serde_json::to_value(record).expect("record encodes");
        value["failure_kind"] = json!("test");
        let error = serde_json::from_value::<EvaluationRecord>(value)
            .expect_err("passed record with failure must fail");
        assert!(error.to_string().contains("must be absent"));
    }

    #[test]
    fn evaluation_record_is_one_stable_jsonl_line() {
        let run = EvaluationRun::new("run-001", "rust-fix-001", "2026-08-17", 1000)
            .expect("valid run")
            .with_provider("fixture")
            .expect("valid provider")
            .with_model("fake-model")
            .expect("valid model")
            .with_prompt_hash("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .expect("valid prompt hash");
        let record = EvaluationRecord::new(run, EvaluationStatus::Failed)
            .with_failure_kind(FailureKind::Test)
            .with_metrics(2, 3, 1, Some(42), Some(7))
            .with_test(TestResult::new("cargo test", false, Some(40)).expect("valid test"))
            .with_denial(DenialRecord::new("network", 1).expect("valid denial"))
            .with_artifact(
                ArtifactRecord::new(
                    "target/report.json",
                    super::ArtifactKind::Json,
                    Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
                )
                .expect("valid artifact"),
            );

        let line = record.to_jsonl().expect("record serializes");
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        let object: Value = serde_json::from_str(&line).expect("JSONL line is an object");
        assert!(object["run"]["prompt_hash"].is_string());
        assert!(!object.to_string().contains("api_key"));
        assert_eq!(
            EvaluationRecord::from_jsonl(&line).expect("record parses"),
            record
        );
    }

    #[test]
    fn jsonl_escapes_unicode_line_separator_characters() {
        let run = EvaluationRun::new("run-001", "task-001", "v1", 1000)
            .expect("valid run")
            .with_model("model\u{2028}\u{2029}name")
            .expect("line separators are valid inside metadata");
        let record = EvaluationRecord::new(run, EvaluationStatus::Passed);
        let line = record.to_jsonl().expect("record serializes");
        assert!(!line.contains('\u{2028}'));
        assert!(!line.contains('\u{2029}'));
        assert_eq!(
            EvaluationRecord::from_jsonl(&line).expect("escaped record parses"),
            record
        );
    }

    #[test]
    fn rejects_incomplete_record_statuses() {
        let run = EvaluationRun::new("run-001", "task-001", "v1", 1000).expect("valid run");
        let failed = EvaluationRecord::new(run.clone(), EvaluationStatus::Failed);
        assert!(failed.finish().is_err());
        let blocked = EvaluationRecord::new(run.clone(), EvaluationStatus::Blocked)
            .with_failure_kind(FailureKind::Test);
        assert!(blocked.finish().is_err());
        let cancelled = EvaluationRecord::new(run.clone(), EvaluationStatus::Cancelled)
            .with_failure_kind(FailureKind::Cancelled);
        assert!(cancelled.finish().is_ok());
        let passed = EvaluationRecord::new(run, EvaluationStatus::Passed)
            .with_test(TestResult::new("cargo test", false, None).expect("valid test"));
        assert!(passed.finish().is_err());
    }

    #[test]
    fn rejects_extra_jsonl_framing() {
        let run = EvaluationRun::new("run-001", "task-001", "v1", 1000).expect("valid run");
        let record = EvaluationRecord::new(run, EvaluationStatus::Passed);
        let line = record.to_jsonl().expect("record serializes");
        assert!(EvaluationRecord::from_jsonl(&format!("{line}\n")).is_err());
        assert!(EvaluationRecord::from_jsonl(&format!(" {line}")).is_err());
        assert!(EvaluationRecord::from_jsonl(line.trim_end()).is_ok());
    }

    #[test]
    fn rejects_invalid_nested_record_values_through_serde() {
        let run = EvaluationRun::new("run-001", "task-001", "v1", 1000).expect("valid run");
        let record = EvaluationRecord::new(run, EvaluationStatus::Passed);
        let mut value: Value = serde_json::to_value(record).expect("record encodes");
        value["denials"] = json!([{ "code": "network", "count": 0 }]);
        assert!(serde_json::from_value::<EvaluationRecord>(value).is_err());

        let run = EvaluationRun::new("run-002", "task-001", "v1", 1000).expect("valid run");
        let record = EvaluationRecord::new(run, EvaluationStatus::Passed);
        let mut value: Value = serde_json::to_value(record).expect("record encodes");
        value["tests"] = json!([{ "name": "", "passed": true, "duration_ms": null }]);
        assert!(serde_json::from_value::<EvaluationRecord>(value).is_err());
    }

    #[test]
    fn rejects_unknown_record_fields_and_unsupported_versions() {
        let run = EvaluationRun::new("run-001", "task-001", "v1", 1000).expect("valid run");
        let record = EvaluationRecord::new(run, EvaluationStatus::Passed);
        let encoded = serde_json::to_string(&record).expect("record encodes");
        let with_unknown = format!(
            r#"{{"unknown":true,{encoded_trimmed}}}"#,
            encoded_trimmed = &encoded[1..]
        );
        let error =
            EvaluationRecord::from_jsonl(&with_unknown).expect_err("unknown field must fail");
        assert!(error.to_string().contains("unknown field"));

        let mut unsupported_value: Value =
            serde_json::from_str(&encoded).expect("record JSON object");
        unsupported_value["schema_version"] = json!(2);
        let unsupported = serde_json::to_string(&unsupported_value).expect("record JSON");
        let error = EvaluationRecord::from_jsonl(&unsupported)
            .expect_err("unsupported record version must fail");
        assert!(
            error
                .to_string()
                .contains("unsupported evaluation record version 2")
        );
    }
}
