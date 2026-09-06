use crate::support::{
    events::{
        assert_no_completion, assert_no_failed, assistant_output_artifact, event_kind_names,
        pending_tool_call,
    },
    models::{
        ScriptedModelProvider, completed_event, completed_outputs_event, completed_text_event,
        model_name, model_tool_call,
    },
    runtime::{
        artifact_id, collect_step, runtime_with_provider, runtime_with_scripted_provider,
        session_id,
    },
};
use merry_core::{ArtifactKind, ArtifactRef, ToolCallResult};
use merry_llm::{
    FinishReason, ModelEvent, ModelMessageRole, ModelOutput, testing::FakeModelProvider,
};
use merry_runtime::{ArtifactContent, Runtime};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn runtime_profile_progress_commentary_adds_stable_prefix_guidance() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let profile = merry_runtime::RuntimeProfile::builder()
        .progress_commentary(true)
        .build()
        .expect("profile should build");
    let runtime = Runtime::builder(session_id("provider-progress-commentary-profile"))
        .with_profile(profile)
        .expect("profile should install")
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "Inspect progress commentary config.").await;

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
    let request = &requests[0];
    assert_eq!(request.messages().len(), 3);
    assert_eq!(request.stable_prefix_message_count(), 2);
    assert_eq!(request.messages()[1].role(), ModelMessageRole::System);
    assert!(
        request.messages()[1]
            .content()
            .as_text()
            .contains("Do not add a progress note before routine")
    );
    assert!(
        request.messages()[1]
            .content()
            .as_text()
            .contains("user's current input language")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_mixed_text_and_tool_call_records_commentary_and_pending() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![
            ModelOutput::text("partial answer"),
            ModelOutput::tool_call(model_tool_call()),
        ],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-mixed-output", provider);

    let events = collect_step(&runtime, "Mix text and tool call.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "ToolCallPending"
        ]
    );
    let artifact = assistant_output_artifact(&events);
    let content = runtime
        .read_artifact_content(artifact.id())
        .await
        .expect("commentary artifact should be readable");
    assert_eq!(content.as_text(), Some("partial answer"));
    assert_eq!(pending_tool_call(&events).id().as_str(), "call-1");
    assert_no_failed(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_tool_call_after_non_empty_text_delta_records_commentary_and_pending() {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::OutputTextDelta {
            delta: "thinking aloud".to_owned(),
        }),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call(),
        }),
        Ok(completed_outputs_event(
            vec![
                ModelOutput::text("thinking aloud"),
                ModelOutput::tool_call(model_tool_call()),
            ],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-after-text-delta", provider);

    let events = collect_step(&runtime, "Emit text before tool call.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputDelta",
            "AssistantOutputRecorded",
            "ToolCallPending"
        ]
    );
    let artifact = assistant_output_artifact(&events);
    let content = runtime
        .read_artifact_content(artifact.id())
        .await
        .expect("commentary artifact should be readable");
    assert_eq!(content.as_text(), Some("thinking aloud"));
    assert_eq!(pending_tool_call(&events).id().as_str(), "call-1");
    assert_no_failed(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn tool_progress_commentary_is_replayed_to_next_provider_step() {
    let provider = ScriptedModelProvider::new(vec![
        vec![
            Ok(ModelEvent::OutputTextDelta {
                delta: "I will inspect the notes first.".to_owned(),
            }),
            Ok(ModelEvent::ToolCallRequested {
                call: model_tool_call(),
            }),
            Ok(completed_outputs_event(
                vec![
                    ModelOutput::text("I will inspect the notes first."),
                    ModelOutput::tool_call(model_tool_call()),
                ],
                FinishReason::ToolCalls,
            )),
        ],
        vec![Ok(completed_text_event("continued"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-commentary-replay", provider.clone());

    let pending_events = collect_step(&runtime, "Need notes.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact =
        ArtifactRef::new(artifact_id("manual-result-commentary"), ArtifactKind::Text);
    let result = ToolCallResult::succeeded(call.id().clone(), result_artifact.clone());
    runtime
        .submit_tool_result(result, ArtifactContent::text("note result\n"))
        .await
        .expect("tool result should resolve");
    let final_events = collect_step(&runtime, "Continue.").await;

    assert_eq!(
        event_kind_names(&final_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].messages().iter().any(|message| {
            message.role() == ModelMessageRole::Assistant
                && message.content().as_text() == "I will inspect the notes first."
        }),
        "tool-progress commentary should be stored as assistant history for provider continuity"
    );
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].result().content().as_str(),
        "note result\n"
    );
}
