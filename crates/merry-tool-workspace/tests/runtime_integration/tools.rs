use super::support::*;

#[tokio::test(flavor = "current_thread")]
async fn registered_read_file_tool_records_artifact_before_resolving_pending_call() {
    let temp = TempWorkspace::new("event-order");
    temp.write_text("note.txt", "alpha\n");
    let runtime = runtime_with_workspace_tools(temp.path(), pending_read_file_call("note.txt"));

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
        .expect("registered read file tool should execute");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = match &execution_events[1].payload {
        RuntimeJournalPayload::ToolCallResolved { result } => result,
        other => panic!("expected tool resolution, got {other:?}"),
    };
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn registered_read_file_domain_failure_records_failed_json_before_resolving_pending_call() {
    let temp = TempWorkspace::new("domain-failure");
    let runtime = runtime_with_workspace_tools(temp.path(), pending_read_file_call("missing.txt"));

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
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = match &execution_events[1].payload {
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

#[tokio::test(flavor = "current_thread")]
async fn registered_list_dir_tool_records_json_artifact_before_resolving_pending_call() {
    let temp = TempWorkspace::new("list-success");
    temp.write_text("notes/alpha.txt", "alpha\n");
    temp.write_text("notes/nested/beta.txt", "beta\n");
    let runtime = runtime_with_workspace_tools(temp.path(), pending_list_dir_call("notes"));

    let execution_events = execute_first_pending_call(&runtime, "list notes").await;

    assert_succeeded_json_result(&execution_events);
}

#[tokio::test(flavor = "current_thread")]
async fn registered_list_dir_domain_failure_records_failed_json_without_runtime_failed() {
    let temp = TempWorkspace::new("list-domain-failure");
    temp.write_text("notes/alpha.txt", "alpha\n");
    let (runtime, provider) =
        runtime_with_workspace_tools_and_provider(temp.path(), pending_list_dir_call("../outside"));

    let execution_events = execute_first_pending_call(&runtime, "list outside").await;

    assert_failed_json_result(&execution_events, "workspace_path_denied");
    assert_failed_json_artifact_visible_in_next_model_request(
        &runtime,
        &provider,
        WORKSPACE_LIST_DIR_TOOL,
        "workspace_path_denied",
        temp.path(),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn registered_search_text_tool_records_json_artifact_before_resolving_pending_call() {
    let temp = TempWorkspace::new("search-success");
    temp.write_text("notes/alpha.txt", "first alpha\n");
    temp.write_text("notes/nested/beta.txt", "second alpha\n");
    temp.write_text("notes/nested/gamma.txt", "unmatched\n");
    let runtime =
        runtime_with_workspace_tools(temp.path(), pending_search_text_call("notes", "alpha"));

    let execution_events = execute_first_pending_call(&runtime, "search notes").await;

    assert_succeeded_json_result(&execution_events);
}

#[tokio::test(flavor = "current_thread")]
async fn registered_search_text_domain_failure_records_failed_json_without_runtime_failed() {
    let temp = TempWorkspace::new("search-domain-failure");
    temp.write_text("notes/alpha.txt", "alpha\n");
    let (runtime, provider) = runtime_with_workspace_tools_and_provider(
        temp.path(),
        pending_search_text_call("../outside", "alpha"),
    );

    let execution_events = execute_first_pending_call(&runtime, "search outside").await;

    assert_failed_json_result(&execution_events, "workspace_path_denied");
    assert_failed_json_artifact_visible_in_next_model_request(
        &runtime,
        &provider,
        WORKSPACE_SEARCH_TEXT_TOOL,
        "workspace_path_denied",
        temp.path(),
    )
    .await;
}
