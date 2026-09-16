use super::support::*;

#[tokio::test(flavor = "current_thread")]
async fn registered_apply_patch_tool_is_policy_denied_before_mutating_file() {
    let temp = TempWorkspace::new("patch-policy-denied");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let runtime =
        runtime_with_apply_patch_tools(temp.path(), pending_patch_call("note.txt", "old", "new"));

    let pending_events = collect_step(&runtime, "patch note").await;
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
        .expect("runtime policy denial should resolve pending call");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        "action_policy_denied"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).expect("workspace file should read"),
        "alpha\nold\nomega\n"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn denied_apply_patch_leaves_file_unchanged_and_returns_sanitized_result() {
    let temp = TempWorkspace::new("patch-policy-proposed");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let runtime =
        runtime_with_apply_patch_tools(temp.path(), pending_patch_call("note.txt", "old", "new"));
    let _pending_events = collect_step(&runtime, "patch note").await;
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("runtime policy denial should resolve pending call");

    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).expect("workspace file should read"),
        "alpha\nold\nomega\n"
    );
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        "action_policy_denied"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_apply_patch_tool_applies_patch_and_records_artifact_before_resolution() {
    let temp = TempWorkspace::new("patch-opt-in-success");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let runtime = runtime_with_opt_in_apply_patch_tools(
        temp.path(),
        pending_patch_call("note.txt", "old", "new"),
    );

    let pending_events = collect_step(&runtime, "patch note").await;
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
        .expect("opted-in workspace patch should execute");

    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).expect("workspace file should read"),
        "alpha\nnew\nomega\n"
    );
    assert_succeeded_json_result(&execution_events);
    let lifecycle = lifecycle_kinds(&runtime.ledger_projection().await);
    let audit_indexes = lifecycle
        .iter()
        .enumerate()
        .filter_map(|(index, kind)| (*kind == LedgerFactKind::ActionAuditRecorded).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(audit_indexes.len(), 2);
    let artifact_index = lifecycle
        .iter()
        .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
        .expect("artifact lifecycle should exist");
    let resolved_index = lifecycle
        .iter()
        .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
        .expect("resolution lifecycle should exist");
    assert!(audit_indexes[0] < audit_indexes[1]);
    assert!(audit_indexes[1] < artifact_index);
    assert!(artifact_index < resolved_index);
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_apply_patch_tool_adds_new_file_through_runtime() {
    let temp = TempWorkspace::new("patch-opt-in-add-success");
    fs::create_dir_all(temp.path().join("notes")).expect("parent directory should be created");
    let runtime = runtime_with_opt_in_apply_patch_tools(
        temp.path(),
        pending_add_patch_call("notes/new.txt", &["alpha", "beta"]),
    );

    let execution_events = execute_first_pending_call(&runtime, "add a new note").await;

    assert_eq!(
        fs::read_to_string(temp.path().join("notes/new.txt"))
            .expect("new workspace file should read"),
        "alpha\nbeta\n"
    );
    assert_succeeded_json_result(&execution_events);
    let lifecycle = lifecycle_kinds(&runtime.ledger_projection().await);
    let artifact_index = lifecycle
        .iter()
        .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
        .expect("artifact lifecycle should exist");
    let resolved_index = lifecycle
        .iter()
        .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
        .expect("resolution lifecycle should exist");
    assert!(artifact_index < resolved_index);
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_apply_patch_preflight_failure_resolves_with_patch_diagnostic() {
    let temp = TempWorkspace::new("patch-opt-in-preflight-failure");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let runtime = runtime_with_opt_in_apply_patch_tools(
        temp.path(),
        pending_patch_call("note.txt", "missing", "new"),
    );

    let _pending_events = collect_step(&runtime, "patch note").await;
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("patch preflight failure should resolve as a failed tool result");

    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).expect("workspace file should read"),
        "alpha\nold\nomega\n"
    );
    assert_failed_json_result(&execution_events, "apply_patch_preimage_absent");
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_patch_success_continuation_does_not_leak_internal_evidence() {
    let temp = TempWorkspace::new("patch-opt-in-no-leak");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(pending_patch_call("note.txt", "old", "new"))],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("continued after patch")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let provider_handle = provider.clone();
    let runtime = runtime_with_opt_in_apply_patch_tools_and_provider(temp.path(), provider);
    let _pending_events = collect_step(&runtime, "patch note").await;
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("opted-in workspace patch should execute");
    assert_succeeded_json_result(&execution_events);

    let _continuation_events = collect_step(&runtime, "continue after patch").await;
    let requests = provider_handle.recorded_requests();
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("successful tool result should be compiled as continuation");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    let ModelToolResultContent::Json(continuation_json) = continuation.result().content() else {
        panic!("successful patch continuation should be JSON");
    };
    assert_successful_patch_content_does_not_leak_internal_metadata(continuation_json);
}

