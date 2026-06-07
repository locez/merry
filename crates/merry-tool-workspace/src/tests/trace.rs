use super::*;

#[tokio::test(flavor = "current_thread")]
async fn workspace_read_file_traces_start_and_finish_without_file_contents() {
    let temp = TempWorkspace::new("trace-read-file");
    temp.write_text("lib.rs", "secret source text\n");
    let tools = tools_for(temp.path());
    let executor = ReadFileExecutor {
        state: Arc::clone(&tools.state),
    };
    let call = pending_call_with_id(
        WORKSPACE_READ_FILE_TOOL,
        "call-trace-read-file",
        json!({ "path": "lib.rs" }),
    );

    let (outcome, logs) = capture_traces_for(
        "call-trace-read-file",
        executor.execute(call, ToolExecutionContext::default()),
    )
    .await;
    let outcome = outcome.expect("read should succeed");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert!(outcome.diagnostic().is_none());
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.start\""));
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
    assert!(logs.contains("\"status\":\"succeeded\""));
    assert!(logs.contains("\"tool_name\":\"workspace_read_file\""));
    assert!(logs.contains("\"tool_call_id\":\"call-trace-read-file\""));
    assert!(logs.contains("\"path\":\"lib.rs\""));
    assert!(!logs.contains("secret source text"));
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_read_file_failure_trace_includes_diagnostic_code() {
    let temp = TempWorkspace::new("trace-read-failure");
    let tools = tools_for(temp.path());
    let executor = ReadFileExecutor {
        state: Arc::clone(&tools.state),
    };
    let call = pending_call_with_id(
        WORKSPACE_READ_FILE_TOOL,
        "call-trace-read-failure",
        json!({ "path": "../secret.txt" }),
    );

    let (outcome, logs) = capture_traces_for(
        "call-trace-read-failure",
        executor.execute(call, ToolExecutionContext::default()),
    )
    .await;
    let outcome = outcome.expect("path denial should resolve as a domain result");

    assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        outcome.diagnostic().expect("diagnostic").code(),
        ERROR_PATH_DENIED
    );
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
    assert!(logs.contains("\"status\":\"failed\""));
    assert!(logs.contains("\"diagnostic_code\":\"workspace_path_denied\""));
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_path_traces_are_bounded_summaries() {
    let temp = TempWorkspace::new("trace-bounded-path");
    let tools = tools_for(temp.path());
    let executor = ReadFileExecutor {
        state: Arc::clone(&tools.state),
    };
    let long_path = format!("{}tail.txt", "nested/".repeat(32));
    let expected_summary = bounded_trace_text(&long_path, TRACE_PATH_MAX_CHARS);
    let call = pending_call_with_id(
        WORKSPACE_READ_FILE_TOOL,
        "call-trace-bounded-path",
        json!({ "path": long_path }),
    );

    let (_outcome, logs) = capture_traces_for(
        "call-trace-bounded-path",
        executor.execute(call, ToolExecutionContext::default()),
    )
    .await;

    assert!(logs.contains(expected_summary.as_str()));
    assert!(!logs.contains("nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/tail.txt"));
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_started_tool_trace_finishes_cancelled_when_token_cancels() {
    let temp = TempWorkspace::new("trace-cancelled");
    temp.write_text("note.txt", "ok\n");
    let tools = tools_for(temp.path());
    let executor = ReadFileExecutor {
        state: Arc::clone(&tools.state),
    };
    let call = pending_call_with_id(
        WORKSPACE_READ_FILE_TOOL,
        "call-trace-cancelled",
        json!({ "path": "note.txt" }),
    );
    let token = tokio_util::sync::CancellationToken::new();
    install_trace_start_cancellation_token(token.clone());
    install_trace_start_test_hook("call-trace-cancelled", cancel_trace_start_token);

    let (result, logs) = capture_traces_for(
        "call-trace-cancelled",
        executor.execute(call, ToolExecutionContext::new(token)),
    )
    .await;
    let error = result.expect_err("cancelled execution should return cancellation error");

    assert!(matches!(error, ToolExecutionError::Cancelled));
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
    assert!(logs.contains("\"status\":\"cancelled\""));
    assert!(logs.contains("\"diagnostic_code\":\"workspace_tool_cancelled\""));
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_search_trace_uses_query_bytes_without_long_query_text() {
    let temp = TempWorkspace::new("trace-search");
    temp.write_text("notes.txt", "needle\n");
    let tools = tools_for(temp.path());
    let executor = SearchTextExecutor {
        state: Arc::clone(&tools.state),
    };
    let long_query = "needle-".repeat(24);
    let call = pending_call_with_id(
        WORKSPACE_SEARCH_TEXT_TOOL,
        "call-trace-search",
        json!({ "query": long_query }),
    );

    let (outcome, logs) = capture_traces_for(
        "call-trace-search",
        executor.execute(call, ToolExecutionContext::default()),
    )
    .await;
    let outcome = outcome.expect("search should resolve as a domain result");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.start\""));
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
    assert!(logs.contains("\"tool_name\":\"workspace_search_text\""));
    assert!(logs.contains("\"query_bytes\":168"));
    assert!(!logs.contains(long_query.as_str()));
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_patch_trace_uses_byte_counts_without_patch_text() {
    let temp = TempWorkspace::new("trace-patch");
    temp.write_text("src/lib.rs", "old secret text\n");
    let tools = tools_for(temp.path());
    let executor = WorkspacePatchExecutor {
        state: Arc::clone(&tools.state),
    };
    let patch = update_patch("src/lib.rs", "old secret text", "new secret text");
    let call = pending_call_with_id(
        WORKSPACE_PATCH_TOOL,
        "call-trace-patch",
        json!({ "patch": patch }),
    );

    let (outcome, logs) = capture_traces_for(
        "call-trace-patch",
        executor.execute(call, ToolExecutionContext::default()),
    )
    .await;
    let outcome = outcome.expect("patch should succeed");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.start\""));
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
    assert!(logs.contains("\"tool_name\":\"workspace_patch\""));
    assert!(logs.contains("\"patch_bytes\":"));
    assert!(!logs.contains("old secret text"));
    assert!(!logs.contains("new secret text"));
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_list_dir_traces_start_and_finish_without_entry_contents() {
    let temp = TempWorkspace::new("trace-list-dir");
    temp.write_text("root/secret-name.txt", "secret file content\n");
    let tools = tools_for(temp.path());
    let executor = ListDirExecutor {
        state: Arc::clone(&tools.state),
    };
    let call = pending_call_with_id(
        WORKSPACE_LIST_DIR_TOOL,
        "call-trace-list-dir",
        json!({ "path": "root" }),
    );

    let (outcome, logs) = capture_traces_for(
        "call-trace-list-dir",
        executor.execute(call, ToolExecutionContext::default()),
    )
    .await;
    let outcome = outcome.expect("list should succeed");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.start\""));
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
    assert!(logs.contains("\"status\":\"succeeded\""));
    assert!(logs.contains("\"tool_name\":\"workspace_list_dir\""));
    assert!(logs.contains("\"tool_call_id\":\"call-trace-list-dir\""));
    assert!(logs.contains("\"path\":\"root\""));
    assert!(!logs.contains("secret file content"));
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_tool_invalid_arguments_trace_failed_without_payload() {
    let temp = TempWorkspace::new("trace-invalid-args");
    let tools = tools_for(temp.path());
    let invalid_arguments = json!({ "unexpected": "sensitive invalid payload" });

    let read_executor = ReadFileExecutor {
        state: Arc::clone(&tools.state),
    };
    let read_call = pending_call_with_id(
        WORKSPACE_READ_FILE_TOOL,
        "call-trace-read-invalid-args",
        invalid_arguments.clone(),
    );
    let (read_outcome, read_logs) = capture_traces_for(
        "call-trace-read-invalid-args",
        read_executor.execute(read_call, ToolExecutionContext::default()),
    )
    .await;
    assert_invalid_arguments_trace(
        read_outcome.expect("read invalid args should resolve as a failed outcome"),
        &read_logs,
        WORKSPACE_READ_FILE_TOOL,
        "call-trace-read-invalid-args",
    );

    let list_executor = ListDirExecutor {
        state: Arc::clone(&tools.state),
    };
    let list_call = pending_call_with_id(
        WORKSPACE_LIST_DIR_TOOL,
        "call-trace-list-invalid-args",
        invalid_arguments.clone(),
    );
    let (list_outcome, list_logs) = capture_traces_for(
        "call-trace-list-invalid-args",
        list_executor.execute(list_call, ToolExecutionContext::default()),
    )
    .await;
    assert_invalid_arguments_trace(
        list_outcome.expect("list invalid args should resolve as a failed outcome"),
        &list_logs,
        WORKSPACE_LIST_DIR_TOOL,
        "call-trace-list-invalid-args",
    );

    let search_executor = SearchTextExecutor {
        state: Arc::clone(&tools.state),
    };
    let search_call = pending_call_with_id(
        WORKSPACE_SEARCH_TEXT_TOOL,
        "call-trace-search-invalid-args",
        invalid_arguments.clone(),
    );
    let (search_outcome, search_logs) = capture_traces_for(
        "call-trace-search-invalid-args",
        search_executor.execute(search_call, ToolExecutionContext::default()),
    )
    .await;
    assert_invalid_arguments_trace(
        search_outcome.expect("search invalid args should resolve as a failed outcome"),
        &search_logs,
        WORKSPACE_SEARCH_TEXT_TOOL,
        "call-trace-search-invalid-args",
    );

    let patch_executor = WorkspacePatchExecutor {
        state: Arc::clone(&tools.state),
    };
    let patch_call = pending_call_with_id(
        WORKSPACE_PATCH_TOOL,
        "call-trace-patch-invalid-args",
        invalid_arguments,
    );
    let (patch_outcome, patch_logs) = capture_traces_for(
        "call-trace-patch-invalid-args",
        patch_executor.execute(patch_call, ToolExecutionContext::default()),
    )
    .await;
    assert_invalid_arguments_trace(
        patch_outcome.expect("patch invalid args should resolve as a failed outcome"),
        &patch_logs,
        WORKSPACE_PATCH_TOOL,
        "call-trace-patch-invalid-args",
    );
}
