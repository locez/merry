use crate::{
    runtime::{
        Runtime,
        tests::support::{
            common::{
                completed_event_with, event_kind_names, model_name, model_tool_call, session_id,
            },
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
        },
    },
    session::{ModelTurnId, ModelTurnStatus},
};
use futures_util::StreamExt;
use merry_core::RuntimeJournalPayload;
use merry_llm::{FinishReason, ModelOutput};
use std::{num::NonZeroUsize, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn cancellation_after_atomic_tool_response_preserves_awaiting_turn() {
    let call = model_tool_call("reduced-before-cancel");
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![
                    ModelOutput::text("Tool commentary."),
                    ModelOutput::tool_call(call),
                ],
                FinishReason::ToolCalls,
            ),
        )])]);
    let runtime = Runtime::builder(session_id("runtime-reduced-tool-response-cancel"))
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero event buffer"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    let token = CancellationToken::new();
    let mut stream = runtime
        .step(
            crate::StepInput::user_text("Request a tool call.").expect("valid input"),
            crate::StepContext::new(token.clone()),
        )
        .expect("step should start");

    assert!(matches!(
        stream.next().await.expect("session start event").payload,
        RuntimeJournalPayload::SessionStarted
    ));
    assert!(matches!(
        stream.next().await.expect("step start event").payload,
        RuntimeJournalPayload::StepStarted
    ));
    let commentary = stream.next().await.expect("commentary event");
    assert!(matches!(
        commentary.payload,
        RuntimeJournalPayload::AssistantOutputRecorded { .. }
    ));
    token.cancel();
    let remaining = stream.collect::<Vec<_>>().await;

    assert_eq!(
        event_kind_names(&remaining),
        ["ToolCallPending"],
        "the already-committed response must drain without retroactive cancellation"
    );
    assert!(
        remaining[0].sequence == commentary.sequence + 1,
        "the atomic response batch must preserve contiguous sequences"
    );
    assert_eq!(runtime.pending_tool_calls().await.len(), 1);
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::AwaitingToolResults)
    );
}
