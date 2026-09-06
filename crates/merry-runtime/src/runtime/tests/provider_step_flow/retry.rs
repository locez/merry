use crate::{
    runtime::{
        Runtime,
        tests::support::{
            common::{
                collect_step, completed_event_with, event_kind_names, model_name, session_id,
            },
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
        },
    },
    session::{ModelTurnId, ModelTurnStatus},
};
use merry_core::RuntimeJournalPayload;
use merry_llm::{
    FinishReason, ModelError, ModelEvent, ModelOutput, ModelRetryPolicy, ProviderErrorKind,
};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn model_retry_events_are_emitted_for_failure_before_output() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![
            Ok(ModelEvent::Started),
            Err(ModelError::provider(
                ProviderErrorKind::Unavailable,
                "stream interrupted",
            )),
        ]),
        ScriptedModelProviderResponse::Stream(vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: "successful attempt".to_owned(),
            }),
            Ok(completed_event_with(
                vec![ModelOutput::text("successful attempt")],
                FinishReason::Stop,
            )),
        ]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-model-retry-events"))
        .model_retry_policy(
            ModelRetryPolicy::new(
                true,
                3,
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(100),
                false,
            )
            .expect("valid retry policy"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "Retry provider stream.",
        crate::StepContext::default(),
    )
    .await;

    assert_eq!(provider.recorded_requests().len(), 2);
    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ModelRetryAttemptStarted",
            "ModelRetryScheduled",
            "ModelRetryAttemptStarted",
            "AssistantOutputDelta",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    let artifact_id = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::AssistantOutputRecorded { artifact } => {
                Some(artifact.id().clone())
            }
            _ => None,
        })
        .expect("assistant output artifact should be recorded");
    let content = runtime
        .read_artifact_content(&artifact_id)
        .await
        .expect("artifact should be readable");
    assert_eq!(content.as_text(), Some("successful attempt"));
    let session = runtime.inner.session.lock().await;
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(session.model_turn_status(ModelTurnId::new(2)), None);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_setup_retry_reuses_one_model_turn() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::SetupError(ModelError::provider(
            ProviderErrorKind::Unavailable,
            "retry setup",
        )),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::text("setup retry succeeded")],
            FinishReason::Stop,
        ))]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-setup-retry-turn"))
        .model_retry_policy(
            ModelRetryPolicy::new(
                true,
                2,
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(100),
                false,
            )
            .expect("valid retry policy"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "Retry setup with one turn.",
        crate::StepContext::default(),
    )
    .await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    assert_eq!(provider.recorded_requests().len(), 2);
    let session = runtime.inner.session.lock().await;
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(session.model_turn_status(ModelTurnId::new(2)), None);
}

#[tokio::test(flavor = "current_thread")]
async fn model_stream_failure_after_output_is_not_retried_or_recorded_as_complete() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: "visible partial output".to_owned(),
            }),
            Err(ModelError::provider(
                ProviderErrorKind::Unavailable,
                "stream interrupted",
            )),
        ]),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::text("must not be replayed")],
            FinishReason::Stop,
        ))]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-model-no-retry-after-output"))
        .model_retry_policy(
            ModelRetryPolicy::new(
                true,
                3,
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(100),
                false,
            )
            .expect("valid retry policy"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "Do not replay visible output.",
        crate::StepContext::default(),
    )
    .await;

    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ModelRetryAttemptStarted",
            "AssistantOutputDelta",
            "Failed",
        ]
    );
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        RuntimeJournalPayload::AssistantOutputDelta { delta }
            if delta == "visible partial output"
    )));
    assert!(!events.iter().any(|event| matches!(
        event.payload,
        RuntimeJournalPayload::AssistantOutputRecorded { .. }
            | RuntimeJournalPayload::StepCompleted
    )));
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
