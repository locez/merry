use crate::support::{
    events::{event_kind_names, pending_tool_call},
    models::{
        ScriptedModelProvider, completed_outputs_event, completed_text_event, model_tool_call,
    },
    runtime::{collect_step, runtime_with_provider, runtime_with_scripted_provider},
    tools::failed_tool_result,
};
use merry_core::{
    RuntimeJournalPayload, ToolCallResult, ToolCallResultStatus, ToolOutput,
    TrajectoryRecordDetails, TrajectoryRecordKind, TrajectoryRecordStatus,
};
use merry_llm::{FinishReason, ModelOutput, testing::FakeModelProvider};
use merry_runtime::{LedgerFactKind, LedgerProjection};

#[tokio::test(flavor = "current_thread")]
async fn failed_tool_result_status_diagnostic_and_content_are_compiled_without_runtime_failed_event()
 {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("handled failed tool result"))],
    ]);
    let runtime = runtime_with_scripted_provider(
        "provider-tool-continuation-failed-result",
        provider.clone(),
    );
    let pending_events = collect_step(&runtime, "Request a failing tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result = failed_tool_result(
        call.id().clone(),
        "tool_failed",
        "Tool exited with status 2",
    );
    let resolved_events = runtime
        .submit_tool_result(
            result,
            ToolOutput::json(r#"{"stderr":"permission denied"}"#),
        )
        .await
        .expect("failed tool result should resolve");
    let continuation_events = collect_step(&runtime, "Continue after failed tool.").await;

    assert!(
        pending_events
            .iter()
            .chain(resolved_events.iter())
            .chain(continuation_events.iter())
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::Failed { .. })),
        "tool execution failure should be model-visible data, not runtime failure"
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("failed result continuation should be compiled");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .map(merry_core::ErrorInfo::code),
        Some("tool_failed")
    );
    assert_eq!(
        continuation.result().content().as_json(),
        Some(r#"{"stderr":"permission denied"}"#)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn t6_failure_links_journal_output_ledger_and_trajectory() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-failed", provider);
    let pending_events = collect_step(&runtime, "Request a failing tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let diagnostic = merry_core::ErrorInfo::new("tool_failed", "Tool exited with status 2")
        .expect("valid diagnostic");
    let result = ToolCallResult::failed(call.id().clone(), diagnostic);

    let events = runtime
        .submit_tool_result(
            result.clone(),
            ToolOutput::json(r#"{"stderr":"permission denied"}"#),
        )
        .await
        .expect("failed tool result should resolve");

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
        RuntimeJournalPayload::ToolCallResolved { result: resolved }
            if resolved.status() == ToolCallResultStatus::Failed
                && resolved.diagnostic().map(merry_core::ErrorInfo::code) == Some("tool_failed")
                && resolved == &result
    ));
    assert!(
        pending_events
            .iter()
            .chain(events.iter())
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::Failed { .. })),
        "tool execution failure must be represented as ToolCallResolved, not RuntimeJournalPayload::Failed"
    );
    assert_eq!(
        runtime
            .read_tool_output(call.id())
            .await
            .expect("failure output read"),
        Some(ToolOutput::json(r#"{"stderr":"permission denied"}"#))
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
                sequence: 3,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 3,
                kind: LedgerFactKind::ToolCallResolved,
            },
        ]
    );

    let pending_sequence = pending_events
        .iter()
        .find_map(|event| match event.payload {
            RuntimeJournalPayload::ToolCallPending {
                call: ref pending_call,
            } if pending_call.id() == call.id() => Some(event.sequence),
            _ => None,
        })
        .expect("pending tool event should have a journal sequence");
    let resolved_sequence = events
        .iter()
        .find_map(|event| match event.payload {
            RuntimeJournalPayload::ToolCallResolved {
                result: ref resolved_result,
            } if resolved_result.call_id() == call.id() => Some(event.sequence),
            _ => None,
        })
        .expect("resolved tool event should have a journal sequence");
    let trajectory = runtime
        .trajectory_snapshot()
        .await
        .expect("trajectory snapshot should be available");
    assert_eq!(trajectory.latest_sequence(), resolved_sequence);
    let tool_record = trajectory
        .records()
        .iter()
        .find(|record| record.tool_call_id() == Some(call.id()))
        .expect("trajectory should locate the resolved tool call");
    assert_eq!(tool_record.kind(), TrajectoryRecordKind::ToolCall);
    assert_eq!(tool_record.status(), TrajectoryRecordStatus::Failed);
    assert_eq!(tool_record.start_sequence(), pending_sequence);
    assert_eq!(tool_record.end_sequence(), Some(resolved_sequence));
    assert_eq!(
        tool_record.diagnostic().map(merry_core::ErrorInfo::code),
        Some("tool_failed")
    );
    assert!(matches!(
        tool_record.details(),
        TrajectoryRecordDetails::Tool { tool }
            if tool.output().map(|output| output.content())
                == Some(r#"{"stderr":"permission denied"}"#)
    ));
}
