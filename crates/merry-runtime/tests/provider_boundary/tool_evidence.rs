use crate::support::{
    events::{event_kind_names, pending_tool_call, resolved_tool_result},
    models::{
        ScriptedModelProvider, completed_outputs_event, completed_text_event, model_name,
        model_tool_call_with_args, model_tool_call_with_id,
    },
    runtime::{artifact_id, collect_step, runtime_with_registered_tool, session_id},
    tools::{
        ScriptedToolExecutor, assert_tools_are_default_checkpoint_ref_plus, path_tool_spec,
        test_tool_spec,
    },
};
use merry_core::{
    ArtifactKind, ArtifactRef, EvidenceLocator, RuntimeJournalPayload, ToolCallResultStatus,
    ToolName,
};
use merry_llm::{FinishReason, ModelOutput, ModelToolCall, testing::FakeModelProvider};
use merry_runtime::{
    ArtifactContent, ArtifactError, LedgerFactKind, LedgerProjection, ProcessActionIntent,
    ProcessExitStatus, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
    ProcessRunnerFuture, ProcessRunnerOutput, RegisteredTool, Runtime, SkillCatalog, SkillMetadata,
    ToolExecutionContext, process_command_tool,
};
use serde_json::{Map, Value, json};
use std::{path::PathBuf, sync::Arc};

fn shell_process_tool_call() -> ModelToolCall {
    model_tool_call_with_args(
        "call-real-shell",
        "run_process",
        Map::from_iter([
            ("command".to_owned(), json!("echo ProcessRunner | wc -l")),
            ("cwd".to_owned(), json!(".")),
        ]),
    )
}

#[derive(Debug, Clone, Copy)]
struct ReadOnlyShellProcessRunner;

impl ProcessRunner for ReadOnlyShellProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ProcessRunnerError::Cancelled);
            }

            ProcessRunnerOutput::new(
                &intent,
                ProcessExitStatus::Exited(0),
                "1\n",
                false,
                "",
                false,
            )
            .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_only_shell_wrapper_records_input_and_result_artifacts() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(shell_process_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = Runtime::builder(session_id("provider-real-shell-runner"))
        .model_provider(Arc::new(provider), model_name())
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a shell command through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .allow_read_only_shell_process_actions(Arc::new(ReadOnlyShellProcessRunner))
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Run read-only shell pipeline.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("read-only shell wrapper should execute through the process runner");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
    );
    let input_artifact = execution_events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ArtifactRecorded { artifact }
                if artifact.id().as_str().starts_with("process-input-") =>
            {
                Some(artifact)
            }
            _ => None,
        })
        .expect("shell input artifact should be recorded before result");
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);

    let input_content = runtime
        .read_artifact_content(input_artifact.id())
        .await
        .expect("input artifact should be readable");
    let input_text = input_content
        .as_text()
        .expect("input artifact should be textual JSON");
    let input_payload: Value = serde_json::from_str(input_text).expect("input JSON should parse");
    assert_eq!(input_payload["kind"], "shell_command_input");
    assert_eq!(
        input_payload["permission_profile_id"],
        "process.shell.read_only"
    );
    assert_eq!(
        input_payload["input_evidence"]["script"],
        "echo ProcessRunner | wc -l"
    );

    let result_content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("result artifact should be readable");
    let result_text = result_content
        .as_text()
        .expect("result artifact should be textual JSON");
    let result_payload: Value =
        serde_json::from_str(result_text).expect("result JSON should parse");
    assert_eq!(
        result_payload["permission_profile_id"],
        "process.shell.read_only"
    );
    assert_eq!(
        result_payload["input_artifact"],
        json!({
            "id": input_artifact.id().as_str(),
            "kind": "json",
        })
    );
    assert!(result_payload.get("input_evidence").is_none());
    assert_eq!(result_payload["stdout"]["text"], "1\n");
    assert_eq!(result_payload["stderr"]["text"], "");
}