#[tokio::test(flavor = "current_thread")]
async fn patch_proposal_and_audit_do_not_leak_into_sanitized_result_or_continuation() {
    let temp = TempWorkspace::new("patch-policy-no-leak");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = WorkspaceTools::new(WorkspaceToolsConfig::new(temp.path().to_path_buf()))
        .expect("workspace tools should construct");
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(pending_patch_call("note.txt", "old", "new"))],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("continued after denial")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let provider_handle = provider.clone();
    let mut builder =
        Runtime::builder(session_id()).model_provider(Arc::new(provider), model_name());
    for tool in tools.into_registered_tools_with_patch() {
        builder = builder.register_tool(tool);
    }
    let runtime = builder.build().expect("runtime should build");
    let _pending_events = collect_step(&runtime, "patch note").await;
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("runtime policy denial should resolve pending call");

    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        "action_policy_denied"
    );

    let _continuation_events = collect_step(&runtime, "continue after denial").await;
    let requests = provider_handle.recorded_requests();
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("denied tool result should be compiled as continuation");
    let ModelToolResultContent::Json(continuation_json) = continuation.result().content() else {
        panic!("denial continuation should be JSON");
    };
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .expect("denial continuation should include diagnostic")
            .code(),
        "action_policy_denied"
    );
    assert_patch_denial_json_sanitized(continuation_json, APPLY_PATCH_TOOL);
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_apply_patch_tool_deletes_file_and_reports_the_operation() {
    let temp = TempWorkspace::new("patch-opt-in-delete-success");
    temp.write_text("dir/gone.txt", "alpha\nbeta\n");
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(pending_delete_patch_call("dir/gone.txt"))],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("continued after delete")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let provider_handle = provider.clone();
    let runtime = runtime_with_opt_in_apply_patch_tools_and_provider(temp.path(), provider);
    let _pending_events = collect_step(&runtime, "delete the old note").await;
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("opted-in delete should execute");

    assert_succeeded_json_result(&execution_events);
    assert!(
        !temp.path().join("dir/gone.txt").exists(),
        "an executed delete must remove the file"
    );
    assert!(
        temp.path().join("dir").is_dir(),
        "deleting a file must leave its parent directory in place"
    );
    let lifecycle = lifecycle_kinds(&runtime.ledger_projection().await);
    let artifact_index = lifecycle
        .iter()
        .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
        .expect("artifact lifecycle should exist");
    let resolved_index = lifecycle
        .iter()
        .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
        .expect("resolution lifecycle should exist");
    assert!(artifact_index < resolved_index);

    // The provider-visible result is what the model sees, so it must name the
    // removal instead of leaving a delete and an emptied file indistinguishable.
    let _continuation_events = collect_step(&runtime, "continue after delete").await;
    let requests = provider_handle.recorded_requests();
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("successful tool result should be compiled as continuation");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    let ModelToolResultContent::Json(json) = continuation.result().content() else {
        panic!("successful delete continuation should be JSON");
    };
    let envelope: Value = serde_json::from_str(json).expect("delete result should be JSON");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["tool"], APPLY_PATCH_TOOL);
    assert_eq!(envelope["changes"][0]["path"], "dir/gone.txt");
    assert_eq!(envelope["changes"][0]["op"], "delete");
    assert_eq!(envelope["changes"][0]["lines_before"], 2);
    assert_eq!(envelope["changes"][0]["lines_after"], 0);
    assert_eq!(envelope["changes"][0]["bytes_after"], 0);
}

#[tokio::test(flavor = "current_thread")]
async fn policy_denied_delete_resolves_without_removing_the_file() {
    let temp = TempWorkspace::new("patch-delete-policy-denied");
    temp.write_text("note.txt", "alpha\n");
    let runtime =
        runtime_with_apply_patch_tools(temp.path(), pending_delete_patch_call("note.txt"));

    let _pending_events = collect_step(&runtime, "delete the note").await;
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("runtime policy denial should resolve pending call");

    assert_failed_json_result(&execution_events, "action_policy_denied");
    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).expect("workspace file should read"),
        "alpha\n",
        "a denied delete must leave the file unchanged"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn delete_outside_the_write_scope_is_denied_and_keeps_the_file() {
    let temp = TempWorkspace::new("patch-delete-scope-denied");
    temp.write_text("allowed/note.txt", "alpha\n");
    temp.write_text("denied/note.txt", "alpha\n");
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(temp.path().to_path_buf())
            .with_patch_write_scope(Some(vec![PathBuf::from("allowed")])),
    )
    .expect("workspace tools should construct");
    let provider = FakeModelProvider::new(vec![Ok(pending_delete_patch_call("denied/note.txt"))]);
    let mut builder = Runtime::builder(session_id())
        .model_provider(Arc::new(provider), model_name())
        .allow_low_risk_apply_patches();
    for tool in tools.into_registered_tools_with_patch() {
        builder = builder.register_tool(tool);
    }
    let runtime = builder.build().expect("runtime should build");

    let execution_events =
        execute_first_pending_call(&runtime, "delete a note outside scope").await;

    assert_failed_json_result(&execution_events, "workspace_path_denied");
    assert_eq!(
        fs::read_to_string(temp.path().join("denied/note.txt")).expect("denied file should read"),
        "alpha\n",
        "a delete outside the write scope must leave the file unchanged"
    );
    assert!(
        temp.path().join("allowed/note.txt").exists(),
        "a denied delete must not remove an unrelated file"
    );
}
