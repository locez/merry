use super::*;
use merry_llm::testing::FakeModelProvider;

fn fake_provider() -> Arc<dyn ModelProvider> {
    Arc::new(FakeModelProvider::new(Vec::new()))
}

#[test]
fn resolves_four_level_model_and_generation_inheritance() {
    let mut catalog =
        SubagentRoleCatalog::builtin().with_default_override(SubagentDefaultOverride {
            model_override: Some(SubagentModelOverride {
                provider: fake_provider(),
                model: ModelName::new("default-sub-model").expect("valid"),
                retry_policy: None,
            }),
            reasoning_effort: Some(ReasoningEffort::new("medium").expect("valid")),
            max_model_turns: None,
            timeout_secs: Some(500),
            max_total_tokens: Some(400_000),
        });
    catalog
        .apply_role_overlay(
            "explore",
            SubagentRoleOverlay {
                model_override: Some(SubagentModelOverride {
                    provider: fake_provider(),
                    model: ModelName::new("explore-fast-model").expect("valid"),
                    retry_policy: None,
                }),
                reasoning_effort: Some(ReasoningEffort::new("low").expect("valid")),
                ..Default::default()
            },
        )
        .expect("explore overlay valid");

    let parent_live = ParentPrimaryModel {
        provider: fake_provider(),
        model: ModelName::new("live-switched-parent").expect("valid"),
        retry_policy: ModelRetryPolicy::disabled(),
    };
    let parent_gen = GenerationConfig::default()
        .with_reasoning_effort(Some(ReasoningEffort::new("high").expect("valid")));
    let parent_tools = tool_names(&[
        "read_text",
        "apply_patch",
        "run_process",
        "read_background_process",
        "stop_background_process",
        "request_permissions",
        "web_fetch",
        "spawn_subagents",
        "send_subagent_message",
        "wait_subagents",
        "cancel_subagents",
    ]);
    let scope = ChildWorkspaceScope::workspace_root();
    let fallback_model = ModelName::new("initial-fallback").expect("valid");

    // Role override wins over default & live parent.
    let explore_task = SubagentTaskSpec::new("Find callers.", 80)
        .expect("valid")
        .with_max_model_turns_explicit(false)
        .with_role(Some("explore".to_owned()))
        .expect("valid");
    let resolved_explore = resolve_subagent_runtime(
        &catalog,
        &SubagentResolutionInput {
            task: &explore_task,
            parent_allowed_tools: &parent_tools,
            workspace_scope: &scope,
            parent_workspace_writable: true,
            depth: 1,
            max_depth: 1,
            max_model_turns_cap: 120,
            parent_primary_model: Some(&parent_live),
            fallback_provider: fake_provider(),
            fallback_model: &fallback_model,
            parent_generation_config: &parent_gen,
        },
    )
    .expect("explore resolves");
    assert_eq!(resolved_explore.model.as_str(), "explore-fast-model");
    assert_eq!(
        resolved_explore
            .generation_config
            .reasoning_effort()
            .map(ReasoningEffort::as_str),
        Some("low")
    );
    assert!(!resolved_explore.workspace_writable);
    assert_eq!(resolved_explore.max_model_turns, 30);
    assert_eq!(resolved_explore.timeout_secs, Some(180));
    assert_eq!(resolved_explore.max_total_tokens, Some(400_000));
    assert!(
        !resolved_explore
            .allowed_tools
            .iter()
            .any(|t| t.as_str() == "apply_patch" || t.as_str() == "wait_subagents")
    );

    // Task reasoning_effort (Level 1) overrides role & default.
    let explicit_effort_task = explore_task
        .clone()
        .with_reasoning_effort(Some(ReasoningEffort::new("xhigh").expect("valid")));
    let resolved_override = resolve_subagent_runtime(
        &catalog,
        &SubagentResolutionInput {
            task: &explicit_effort_task,
            parent_allowed_tools: &parent_tools,
            workspace_scope: &scope,
            parent_workspace_writable: true,
            depth: 1,
            max_depth: 1,
            max_model_turns_cap: 120,
            parent_primary_model: Some(&parent_live),
            fallback_provider: fake_provider(),
            fallback_model: &fallback_model,
            parent_generation_config: &parent_gen,
        },
    )
    .expect("resolves");
    assert_eq!(
        resolved_override
            .generation_config
            .reasoning_effort()
            .map(ReasoningEffort::as_str),
        Some("xhigh")
    );

    // Without default/role model overrides, live parent model & generation win!
    let bare_catalog = SubagentRoleCatalog::builtin();
    let coder_task = SubagentTaskSpec::new("Implement.", 80)
        .expect("valid")
        .with_max_model_turns_explicit(false);
    let resolved_coder = resolve_subagent_runtime(
        &bare_catalog,
        &SubagentResolutionInput {
            task: &coder_task,
            parent_allowed_tools: &parent_tools,
            workspace_scope: &scope,
            parent_workspace_writable: false, // parent RO must narrow coder to RO!
            depth: 1,
            max_depth: 1,
            max_model_turns_cap: 120,
            parent_primary_model: Some(&parent_live),
            fallback_provider: fake_provider(),
            fallback_model: &fallback_model,
            parent_generation_config: &parent_gen,
        },
    )
    .expect("coder resolves");
    assert_eq!(resolved_coder.model.as_str(), "live-switched-parent");
    assert_eq!(
        resolved_coder
            .generation_config
            .reasoning_effort()
            .map(ReasoningEffort::as_str),
        Some("high")
    );
    assert!(
        !resolved_coder.workspace_writable,
        "capability intersection must keep child read-only when parent workspace is read-only"
    );
    assert!(
        !resolved_coder
            .allowed_tools
            .iter()
            .any(|t| t.as_str() == "apply_patch"),
        "apply_patch must be pruned when workspace is read-only"
    );
    assert!(
        !resolved_coder
            .allowed_tools
            .iter()
            .any(|t| t.as_str() == "wait_subagents"),
        "subagent control tools must be pruned at max_depth"
    );
}

