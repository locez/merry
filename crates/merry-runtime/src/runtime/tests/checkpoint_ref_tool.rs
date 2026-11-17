use crate::{
    CheckpointRefId, CitationCompactionPolicy, CompactedCheckpoint, RuntimeError, StepContext,
    artifact::ArtifactContent,
    compaction::{CompactionCoverageBudget, CompactionPreparation, CompactionWindowBudget},
    runtime::{
        CompactionConfig, Runtime, merry_read_checkpoint_ref_tool_name,
        tests::support::{
            common::{
                RuntimeSessionStateTestExt, collect_step, completed_event, event_kind_names,
                failed_code, model_name, pending_tool_call, session_id,
            },
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
            tool_executors::SuccessfulToolExecutor,
            tool_helpers::{
                event_kind_names_for_tool_execution, resolved_artifact_json, resolved_tool_result,
            },
        },
    },
    tool::{RegisteredTool, ToolExecutionContext},
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, PendingToolCall, PendingToolCallBatch,
    ToolCallArguments, ToolCallBatchId, ToolCallId, ToolCallResult, ToolCallResultStatus,
    ToolInputSchema, ToolSpec,
};
use merry_llm::ModelCapabilities;
use schemars::Schema;
use serde_json::json;
use std::sync::Arc;

#[test]
fn runtime_builder_registers_checkpoint_ref_tool_when_auto_compaction_enabled() {
    let runtime = Runtime::builder(session_id("runtime-checkpoint-ref-tool-default"))
        .build()
        .expect("runtime should build");
    let name = merry_read_checkpoint_ref_tool_name();

    assert!(
        runtime.inner.tool_registry.registered_tool(&name).is_some(),
        "default automatic compaction should expose the checkpoint ref tool"
    );
}

#[test]
fn runtime_builder_omits_checkpoint_ref_tool_when_auto_compaction_disabled() {
    let runtime = Runtime::builder(session_id("runtime-checkpoint-ref-tool-disabled"))
        .automatic_compaction(CompactionConfig::disabled())
        .build()
        .expect("runtime should build");
    let name = merry_read_checkpoint_ref_tool_name();

    assert!(
        runtime.inner.tool_registry.registered_tool(&name).is_none(),
        "disabling automatic compaction should hide the checkpoint ref tool"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_request_includes_checkpoint_ref_tool_when_auto_compaction_enabled() {
    let provider = RecordingModelProvider::new();
    let runtime = Runtime::builder(session_id("runtime-provider-checkpoint-ref-tool"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "hello", StepContext::default()).await;
    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    let tool_names = request
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"merry_read_checkpoint_ref"));
}

#[tokio::test(flavor = "current_thread")]
async fn provider_step_fails_when_checkpoint_ref_tool_requires_unsupported_tool_calls() {
    let provider = RecordingModelProvider::with_script_and_capabilities(
        vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event(),
        )])],
        ModelCapabilities::new(true, false, false, true, Some(64_000), None)
            .expect("valid capabilities"),
    );
    let runtime = Runtime::builder(session_id("runtime-provider-checkpoint-ref-tool-no-tools"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "hello", StepContext::default()).await;
    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("provider_tool_calls_unsupported")
    );
    assert!(
        provider.recorded_requests().is_empty(),
        "providers without tool-call support must fail before request dispatch when runtime tools are registered"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_step_fails_when_final_output_contract_requires_unsupported_tool_calls() {
    let provider = RecordingModelProvider::with_script_and_capabilities(
        vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event(),
        )])],
        ModelCapabilities::new(true, false, false, true, Some(64_000), None)
            .expect("valid capabilities"),
    );
    let runtime = Runtime::builder(session_id("runtime-final-output-no-tool-provider"))
        .automatic_compaction(CompactionConfig::disabled())
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "return structured output",
        StepContext::default().with_final_output_contract(final_output_contract()),
    )
    .await;
    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("provider_tool_calls_unsupported")
    );
    assert!(
        provider.recorded_requests().is_empty(),
        "providers without tool-call support must fail before request dispatch when final output is tool-backed"
    );
}

fn final_output_contract() -> crate::FinalOutputContract {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "Concise final answer."
            }
        },
        "required": ["summary"],
        "additionalProperties": false
    }))
    .expect("test schema should be a JSON schema");
    crate::FinalOutputContract::new(ToolInputSchema::new(schema).expect("valid tool input schema"))
        .expect("valid final output contract")
}

fn checkpoint_ref_pending_tool_call(call_id: &str, ref_id: &str) -> PendingToolCall {
    checkpoint_ref_pending_tool_call_with_arguments(call_id, json!({ "ref": ref_id }))
}

