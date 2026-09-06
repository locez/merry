use crate::support::{
    events::{
        assert_no_artifact_recorded, assistant_output_artifact, event_kind_names, failed_code,
    },
    models::{GatedStreamingProvider, completed_event, completed_text_event, model_name},
    runtime::{
        artifact_id, collect_step, collect_step_with_context, record_valid_context,
        runtime_with_provider, runtime_with_provider_event_buffer, session_id,
    },
    tools::assert_default_checkpoint_ref_tool,
};
use futures_util::StreamExt;
use merry_core::{
    ArtifactKind, ArtifactRef, EvidenceLocator, RuntimeJournalPayload, TrajectoryLane,
};
use merry_llm::{
    GenerationConfig, ModelEvent, ModelMessageRole, ModelRetryPolicy, ParallelToolCalls,
    testing::FakeModelProvider,
};
use merry_runtime::{
    ArtifactContent, ArtifactError, ContextSummary, LedgerFactKind, LedgerProjection, Runtime,
    StepContext, StepInput,
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, time::timeout};
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_with_provider_compiles_user_text_request_and_records_assistant_output_artifact()
 {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::OutputTextDelta {
            delta: "ignored".to_owned(),
        }),
        Ok(completed_event()),
    ]);
    let runtime = runtime_with_provider("provider-user-text", provider.clone());

    let events = collect_step(&runtime, "Explain the runtime boundary.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputDelta",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
    let artifact = assistant_output_artifact(&events);
    assert_eq!(artifact.id().as_str(), "assistant-output-3");
    assert_eq!(artifact.kind(), &ArtifactKind::Text);
    let evidence = runtime
        .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("artifact event should be observable only after artifact is readable");
    assert_eq!(evidence.artifact_id, *artifact.id());

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
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 3,
                kind: LedgerFactKind::StepCompleted,
            },
        ]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.model(), &model_name());
    assert_eq!(request.messages().len(), 2);
    assert_eq!(request.stable_prefix_message_count(), 1);
    assert_eq!(request.messages()[0].role(), ModelMessageRole::System);
    assert!(
        request.messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a software engineering agent")
    );
    let base_instructions = request.messages()[0].content().as_text();
    assert!(base_instructions.contains("Interpret the request before acting:"));
    assert!(base_instructions.contains("Work from evidence."));
    assert!(base_instructions.contains("Choose the right scope."));
    assert!(base_instructions.contains("Do not stop after a fixed number of attempts."));
    assert!(base_instructions.contains("Request broader capability only for an exact action"));
    assert!(!base_instructions.contains("roughly 120"));
    assert!(!base_instructions.contains("roughly 250"));
    assert!(!base_instructions.contains("merry_outer_sandbox:"));
    assert!(!base_instructions.contains("OpenAI"));
    assert!(!base_instructions.contains("Anthropic"));
    assert!(!base_instructions.contains("GPT-"));
    assert!(
        !request.messages()[0]
            .content()
            .as_text()
            .contains("Do not add a progress note before routine"),
        "plain runtime requests must not induce tool-progress commentary by default"
    );
    assert_eq!(request.messages()[1].role(), ModelMessageRole::User);
    assert_eq!(
        request.messages()[1].content().as_text(),
        "Explain the runtime boundary."
    );
    assert_default_checkpoint_ref_tool(request.tools());
    assert_eq!(request.generation().max_output_tokens(), None);
    assert_eq!(request.generation().reasoning_effort(), None);
    assert_eq!(
        request.generation().parallel_tool_calls(),
        ParallelToolCalls::Disabled
    );
    assert!(!request.generation().allow_parallel_tool_calls());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_exposes_live_delta_before_provider_completion() {
    let (sender, receiver) = mpsc::channel(8);
    let runtime = Runtime::builder(session_id("provider-live-delta"))
        .model_provider(
            Arc::new(GatedStreamingProvider::new(receiver)),
            model_name(),
        )
        .model_retry_policy(ModelRetryPolicy::coding_agent_default())
        .build()
        .expect("runtime should build");
    let mut events = runtime
        .step(
            StepInput::user_text("Stream this response.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("step should start");

    sender
        .send(Ok(ModelEvent::Started))
        .await
        .expect("provider receiver should be open");
    sender
        .send(Ok(ModelEvent::OutputTextDelta {
            delta: "live".to_owned(),
        }))
        .await
        .expect("provider receiver should be open");

    loop {
        let event = timeout(Duration::from_millis(100), events.next())
            .await
            .expect("runtime delta should arrive before completion")
            .expect("runtime event stream should remain open");
        if matches!(
            event.payload,
            RuntimeJournalPayload::AssistantOutputDelta { ref delta } if delta == "live"
        ) {
            break;
        }
    }

    sender
        .send(Ok(completed_text_event("live")))
        .await
        .expect("provider receiver should be open");
    let remaining = events.collect::<Vec<_>>().await;
    assert!(
        remaining
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reserved_assistant_output_external_recording_does_not_block_runtime_owned_output() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-reserved-assistant-output", provider);
    let artifact = ArtifactRef::new(artifact_id("assistant-output-3"), ArtifactKind::Text);
    let before = runtime.ledger_projection().await;

    let err = runtime
        .record_artifact(
            artifact.clone(),
            ArtifactContent::text("external shadow output\n"),
        )
        .await
        .expect_err("external recording should not use runtime-owned assistant output ids");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::ReservedArtifactId { artifact_id } if artifact_id == *artifact.id()
    ));
    assert_eq!(before, after);
    let evidence_err = runtime
        .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("reserved artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == *artifact.id()
    ));

    let events = collect_step(&runtime, "after reserved artifact").await;
    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    let generated = assistant_output_artifact(&events);
    assert_eq!(generated.id().as_str(), "assistant-output-2");
    let evidence = runtime
        .evidence_ref(generated.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("runtime-owned assistant output should be readable");
    assert_eq!(evidence.artifact_id, *generated.id());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_with_provider_uses_step_generation_config() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-generation-config", provider.clone());
    let context = StepContext::new(CancellationToken::new()).with_generation_config(
        GenerationConfig::new(Some(16), false).expect("valid generation config"),
    );

    let events = collect_step_with_context(&runtime, "Limit the output.", context).await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].generation().max_output_tokens(), Some(16));
    assert!(!requests[0].generation().allow_parallel_tool_calls());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_rejects_explicit_parallel_calls_when_provider_lacks_capability() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-parallel-unsupported", provider.clone());
    let context = StepContext::new(CancellationToken::new()).with_generation_config(
        GenerationConfig::default().with_parallel_tool_calls(ParallelToolCalls::Enabled),
    );

    let events = collect_step_with_context(&runtime, "Use parallel tools.", context).await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("provider_parallel_tool_calls_unsupported")
    );
    assert!(provider.recorded_requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_with_provider_includes_compiled_context_as_system_message() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-context", provider.clone());
    let expected_snapshot = record_valid_context(&runtime).await;

    let events = collect_step(&runtime, "Use the stored context.").await;

    assert_eq!(
        event_kind_names(&events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.messages().len(), 3);
    assert_eq!(request.stable_prefix_message_count(), 1);
    assert_eq!(request.messages()[0].role(), ModelMessageRole::System);
    assert!(
        request.messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a software engineering agent")
    );
    assert_eq!(request.messages()[1].role(), ModelMessageRole::System);
    assert_eq!(
        request.messages()[1].content().as_text(),
        format!("<merry_compiled_context>\n{expected_snapshot}\n</merry_compiled_context>")
    );
    assert_eq!(request.messages()[2].role(), ModelMessageRole::User);
    assert_eq!(
        request.messages()[2].content().as_text(),
        "Use the stored context."
    );
    assert_default_checkpoint_ref_tool(request.tools());
    assert!(!request.generation().allow_parallel_tool_calls());

    let trajectory = runtime
        .trajectory_snapshot()
        .await
        .expect("trajectory snapshot should be available");
    assert!(
        trajectory
            .records()
            .iter()
            .all(|record| record.lane() != TrajectoryLane::System)
    );
    assert_eq!(trajectory.prompt().stable_blocks().len(), 1);
    assert!(
        trajectory.prompt().stable_blocks()[0]
            .content()
            .contains("You are Merry, a software engineering agent")
    );
    assert_eq!(trajectory.prompt().dynamic_context_count(), 1);
    assert!(trajectory.prompt().latest_dynamic_sequence().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_multiline_text_artifact_supports_line_evidence() {
    let provider = FakeModelProvider::new(vec![Ok(completed_text_event(
        "first line\nsecond line\nthird line\n",
    ))]);
    let runtime = runtime_with_provider("provider-multiline-output", provider);

    let events = collect_step(&runtime, "Return multiple lines.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    let artifact = assistant_output_artifact(&events);
    let evidence = runtime
        .evidence_ref(
            artifact.id(),
            EvidenceLocator::line_range(2, 2).expect("valid line range"),
        )
        .await
        .expect("assistant output line evidence should resolve");
    assert_eq!(evidence.artifact_id, *artifact.id());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_success_with_single_slot_event_buffer_emits_artifact_and_completion() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider_event_buffer("provider-stop-buffer-one", provider, 1);

    let events = collect_step(&runtime, "Use a single-slot event buffer.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn second_provider_step_continues_sequences_and_replays_transcript() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-second-step", provider.clone());

    let first_events = collect_step(&runtime, "First request.").await;
    let second_events = collect_step(&runtime, "Second request.").await;

    assert_eq!(
        first_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        second_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5, 6]
    );
    assert_eq!(
        event_kind_names(&second_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert_eq!(
        assistant_output_artifact(&first_events).id().as_str(),
        "assistant-output-2"
    );
    assert_eq!(
        assistant_output_artifact(&second_events).id().as_str(),
        "assistant-output-5"
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages().len(), 4);
    assert_eq!(requests[1].messages()[0].role(), ModelMessageRole::System);
    assert!(
        requests[1].messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a software engineering agent")
    );
    assert_eq!(requests[1].messages()[1].role(), ModelMessageRole::User);
    assert_eq!(
        requests[1].messages()[1].content().as_text(),
        "First request."
    );
    assert_eq!(
        requests[1].messages()[2].role(),
        ModelMessageRole::Assistant
    );
    assert_eq!(
        requests[1].messages()[2].content().as_text(),
        "model result"
    );
    assert_eq!(requests[1].messages()[3].role(), ModelMessageRole::User);
    assert_eq!(
        requests[1].messages()[3].content().as_text(),
        "Second request."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_rejects_invalid_context_summary_before_provider_step() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-context-failure", provider.clone());
    let error = runtime
        .record_context_summary(
            ContextSummary::new(
                "invalid-summary",
                "This summary has no evidence.",
                Vec::new(),
            )
            .expect("summary construction allows compiler validation"),
        )
        .await
        .expect_err("invalid context summary is rejected before provider step");

    assert_eq!(provider.recorded_requests().len(), 0);
    assert_eq!(
        error.to_string(),
        "context state error: context summary invalid-summary has no exact evidence references"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_absent_step_preserves_skeleton_behavior() {
    let runtime = Runtime::builder(session_id("provider-absent"))
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "Run without provider.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "StepCompleted"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_no_artifact_recorded(&events);
}
