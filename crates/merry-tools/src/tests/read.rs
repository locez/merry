use super::*;

#[test]
fn read_text_returns_requested_line_window_without_host_root() {
    let temp = TempWorkspace::new("read-window");
    temp.write_text("dir/note.txt", "one\ntwo\nthree\nfour\nfive\n");
    let tools = tools_for(temp.path());
    let outcome = read_text_range_outcome(&tools, "dir/note.txt", 3, 2);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        json_content(&outcome),
        json!({
            "ok": true,
            "tool": READ_TEXT_TOOL,
            "path": "dir/note.txt",
            "start_line": 3,
            "end_line": 4,
            "lines": 2,
            "bytes": 11,
            "truncated": true,
            "content": "three\nfour\n"
        })
    );
    assert!(
        !outcome
            .content()
            .as_text()
            .expect("json content")
            .contains(temp.path().to_str().expect("temp path utf8"))
    );
}

#[test]
fn read_text_defaults_to_a_bounded_first_window() {
    let temp = TempWorkspace::new("read-default-window");
    let content = (1..=205)
        .map(|line| format!("line-{line}\n"))
        .collect::<String>();
    temp.write_text("large.txt", &content);
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "large.txt");
    let payload = json_content(&outcome);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(payload["start_line"], 1);
    assert_eq!(payload["lines"], 200);
    assert_eq!(payload["end_line"], 200);
    assert_eq!(payload["truncated"], true);
    assert!(
        payload["content"]
            .as_str()
            .expect("content text")
            .contains("line-200\n")
    );
    assert!(
        !payload["content"]
            .as_str()
            .expect("content text")
            .contains("line-201\n")
    );
}

#[test]
fn read_text_reads_a_range_from_a_file_larger_than_the_read_limit() {
    let temp = TempWorkspace::new("read-large-file-range");
    temp.write_text("large.txt", &"0123456789\n".repeat(20));
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_read_bytes: 32,
                max_read_lines: 2,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");

    let outcome = read_text_range_outcome(&tools, "large.txt", 1, 2);
    let payload = json_content(&outcome);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(payload["lines"], 2);
    assert_eq!(payload["content"], "0123456789\n0123456789\n");
}

#[test]
fn read_text_rejects_ranges_outside_configured_limits() {
    let temp = TempWorkspace::new("read-range-validation");
    temp.write_text("note.txt", "one\ntwo\n");
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_read_lines: 2,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");
    let executor = ReadTextExecutor {
        state: Arc::clone(&tools.state),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime should build");

    for arguments in [
        json!({ "path": "note.txt", "start_line": 0, "max_lines": 1 }),
        json!({ "path": "note.txt", "start_line": 1, "max_lines": 3 }),
    ] {
        let outcome = runtime
            .block_on(executor.execute(pending_call(arguments), ToolExecutionContext::default()))
            .expect("invalid range should resolve as a failed outcome");
        assert_failed_json(
            &outcome,
            ERROR_INVALID_ARGUMENTS,
            Some("note.txt"),
            temp.path(),
        );
    }
}

#[test]
fn read_text_reports_missing_non_utf8_and_hidden_path_failures() {
    let temp = TempWorkspace::new("read-failures");
    temp.write_bytes("binary.bin", &[0xff, 0xfe, 0xfd]);
    temp.write_text("visible.txt", "ok\n");
    let tools = tools_for(temp.path());

    assert_failed_json(
        &read_outcome(&tools, "missing.txt"),
        ERROR_FILE_NOT_FOUND,
        Some("missing.txt"),
        temp.path(),
    );
    assert_failed_json(
        &read_outcome(&tools, "binary.bin"),
        ERROR_NOT_UTF8,
        Some("binary.bin"),
        temp.path(),
    );
    assert_failed_json(
        &read_outcome(&tools, ".secret"),
        ERROR_PATH_DENIED,
        Some(".secret"),
        temp.path(),
    );
}

#[cfg(unix)]
#[test]
fn read_text_rejects_symlink_without_following_it() {
    let temp = TempWorkspace::new("read-symlink");
    temp.write_text("target.txt", "secret\n");
    symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "link.txt");
    assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        outcome.diagnostic().expect("diagnostic").code(),
        ERROR_PATH_DENIED
    );
}

#[test]
fn read_text_executor_returns_cancelled_when_token_is_cancelled() {
    let temp = TempWorkspace::new("read-cancelled");
    temp.write_text("note.txt", "ok\n");
    let tools = tools_for(temp.path());
    let executor = ReadTextExecutor {
        state: Arc::clone(&tools.state),
    };
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime should build");

    let err = runtime
        .block_on(executor.execute(
            pending_call(json!({ "path": "note.txt" })),
            ToolExecutionContext::new(token),
        ))
        .expect_err("cancelled execution should return cancellation error");

    assert!(matches!(err, ToolExecutionError::Cancelled));
}

fn read_text_range_outcome(
    tools: &WorkspaceTools,
    path: &str,
    start_line: usize,
    max_lines: usize,
) -> ToolExecutionOutcome {
    read_text_blocking(
        &tools.state,
        ReadTextInput {
            path: path.to_owned(),
            start_line: Some(start_line),
            max_lines: Some(max_lines),
        },
    )
}
