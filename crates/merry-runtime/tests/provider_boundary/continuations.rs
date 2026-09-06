use crate::support::{
    events::{assert_no_failed, event_kind_names, failed_code, pending_tool_call},
    models::{
        ScriptedModelProvider, ScriptedProviderStep, completed_outputs_event, completed_text_event,
        model_tool_call, model_tool_call_with_args, model_tool_call_with_id,
    },
    runtime::{artifact_id, collect_step, runtime_with_scripted_provider},
};
use merry_core::{ArtifactKind, ArtifactRef, ToolCallResult, ToolCallResultStatus};
use merry_llm::{FinishReason, ModelError, ModelInputItem, ModelOutput, ProviderErrorKind};
use merry_runtime::ArtifactContent;
use serde_json::{Map, json};

#[tokio::test(flavor = "current_thread")]
async fn provider_request_keeps_tool_exchange_before_later_user_turn() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-read"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-transcript-order", provider.clone());

    let first_events = collect_step(&runtime, "Read the file.").await;
    let pending = pending_tool_call(&first_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                pending.id().clone(),
                ArtifactRef::new(
                    artifact_id("provider-transcript-result"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("file contents\n"),
        )
        .await
        .expect("tool result should resolve");

    collect_step(&runtime, "Now answer a new request.").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let dynamic = requests[1].dynamic_input();
    assert!(matches!(dynamic[0], ModelInputItem::Message(_)));
    assert!(matches!(dynamic[1], ModelInputItem::ToolCall(_)));
    assert!(matches!(dynamic[2], ModelInputItem::ToolResult(_)));
    assert!(matches!(dynamic[3], ModelInputItem::Message(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn submitted_tool_result_is_compiled_as_provider_neutral_continuation() {
    let arguments = Map::from_iter([
        ("query".to_owned(), json!("runtime continuation")),
        ("limit".to_owned(), json!(2)),
    ]);
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_args(
                "call.provider/opaque.id:42",
                "search_notes",
                arguments.clone(),
            ))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after search"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-tool-continuation", provider.clone());
    let pending_events = collect_step(&runtime, "Request a search.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact = ArtifactRef::new(
        artifact_id("manual-result-continuation-text"),
        ArtifactKind::Text,
    );
    let result = ToolCallResult::succeeded(call.id().clone(), result_artifact.clone());
    runtime
        .submit_tool_result(result, ArtifactContent::text("exact search result\n"))
        .await
        .expect("tool result should resolve");

    let events = collect_step(&runtime, "Use the tool result.").await;

    assert_eq!(
        event_kind_names(&events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("continuation should be compiled");
    assert_eq!(
        continuation.call().id().as_str(),
        "call.provider/opaque.id:42"
    );
    assert_eq!(continuation.call().name().as_str(), "search_notes");
    assert_eq!(continuation.call().arguments().as_object(), &arguments);
    assert_eq!(
        continuation.result().call_id().as_str(),
        "call.provider/opaque.id:42"
    );
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(
        continuation.result().content().as_text(),
        Some("exact search result\n")
    );
    assert!(continuation.result().diagnostic().is_none());

    let value = serde_json::to_value(&requests[1]).expect("request should serialize");
    assert!(value.get("session_id").is_none());
    assert!(value.get("ledger_id").is_none());
    assert!(value.get("artifact_id").is_none());
    assert!(value.get("previous_response_id").is_none());
    assert!(value.get("store").is_none());
    assert!(value.get("tool_call_id").is_none());
    assert!(
        value["continuations"][0]["call"]
            .get("tool_call_id")
            .is_none()
    );
    assert!(
        value["continuations"][0]["result"]
            .get("tool_call_id")
            .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn successful_provider_step_keeps_tool_exchange_until_compaction() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used tool result"))],
        vec![Ok(completed_text_event("fresh request"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-exchange-success", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact = ArtifactRef::new(
        artifact_id("manual-result-tool-exchange-success"),
        ArtifactKind::Text,
    );
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(call.id().clone(), result_artifact),
            ArtifactContent::text("result remains raw before compaction\n"),
        )
        .await
        .expect("tool result should resolve");

    let continuation_events = collect_step(&runtime, "Use tool result.").await;
    let next_events = collect_step(&runtime, "Continue without compaction.").await;

    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert_eq!(
        event_kind_names(&next_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[2].continuations().len(),
        1,
        "successful provider completion is not checkpoint/compaction"
    );
    assert_eq!(
        requests[2].continuations()[0].call().id().as_str(),
        requests[1].continuations()[0].call().id().as_str()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn new_pending_tool_call_keeps_prior_tool_exchange() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-old"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-new"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used both results"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-new-pending", provider.clone());
    let pending_events = collect_step(&runtime, "Request first tool.").await;
    let old_call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                old_call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-before-new-pending"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("old result\n"),
        )
        .await
        .expect("old tool result should resolve");

    let new_pending_events = collect_step(&runtime, "Use old result and request another.").await;
    let new_call = pending_tool_call(&new_pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                new_call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-after-new-pending"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("new result\n"),
        )
        .await
        .expect("new tool result should resolve");
    let completed_events = collect_step(&runtime, "Use all resolved tool results.").await;

    assert_eq!(
        event_kind_names(&new_pending_events),
        ["StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_tool_call(&new_pending_events).id().as_str(),
        "call-new"
    );
    assert_eq!(
        event_kind_names(&completed_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-old"
    );
    assert_eq!(
        requests[2]
            .continuations()
            .iter()
            .map(|continuation| continuation.call().id().as_str())
            .collect::<Vec<_>>(),
        ["call-old", "call-new"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_new_tool_call_id_keeps_tool_exchange_for_retry() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-old"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-old"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("retry uses old result"))],
    ]);
    let runtime = runtime_with_scripted_provider(
        "provider-tool-continuation-duplicate-new-id",
        provider.clone(),
    );
    let pending_events = collect_step(&runtime, "Request first tool.").await;
    let old_call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                old_call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-before-duplicate-new-id"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("old result\n"),
        )
        .await
        .expect("old tool result should resolve");

    let duplicate_events = collect_step(&runtime, "Provider repeats resolved id.").await;
    let retry_events = collect_step(&runtime, "Retry after duplicate id.").await;

    assert_eq!(
        event_kind_names(&duplicate_events),
        ["StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&duplicate_events), Some("tool_call_duplicate"));
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-old"
    );
    assert_eq!(
        requests[2].continuations()[0].call().id().as_str(),
        "call-old"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_error_keeps_tool_exchange_for_retry() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Err(ModelError::provider(
            ProviderErrorKind::Protocol,
            "transient continuation failure",
        ))],
        vec![Ok(completed_text_event("retry succeeded"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-error", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-retry-after-error"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("retry me\n"),
        )
        .await
        .expect("tool result should resolve");

    let error_events = collect_step(&runtime, "Use result, provider fails.").await;
    let retry_events = collect_step(&runtime, "Retry with same result.").await;

    assert_eq!(event_kind_names(&error_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&error_events), Some("model_protocol"));
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_setup_error_keeps_tool_exchange_for_retry() {
    let provider = ScriptedModelProvider::new_steps(vec![
        ScriptedProviderStep::Stream(vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))]),
        ScriptedProviderStep::SetupError(ModelError::provider(
            ProviderErrorKind::Unavailable,
            "setup unavailable",
        )),
        ScriptedProviderStep::Stream(vec![Ok(completed_text_event("retry after setup"))]),
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-setup-error", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-retry-after-setup-error"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("retry after setup\n"),
        )
        .await
        .expect("tool result should resolve");

    let error_events = collect_step(&runtime, "Setup fails.").await;
    let retry_events = collect_step(&runtime, "Retry setup.").await;

    assert_eq!(event_kind_names(&error_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&error_events), Some("model_unavailable"));
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_cancel_keeps_tool_exchange_for_retry() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Err(ModelError::Cancelled)],
        vec![Ok(completed_text_event("retry after cancel"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-cancel", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-retry-after-cancel"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("retry after cancel\n"),
        )
        .await
        .expect("tool result should resolve");

    let cancel_events = collect_step(&runtime, "Provider cancels.").await;
    let retry_events = collect_step(&runtime, "Retry after cancel.").await;

    assert_eq!(
        event_kind_names(&cancel_events),
        ["StepStarted", "Cancelled"]
    );
    assert_no_failed(&cancel_events);
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
}
