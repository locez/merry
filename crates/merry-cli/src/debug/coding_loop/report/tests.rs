use super::*;
use crate::config::{MerryConfig, XdgPaths};
use crate::debug::coding_loop::{
    assert_permission_network_smoke_result, build_scripted_permission_network_smoke_runtime,
    coding_loop_process_call,
};
use crate::runtime_config::automatic_compaction_config;
use crate::runtime_events::{collect_runtime_step_events, first_pending_tool_call};
use crate::testing::{FakeProcessRunner, FakeProcessRunnerStep, ScriptedProvider};
use merry_core::RuntimeJournalEvent;
use merry_llm::ModelName;
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, CheckpointId, CheckpointRef,
    CheckpointRefId, CheckpointRefManifest, CheckpointSequenceRange, CheckpointSourceKind,
    CheckpointValidationPolicy, CitationBackedCheckpoint, CompactedCheckpoint,
    CompactedCheckpointCandidate, Runtime, RuntimeProfile, StepContext, StepInput,
    ToolExecutionContext,
};
use merry_tool_workspace::{
    WorkspaceCodingLoopProfile, WorkspaceRuntimeProfileBuilderExt, WorkspaceToolsConfig,
};
use std::{path::PathBuf, sync::Arc};

#[tokio::test]
async fn task_live_smoke_report_preserves_runtime_events_on_failure() {
    let runtime =
        Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
            .build()
            .expect("runtime should build");
    let event = RuntimeJournalEvent::new(
        merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap(),
        1,
        merry_core::RuntimeJournalPayload::StepStarted,
    );
    let mut output = Vec::new();

    write_coding_loop_task_live_smoke_report(
        &runtime,
        merry_runtime::AutomaticCompactionConfig::default(),
        false,
        &[event],
        &mut output,
    )
    .await
    .unwrap_or_else(|_| panic!("task live smoke report should write"));

    let text = String::from_utf8(output).expect("output should be utf-8");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("coding-loop-task-live-smoke: failed"));
    let event = lines
        .next()
        .map(|line| serde_json::from_str::<RuntimeJournalEvent>(line).expect("event should parse"))
        .expect("failure report should include runtime event JSONL");
    assert!(matches!(
        event.payload,
        merry_core::RuntimeJournalPayload::StepStarted
    ));
    let config_summary = lines
        .next()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("line should parse"))
        .expect("failure report should include compaction config summary");
    assert_eq!(
        config_summary["type"],
        serde_json::Value::String("runtime_compaction_config_summary".to_owned())
    );
    let compaction_summary = lines
        .next()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("line should parse"))
        .expect("failure report should include compaction summary");
    assert_eq!(
        compaction_summary["type"],
        serde_json::Value::String("runtime_compaction_summary".to_owned())
    );
    assert_eq!(compaction_summary["checkpoint_present"], false);
    assert!(lines.next().is_none());
}

#[tokio::test]
async fn task_live_smoke_report_includes_process_artifact_preview() {
    let call_id = "call-check";
    let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_process_call(
        call_id,
        &["cargo", "check"],
        Some("."),
    )
    .expect("process call event should build"))]]);
    let runtime =
        Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
            .model_provider(Arc::new(provider), ModelName::new("debug-model").unwrap());
    let temp = tempfile::tempdir().expect("temp dir should be created");
    let profile =
        WorkspaceCodingLoopProfile::new(WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]))
            .expect("workspace profile should build")
            .with_cli_bwrap_process_runner(
                AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
                Arc::new(FakeProcessRunner::scripted([
                    FakeProcessRunnerStep::failure("cargo failed\n"),
                ])),
            );
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .expect("workspace profile should apply")
        .build()
        .expect("runtime profile should build");
    let runtime = runtime
        .with_profile(profile)
        .expect("runtime profile should apply")
        .build()
        .expect("runtime should build");
    let events = collect_runtime_step_events(
        &runtime,
        StepInput::user_text("run check").expect("valid step input"),
        StepContext::default(),
    )
    .await
    .expect("step should collect process call");
    let pending = first_pending_tool_call(&events).expect("pending tool call should exist");
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("process call should execute");
    let mut output = Vec::new();

    write_coding_loop_task_live_smoke_report(
        &runtime,
        merry_runtime::AutomaticCompactionConfig::default(),
        false,
        &execution_events,
        &mut output,
    )
    .await
    .expect("task live smoke report should write");

    let text = String::from_utf8(output).expect("output should be utf-8");
    assert!(text.contains("\"type\":\"tool_call_resolved\""));
    assert!(text.contains("\"type\":\"process_artifact_preview\""));
    assert!(text.contains("\"call_id\":\"call-check\""));
    assert!(text.contains("\"stderr\":\"cargo failed\\n\""));
}

