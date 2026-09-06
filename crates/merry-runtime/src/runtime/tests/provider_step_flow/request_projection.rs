use crate::{
    artifact::ArtifactContent,
    runtime::{
        Runtime,
        tests::support::{
            common::{
                artifact_id, collect_step, completed_event_with, event_kind_names, failed_code,
                model_name, model_tool_call, session_id,
            },
            memory::{
                assert_activated_memory_projection_cleared,
                assert_activated_memory_projection_retained,
                runtime_with_provider_and_single_memory,
            },
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
            runtime_factories::runtime_with_provider,
        },
    },
    session::{ModelTurnId, ModelTurnStatus},
};
use futures_util::StreamExt;
use merry_core::{ArtifactKind, ArtifactRef, RuntimeJournalPayload, ToolCallResult};
use merry_llm::{FinishReason, ModelOutput};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn step_projection_is_updated_before_journal_stream_is_polled() {
    let (provider_started_tx, provider_started_rx) = oneshot::channel();
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::PendingSetup(
            provider_started_tx,
        )]);
    let runtime = runtime_with_provider("runtime-trajectory-before-poll", provider);

    let stream = runtime
        .step(
            crate::StepInput::user_text("observe before polling").expect("valid step input"),
            crate::StepContext::default(),
        )
        .expect("step should start");
    provider_started_rx
        .await
        .expect("provider setup should reach the runtime");

    let snapshot = runtime
        .trajectory_snapshot()
        .await
        .expect("trajectory snapshot should be readable");
    assert!(
        snapshot.latest_sequence() >= 1,
        "step lifecycle events must project before a consumer polls the journal stream"
    );

    drop(stream);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stream_context_uses_runtime_session_as_prompt_cache_key() {
    let provider = RecordingModelProvider::new();
    let runtime = Runtime::builder(session_id("runtime-cache-key"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "Use the runtime session as the cache key.",
        crate::StepContext::default(),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    let contexts = provider.recorded_contexts();
    assert_eq!(contexts.len(), 1);
    assert_eq!(
        contexts[0]
            .prompt_cache_key()
            .expect("prompt cache key should be set")
            .as_str(),
        "runtime-cache-key"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn coordinator_request_explains_when_no_plan_is_active() {
    let provider = RecordingModelProvider::new();
    let runtime = Runtime::builder(session_id("runtime-plan-inactive-context"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .coordinator_plan_tools()
        .build()
        .expect("runtime should build");

    collect_step(
        &runtime,
        "Inspect the repository and decide what to do.",
        crate::StepContext::default(),
    )
    .await;

    let request = provider
        .recorded_requests()
        .into_iter()
        .next()
        .expect("provider request should be recorded");
    let request_text = request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(request_text.contains("<plan_context>"));
    assert!(request_text.contains("inactive"));
    assert!(request_text.contains("Do not call read_plan"));
}

#[tokio::test(flavor = "current_thread")]
async fn user_text_burst_records_every_item_in_one_model_turn() {
    let runtime = Runtime::builder(session_id("runtime-user-burst-one-turn"))
        .model_provider(Arc::new(RecordingModelProvider::new()), model_name())
        .build()
        .expect("runtime should build");
    let stream = runtime
        .step(
            crate::StepInput::user_texts(["first exact user item", "second exact user item"])
                .expect("valid user burst"),
            crate::StepContext::default(),
        )
        .expect("step should start");
    let events = stream.collect::<Vec<_>>().await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .transcript_model_turn_ids_for_tests(),
        [
            ModelTurnId::new(1),
            ModelTurnId::new(1),
            ModelTurnId::new(1),
        ],
        "both user source items and the response belong to one durable model turn"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_step_during_provider_setup_with_held_session_lock_defers_projection_cleanup() {
    let (provider_started_tx, provider_started_rx) = oneshot::channel();
    let (provider_dropped_tx, provider_dropped_rx) = oneshot::channel();
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::PendingSetupWithDrop {
            started: provider_started_tx,
            dropped: provider_dropped_tx,
        },
    ]);
    let (runtime, source) = runtime_with_provider_and_single_memory(
        "runtime-memory-provider-setup-drop-spawned-cleanup",
        provider.clone(),
        "memory-provider-setup-drop-spawned",
        "Activated memory is cleared by spawned cleanup when drop cannot lock session.",
        "memory-provider-setup-drop-spawned-artifact",
    );

    let stream = runtime
        .step(
            crate::StepInput::user_text("Topic request.").expect("valid step input"),
            crate::StepContext::new(CancellationToken::new()),
        )
        .expect("step should start");
    provider_started_rx
        .await
        .expect("provider setup future should start");

    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-setup-drop-spawned",
        "Activated memory is cleared by spawned cleanup when drop cannot lock session.",
    )
    .await;

    let session = runtime.inner.session.lock().await;
    drop(stream);
    provider_dropped_rx
        .await
        .expect("provider setup future should be aborted");
    tokio::task::yield_now().await;

    let snapshot = crate::ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("context compiles while cleanup waits for session lock")
        .to_snapshot();
    assert!(
        snapshot.contains("memory:memory-provider-setup-drop-spawned"),
        "projection should remain while spawned cleanup is waiting for session lock; snapshot:\n{snapshot}"
    );
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::InProgress),
        "turn cleanup must wait without blocking while the session lock is held"
    );

    drop(session);
    for _ in 0..32 {
        if runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1))
            == Some(ModelTurnStatus::Aborted)
        {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert_activated_memory_projection_cleared(&runtime).await;
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Aborted)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_continuation_without_user_input_starts_a_new_model_turn() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::text("first response")],
            FinishReason::Stop,
        ))]),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::text("continuation response")],
            FinishReason::Stop,
        ))]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-no-input-new-turn"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "Start the loop.", crate::StepContext::default()).await;
    runtime
        .step(
            crate::StepInput::no_new_user_input(),
            crate::StepContext::default(),
        )
        .expect("continuation step should start")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(provider.recorded_requests().len(), 2);
    let session = runtime.inner.session.lock().await;
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(2)),
        Some(ModelTurnStatus::Completed)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_tool_call_response_aborts_turn_before_failed_event() {
    let duplicate = model_tool_call("duplicate-turn-call");
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::tool_call(duplicate.clone())],
            FinishReason::ToolCalls,
        ))]),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![
                ModelOutput::text("commentary that must not partially commit"),
                ModelOutput::tool_call(duplicate),
            ],
            FinishReason::ToolCalls,
        ))]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-duplicate-call-aborts-turn"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    collect_step(
        &runtime,
        "Request the first call.",
        crate::StepContext::default(),
    )
    .await;
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("first call should be pending");
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                pending.id().clone(),
                ArtifactRef::new(artifact_id("duplicate-turn-result"), ArtifactKind::Text),
            ),
            ArtifactContent::text("resolved"),
        )
        .await
        .expect("first call should resolve");
    let transcript_before = runtime
        .inner
        .session
        .lock()
        .await
        .full_transcript_snapshot()
        .expect("transcript should be readable");

    let events = collect_step(
        &runtime,
        "Repeat the same call id.",
        crate::StepContext::default(),
    )
    .await;

    assert_eq!(failed_code(&events), Some("tool_call_duplicate"));
    assert_eq!(
        event_kind_names(&events),
        ["StepStarted", "Failed"],
        "a rejected compound response must not hide a committed commentary event"
    );
    assert!(
        events
            .windows(2)
            .all(|pair| pair[1].sequence == pair[0].sequence + 1),
        "rejected response events must not contain an unobservable sequence gap"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::Cancelled { .. }))
    );
    let session = runtime.inner.session.lock().await;
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(2)),
        Some(ModelTurnStatus::Aborted)
    );
    let transcript_after = session
        .full_transcript_snapshot()
        .expect("transcript should remain readable");
    assert_eq!(
        &transcript_after[..transcript_before.len()],
        transcript_before
    );
    assert!(matches!(
        transcript_after.last(),
        Some(crate::session::TranscriptItemSnapshot::UserMessage { text, .. })
            if text == "Repeat the same call id."
    ));
    assert_eq!(
        transcript_after.len(),
        transcript_before.len() + 1,
        "only the new user source may survive rejected tool-call admission"
    );
    assert_eq!(
        session.next_sequence(),
        events.last().expect("failed event should exist").sequence + 1
    );
}
