use super::support::*;

#[tokio::test]
async fn read_success_and_failure_continue_with_compact_body_and_full_artifact() {
    for path in ["note.txt", "missing.txt"] {
        let temp = TempWorkspace::new("read-model-body");
        temp.write_text("note.txt", "alpha\n");
        let provider = ScriptedModelProvider::new(vec![
            vec![Ok(pending_read_text_call(path))],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("done")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime =
            runtime_with_opt_in_apply_patch_tools_and_provider(temp.path(), provider.clone());
        let events = execute_first_pending_call(&runtime, "read file").await;
        let result = resolved_tool_result(&events);
        let full = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("full artifact");
        let full: Value =
            serde_json::from_str(full.as_text().expect("JSON artifact")).expect("JSON");
        let events = collect_step(&runtime, "continue").await;
        assert_eq!(
            event_kind_names(&events),
            ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
        );
        let requests = provider.recorded_requests();
        let model_result = requests[1]
            .continuations()
            .first()
            .expect("continuation")
            .result();
        assert_eq!(model_result.status(), result.status());
        if path == "note.txt" {
            assert_eq!(
                model_result.content().as_text(),
                Some("note.txt:1-1\nalpha\n")
            );
            assert_eq!(full["content"], "alpha\n");
        } else {
            let model: Value =
                serde_json::from_str(model_result.content().as_json().expect("failure JSON"))
                    .expect("JSON");
            assert_eq!(model["error"], full["error"]);
            assert_eq!(model["guidance"], full["guidance"]["message"]);
            assert!(model.get("recovery").is_none());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn registered_read_text_tool_records_artifact_before_resolving_pending_call() {
    let temp = TempWorkspace::new("event-order");
    temp.write_text("note.txt", "alpha\n");
    let runtime = runtime_with_workspace_tools(temp.path(), pending_read_text_call("note.txt"));

    let pending_events = collect_step(&runtime, "read note").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("registered read_text tool should execute");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
    );
    let result = match &execution_events[2].payload {
        RuntimeJournalPayload::ToolCallResolved { result } => result,
        other => panic!("expected tool resolution, got {other:?}"),
    };
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn registered_read_text_domain_failure_records_failed_json_before_resolving_pending_call() {
    let temp = TempWorkspace::new("domain-failure");
    let runtime = runtime_with_workspace_tools(temp.path(), pending_read_text_call("missing.txt"));

    let pending_events = collect_step(&runtime, "read missing note").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("domain failure should resolve pending call");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
    );
    let result = match &execution_events[2].payload {
        RuntimeJournalPayload::ToolCallResolved { result } => result,
        other => panic!("expected tool resolution, got {other:?}"),
    };
    assert!(matches!(
        &execution_events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should include diagnostic")
            .code(),
        "workspace_file_not_found"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}
