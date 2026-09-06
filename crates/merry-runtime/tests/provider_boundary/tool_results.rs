use crate::support::{
    events::{event_kind_names, failed_code, pending_tool_call},
    models::{
        ScriptedModelProvider, completed_outputs_event, completed_text_event, model_tool_call,
    },
    runtime::{
        artifact_id, collect_step, runtime_with_provider, runtime_with_scripted_provider,
        session_id,
    },
};
use merry_core::{
    ArtifactKind, ArtifactRef, EvidenceLocator, RuntimeJournalPayload, ToolCallId, ToolCallResult,
};
use merry_llm::{FinishReason, ModelOutput, testing::FakeModelProvider};
use merry_runtime::{ArtifactContent, LedgerFactKind, LedgerProjection};

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_records_success_artifact_resolves_pending_and_updates_ledger() {
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
    let result_artifact =
        ArtifactRef::new(artifact_id("manual-result-success"), ArtifactKind::Text);
    let result = ToolCallResult::succeeded(call.id().clone(), result_artifact.clone());

    let events = runtime
        .submit_tool_result(result.clone(), ArtifactContent::text("exact tool output\n"))
        .await
        .expect("tool result should resolve");

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
        &events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == &result_artifact
    ));
    assert!(matches!(
        &events[1].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == &result
    ));
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence = runtime
        .evidence_ref(result_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("tool result artifact should be readable");
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
        vec![5, 6, 7]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_rejects_reserved_artifact_ids_without_mutation() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-reserved-submit", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;

    for reserved_id in ["tool-result-4", "assistant-output-4", "process-input-4"] {
        let artifact = ArtifactRef::new(artifact_id(reserved_id), ArtifactKind::Text);
        let result = ToolCallResult::succeeded(call.id().clone(), artifact.clone());
        let err = runtime
            .submit_tool_result(result, ArtifactContent::text("external shadow result\n"))
            .await
            .expect_err("external submit should not use runtime-owned artifact ids");
        let after = runtime.ledger_projection().await;

        assert!(matches!(
            err,
            merry_runtime::RuntimeError::ReservedArtifactId { artifact_id }
                if artifact_id == *artifact.id()
        ));
        assert_eq!(before, after);
        assert_eq!(runtime.pending_tool_calls().await, vec![call.clone()]);
        let evidence_err = runtime
            .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
            .await
            .expect_err("reserved submitted result artifact must not be recorded");
        assert!(matches!(
            evidence_err,
            merry_runtime::RuntimeError::Artifact {
                source: merry_runtime::ArtifactError::MissingArtifact { id }
            } if id == *artifact.id()
        ));
    }
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
    let first_artifact = ArtifactRef::new(artifact_id("manual-result-first"), ArtifactKind::Text);
    let first = ToolCallResult::succeeded(call.id().clone(), first_artifact.clone());
    runtime
        .submit_tool_result(first, ArtifactContent::text("first result\n"))
        .await
        .expect("first result should resolve");
    let projection_before_duplicate = runtime.ledger_projection().await;
    let duplicate_artifact =
        ArtifactRef::new(artifact_id("manual-result-duplicate"), ArtifactKind::Text);
    let duplicate = ToolCallResult::succeeded(call.id().clone(), duplicate_artifact.clone());

    let err = runtime
        .submit_tool_result(duplicate, ArtifactContent::text("duplicate result\n"))
        .await
        .expect_err("duplicate result should be rejected");
    let projection_after_duplicate = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::ToolCallAlreadyResolved {
            session_id: rejected_session,
            call_id
        } if rejected_session == session_id("provider-tool-result-duplicate")
            && call_id == ToolCallId::new("call-1").expect("valid call id")
    ));
    assert_eq!(projection_before_duplicate, projection_after_duplicate);
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence_err = runtime
        .evidence_ref(duplicate_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("duplicate artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *duplicate_artifact.id()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn artifact_error_while_submitting_tool_result_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-artifact-error", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let duplicate_artifact =
        ArtifactRef::new(artifact_id("manual-result-conflict"), ArtifactKind::Text);
    runtime
        .record_artifact(
            duplicate_artifact.clone(),
            ArtifactContent::text("existing artifact\n"),
        )
        .await
        .expect("conflicting artifact should record before submit");
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(call.id().clone(), duplicate_artifact.clone());

    let err = runtime
        .submit_tool_result(result, ArtifactContent::text("replacement\n"))
        .await
        .expect_err("duplicate artifact id should reject submit");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::DuplicateId { id }
        } if id == *duplicate_artifact.id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);

    let next_events = collect_step(&runtime, "after duplicate artifact submit").await;
    assert_eq!(event_kind_names(&next_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&next_events), Some("tool_call_result_required"));
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn incompatible_tool_result_content_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-incompatible", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(
            artifact_id("manual-result-json-mismatch"),
            ArtifactKind::Json,
        ),
    );

    let err = runtime
        .submit_tool_result(
            result.clone(),
            ArtifactContent::text("not json content kind\n"),
        )
        .await
        .expect_err("content kind mismatch should reject submit");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::IncompatibleContent {
                id,
                artifact_kind,
                content_kind
            }
        } if id == *result.artifact().id()
            && artifact_kind == ArtifactKind::Json
            && content_kind == merry_runtime::ArtifactContentKind::Text
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("incompatible result artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *result.artifact().id()
    ));
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
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(artifact_id("manual-result-blank-text"), ArtifactKind::Text),
    );

    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::text(" \n\t "))
        .await
        .expect_err("blank text tool result should be rejected before resolution");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: merry_runtime::ArtifactContentKind::Text
        } if artifact_id == *result.artifact().id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("blank result artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *result.artifact().id()
    ));

    let next_events = collect_step(&runtime, "after blank text submit").await;
    assert_eq!(event_kind_names(&next_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&next_events), Some("tool_call_result_required"));
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
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
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(artifact_id("manual-result-blank-json"), ArtifactKind::Json),
    );

    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::json(" \n\t "))
        .await
        .expect_err("blank json tool result should be rejected before resolution");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: merry_runtime::ArtifactContentKind::Json
        } if artifact_id == *result.artifact().id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("blank result artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *result.artifact().id()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_rejects_unsupported_content_kind_without_mutation() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-unsupported", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(artifact_id("manual-result-binary"), ArtifactKind::Binary),
    );

    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::binary([1, 2, 3]))
        .await
        .expect_err("binary tool result content is not accepted in MVP");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: merry_runtime::ArtifactContentKind::Binary
        } if artifact_id == *result.artifact().id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
}
