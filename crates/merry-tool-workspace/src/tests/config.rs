use super::*;

#[test]
fn config_rejects_missing_roots() {
    let err = ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(Vec::new()))
        .expect_err("empty roots should be rejected");
    assert!(matches!(err, WorkspaceToolConfigError::NoRoots));
}

#[test]
fn config_rejects_non_directory_root() {
    let temp = TempWorkspace::new("non-directory-root");
    temp.write_text("file.txt", "content\n");

    let err = ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![
        temp.path().join("file.txt"),
    ]))
    .expect_err("file root should be rejected");
    assert!(matches!(
        err,
        WorkspaceToolConfigError::RootNotDirectory { .. }
    ));
}

#[test]
fn config_rejects_zero_read_limit() {
    let temp = TempWorkspace::new("zero-limit");
    let config = WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
        WorkspaceToolLimits {
            max_read_bytes: 0,
            ..WorkspaceToolLimits::default()
        },
    );

    let err = ReadOnlyWorkspaceTools::new(config).expect_err("zero limit should be rejected");
    assert!(matches!(
        err,
        WorkspaceToolConfigError::InvalidLimit {
            name: "max_read_bytes"
        }
    ));
}

#[test]
fn config_rejects_each_zero_limit() {
    let temp = TempWorkspace::new("zero-all-limits");

    for invalid_name in [
        "max_read_bytes",
        "max_write_bytes",
        "max_patch_bytes",
        "max_list_entries",
        "max_search_matches",
        "max_search_files",
        "max_search_entries",
        "max_search_bytes",
        "max_search_line_bytes",
        "max_search_query_bytes",
    ] {
        let mut limits = WorkspaceToolLimits::default();
        match invalid_name {
            "max_read_bytes" => limits.max_read_bytes = 0,
            "max_write_bytes" => limits.max_write_bytes = 0,
            "max_patch_bytes" => limits.max_patch_bytes = 0,
            "max_list_entries" => limits.max_list_entries = 0,
            "max_search_matches" => limits.max_search_matches = 0,
            "max_search_files" => limits.max_search_files = 0,
            "max_search_entries" => limits.max_search_entries = 0,
            "max_search_bytes" => limits.max_search_bytes = 0,
            "max_search_line_bytes" => limits.max_search_line_bytes = 0,
            "max_search_query_bytes" => limits.max_search_query_bytes = 0,
            other => panic!("unexpected limit name {other}"),
        }

        let config = WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(limits);
        let err = ReadOnlyWorkspaceTools::new(config).expect_err("zero limit should be rejected");
        assert!(matches!(
            err,
            WorkspaceToolConfigError::InvalidLimit { name } if name == invalid_name
        ));
    }
}

#[test]
fn config_rejects_invalid_patch_scope_paths() {
    let temp = TempWorkspace::new("invalid-patch-scope");

    for config in [
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()])
            .with_patch_write_scope(Some(vec![PathBuf::from("../outside")])),
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()])
            .with_forbidden_paths(vec![PathBuf::from("bad\npath")]),
    ] {
        let err =
            ReadOnlyWorkspaceTools::new(config).expect_err("invalid scope should be rejected");
        assert!(matches!(
            err,
            WorkspaceToolConfigError::InvalidScopePath { .. }
        ));
    }
}

#[test]
fn into_registered_tools_exposes_read_list_and_search() {
    let temp = TempWorkspace::new("registered-tools");
    let tools = tools_for(temp.path()).into_registered_tools();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool.spec().name().as_str())
        .collect();

    assert_eq!(
        names,
        [
            WORKSPACE_READ_FILE_TOOL,
            WORKSPACE_LIST_DIR_TOOL,
            WORKSPACE_SEARCH_TEXT_TOOL
        ]
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool.concurrency() == ToolConcurrency::ParallelSafe)
    );
}

#[test]
fn patch_tool_registration_is_opt_in_and_workspace_write() {
    let temp = TempWorkspace::new("registered-patch-tools");

    let read_only_tools = tools_for(temp.path()).into_registered_tools();
    assert!(
        read_only_tools
            .iter()
            .all(|tool| tool.spec().name().as_str() != WORKSPACE_PATCH_TOOL)
    );

    let tools = tools_for(temp.path()).into_registered_tools_with_patch();
    let patch = tools
        .iter()
        .find(|tool| tool.spec().name().as_str() == WORKSPACE_PATCH_TOOL)
        .expect("patch tool should be registered only by opt-in method");
    assert_eq!(patch.action_kind(), ToolActionKind::WorkspaceWrite);
    assert_eq!(patch.concurrency(), ToolConcurrency::Exclusive);
    assert!(patch.proposals_enabled());
    assert_eq!(tools.len(), 4);
    assert!(
        tools
            .iter()
            .filter(|tool| tool.spec().name().as_str() != WORKSPACE_PATCH_TOOL)
            .all(|tool| {
                tool.action_kind() == ToolActionKind::ReadOnly && !tool.proposals_enabled()
            })
    );
}

#[test]
fn hidden_paths_can_be_enabled_explicitly() {
    let temp = TempWorkspace::new("allow-hidden");
    temp.write_text(".secret", "ok\n");
    let tools = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_allow_hidden(true),
    )
    .expect("workspace tools should construct");

    let outcome = read_outcome(&tools, ".secret");
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
}

#[test]
fn non_utf8_component_is_rejected_when_constructible() {
    let path = PathBuf::from(OsStr::new("plain"));
    let text = path.to_str().expect("plain path is utf8");
    let validated = validate_relative_path(text, false).expect("plain path validates");
    assert_eq!(validated.display, "plain");
}
