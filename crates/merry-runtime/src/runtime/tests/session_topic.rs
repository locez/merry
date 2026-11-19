use std::sync::Arc;

use crate::{
    FileSessionStore, RuntimeModelRole,
    runtime::{
        Runtime,
        tests::support::{
            common::{collect_step, named_model, session_id},
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
        },
    },
};
use merry_llm::{FinishReason, ModelEvent, ModelOutput, ModelResponse};

fn topic_event(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn first_user_turn_names_the_session_once_and_resume_keeps_it() {
    let primary = RecordingModelProvider::new();
    let summary = RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(
        vec![Ok(topic_event("\"Persistence refactor\""))],
    )]);
    let runtime = Runtime::builder(session_id("runtime-session-topic"))
        .model_provider(Arc::new(primary), named_model("fake/primary"))
        .model_provider_for_role(
            RuntimeModelRole::SummaryMemory,
            Arc::new(summary.clone()),
            named_model("fake/summary"),
        )
        .build()
        .expect("runtime builds");

    collect_step(
        &runtime,
        "rewrite the session journal",
        crate::StepContext::default(),
    )
    .await;
    assert_eq!(
        runtime.session_topic().await.as_deref(),
        Some("Persistence refactor")
    );

    collect_step(&runtime, "a later question", crate::StepContext::default()).await;
    assert_eq!(summary.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        runtime.session_topic().await.as_deref(),
        Some("Persistence refactor")
    );

    let saved = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(saved.path());
    runtime
        .save_session_to(store.clone())
        .await
        .expect("session saves");
    let resumed = Runtime::builder(session_id("runtime-session-topic"))
        .model_provider(
            Arc::new(RecordingModelProvider::new()),
            named_model("fake/primary"),
        )
        .model_provider_for_role(
            RuntimeModelRole::SummaryMemory,
            Arc::new(summary),
            named_model("fake/summary"),
        )
        .resume_from_store(store)
        .await
        .expect("session resumes");
    assert_eq!(
        resumed.session_topic().await.as_deref(),
        Some("Persistence refactor")
    );
}