#[test]
fn prunes_orphaned_background_tools_when_run_process_is_absent_and_respects_toggles() {
    let mut catalog = SubagentRoleCatalog::builtin();
    catalog
        .apply_role_overlay(
            "browser",
            SubagentRoleOverlay {
                base: Some("review".to_owned()),
                description: Some("UI verification".to_owned()),
                allowed_tools: Some(tool_names(&[
                    "browser_tab",
                    "browser_network",
                    "read_background_process",
                    "stop_background_process",
                    "request_permissions",
                    "wait_subagents",
                ])),
                ..Default::default()
            },
        )
        .expect("browser role overlay valid");

    let parent_tools = tool_names(&[
        "read_text",
        "browser_tab",
        "browser_network",
        "read_background_process",
        "stop_background_process",
        "request_permissions",
        "wait_subagents",
    ]);
    let task = SubagentTaskSpec::new("Check UI.", 20)
        .expect("valid")
        .with_role(Some("browser".to_owned()))
        .expect("valid");
    let resolved = resolve_subagent_runtime(
        &catalog,
        &SubagentResolutionInput {
            task: &task,
            parent_allowed_tools: &parent_tools,
            workspace_scope: &ChildWorkspaceScope::workspace_root(),
            parent_workspace_writable: true,
            depth: 1,
            max_depth: 1,
            max_model_turns_cap: 80,
            parent_primary_model: None,
            fallback_provider: fake_provider(),
            fallback_model: &ModelName::new("fallback").expect("valid"),
            parent_generation_config: &GenerationConfig::default(),
        },
    )
    .expect("browser role resolves");
    let names: Vec<&str> = resolved
        .allowed_tools
        .iter()
        .map(ToolName::as_str)
        .collect();
    assert_eq!(
        names,
        vec!["browser_tab", "browser_network", "request_permissions"]
    );

    let mut toggles = BTreeMap::new();
    toggles.insert("review".to_owned(), false);
    catalog.apply_toggles(&toggles).expect("toggle succeeds");
    let review_task = SubagentTaskSpec::new("Review.", 20)
        .expect("valid")
        .with_role(Some("review".to_owned()))
        .expect("valid");
    let err = resolve_subagent_runtime(
        &catalog,
        &SubagentResolutionInput {
            task: &review_task,
            parent_allowed_tools: &parent_tools,
            workspace_scope: &ChildWorkspaceScope::workspace_root(),
            parent_workspace_writable: true,
            depth: 1,
            max_depth: 1,
            max_model_turns_cap: 80,
            parent_primary_model: None,
            fallback_provider: fake_provider(),
            fallback_model: &ModelName::new("fallback").expect("valid"),
            parent_generation_config: &GenerationConfig::default(),
        },
    )
    .err()
    .expect("disabled role must fail");
    assert!(matches!(err, SubagentRoleError::DisabledRole { .. }));
}

#[test]
fn explicit_role_max_model_turns_beats_default_override_without_magic_numbers() {
    let mut catalog =
        SubagentRoleCatalog::builtin().with_default_override(SubagentDefaultOverride {
            max_model_turns: Some(120),
            ..Default::default()
        });
    // Explicit role overlay setting coder back to 80 must beat [subagents.default]=120.
    catalog
        .apply_role_overlay(
            "coder",
            SubagentRoleOverlay {
                max_model_turns: Some(80),
                ..Default::default()
            },
        )
        .expect("coder overlay valid");
    // Custom role with no explicit max_model_turns must inherit [subagents.default]=120.
    catalog
        .apply_role_overlay(
            "custom_helper",
            SubagentRoleOverlay {
                description: Some("custom helper".to_owned()),
                ..Default::default()
            },
        )
        .expect("custom role overlay valid");

    let scope = ChildWorkspaceScope::workspace_root();
    let fallback_model = ModelName::new("fallback").expect("valid");
    let generation = GenerationConfig::default();

    let coder_task = SubagentTaskSpec::new("Code.", 120)
        .expect("valid")
        .with_max_model_turns_explicit(false)
        .with_role(Some("coder".to_owned()))
        .expect("valid");
    let resolved_coder = resolve_subagent_runtime(
        &catalog,
        &SubagentResolutionInput {
            task: &coder_task,
            parent_allowed_tools: &[],
            workspace_scope: &scope,
            parent_workspace_writable: true,
            depth: 1,
            max_depth: 1,
            max_model_turns_cap: 200,
            parent_primary_model: None,
            fallback_provider: fake_provider(),
            fallback_model: &fallback_model,
            parent_generation_config: &generation,
        },
    )
    .expect("coder resolves");
    assert_eq!(resolved_coder.max_model_turns, 80);

    let custom_task = SubagentTaskSpec::new("Help.", 120)
        .expect("valid")
        .with_max_model_turns_explicit(false)
        .with_role(Some("custom_helper".to_owned()))
        .expect("valid");
    let resolved_custom = resolve_subagent_runtime(
        &catalog,
        &SubagentResolutionInput {
            task: &custom_task,
            parent_allowed_tools: &[],
            workspace_scope: &scope,
            parent_workspace_writable: true,
            depth: 1,
            max_depth: 1,
            max_model_turns_cap: 200,
            parent_primary_model: None,
            fallback_provider: fake_provider(),
            fallback_model: &fallback_model,
            parent_generation_config: &generation,
        },
    )
    .expect("custom resolves");
    assert_eq!(resolved_custom.max_model_turns, 120);
}
