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
    let text = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::AssistantOutputRecorded { text } => Some(text.as_str()),
            _ => None,
        })
        .expect("assistant output should be recorded");
    assert_eq!(text, "successful attempt");
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

#[tokio::test(flavor = "current_thread")]
async fn model_invalid_tool_call_after_commentary_retries_and_discards_partial_attempt() {
    let stale_call = merry_llm::ModelToolCall::new(
        merry_llm::ModelToolCallId::new("call_stale").expect("valid call id"),
        merry_core::ToolName::new("lookup").expect("valid tool name"),
        merry_llm::ToolArguments::try_from(serde_json::json!({ "query": "stale" }))
            .expect("valid arguments"),
    );
    let recovered_call = merry_llm::ModelToolCall::new(
        merry_llm::ModelToolCallId::new("call_recovered").expect("valid call id"),
        merry_core::ToolName::new("lookup").expect("valid tool name"),
        merry_llm::ToolArguments::try_from(serde_json::json!({ "query": "recovered" }))
            .expect("valid arguments"),
    );
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: "stale commentary before truncated json".to_owned(),
            }),
            Ok(ModelEvent::ToolCallRequested { call: stale_call }),
            Err(ModelError::provider(
                ProviderErrorKind::InvalidToolCall,
                "function call arguments must be valid JSON: EOF while parsing an object",
            )),
        ]),
        ScriptedModelProviderResponse::Stream(vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: "fresh commentary".to_owned(),
            }),
            Ok(ModelEvent::ToolCallRequested {
                call: recovered_call.clone(),
            }),
            Ok(completed_event_with(
                vec![
                    ModelOutput::text("fresh commentary"),
                    ModelOutput::tool_call(recovered_call.clone()),
                ],
                FinishReason::ToolCalls,
            )),
        ]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-retry-invalid-tool-call-commentary"))
        .model_retry_policy(
            ModelRetryPolicy::new(
                true,
                3,
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(200),
                false,
            )
            .expect("valid retry policy"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "Retry truncated tool call after commentary.",
        crate::StepContext::default(),
    )
    .await;

    assert_eq!(provider.recorded_requests().len(), 2);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::Failed { .. })),
        "retried turn should not fail: {events:?}"
    );
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        RuntimeJournalPayload::ModelRetryScheduled {
            attempt: 1,
            next_attempt: 2,
            error_kind,
            ..
        } if error_kind == "invalid_tool_call"
    )));
    let recorded_text = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::AssistantOutputRecorded { text } => Some(text.as_str()),
            _ => None,
        })
        .expect("retried commentary should be recorded");
    assert_eq!(recorded_text, "fresh commentary");
    let requested_ids = events
        .iter()
        .filter_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => Some(call.id().as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(requested_ids, ["call_recovered"]);
}