fn checkpoint_ref_pending_tool_call_with_arguments(
    call_id: &str,
    arguments: serde_json::Value,
) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(call_id).expect("valid tool call id"),
        merry_read_checkpoint_ref_tool_name(),
        ToolCallArguments::try_from(arguments).expect("valid checkpoint ref tool arguments"),
    )
}

fn invalid_checkpoint_ref_pending_tool_call(
    call_id: &str,
    arguments: serde_json::Value,
) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(call_id).expect("valid tool call id"),
        merry_read_checkpoint_ref_tool_name(),
        ToolCallArguments::try_from(arguments).expect("valid JSON arguments"),
    )
}

struct ArchivedToolRef {
    ref_id: CheckpointRefId,
    artifact_id: String,
}

fn record_archivable_tool_turn(
    session: &mut crate::session::SessionState,
    call_id: &str,
    body: &str,
) {
    let turn_id = session.begin_model_turn().expect("tool turn begins");
    session
        .record_user_message_body(turn_id, &format!("use {call_id}"))
        .expect("tool user message records");
    let call = pending_tool_call(call_id);
    session
        .record_tool_call_batch_pending(
            turn_id,
            PendingToolCallBatch::new(
                ToolCallBatchId::new(&format!("batch-{call_id}")).expect("valid batch id"),
                vec![call.clone()],
            )
            .expect("valid tool batch"),
        )
        .expect("tool call records");
    session
        .close_model_response(turn_id, true)
        .expect("tool response closes");
    session
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    ArtifactId::new(&format!("archive-source-{call_id}"))
                        .expect("valid artifact id"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text(body),
        )
        .expect("tool result records");
}

