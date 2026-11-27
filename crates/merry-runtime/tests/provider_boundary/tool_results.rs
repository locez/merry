use crate::support::{
    events::{event_kind_names, failed_code, pending_tool_call},
    models::{
        ScriptedModelProvider, completed_outputs_event, completed_text_event, model_tool_call,
    },
    runtime::{collect_step, runtime_with_provider, runtime_with_scripted_provider, session_id},
};
use merry_core::{RuntimeJournalPayload, ToolCallId, ToolCallResult, ToolOutput, ToolOutputKind};
use merry_llm::{
    FinishReason, ModelEvent, ModelInputItem, ModelOutput, ModelResponse,
    testing::FakeModelProvider,
};

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_records_success_output_and_resolves_pending() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after tool result"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-tool-result-success", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result = ToolCallResult::succeeded(call.id().clone());

    let events = runtime
        .submit_tool_result(result.clone(), ToolOutput::text("exact tool output\n"))
        .await
        .expect("tool result should resolve");

    assert_eq!(event_kind_names(&events), ["ToolCallResolved"]);
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4]
    );
    assert!(matches!(
        &events[0].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == &result
    ));
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(
        runtime
            .read_tool_output(call.id())
            .await
            .expect("tool result output read"),
        Some(ToolOutput::text("exact tool output\n"))
    );

    let next_events = collect_step(&runtime, "after tool result").await;
    assert_eq!(
        event_kind_names(&next_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert_eq!(provider.recorded_requests().len(), 2);
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6, 7, 8]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tool_turn_reasoning_is_replayed_before_its_tool_call() {
    let reasoning = "Inspect the notes before answering.";
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(model_tool_call())],
                FinishReason::ToolCalls,
                None,
            )
            .with_reasoning(Some(reasoning.to_owned())),
        })],
        vec![Ok(completed_text_event("continued with reasoning"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-reasoning-replay", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result = ToolCallResult::succeeded(call.id().clone());
    runtime
        .submit_tool_result(result, ToolOutput::text("note result\n"))
        .await
        .expect("tool result should resolve");

    collect_step(&runtime, "Continue.").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let input = requests[1].input();
    let reasoning_index = input
        .iter()
        .position(
            |item| matches!(item, ModelInputItem::Reasoning(value) if value.text() == reasoning),
        )
        .expect("the reasoning from the tool turn should be replayed");
    let tool_call_index = input
        .iter()
        .position(|item| {
            matches!(item, ModelInputItem::ToolCall(value) if value.id().as_str() == call.id().as_str())
        })
        .expect("the tool call should remain in history");
    assert!(
        reasoning_index < tool_call_index,
        "reasoning must precede the assistant tool call it belongs to"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_tool_result_after_resolved_does_not_mutate_session() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-duplicate", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let first = ToolCallResult::succeeded(call.id().clone());
    runtime
        .submit_tool_result(first, ToolOutput::text("first result\n"))
        .await
        .expect("first result should resolve");
    let transcript_before_duplicate = runtime
        .session_transcript()
        .await
        .expect("transcript should read");
    let duplicate = ToolCallResult::succeeded(call.id().clone());

    let err = runtime
        .submit_tool_result(duplicate, ToolOutput::text("duplicate result\n"))
        .await
        .expect_err("duplicate result should be rejected");
    let transcript_after_duplicate = runtime
        .session_transcript()
        .await
        .expect("transcript should read");

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::ToolCallAlreadyResolved {
            session_id: rejected_session,
            call_id
        } if rejected_session == session_id("provider-tool-result-duplicate")
            && call_id == ToolCallId::new("call-1").expect("valid call id")
    ));
    assert_eq!(transcript_before_duplicate, transcript_after_duplicate);
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(
        runtime
            .read_tool_output(call.id())
            .await
            .expect("first output read"),
        Some(ToolOutput::text("first result\n"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn blank_text_tool_result_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-blank-text", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let transcript_before_error = runtime
        .session_transcript()
        .await
        .expect("transcript should read");
    let result = ToolCallResult::succeeded(call.id().clone());

    let err = runtime
        .submit_tool_result(result.clone(), ToolOutput::text(" \n\t "))
        .await
        .expect_err("blank text tool result should be rejected before resolution");
    let transcript_after_error = runtime
        .session_transcript()
        .await
        .expect("transcript should read");

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            call_id,
            output_kind: ToolOutputKind::Text
        } if call_id == *result.call_id()
    ));
    assert_eq!(transcript_before_error, transcript_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call.clone()]);
    assert_eq!(
        runtime
            .read_tool_output(call.id())
            .await
            .expect("blank result output read"),
        None
    );

    let next_events = collect_step(&runtime, "after blank text submit").await;
    assert_eq!(event_kind_names(&next_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&next_events), Some("tool_call_result_required"));
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5, 6]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn blank_json_tool_result_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-blank-json", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let transcript_before_error = runtime
        .session_transcript()
        .await
        .expect("transcript should read");
    let result = ToolCallResult::succeeded(call.id().clone());

    let err = runtime
        .submit_tool_result(result.clone(), ToolOutput::json(" \n\t "))
        .await
        .expect_err("blank json tool result should be rejected before resolution");
    let transcript_after_error = runtime
        .session_transcript()
        .await
        .expect("transcript should read");

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            call_id,
            output_kind: ToolOutputKind::Json
        } if call_id == *result.call_id()
    ));
    assert_eq!(transcript_before_error, transcript_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call.clone()]);
    assert_eq!(
        runtime
            .read_tool_output(call.id())
            .await
            .expect("blank result output read"),
        None
    );
}
