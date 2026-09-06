use crate::support::{
    events::{event_kind_names, failed_code, pending_tool_call, resolved_tool_result},
    models::{
        ScriptedModelProvider, completed_outputs_event, completed_text_event, model_name,
        model_tool_call_with_args, model_tool_call_with_id,
    },
    runtime::{
        collect_step, runtime_with_registered_tool, runtime_with_registered_tool_action, session_id,
    },
    tools::{ScriptedToolExecutor, assert_sanitized_policy_denial_json, test_tool_spec},
};
use merry_core::{
    ArtifactKind, EvidenceLocator, RuntimeJournalPayload, ToolCallResultStatus, ToolName,
};
use merry_llm::{FinishReason, ModelOutput};
use merry_runtime::{
    LedgerFactKind, LedgerProjection, RegisteredTool, Runtime, ToolActionKind, ToolAdmission,
    ToolExecutionContext,
};
use serde_json::{Map, Value};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn unregistered_pending_tool_name_resolves_failed_with_tool_not_registered() {
    let call = model_tool_call_with_args("call-unregistered", "missing_tool", Map::new());
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after missing tool"))],
    ]);
    let runtime = Runtime::builder(session_id("provider-execute-tool-unregistered"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    let pending_events = collect_step(&runtime, "Call missing tool.").await;
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
        .expect("unregistered tool should synthesize failed result");

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
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should have diagnostic")
            .code(),
        "tool_not_registered"
    );
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert_eq!(result.call_id(), pending.id());
    assert!(failed_code(&execution_events).is_none());
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("unregistered tool failure artifact should be readable after ArtifactRecorded");
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

    let continuation_events = collect_step(&runtime, "Continue after missing tool.").await;
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
    assert_eq!(provider.recorded_requests()[1].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn tool_admission_denies_execution_without_changing_provider_tool_surface() {
    let call = model_tool_call_with_args("call-admission-denied", "search_notes", Map::new());
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let runtime = Runtime::builder(session_id("provider-tool-admission"))
        .tool_admission(ToolAdmission::allow_only(Vec::<ToolName>::new()))
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("search_notes"),
            Arc::new(ScriptedToolExecutor::succeeding_text("must not execute")),
        ))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Call a denied tool.").await;
    assert!(
        provider.recorded_requests()[0]
            .tools()
            .iter()
            .any(|tool| tool.name().as_str() == "search_notes")
    );
    let pending = pending_tool_call(&pending_events).clone();
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool admission should resolve a structured failure");

    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("admission denial should have a diagnostic")
            .code(),
        "tool_not_admitted"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn denied_registered_tool_result_is_compiled_as_failed_provider_neutral_continuation() {
    let call = model_tool_call_with_id("call-policy-denied");
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after policy denial"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("executor must not run\n");
    let runtime = runtime_with_registered_tool_action(
        "provider-policy-denied-continuation",
        provider.clone(),
        executor.clone(),
        ToolActionKind::WorkspaceWrite,
    );
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("policy denial should resolve the pending call");
    let continuation_events = collect_step(&runtime, "Continue after denial.").await;

    assert_eq!(executor.calls().len(), 0);
    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("policy denial result should include diagnostic")
            .code(),
        "action_policy_denied"
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("failed policy denial should be compiled as continuation");
    assert_eq!(continuation.call().id().as_str(), "call-policy-denied");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .map(merry_core::ErrorInfo::code),
        Some("action_policy_denied")
    );
    let content = continuation
        .result()
        .content()
        .as_json()
        .expect("policy denial continuation should carry JSON content");
    let value: Value = serde_json::from_str(content).expect("denial JSON should parse");
    assert_sanitized_policy_denial_json(&value, "search_notes");

    let serialized = serde_json::to_value(continuation).expect("continuation should serialize");
    assert!(serialized.get("provider").is_none());
    assert!(serialized.get("wire").is_none());
    assert!(serialized.get("previous_response_id").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_with_bad_or_missing_args_resolves_schema_failure_before_executor() {
    let call = model_tool_call_with_args("call-bad-args", "search_notes", Map::new());
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::succeeding_text("accepted bad args\n");
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-schema-pass",
        provider,
        executor.clone(),
    );
    let pending_events = collect_step(&runtime, "Search with missing args.").await;
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("schema failure should resolve the pending call");

    let calls = executor.calls();
    assert_eq!(calls.len(), 0);
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("schema failure should carry diagnostic")
            .code(),
        "tool_input_schema_invalid"
    );
}
