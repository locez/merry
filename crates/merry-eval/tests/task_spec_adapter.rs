use merry_eval::{ArtifactKind, SuccessCriterion, TaskSpec};

const ADAPTER_TASK_SPEC: &str = include_str!("fixtures/adapter-task.toml");

#[test]
fn local_suite_consumes_the_shared_task_spec_fixture() {
    let task = TaskSpec::from_toml(ADAPTER_TASK_SPEC).expect("shared fixture parses");

    assert_eq!(task.task_id(), "adapter-fixture");
    assert_eq!(task.repository().path(), Some("fixtures/adapter"));
    assert_eq!(task.write_scope(), ["src/**"]);
    assert_eq!(task.setup().len(), 1);
    assert_eq!(task.tests().len(), 1);
    assert_eq!(task.timeout_seconds(), 120);
    assert_eq!(task.success_criteria().len(), 2);
    assert!(matches!(
        &task.success_criteria()[0],
        SuccessCriterion::FileExists { path } if path == "src/lib.rs"
    ));
    assert!(matches!(
        &task.success_criteria()[1],
        SuccessCriterion::CommandPasses {
            program,
            working_dir: Some(working_dir),
            timeout_seconds: Some(90),
            ..
        } if program == "cargo" && working_dir == "fixtures/adapter"
    ));
    assert_eq!(task.expected_artifacts()[0].kind(), ArtifactKind::Json);
}