#[tokio::test(flavor = "current_thread")]
async fn execute_registered_tool_success_records_artifact_resolves_and_compiles_continuation() {
    let call = model_tool_call_with_args(
        "call-success",
        "search_notes",
        Map::from_iter([("query".to_owned(), Value::String("alpha".to_owned()))]),
    );
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call.clone())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used tool result"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-success",
        provider.clone(),
        executor.clone(),
    );

    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending = pending_tool_call(&pending_events).clone();
    let reserved_artifact = ArtifactRef::new(artifact_id("tool-result-4"), ArtifactKind::Text);
    let before_reserved = runtime.ledger_projection().await;
    let reserved_err = runtime
        .record_artifact(
            reserved_artifact.clone(),
            ArtifactContent::text("external shadow result\n"),
        )
        .await
        .expect_err("external recording should not use runtime-owned tool result ids");
    let after_reserved = runtime.ledger_projection().await;

    assert!(matches!(
        reserved_err,
        merry_runtime::RuntimeError::ReservedArtifactId { artifact_id }
            if artifact_id == *reserved_artifact.id()
    ));
    assert_eq!(before_reserved, after_reserved);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending.clone()]);
    let evidence_err = runtime
        .evidence_ref(reserved_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("reserved tool result id must not be externally recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == *reserved_artifact.id()
    ));

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &execution_events[1].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.artifact().kind(), &ArtifactKind::Text);
    assert_eq!(result.call_id(), pending.id());
    let evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("executor result artifact should be readable after ArtifactRecorded");
    assert_eq!(evidence.artifact_id, *result.artifact().id());
    assert_eq!(executor.calls(), vec![pending.clone()]);
    let projection = runtime.ledger_projection().await;
    assert_eq!(
        projection.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::ToolCallResolved,
            },
        ]
    );

    let continuation_events = collect_step(&runtime, "Continue with result.").await;
    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert_eq!(
        continuation_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6, 7]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_tools_are_default_checkpoint_ref_plus(
        requests[0].tools(),
        &[test_tool_spec("search_notes")],
    );
    assert!(requests[0].continuations().is_empty());
    assert_tools_are_default_checkpoint_ref_plus(
        requests[1].tools(),
        &[test_tool_spec("search_notes")],
    );
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("tool result continuation should be compiled");
    assert_eq!(continuation.call().id().as_str(), "call-success");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(
        continuation.result().content().as_text(),
        Some("search result\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reading_catalog_skill_file_emits_skill_used_event() {
    let call = model_tool_call_with_args(
        "call-read-skill",
        "read_text",
        Map::from_iter([("path".to_owned(), Value::String("demo/SKILL.md".to_owned()))]),
    );
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "demo-skill",
            "Use for demo tasks.",
            PathBuf::from("demo/SKILL.md"),
            PathBuf::from("/skills"),
        )
        .expect("valid skill metadata"),
    ])
    .expect("valid skill catalog");
    let runtime = Runtime::builder(session_id("provider-skill-used"))
        .skill_catalog(catalog)
        .register_tool(RegisteredTool::read_only(
            path_tool_spec("read_text"),
            Arc::new(ScriptedToolExecutor::succeeding_text("# Demo\n")),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Use demo skill.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved", "SkillUsed"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[2].payload,
        RuntimeJournalPayload::SkillUsed {
            skill_name,
            skill_md_path,
            tool_call_id,
            artifact,
        } if skill_name == "demo-skill"
            && skill_md_path == "demo/SKILL.md"
            && tool_call_id == pending.id()
            && artifact == result.artifact()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_domain_failure_resolves_failed_without_runtime_failed() {
    let call = model_tool_call_with_id("call-domain-failure");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::failing_json("tool_lookup_failed", r#"{"ok":false}"#);
    let runtime = runtime_with_registered_tool("provider-execute-tool-failure", provider, executor);
    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("domain failure should resolve the pending call");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(
        execution_events
            .iter()
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::Failed { .. })),
        "tool domain failure must not emit RuntimeJournalPayload::Failed: {execution_events:?}"
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &execution_events[1].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert_eq!(result.call_id(), pending.id());
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should have diagnostic")
            .code(),
        "tool_lookup_failed"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("domain failure artifact should be readable after ArtifactRecorded");
    assert_eq!(evidence.artifact_id, *result.artifact().id());
    let projection = runtime.ledger_projection().await;
    assert_eq!(
        projection.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::ToolCallResolved,
            },
        ]
    );
}
