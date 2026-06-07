use super::*;

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
    let result_artifact = ArtifactRef::new(
        artifact_id("manual-result-continuation-failed-json"),
        ArtifactKind::Json,
    );
    let result = failed_tool_result(
        call.id().clone(),
        result_artifact.clone(),
        "tool_failed",
        "Tool exited with status 2",
    );
    let resolved_events = runtime
        .submit_tool_result(
            result,
            ArtifactContent::json(r#"{"stderr":"permission denied"}"#),
        )
        .await
        .expect("failed tool result should resolve");
    let continuation_events = collect_step(&runtime, "Continue after failed tool.").await;

    assert!(
        pending_events
            .iter()
            .chain(resolved_events.iter())
            .chain(continuation_events.iter())
            .all(|event| !matches!(event.kind, RuntimeEventKind::Failed { .. })),
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
async fn submit_failed_tool_result_preserves_diagnostic_and_failure_artifact_without_failed_event()
{
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-failed", provider);
    let pending_events = collect_step(&runtime, "Request a failing tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let diagnostic = merry_core::ErrorInfo::new("tool_failed", "Tool exited with status 2")
        .expect("valid diagnostic");
    let result_artifact = ArtifactRef::new(artifact_id("manual-result-failed"), ArtifactKind::Json);
    let result = ToolCallResult::failed(call.id().clone(), result_artifact.clone(), diagnostic);

    let events = runtime
        .submit_tool_result(
            result.clone(),
            ArtifactContent::json(r#"{"stderr":"permission denied"}"#),
        )
        .await
        .expect("failed tool result should resolve");

    assert_eq!(
        event_kind_names(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(matches!(
        &events[0].kind,
        RuntimeEventKind::ArtifactRecorded { artifact } if artifact == &result_artifact
    ));
    assert!(matches!(
        &events[1].kind,
        RuntimeEventKind::ToolCallResolved { result: resolved }
            if resolved.status() == ToolCallResultStatus::Failed
                && resolved.diagnostic().map(merry_core::ErrorInfo::code) == Some("tool_failed")
                && resolved == &result
    ));
    assert!(
        pending_events
            .iter()
            .chain(events.iter())
            .all(|event| !matches!(event.kind, RuntimeEventKind::Failed { .. })),
        "tool execution failure must be represented as ToolCallResolved, not RuntimeEventKind::Failed"
    );
    let evidence = runtime
        .evidence_ref(result_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("failure artifact should be readable");
    assert_eq!(evidence.artifact_id, *result_artifact.id());

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
