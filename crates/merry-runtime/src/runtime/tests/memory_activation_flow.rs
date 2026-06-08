use super::*;

#[test]
fn memory_activation_seed_uses_step_input_as_user_query_source() {
    let input = crate::StepInput::user_text("  Topic\trequest\n").expect("valid step input");

    let seed = memory_activation_seed_from_step_input(&input)
        .expect("seed builds")
        .expect("user text should activate memory");

    assert_eq!(seed.query(), "topic request");
    assert_eq!(
        seed.provenance().source_kind(),
        MemoryActivationSourceKind::UserQuery
    );
    assert_eq!(seed.provenance().source_label(), "step input");
    assert_eq!(
        seed.provenance().allowed_scopes(),
        &[MemoryScope::Session, MemoryScope::Task, MemoryScope::Step]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn default_stored_source_projects_session_memory_before_user_message() {
    let memory = memory_item(
        "memory-topic",
        "Remember that topic answers should mention runtime timing.",
        "memory-topic-artifact",
        &["topic"],
    );
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider("runtime-memory-context", provider.clone());
    record_memory_artifact(
        &runtime,
        "memory-topic-artifact",
        "exact evidence for timing memory",
    );
    record_memory_item(&runtime, memory);

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted"
        ]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].messages().len(), 3);
    assert_eq!(requests[0].stable_prefix_message_count(), 1);
    assert_eq!(requests[0].messages()[0].role(), ModelMessageRole::System);
    assert!(
        requests[0].messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a pragmatic coding agent.")
    );
    assert_eq!(requests[0].messages()[1].role(), ModelMessageRole::System);
    assert_eq!(requests[0].messages()[2].role(), ModelMessageRole::User);
    assert!(
        requests[0].messages()[1]
            .content()
            .as_text()
            .contains("memory:memory-topic")
    );
    assert!(
        requests[0].messages()[1]
            .content()
            .as_text()
            .contains("memory-text:Remember that topic answers should mention runtime timing.")
    );
    assert_eq!(
        requests[0].messages()[2].content().as_text(),
        "Topic request."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unmatched_stored_memory_does_not_add_system_message() {
    let memory = memory_item(
        "memory-other",
        "This memory should not match topic input.",
        "memory-other-artifact",
        &["other"],
    );
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider("runtime-memory-no-match", provider.clone());
    record_memory_artifact(
        &runtime,
        "memory-other-artifact",
        "exact evidence for unmatched memory",
    );
    record_memory_item(&runtime, memory);

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted"
        ]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].messages().len(), 2);
    assert_eq!(requests[0].stable_prefix_message_count(), 1);
    assert_eq!(requests[0].messages()[0].role(), ModelMessageRole::System);
    assert!(
        requests[0].messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a pragmatic coding agent.")
    );
    assert_eq!(requests[0].messages()[1].role(), ModelMessageRole::User);
    assert_eq!(
        requests[0].messages()[1].content().as_text(),
        "Topic request."
    );
    assert_eq!(compiled_context_snapshot(&runtime).await, "");
}

