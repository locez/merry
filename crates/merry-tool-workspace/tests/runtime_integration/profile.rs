use super::support::*;

#[tokio::test(flavor = "current_thread")]
async fn workspace_coding_loop_profile_registers_expected_tools_and_process_lanes() {
    let temp = TempWorkspace::new("coding-loop-profile-tools");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(Vec::new(), FinishReason::Stop, None),
    })]]);
    let provider_handle = provider.clone();
    let runner = Arc::new(ScriptedProcessRunner::new(Vec::new()));
    let profile =
        WorkspaceCodingLoopProfile::new(WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]))
            .expect("workspace coding loop profile should construct")
            .with_cli_bwrap_process_runner(
                AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
                runner,
            );
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .expect("workspace coding loop profile should apply")
        .build()
        .expect("runtime profile should build");
    let runtime = Runtime::builder(session_id())
        .model_provider(Arc::new(provider), model_name())
        .with_profile(profile)
        .expect("runtime profile should apply")
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "inspect workspace").await;
    let requests = provider_handle.recorded_requests();
    assert_eq!(requests.len(), 1);
    let tool_names = requests[0].tools();
    assert_eq!(tool_names.len(), 5);
    assert!(
        tool_names
            .iter()
            .any(|tool| tool.name().as_str() == "run_process")
    );
    assert!(
        tool_names
            .iter()
            .any(|tool| tool.name().as_str() == "request_permissions")
    );
    assert!(
        tool_names
            .iter()
            .any(|tool| tool.name().as_str() == WORKSPACE_READ_FILE_TOOL)
    );
    assert!(
        tool_names
            .iter()
            .any(|tool| tool.name().as_str() == WORKSPACE_LIST_DIR_TOOL)
    );
    assert!(
        tool_names
            .iter()
            .any(|tool| tool.name().as_str() == WORKSPACE_SEARCH_TEXT_TOOL)
    );
    assert!(
        !tool_names
            .iter()
            .any(|tool| tool.name().as_str() == WORKSPACE_PATCH_TOOL),
        "patch tool should require the explicit with_patch lane"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_coding_loop_profile_executes_read_only_shell_lane() {
    let temp = TempWorkspace::new("coding-loop-profile-read-only-shell");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(pending_process_call(
        "call-read-only-shell",
        &["bash", "-lc", "rg --files"],
    ))]]);
    let runner = Arc::new(ScriptedProcessRunner::new(vec![
        ScriptedProcessResponse::success("src/lib.rs\n"),
    ]));
    let runtime = runtime_with_coding_loop_tools(temp.path(), provider, runner.clone());

    let events = execute_first_pending_call(&runtime, "inspect with shell").await;

    assert_eq!(
        event_kind_names(&events),
        ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("process result artifact should be readable");
    let payload: Value = serde_json::from_str(
        content
            .as_text()
            .expect("process result artifact should be textual JSON"),
    )
    .expect("process result artifact should parse as JSON");
    assert_eq!(
        payload["permission_profile_id"],
        "process.shell.read_only.v1"
    );
    assert_eq!(payload["stdout"]["text"], "src/lib.rs\n");
    assert_eq!(
        runner.observed_intents()[0].argv(),
        ["bash".to_owned(), "-lc".to_owned(), "rg --files".to_owned()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_coding_loop_profile_read_only_process_runner_denies_local_workspace_effect() {
    let temp = TempWorkspace::new("coding-loop-profile-read-only-process-runner");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(pending_process_call(
        "call-local-effect",
        &["cargo", "test"],
    ))]]);
    let runner = Arc::new(ScriptedProcessRunner::new(Vec::new()));
    let profile =
        WorkspaceCodingLoopProfile::new(WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]))
            .expect("workspace coding loop profile should construct")
            .with_read_only_process_runner(runner.clone());
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .expect("workspace coding loop profile should apply")
        .build()
        .expect("runtime profile should build");
    let runtime = Runtime::builder(session_id())
        .model_provider(Arc::new(provider), model_name())
        .with_profile(profile)
        .expect("runtime profile should apply")
        .build()
        .expect("runtime should build");

    let events = execute_first_pending_call(&runtime, "run local effect").await;

    assert_failed_json_result(&events, "action_policy_denied");
    assert!(runner.observed_intents().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_coding_loop_profile_seeds_project_capability_context() {
    let temp = TempWorkspace::new("coding-loop-profile-project-context");
    temp.write_text("Cargo.toml", "[package]\nname = \"fixture\"\n");
    temp.write_text("AGENTS.md", "Use fixture rules.\n");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("inspected")],
            FinishReason::Stop,
            None,
        ),
    })]]);
    let provider_handle = provider.clone();
    let runner = Arc::new(ScriptedProcessRunner::new(Vec::new()));
    let runtime = runtime_with_coding_loop_tools(temp.path(), provider, runner);

    let events = collect_step(&runtime, "inspect project").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ModelRetryAttemptStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ],
        "seeded project context should not emit startup artifact events"
    );
    let requests = provider_handle.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.stable_prefix_message_count(), 2);
    assert_eq!(request.messages().len(), 4);
    let base_instructions = request.messages()[0].content().as_text();
    assert!(base_instructions.contains("You are Merry, a pragmatic coding agent."));
    assert!(base_instructions.contains("Respect project instructions such as AGENTS.md"));
    let progress_commentary = request.messages()[1].content().as_text();
    assert!(progress_commentary.contains("brief progress note first"));
    assert!(progress_commentary.contains("user's current input language"));
    let project_context = request.messages()[2].content().as_text();
    assert!(project_context.contains("summary:project-capabilities"));
    assert!(project_context.contains("Cargo.toml is present"));
    assert!(project_context.contains("Detected AGENTS.md"));
    assert!(project_context.contains("cargo fmt --all --check"));
    assert!(project_context.contains("cargo test --all"));
    assert!(
        project_context
            .contains("evidence:seeded runtime context:context-seed-project-capabilities:whole")
    );
    assert_eq!(request.messages()[3].content().as_text(), "inspect project");
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_coding_loop_profile_can_enable_patch_tool() {
    let temp = TempWorkspace::new("coding-loop-profile-patch");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(Vec::new(), FinishReason::Stop, None),
    })]]);
    let provider_handle = provider.clone();
    let runner = Arc::new(ScriptedProcessRunner::new(Vec::new()));
    let profile =
        WorkspaceCodingLoopProfile::new(WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]))
            .expect("workspace coding loop profile should construct")
            .with_patch_tool()
            .with_cli_bwrap_process_runner(
                AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
                runner,
            );
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .expect("workspace coding loop profile should apply")
        .build()
        .expect("runtime profile should build");
    let runtime = Runtime::builder(session_id())
        .model_provider(Arc::new(provider), model_name())
        .with_profile(profile)
        .expect("runtime profile should apply")
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "inspect workspace").await;
    let requests = provider_handle.recorded_requests();
    assert_eq!(requests.len(), 1);
    let tool_names = requests[0].tools();
    assert_eq!(tool_names.len(), 6);
    assert!(
        tool_names
            .iter()
            .any(|tool| tool.name().as_str() == WORKSPACE_PATCH_TOOL)
    );
    assert!(
        tool_names
            .iter()
            .any(|tool| tool.name().as_str() == "request_permissions")
    );
}
