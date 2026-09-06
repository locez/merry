use crate::support::{
    events::{
        assert_no_artifact_recorded, assert_no_completion, assert_no_tool_call_pending,
        event_kind_names, failed_code, pending_tool_call, pending_tool_call_batch,
    },
    models::{
        ScriptedModelProvider, completed_outputs_event, completed_text_event, model_tool_call,
        model_tool_call_with_args, model_tool_call_with_id,
    },
    runtime::{artifact_id, collect_step, runtime_with_provider, runtime_with_scripted_provider},
};
use merry_core::{ArtifactKind, ArtifactRef, ToolCallId, ToolCallResult};
use merry_llm::{FinishReason, ModelEvent, ModelOutput, testing::FakeModelProvider};
use merry_runtime::{ArtifactContent, LedgerFactKind, LedgerProjection};
use serde_json::{Map, json};

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_tool_call_requested_emits_pending_without_completion() {
    let call = model_tool_call();
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested { call: call.clone() }),
        Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-streamed", provider);

    let events = collect_step(&runtime, "Request a tool.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(pending_tool_call(&events).id().as_str(), "call-1");
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
    assert!(failed_code(&events).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_multiple_tool_call_requests_emit_ordered_batch() {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-1"),
        }),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-2"),
        }),
        Ok(completed_outputs_event(
            vec![
                ModelOutput::tool_call(model_tool_call_with_id("call-1")),
                ModelOutput::tool_call(model_tool_call_with_id("call-2")),
            ],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-streamed-multiple", provider);

    let events = collect_step(&runtime, "Request multiple streamed tools.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallBatchPending"]
    );
    assert_eq!(
        pending_tool_call_batch(&events)
            .calls()
            .iter()
            .map(|call| call.id().as_str())
            .collect::<Vec<_>>(),
        ["call-1", "call-2"]
    );
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_tool_call_then_stop_text_fails_without_artifact_or_pending() {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-1"),
        }),
        Ok(completed_outputs_event(
            vec![ModelOutput::text("fallback text")],
            FinishReason::Stop,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-then-stop-text", provider);

    let events = collect_step(&runtime, "Request tool then stop with text.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_tool_call_mixed_output"));
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_tool_call_then_completed_different_tool_call_fails_without_partial_pending()
 {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-1"),
        }),
        Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-2"))],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-completed-different", provider);

    let events = collect_step(&runtime, "Request one tool and complete another.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("model_tool_call_stream_mismatch")
    );
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_single_tool_call_emits_pending_without_completion() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-finish-tool-calls", provider);

    let events = collect_step(&runtime, "Finish with tool calls.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(pending_tool_call(&events).id().as_str(), "call-1");
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
    assert!(failed_code(&events).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_tool_call_pending_preserves_id_name_arguments_and_ledger_fact() {
    let arguments = Map::from_iter([
        ("query".to_owned(), json!("runtime tool calls")),
        ("limit".to_owned(), json!(3)),
        ("include_archived".to_owned(), json!(false)),
    ]);
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call_with_args(
            "call.provider/opaque.id:42",
            "search_notes",
            arguments.clone(),
        ))],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-payload", provider);

    let events = collect_step(&runtime, "Search notes.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let call = pending_tool_call(&events);
    assert_eq!(call.id().as_str(), "call.provider/opaque.id:42");
    assert_eq!(call.name().as_str(), "search_notes");
    assert_eq!(call.arguments().as_object(), &arguments);
    let mut pending = runtime.pending_tool_calls().await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id().as_str(), "call.provider/opaque.id:42");
    assert_eq!(pending[0].name().as_str(), "search_notes");
    assert_eq!(pending[0].arguments().as_object(), &arguments);

    pending.clear();
    let pending_after_caller_mutation = runtime.pending_tool_calls().await;
    assert_eq!(pending_after_caller_mutation, vec![call.clone()]);

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
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unresolved_pending_tool_call_blocks_next_provider_step_without_calling_provider() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("must not be requested"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-pending-blocks-step", provider.clone());

    let first_events = collect_step(&runtime, "Request a tool.").await;
    let second_events = collect_step(&runtime, "Try to continue without tool result.").await;

    assert_eq!(
        event_kind_names(&first_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
    assert_eq!(
        failed_code(&second_events),
        Some("tool_call_result_required")
    );
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        runtime
            .pending_tool_calls()
            .await
            .iter()
            .map(|call| call.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["call-1".to_owned()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_provider_tool_call_id_after_pending_fails_without_second_pending() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-duplicate-id", provider.clone());

    let first_events = collect_step(&runtime, "Request a tool.").await;
    let second_events = collect_step(&runtime, "Request the same tool id again.").await;

    assert_eq!(
        event_kind_names(&first_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
    assert_eq!(
        failed_code(&second_events),
        Some("tool_call_result_required")
    );
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        runtime
            .pending_tool_calls()
            .await
            .iter()
            .map(|call| call.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["call-1".to_owned()]
    );

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
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::Failed,
            },
        ]
    );

    let result_artifact = ArtifactRef::new(
        artifact_id("manual-result-after-duplicate-pending"),
        ArtifactKind::Text,
    );
    let result = ToolCallResult::succeeded(
        ToolCallId::new("call-1").expect("valid call id"),
        result_artifact.clone(),
    );
    let resolved_events = runtime
        .submit_tool_result(
            result.clone(),
            ArtifactContent::text("only accepted once\n"),
        )
        .await
        .expect("single pending call should resolve");
    let duplicate_result = ToolCallResult::succeeded(
        ToolCallId::new("call-1").expect("valid call id"),
        ArtifactRef::new(
            artifact_id("manual-result-after-duplicate-second"),
            ArtifactKind::Text,
        ),
    );
    let duplicate_err = runtime
        .submit_tool_result(
            duplicate_result,
            ArtifactContent::text("must not resolve twice\n"),
        )
        .await
        .expect_err("resolved call id should reject duplicate result");

    assert_eq!(
        event_kind_names(&resolved_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert!(matches!(
        duplicate_err,
        merry_runtime::RuntimeError::ToolCallAlreadyResolved { call_id, .. }
            if call_id == ToolCallId::new("call-1").expect("valid call id")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_empty_tool_call_args_succeeds() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call_with_args(
            "call-1",
            "search_notes",
            Map::new(),
        ))],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-empty-args", provider);

    let events = collect_step(&runtime, "Call with empty args.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert!(
        pending_tool_call(&events)
            .arguments()
            .as_object()
            .is_empty()
    );
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_multiple_tool_calls_emits_ordered_batch() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![
            ModelOutput::tool_call(model_tool_call_with_id("call-1")),
            ModelOutput::tool_call(model_tool_call_with_id("call-2")),
        ],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-multiple", provider);

    let events = collect_step(&runtime, "Return multiple tool calls.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallBatchPending"]
    );
    assert_eq!(
        pending_tool_call_batch(&events)
            .calls()
            .iter()
            .map(|call| call.id().as_str())
            .collect::<Vec<_>>(),
        ["call-1", "call-2"]
    );
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_tool_calls_finish_but_no_tool_call_fails() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        Vec::new(),
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-missing", provider);

    let events = collect_step(&runtime, "Finish without tool call payload.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_tool_call_missing"));
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}