#[tokio::test]
async fn permission_network_smoke_report_includes_tool_calls_and_process_previews() {
    let temp = tempfile::tempdir().expect("temp dir should be created");
    let runner = Arc::new(FakeProcessRunner::scripted([
        FakeProcessRunnerStep::failure("network unreachable\n"),
        FakeProcessRunnerStep::success("93.184.216.34 example.com\n"),
    ]));
    let runtime = build_scripted_permission_network_smoke_runtime(
        temp.path(),
        AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        runner.clone(),
        Arc::new(merry_runtime::StaticPermissionedProcessRunnerFactory::new(
            runner,
        )),
        merry_runtime::AutomaticCompactionConfig::default(),
    )
    .expect("permission network smoke runtime should build");
    let result = runtime
        .run_agent_loop(
            StepInput::user_text("run permission network smoke").expect("valid step input"),
            StepContext::default(),
            AgentLoopConfig::new(6).expect("valid loop config"),
        )
        .await
        .expect("permission network smoke should run");
    assert_permission_network_smoke_result(&runtime, &result)
        .await
        .expect("permission network smoke assertions should pass");

    let mut output = Vec::new();
    write_permission_network_smoke_report(&runtime, result.events(), &mut output)
        .await
        .expect("permission network smoke report should write");

    let text = String::from_utf8(output).expect("output should be utf-8");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("permission-network-smoke: ok"));
    assert!(text.contains("\"type\":\"tool_call_pending\""));
    assert!(text.contains("\"name\":\"run_process\""));
    assert!(text.contains("\"name\":\"request_permissions\""));
    assert!(text.contains("\"network\":true"));
    assert!(text.contains("\"type\":\"tool_call_resolved\""));
    assert!(text.contains("\"type\":\"process_artifact_preview\""));
    assert!(text.contains("\"call_id\":\"permission-network-smoke-initial-network\""));
    assert!(text.contains("\"call_id\":\"permission-network-smoke-request-network\""));
    assert!(text.contains("\"stderr\":\"network unreachable\\n\""));
    assert!(text.contains("\"stdout\":\"93.184.216.34 example.com\\n\""));
}

#[tokio::test]
async fn task_live_smoke_report_includes_compaction_summary_without_checkpoint_text() {
    let manifest = CheckpointRefManifest::new(
        CheckpointId::new("checkpoint-task-live-smoke").expect("valid checkpoint id"),
        vec![
            CheckpointRef::new(
                CheckpointRefId::new("r1").expect("valid ref id"),
                CheckpointSourceKind::UserMessage,
                "history:1",
                CheckpointSequenceRange::new(1, 1).expect("valid sequence range"),
                "body[0]",
                "sensitive old task detail should not appear in smoke report",
            )
            .expect("valid ref"),
        ],
    )
    .expect("valid manifest");
    let candidate = CompactedCheckpointCandidate::from_json(
        r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "current_state",
              "text": "The old task window was compacted.",
              "refs": ["r1"]
            }
          ],
          "working_intent": null
        }"#,
    )
    .expect("valid candidate");
    let citation = CitationBackedCheckpoint::from_candidate(
        CheckpointId::new("checkpoint-task-live-smoke").expect("valid checkpoint id"),
        candidate,
        manifest,
        CheckpointValidationPolicy::default(),
    )
    .expect("valid citation-backed checkpoint");
    let checkpoint = CompactedCheckpoint::from_citation_backed(citation).expect("valid checkpoint");
    let runtime =
        Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
            .compacted_checkpoint(checkpoint)
            .build()
            .expect("runtime should build");
    let mut output = Vec::new();

    write_coding_loop_task_live_smoke_report(
        &runtime,
        merry_runtime::AutomaticCompactionConfig::default(),
        true,
        &[],
        &mut output,
    )
    .await
    .expect("task live smoke report should write");

    let text = String::from_utf8(output).expect("output should be utf-8");
    assert!(text.contains("\"type\":\"runtime_compaction_summary\""));
    assert!(text.contains("\"checkpoint_present\":true"));
    assert!(text.contains("\"citation_backed\":true"));
    assert!(text.contains("\"checkpoint_id\":\"checkpoint-task-live-smoke\""));
    assert!(text.contains("\"claim_count\":1"));
    assert!(text.contains("\"ref_count\":1"));
    assert!(!text.contains("The old task window was compacted."));
    assert!(!text.contains("sensitive old task detail"));
}

#[tokio::test]
async fn task_live_smoke_report_includes_effective_compaction_config_from_toml() {
    let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[runtime.auto_compaction]
enabled = true
target_output_tokens = 144
model_output_token_limit = 233
max_accepted_output_bytes = 3456
retained_raw_tail_items = 5
max_ref_excerpt_bytes = 789
max_carried_prior_refs = 10
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");
    let auto_compaction =
        automatic_compaction_config(Some(&config)).expect("auto compaction should validate");
    let runtime =
        Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
            .build()
            .expect("runtime should build");
    let mut output = Vec::new();

    write_coding_loop_task_live_smoke_report(&runtime, auto_compaction, true, &[], &mut output)
        .await
        .expect("task live smoke report should write");

    let text = String::from_utf8(output).expect("output should be utf-8");
    assert!(text.contains("\"type\":\"runtime_compaction_config_summary\""));
    assert!(text.contains("\"auto_compaction_enabled\":true"));
    assert!(text.contains("\"target_output_tokens\":144"));
    assert!(text.contains("\"model_output_token_limit\":233"));
    assert!(text.contains("\"max_accepted_output_bytes\":3456"));
    assert!(text.contains("\"retained_raw_tail_items\":5"));
    assert!(text.contains("\"max_ref_excerpt_bytes\":789"));
    assert!(text.contains("\"max_carried_prior_refs\":10"));
}