#[tokio::test(flavor = "current_thread")]
async fn stored_memory_with_missing_evidence_fails_before_provider_call() {
    let memory = memory_item(
        "memory-missing-evidence",
        "This memory has no readable evidence artifact.",
        "memory-missing-evidence-artifact",
        &["topic"],
    );
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider("runtime-memory-missing-evidence", provider.clone());
    record_memory_item(&runtime, memory);

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("context_compile"));
    assert_eq!(provider.recorded_requests().len(), 0);
    assert_eq!(
        crate::ContextCompiler::new()
            .compile(&runtime.context_snapshot().await)
            .expect("context compiles after missing evidence cleanup")
            .to_snapshot(),
        ""
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_step_replaces_activated_memories_between_requests() {
    let first_memory = activated_memory(
        "memory-stale",
        "Stale memory must not survive the next projection.",
        "memory-stale-artifact",
    );
    let source = ScriptedMemoryActivationSource::new(vec![vec![first_memory], Vec::new()]);
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider_and_memory_source(
        "runtime-memory-replace",
        provider.clone(),
        source.clone(),
    );
    record_memory_artifact(
        &runtime,
        "memory-stale-artifact",
        "exact evidence for stale memory",
    );

    let first_events = collect_step(
        &runtime,
        "First topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;
    let second_events = collect_step(
        &runtime,
        "Second topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&first_events),
        [
            "SessionStarted",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted"
        ]
    );
    assert_eq!(
        event_kind_names(&second_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    assert_eq!(source.call_count(), 2);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages().len(), 3);
    assert_eq!(requests[0].stable_prefix_message_count(), 1);
    assert!(
        requests[0].messages()[1]
            .content()
            .as_text()
            .contains("memory:memory-stale")
    );
    assert_eq!(requests[1].messages().len(), 4);
    assert_eq!(requests[1].stable_prefix_message_count(), 1);
    assert_eq!(requests[1].messages()[0].role(), ModelMessageRole::System);
    assert!(
        requests[1].messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a pragmatic coding agent.")
    );
    assert_eq!(requests[1].messages()[1].role(), ModelMessageRole::User);
    assert_eq!(
        requests[1].messages()[1].content().as_text(),
        "First topic request."
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
        "Second topic request."
    );
    assert!(
        requests[1]
            .messages()
            .iter()
            .all(|message| !message.content().as_text().contains("memory-stale"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn activation_source_error_clears_previous_successful_projection() {
    let memory = activated_memory(
        "memory-success",
        "Previous successful memory must not survive activation failure.",
        "memory-success-artifact",
    );
    let source = ScriptedMemoryActivationSource::with_script(vec![
        ScriptedMemoryActivationResponse::Memories(vec![memory]),
        ScriptedMemoryActivationResponse::Error(MemoryError::BlankField {
            field: "memory activation source label",
        }),
    ]);
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider_and_memory_source(
        "runtime-memory-source-error-clears",
        provider.clone(),
        source.clone(),
    );
    record_memory_artifact(
        &runtime,
        "memory-success-artifact",
        "exact evidence for successful memory",
    );

    let first_events = collect_step(
        &runtime,
        "First topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;
    let second_events = collect_step(
        &runtime,
        "Second topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&first_events),
        [
            "SessionStarted",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted"
        ]
    );
    assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&second_events), Some("memory_activation"));
    assert_eq!(source.call_count(), 2);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        crate::ContextCompiler::new()
            .compile(&runtime.context_snapshot().await)
            .expect("context compiles after activation source failure")
            .to_snapshot(),
        ""
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unresolved_pending_tool_call_blocks_memory_activation() {
    let source = ScriptedMemoryActivationSource::new(vec![vec![activated_memory(
        "memory-blocked",
        "This memory must not activate while a tool call is pending.",
        "memory-blocked-artifact",
    )]]);
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider_and_memory_source(
        "runtime-memory-pending-gate",
        provider.clone(),
        source.clone(),
    );
    {
        let mut session = runtime.inner.session.lock().await;
        session
            .record_tool_call_pending(pending_tool_call("pending-call"))
            .expect("pending call records");
    }

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some(DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED)
    );
    assert_eq!(source.call_count(), 0);
    assert_eq!(provider.recorded_requests().len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn pre_cancelled_provider_step_does_not_activate_memory() {
    let source = ScriptedMemoryActivationSource::new(vec![vec![activated_memory(
        "memory-cancelled",
        "This memory must not activate for a pre-cancelled step.",
        "memory-cancelled-artifact",
    )]]);
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider_and_memory_source(
        "runtime-memory-pre-cancelled",
        provider.clone(),
        source.clone(),
    );
    let token = CancellationToken::new();
    token.cancel();

    let events = collect_step(&runtime, "Topic request.", crate::StepContext::new(token)).await;

    assert_eq!(event_kind_names(&events), ["Cancelled"]);
    assert_eq!(source.call_count(), 0);
    assert_eq!(provider.recorded_requests().len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_absent_step_does_not_activate_memory() {
    let source = ScriptedMemoryActivationSource::new(vec![vec![activated_memory(
        "memory-no-provider",
        "This memory must not activate without a provider.",
        "memory-no-provider-artifact",
    )]]);
    let runtime =
        runtime_without_provider_with_memory_source("runtime-memory-no-provider", source.clone());

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "StepCompleted"]
    );
    assert_eq!(source.call_count(), 0);
    assert_eq!(
        crate::ContextCompiler::new()
            .compile(&runtime.context_snapshot().await)
            .expect("empty context compiles")
            .to_snapshot(),
        ""
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unreadable_memory_evidence_from_activation_fails_before_provider_call() {
    let source =
        ScriptedMemoryActivationSource::new(vec![vec![activated_memory_with_unreadable_evidence(
            "memory-unreadable",
        )]]);
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider_and_memory_source(
        "runtime-memory-context-compile-failure",
        provider.clone(),
        source.clone(),
    );

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("context_compile"));
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 0);
    assert_eq!(
        crate::ContextCompiler::new()
            .compile(&runtime.context_snapshot().await)
            .expect("context compiles after bad projection cleanup")
            .to_snapshot(),
        ""
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_pending_memory_activation_emits_cancelled_without_provider_call() {
    let (source, activation_started_rx, activation_dropped_rx) = pending_memory_activation_source();
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider_and_memory_source(
        "runtime-memory-pending-activation-cancel",
        provider.clone(),
        source.clone(),
    );
    let token = CancellationToken::new();
    let mut stream = runtime
        .step(
            crate::StepInput::user_text("Topic request.").expect("valid step input"),
            crate::StepContext::new(token.clone()),
        )
        .expect("step should start");

    assert!(matches!(
        stream.next().await.expect("session started event"),
        RuntimeEvent {
            kind: RuntimeEventKind::SessionStarted,
            ..
        }
    ));
    assert!(matches!(
        stream.next().await.expect("step started event"),
        RuntimeEvent {
            kind: RuntimeEventKind::StepStarted,
            ..
        }
    ));
    activation_started_rx
        .await
        .expect("activation future should start");

    token.cancel();
    activation_dropped_rx
        .await
        .expect("activation future should be dropped on cancellation");
    let remaining: Vec<_> = stream.collect().await;

    assert_eq!(event_kind_names(&remaining), ["Cancelled"]);
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 0);
    assert_activated_memory_projection_cleared(&runtime).await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_stream_while_memory_activation_pending_drops_activation_without_provider_call() {
    let (source, activation_started_rx, activation_dropped_rx) = pending_memory_activation_source();
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider_and_memory_source(
        "runtime-memory-pending-activation-drop",
        provider.clone(),
        source.clone(),
    );
    let mut stream = runtime
        .step(
            crate::StepInput::user_text("Topic request.").expect("valid step input"),
            crate::StepContext::new(CancellationToken::new()),
        )
        .expect("step should start");

    assert!(matches!(
        stream.next().await.expect("session started event"),
        RuntimeEvent {
            kind: RuntimeEventKind::SessionStarted,
            ..
        }
    ));
    assert!(matches!(
        stream.next().await.expect("step started event"),
        RuntimeEvent {
            kind: RuntimeEventKind::StepStarted,
            ..
        }
    ));
    activation_started_rx
        .await
        .expect("activation future should start");

    drop(stream);
    activation_dropped_rx
        .await
        .expect("activation future should be dropped when stream is dropped");
    tokio::task::yield_now().await;

    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 0);
    assert_activated_memory_projection_cleared(&runtime).await;
}