async fn runtime_with_archived_tool_ref(
    session_name: &str,
    body: &str,
) -> (Runtime, ArchivedToolRef) {
    let runtime = Runtime::builder(session_id(session_name))
        .build()
        .expect("runtime should build");
    let archived = {
        let mut session = runtime.inner.session.lock().await;
        for turn in 1..=5 {
            record_archivable_tool_turn(&mut session, &format!("archive-call-{turn}"), body);
        }
        let policy = CitationCompactionPolicy::new(None, None, 5).expect("valid policy");
        let preparation = session
            .build_compaction_preparation_with_window_budget(
                policy,
                policy.resolve(64_000).expect("budget resolves"),
                CompactionWindowBudget::new(64_000, 1_300, 0, 0, 128).expect("valid window budget"),
                CompactionCoverageBudget::limited(0),
            )
            .expect("preparation builds")
            .expect("archive-only preparation is required");
        let CompactionPreparation::ArchiveToolResults(input) = preparation else {
            panic!("expected archive-only preparation");
        };
        let reference = input.archived_refs().first().expect("archived ref").clone();
        let archived = ArchivedToolRef {
            ref_id: reference.id().clone(),
            artifact_id: reference.evidence().artifact_id.as_str().to_owned(),
        };
        session
            .install_archive_only_compaction(input)
            .expect("archive-only install succeeds");
        archived
    };
    (runtime, archived)
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_returns_default_original_page_json() {
    let source = format!("{}PAGE_TWO", "x".repeat(5000));
    let (runtime, archived) =
        runtime_with_archived_tool_ref("runtime-checkpoint-ref-tool-success", &source).await;
    let pending =
        checkpoint_ref_pending_tool_call("call-read-checkpoint-ref", archived.ref_id.as_str());
    {
        let mut session = runtime.inner.session.lock().await;
        session.record_session_started_if_needed();
        session
            .record_test_tool_call_pending(pending.clone())
            .expect("pending call should record");
    }

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("checkpoint ref tool should resolve");

    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    let payload = resolved_artifact_json(&runtime, result, "checkpoint ref result").await;
    assert_eq!(
        payload,
        json!({
            "ref": archived.ref_id.as_str(),
            "source_kind": "tool_result",
            "artifact_id": archived.artifact_id,
            "offset": 0,
            "content": &source[..4096],
            "next_offset": 4096,
            "total_bytes": source.len(),
            "done": false
        })
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_page_reads_second_page_from_original_content() {
    let source = format!("{}SENTINEL_AFTER_1200_BYTES", "x".repeat(1200));
    let (runtime, archived) =
        runtime_with_archived_tool_ref("runtime-checkpoint-ref-page-second", &source).await;
    let ref_id = archived.ref_id;

    let first = runtime
        .read_checkpoint_ref_page(&ref_id, 0, 1024)
        .await
        .expect("first page should read");
    let second = runtime
        .read_checkpoint_ref_page(
            &ref_id,
            first.next_offset().expect("first page should continue"),
            1024,
        )
        .await
        .expect("second page should read");

    assert!(second.content().contains("SENTINEL_AFTER_1200_BYTES"));
    assert_eq!(second.artifact_id(), first.artifact_id());
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_reports_not_found_without_checkpoint() {
    assert_checkpoint_ref_not_found(
        "call-missing-checkpoint",
        "bootstrap-ref",
        Runtime::builder(session_id("runtime-checkpoint-ref-tool-no-checkpoint"))
            .build()
            .expect("runtime should build"),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_reports_not_found_for_plain_checkpoint() {
    let runtime = Runtime::builder(session_id("runtime-checkpoint-ref-tool-plain"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("plain checkpoint").expect("valid checkpoint"),
        )
        .build()
        .expect("runtime should build");
    assert_checkpoint_ref_not_found("call-plain-checkpoint", "bootstrap-ref", runtime).await;
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_reports_not_found_for_unknown_ref() {
    let (runtime, _archived) =
        runtime_with_archived_tool_ref("runtime-checkpoint-ref-tool-unknown", &"x".repeat(5_000))
            .await;
    assert_checkpoint_ref_not_found("call-unknown-checkpoint-ref", "r9", runtime).await;
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_invalid_ref_string_reports_not_found() {
    let runtime = Runtime::builder(session_id("runtime-checkpoint-ref-tool-invalid-ref-string"))
        .build()
        .expect("runtime should build");
    assert_checkpoint_ref_not_found("call-invalid-ref-string", " bad ref ", runtime).await;
}

async fn assert_checkpoint_ref_not_found(call_id: &str, ref_id: &str, runtime: Runtime) {
    let pending = checkpoint_ref_pending_tool_call(call_id, ref_id);
    {
        let mut session = runtime.inner.session.lock().await;
        session.record_session_started_if_needed();
        session
            .record_test_tool_call_pending(pending.clone())
            .expect("pending call should record");
    }

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("checkpoint ref tool should resolve as domain failure");
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("not found should include diagnostic")
            .code(),
        "checkpoint_ref_not_found"
    );
    let payload = resolved_artifact_json(&runtime, result, "checkpoint ref failure").await;
    assert_eq!(
        payload,
        json!({
            "error": "checkpoint_ref_not_found",
            "ref": ref_id,
        })
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_invalid_input_uses_schema_validation() {
    for (call_id, arguments) in [
        ("call-checkpoint-ref-missing-ref", json!({})),
        (
            "call-checkpoint-ref-extra-field",
            json!({ "ref": "bootstrap-ref", "checkpoint_id": "c1" }),
        ),
        ("call-checkpoint-ref-non-string", json!({ "ref": 9 })),
        (
            "call-checkpoint-ref-negative-offset",
            json!({ "ref": "bootstrap-ref", "offset": -1 }),
        ),
        (
            "call-checkpoint-ref-zero-page",
            json!({ "ref": "bootstrap-ref", "max_bytes": 0 }),
        ),
        (
            "call-checkpoint-ref-page-too-large",
            json!({ "ref": "bootstrap-ref", "max_bytes": 16_385 }),
        ),
        (
            "call-checkpoint-ref-fractional-page",
            json!({ "ref": "bootstrap-ref", "max_bytes": 1.5 }),
        ),
    ] {
        let pending = invalid_checkpoint_ref_pending_tool_call(call_id, arguments);
        let runtime = Runtime::builder(session_id(&format!("runtime-{call_id}")))
            .build()
            .expect("runtime should build");
        {
            let mut session = runtime.inner.session.lock().await;
            session.record_session_started_if_needed();
            session
                .record_test_tool_call_pending(pending.clone())
                .expect("pending call should record");
        }

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("invalid input should resolve as schema failure");
        let result = resolved_tool_result(&events);
        assert_eq!(result.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            result
                .diagnostic()
                .expect("schema failure should include diagnostic")
                .code(),
            "tool_input_schema_invalid"
        );
    }
}

#[test]
fn runtime_builder_rejects_manual_checkpoint_ref_tool_registration() {
    let duplicate = RegisteredTool::read_only(
        ToolSpec::new(
            merry_read_checkpoint_ref_tool_name(),
            "Duplicate checkpoint ref tool",
            ToolInputSchema::new(
                Schema::try_from(json!({
                    "type": "object",
                    "additionalProperties": false
                }))
                .expect("test schema should be a JSON object"),
            )
            .expect("valid tool input schema"),
        )
        .expect("valid duplicate tool spec"),
        Arc::new(SuccessfulToolExecutor::new()),
    );

    let error = match Runtime::builder(session_id("runtime-checkpoint-ref-tool-duplicate"))
        .register_tool(duplicate)
        .build()
    {
        Ok(_) => panic!("duplicate built-in tool name should fail"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        RuntimeError::DuplicateToolRegistration { ref name }
            if name.as_str() == "merry_read_checkpoint_ref"
    ));
}
