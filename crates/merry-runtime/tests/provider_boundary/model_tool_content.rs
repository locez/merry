use crate::support::{
    events::{event_kind_names, pending_tool_call, resolved_tool_result},
    models::{
        ScriptedModelProvider, completed_outputs_event, completed_text_event, model_tool_call,
    },
    runtime::{collect_step, runtime_with_registered_tool},
    tools::{ScriptedToolExecutor, ToolExecutorResponse},
};
use merry_llm::{FinishReason, ModelOutput};
use merry_runtime::{ArtifactContent, ToolExecutionContext, ToolExecutionOutcome};

#[tokio::test]
async fn provider_replays_only_model_body_while_artifact_remains_exact() {
    let full = "display-only details ".repeat(300);
    let body = "found: exact result\n";
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used result"))],
        vec![Ok(completed_text_event("still using result"))],
    ]);
    let executor = ScriptedToolExecutor::new(ToolExecutorResponse::Outcome(
        ToolExecutionOutcome::succeeded_text(&full).with_model_text(body),
    ));
    let runtime = runtime_with_registered_tool("model-body-replay", provider.clone(), executor);
    let events = collect_step(&runtime, "lookup").await;
    let pending = pending_tool_call(&events);
    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("execute");
    assert_eq!(
        event_kind_names(&events),
        ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(
        runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("full artifact"),
        ArtifactContent::text(&full)
    );
    collect_step(&runtime, "continue").await;
    collect_step(&runtime, "continue again").await;
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    for request in &requests[1..] {
        let result = request
            .continuations()
            .first()
            .expect("continuation")
            .result();
        assert_eq!(result.content().as_text(), Some(body));
        assert!(result.content().as_str().len() * 10 < full.len());
        assert_eq!(request.tools(), requests[0].tools());
        assert_eq!(
            request.stable_prefix_hash(),
            requests[0].stable_prefix_hash()
        );
    }
}
