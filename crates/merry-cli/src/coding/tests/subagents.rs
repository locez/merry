use crate::{
    coding::{
        CodingRuntimeOptions, CodingSubagentsConfig, build_coding_runtime,
        tests::test_process_backend,
    },
    runtime_events::collect_runtime_step_events,
    testing::{ScriptedProvider, model_name},
};
use merry_core::ToolName;
use merry_llm::{
    FinishReason, ModelEvent, ModelMessageRole, ModelOutput, ModelResponse, ModelToolCall,
    ModelToolCallId, ToolArguments,
};
use merry_runtime::{AgentLoopConfig, AgentLoopStatus, StepContext, StepInput, SubagentConfig};
use serde_json::{Map, Value};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn hides_subagent_tools_by_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_runtime(
        "coding-loop-subagents-default-off",
        &workspace,
        Arc::new(provider.clone()),
        model_name(),
        CodingRuntimeOptions {
            approval_review: None,
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect available tools.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");
    let requests = provider.recorded_requests();
    let tool_names = requests[0]
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();

    assert!(!tool_names.contains(&"spawn_subagents"));
    assert!(!tool_names.contains(&"wait_subagents"));
    assert!(!tool_names.contains(&"cancel_subagents"));
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_subagent_tools_when_enabled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_runtime(
        "coding-loop-subagents-enabled",
        &workspace,
        Arc::new(provider.clone()),
        model_name(),
        CodingRuntimeOptions {
            approval_review: None,
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::enabled(
                SubagentConfig::new(2, 1)
                    .expect("valid subagent config")
                    .with_model_turn_bounds(2048, 2048)
                    .expect("coding subagent bounds should be valid"),
            ),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect available tools.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");
    let requests = provider.recorded_requests();
    let tool_names = requests[0]
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();

    assert!(tool_names.contains(&"spawn_subagents"));
    assert!(tool_names.contains(&"wait_subagents"));
    assert!(tool_names.contains(&"cancel_subagents"));
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_with_narrow_tools_keeps_stable_profile_and_runtime_admission() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::write(workspace.join("README.md"), "child fixture\n").expect("write fixture");
    std::fs::write(
        workspace.join("AGENTS.md"),
        "Child must receive root rule sentinel.\n",
    )
    .expect("write root project rules");

    let completion_step = || {
        vec![Ok::<_, merry_llm::ModelError>(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]
    };
    let provider = ScriptedProvider::new(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-spawn").expect("valid call id"),
                    ToolName::new("spawn_subagents").expect("valid tool name"),
                    ToolArguments::try_from(Value::Object(Map::from_iter([(
                        "tasks".to_owned(),
                        Value::Array(vec![Value::Object(Map::from_iter([
                            (
                                "task".to_owned(),
                                Value::String("Inspect the fixture.".to_owned()),
                            ),
                            (
                                "max_model_turns".to_owned(),
                                Value::Number(serde_json::Number::from(2048)),
                            ),
                            (
                                "allowed_tools".to_owned(),
                                Value::Array(vec![Value::String("read_file".to_owned())]),
                            ),
                            ("write_scope".to_owned(), Value::Array(Vec::new())),
                        ]))]),
                    )])))
                    .expect("valid spawn args"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        completion_step(),
        completion_step(),
        completion_step(),
        completion_step(),
        completion_step(),
        completion_step(),
        completion_step(),
    ]);
    let runtime = build_coding_runtime(
        "coding-loop-subagent-narrow-tools",
        &workspace,
        Arc::new(provider.clone()),
        model_name(),
        CodingRuntimeOptions {
            approval_review: None,
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::enabled(
                SubagentConfig::new(2, 1)
                    .expect("valid subagent config")
                    .with_model_turn_bounds(2048, 2048)
                    .expect("coding subagent bounds should be valid"),
            ),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    std::fs::write(
        workspace.join("AGENTS.md"),
        "Child must not reread changed root rule sentinel.\n",
    )
    .expect("replace root project rules after parent build");

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Delegate fixture inspection.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(3).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    let requests = provider.recorded_requests();
    assert!(
        requests.len() >= 2,
        "parent and child requests should be recorded"
    );
    let parent_system_text = requests[0]
        .dynamic_messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::System)
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(parent_system_text.contains("read_scope: [\".\"]"));
    assert!(parent_system_text.contains("write_scope: [\".\"]"));
    assert!(parent_system_text.contains("forbidden_paths: []"));
    let child_request = requests
        .iter()
        .find(|request| {
            request
                .dynamic_messages()
                .iter()
                .any(|message| message.content().as_text().contains("Inspect the fixture."))
        })
        .expect("child request should be recorded");
    let child_system_text = child_request
        .dynamic_messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::System)
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(child_system_text.contains("<merry_subagent_workspace_scope>"));
    assert!(child_system_text.contains("read_scope: [\".\"]"));
    assert!(child_system_text.contains("write_scope: []"));
    assert!(child_system_text.contains("forbidden_paths: []"));
    let child_user_text = child_request
        .dynamic_messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::User)
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(child_user_text, "Inspect the fixture.");
    assert!(!child_user_text.contains("read_scope"));
    let child_tool_names = child_request
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();
    let child_stable_text = child_request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(child_stable_text.contains("project-rules-source:AGENTS.md"));
    assert!(child_stable_text.contains("Child must receive root rule sentinel."));
    assert!(!child_stable_text.contains("Child must not reread changed root rule sentinel."));
    assert!(child_tool_names.contains(&"read_file"));
    assert_eq!(
        child_tool_names,
        [
            "run_process",
            "request_permissions",
            "read_file",
            "apply_patch",
            "web_fetch",
            "browser_tab",
            "browser_network",
            "spawn_subagents",
            "send_subagent_message",
            "wait_subagents",
            "cancel_subagents",
        ]
    );
    assert!(
        child_request
            .tool_profile_hash()
            .as_str()
            .starts_with("fnv1a64:")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn custom_browser_role_routes_subagents_to_role_provider_and_model() {
    use merry::profiles::{SubagentModelOverride, SubagentRoleCatalog, SubagentRoleOverlay};
    use merry_llm::ModelName;

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let completion_step = || {
        vec![Ok::<_, merry_llm::ModelError>(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]
    };
    let primary = ScriptedProvider::new(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-spawn-browser").expect("valid call id"),
                    ToolName::new("spawn_subagents").expect("valid tool name"),
                    ToolArguments::try_from(Value::Object(Map::from_iter([(
                        "tasks".to_owned(),
                        Value::Array(vec![Value::Object(Map::from_iter([
                            (
                                "task".to_owned(),
                                Value::String("Verify the login form UI.".to_owned()),
                            ),
                            ("role".to_owned(), Value::String("browser".to_owned())),
                        ]))]),
                    )])))
                    .expect("valid spawn args"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        completion_step(),
        completion_step(),
        completion_step(),
        completion_step(),
    ]);
    let browser_provider = ScriptedProvider::new(vec![
        completion_step(),
        completion_step(),
        completion_step(),
    ]);

    let mut catalog = SubagentRoleCatalog::builtin();
    catalog
        .apply_role_overlay(
            "browser",
            SubagentRoleOverlay {
                base: Some("review".to_owned()),
                description: Some("UI verification specialist".to_owned()),
                model_override: Some(SubagentModelOverride {
                    provider: Arc::new(browser_provider.clone()),
                    model: ModelName::new("browser-vision-test").expect("valid browser model"),
                    retry_policy: Some(merry_llm::ModelRetryPolicy::disabled()),
                }),
                allowed_tools: Some(vec![
                    ToolName::new("browser_tab").expect("valid tool"),
                    ToolName::new("browser_network").expect("valid tool"),
                    ToolName::new("read_file").expect("valid tool"),
                ]),
                ..SubagentRoleOverlay::default()
            },
        )
        .expect("custom browser role should apply");

    let runtime = build_coding_runtime(
        "coding-loop-browser-role-routing",
        &workspace,
        Arc::new(primary.clone()),
        model_name(),
        CodingRuntimeOptions {
            approval_review: None,
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            retry_policy: Some(merry_llm::ModelRetryPolicy::disabled()),
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::enabled(
                SubagentConfig::new(2, 1)
                    .expect("valid subagent config")
                    .with_model_turn_bounds(1, 80)
                    .expect("coding subagent bounds should be valid"),
            )
            .with_role_catalog(catalog),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Verify UI.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(3).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    // Parent tool description must advertise the custom browser role.
    let parent_spawn_desc = primary.recorded_requests()[0]
        .tools()
        .iter()
        .find(|tool| tool.name().as_str() == "spawn_subagents")
        .expect("spawn_subagents tool present")
        .description()
        .to_owned();
    assert!(
        parent_spawn_desc.contains("browser"),
        "expected custom browser role advertised in spawn_subagents description: {parent_spawn_desc}"
    );

    let browser_requests = browser_provider.recorded_requests();
    assert!(!browser_requests.is_empty());
    assert_eq!(browser_requests[0].model().as_str(), "browser-vision-test");
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_primary_model_switch_routes_subsequent_subagents_to_new_model() {
    use merry_core::{InteractiveRunState, RuntimeEvent};
    use merry_llm::{ModelName, ModelRetryPolicy};
    use merry_runtime::{InteractivePrimaryModel, InteractiveSettingsUpdate};

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let completion_step = || {
        vec![Ok::<_, merry_llm::ModelError>(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]
    };
    let initial_provider = ScriptedProvider::new(vec![completion_step()]);
    let switched_provider = ScriptedProvider::new(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-spawn-switched").expect("valid call id"),
                    ToolName::new("spawn_subagents").expect("valid tool name"),
                    ToolArguments::try_from(Value::Object(Map::from_iter([
                        (
                            "tasks".to_owned(),
                            Value::Array(vec![Value::Object(Map::from_iter([
                                (
                                    "task".to_owned(),
                                    Value::String(
                                        "Inspect workspace after model switch.".to_owned(),
                                    ),
                                ),
                                ("role".to_owned(), Value::String("explore".to_owned())),
                            ]))]),
                        ),
                        ("wait".to_owned(), Value::String("all".to_owned())),
                    ])))
                    .expect("valid spawn args"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        completion_step(),
        completion_step(),
        completion_step(),
    ]);

    let runtime = build_coding_runtime(
        "coding-loop-live-model-switch",
        &workspace,
        Arc::new(initial_provider.clone()),
        ModelName::new("initial-model").expect("valid model"),
        CodingRuntimeOptions {
            approval_review: None,
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            retry_policy: Some(ModelRetryPolicy::disabled()),
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::runtime_controlled(
                true,
                SubagentConfig::new(2, 1)
                    .expect("valid subagent config")
                    .with_model_turn_bounds(1, 80)
                    .expect("coding subagent bounds should be valid"),
            ),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::default(),
            AgentLoopConfig::new(4).expect("valid loop config"),
        )
        .expect("interactive run should start");
    let (mut stream, input, control) = run.split();

    control
        .update_settings(InteractiveSettingsUpdate::default().with_primary_model(
            InteractivePrimaryModel::new(
                Arc::new(switched_provider.clone()),
                ModelName::new("switched-model").expect("valid switched model"),
                ModelRetryPolicy::disabled(),
            ),
        ))
        .await
        .expect("settings update should apply");

    input
        .submit("Delegate after switching models.")
        .await
        .expect("user message should enqueue");

    let mut saw_active = false;
    while let Some(event) = stream
        .next_event()
        .await
        .expect("interactive stream event should succeed")
    {
        match event {
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            } if saw_active => break,
            RuntimeEvent::InteractiveRunStateChanged { state }
                if state != InteractiveRunState::WaitingForInput =>
            {
                saw_active = true;
            }
            _ => {}
        }
    }
    drop(input);
    control.close().await.expect("interactive run should close");
    while stream
        .next_event()
        .await
        .expect("interactive stream drain should succeed")
        .is_some()
    {}

    assert!(
        initial_provider.recorded_requests().is_empty(),
        "initial provider should not be called after switching primary model"
    );
    let switched_requests = switched_provider.recorded_requests();
    assert!(
        switched_requests.len() >= 2,
        "both parent and child should call the switched provider"
    );
    assert!(
        switched_requests
            .iter()
            .all(|req| req.model().as_str() == "switched-model"),
        "child must dynamically inherit switched-model from live parent runtime"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_emits_journal_events_rolls_up_usage_persists_and_resumes_with_wake() {
    use crate::coding::{
        CodingPermissionPolicy, HeadlessCodingRuntimeInput,
        build_headless_coding_with_policy_composition,
        resume_headless_coding_composition_with_loaded_session,
    };
    use merry_core::{ModelUsage, RuntimeJournalPayload, SessionId, SubagentId};
    use merry_runtime::{FileSessionStore, LoadedSession, PermissionReviewRules};

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::create_dir_all(&sessions_dir).expect("mkdir sessions");
    let store = FileSessionStore::new(&sessions_dir);
    let session_id = "coding-subagent-slice4-persist";

    let provider = ScriptedProvider::new(vec![
        // Parent turn 1: spawn subagent with display_name + role and wait.
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-spawn-s4").expect("valid call id"),
                    ToolName::new("spawn_subagents").expect("valid tool name"),
                    ToolArguments::try_from(Value::Object(Map::from_iter([
                        (
                            "tasks".to_owned(),
                            Value::Array(vec![Value::Object(Map::from_iter([
                                (
                                    "task".to_owned(),
                                    Value::String("Audit module layout.".to_owned()),
                                ),
                                (
                                    "display_name".to_owned(),
                                    Value::String("auditor".to_owned()),
                                ),
                                ("role".to_owned(), Value::String("explore".to_owned())),
                            ]))]),
                        ),
                        ("wait".to_owned(), Value::String("all".to_owned())),
                    ])))
                    .expect("valid spawn args"),
                ))],
                FinishReason::ToolCalls,
                Some(ModelUsage::new(500, 50)),
            ),
        })],
        // Child turn 1: completes with reported token usage.
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("Audit complete: all clean.")],
                FinishReason::Stop,
                Some(ModelUsage::new(120, 30)),
            ),
        })],
        // Parent turn 2: final reply after wait_subagents completes.
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("Parent synthesized auditor report.")],
                FinishReason::Stop,
                Some(ModelUsage::new(600, 40)),
            ),
        })],
    ]);

    let make_input = |prov: ScriptedProvider| HeadlessCodingRuntimeInput {
        session_id,
        root: &workspace,
        provider: Arc::new(prov),
        model: model_name(),
        process_backend: test_process_backend(),
        extra_tools: Vec::new(),
        automatic_compaction: merry_runtime::CompactionConfig::disabled(),
        history_retention: merry_runtime::HistoryRetention::default(),
        retry_policy: Some(merry_llm::ModelRetryPolicy::disabled()),
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::enabled(
            SubagentConfig::new(2, 1)
                .expect("valid subagent config")
                .with_model_turn_bounds(1, 80)
                .expect("valid bounds"),
        ),
        workspace_tool_limits: None,
        workspace_writable: true,
        denied_absolute_paths: Vec::new(),
    };

    let runtime = build_headless_coding_with_policy_composition(
        make_input(provider),
        CodingPermissionPolicy::default(),
        PermissionReviewRules::default(),
        Some(store.clone()),
        None,
    )
    .expect("runtime builds")
    .into_runtime();

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Run an audit.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(4).expect("valid loop config"),
        )
        .await
        .expect("agent loop succeeds");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    // 1. Journal events must contain Spawned (with display_name/role), Started, and Completed (with usage).
    assert!(result.events().iter().any(|ev| matches!(
        &ev.payload,
        RuntimeJournalPayload::SubagentSpawned {
            display_name: Some(name),
            role: Some(role),
            ..
        } if name == "auditor" && role == "explore"
    )));
    assert!(
        result
            .events()
            .iter()
            .any(|ev| matches!(&ev.payload, RuntimeJournalPayload::SubagentStarted { .. }))
    );
    assert!(result.events().iter().any(|ev| matches!(
        &ev.payload,
        RuntimeJournalPayload::SubagentCompleted {
            usage: Some(usage),
            ..
        } if usage.input_tokens == 120 && usage.output_tokens == 30
    )));

    // 2. Parent SessionUsage must fold child usage into total & subagent_total while preserving parent last.
    let usage = result
        .session_usage()
        .expect("session usage must be present");
    assert_eq!(usage.total.input_tokens, 500 + 600 + 120);
    assert_eq!(usage.total.output_tokens, 50 + 40 + 30);
    let sub_usage = usage
        .subagent_total
        .expect("subagent_total must be tracked");
    assert_eq!(sub_usage.input_tokens, 120);
    assert_eq!(sub_usage.output_tokens, 30);
    assert_eq!(usage.last.input_tokens, 600);
    assert_eq!(usage.last.output_tokens, 40);

    // 3. Child journal must live under <sessions_dir>/<parent_id>/subagents/<agent_id>/journal.jsonl
    //    and NOT pollute top-level sessions_dir.
    let snapshots = runtime
        .subagent_snapshot()
        .await
        .expect("subagent manager present");
    assert_eq!(snapshots.len(), 1);
    let agent_id = snapshots[0].agent_id.clone();
    let child_journal = sessions_dir
        .join(session_id)
        .join("subagents")
        .join(agent_id.as_str())
        .join("journal.jsonl");
    assert!(
        child_journal.is_file(),
        "expected child journal at {}",
        child_journal.display()
    );
    let top_level_dirs: Vec<_> = std::fs::read_dir(&sessions_dir)
        .expect("read sessions_dir")
        .filter_map(|e| e.ok())
        .filter(|entry| entry.file_type().is_ok_and(|ft| ft.is_dir()))
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(
        top_level_dirs,
        vec![std::ffi::OsString::from(session_id)],
        "only parent session dir should exist at top level"
    );

    // Persist parent session and drop original runtime.
    runtime.save_session().await.expect("save parent session");
    drop(runtime);

    // 4. Rebuild fresh parent Runtime from store: restored cards, lazy inspect, and wake from disk!
    let wake_provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("Follow-up after restart completed.")],
            FinishReason::Stop,
            Some(ModelUsage::new(80, 20)),
        ),
    })]]);
    let loaded = LoadedSession::load(&store, &SessionId::new(session_id).expect("valid id"))
        .await
        .expect("load parent session");
    let resumed = resume_headless_coding_composition_with_loaded_session(
        make_input(wake_provider.clone()),
        loaded,
        CodingPermissionPolicy::default(),
        PermissionReviewRules::default(),
        Some(store.clone()),
        None,
    )
    .expect("resumed runtime builds")
    .into_runtime();

    let restored = resumed
        .subagent_snapshot()
        .await
        .expect("subagent manager present after resume");
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].agent_id, agent_id);
    assert_eq!(restored[0].display_name.as_deref(), Some("auditor"));
    assert_eq!(restored[0].role.as_deref(), Some("explore"));

    // Lazy inspect reads child transcript from disk even though child runtime isn't in memory.
    let inspect = resumed
        .inspect_subagent(&agent_id)
        .await
        .expect("lazy inspect succeeds after resume");
    assert!(
        !inspect.transcript.is_empty(),
        "lazy inspect should load persisted child transcript"
    );

    // Wake persisted subagent via send_subagent_message and confirm historical transcript is preserved!
    resumed
        .send_subagent_message(&agent_id, "Double-check error handling.".to_owned())
        .await
        .expect("waking persisted subagent should succeed");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let views = resumed.subagent_snapshot().await.unwrap_or_default();
        if views
            .iter()
            .find(|v| v.agent_id == agent_id)
            .is_some_and(|v| {
                v.result
                    .as_ref()
                    .is_some_and(|r| r.conclusion.contains("Follow-up after restart"))
            })
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "woken persisted subagent did not finish in time: {views:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let wake_requests = wake_provider.recorded_requests();
    assert_eq!(wake_requests.len(), 1);
    let wake_messages = wake_requests[0]
        .messages()
        .iter()
        .map(|m| m.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        wake_messages.contains("Audit complete: all clean."),
        "woken child must carry its persisted historical transcript: {wake_messages}"
    );
    assert!(wake_messages.contains("Double-check error handling."));
    let _ = SubagentId::new(agent_id.as_str()).expect("valid id");
}

#[tokio::test(flavor = "current_thread")]
async fn child_permission_request_carries_subagent_actor_attribution() {
    use crate::coding::{
        CodingPermissionPolicy, HeadlessCodingRuntimeInput,
        build_headless_coding_with_policy_composition,
    };
    use crate::permission_prompt::{PermissionReviewView, permission_review_lines};
    use merry_runtime::{ChannelPermissionAdmissionSource, PermissionActor, PermissionReviewRules};

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let provider = ScriptedProvider::new(vec![
        // Parent spawns a named coder subagent.
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-spawn-perm").expect("valid call id"),
                    ToolName::new("spawn_subagents").expect("valid tool name"),
                    ToolArguments::try_from(Value::Object(Map::from_iter([
                        (
                            "tasks".to_owned(),
                            Value::Array(vec![Value::Object(Map::from_iter([
                                (
                                    "task".to_owned(),
                                    Value::String("Run network fetch.".to_owned()),
                                ),
                                (
                                    "display_name".to_owned(),
                                    Value::String("fetcher".to_owned()),
                                ),
                                ("role".to_owned(), Value::String("coder".to_owned())),
                            ]))]),
                        ),
                        ("wait".to_owned(), Value::String("all".to_owned())),
                    ])))
                    .expect("valid spawn args"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        // Child requests network permission via request_permissions.
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-child-perm").expect("valid call id"),
                    ToolName::new("request_permissions").expect("valid tool name"),
                    ToolArguments::try_from(serde_json::json!({
                        "reason": "fetch package metadata",
                        "requested": {"network": true},
                        "for_action": {
                            "command": "echo hello",
                            "cwd": "."
                        }
                    }))
                    .expect("valid perm args"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        // Child finishes after approval.
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("child done")],
                FinishReason::Stop,
                None,
            ),
        })],
        // Parent finishes.
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("parent done")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);

    let (source, mut rx) = ChannelPermissionAdmissionSource::channel(4);
    let runtime = build_headless_coding_with_policy_composition(
        HeadlessCodingRuntimeInput {
            session_id: "coding-child-permission-actor",
            root: &workspace,
            provider: Arc::new(provider),
            model: model_name(),
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            history_retention: merry_runtime::HistoryRetention::default(),
            retry_policy: Some(merry_llm::ModelRetryPolicy::disabled()),
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::enabled(
                SubagentConfig::new(2, 1)
                    .expect("valid subagent config")
                    .with_model_turn_bounds(1, 80)
                    .expect("valid bounds"),
            ),
            workspace_tool_limits: None,
            workspace_writable: true,
            denied_absolute_paths: Vec::new(),
        },
        CodingPermissionPolicy::host_decision_only(Arc::new(source)),
        PermissionReviewRules::default(),
        None,
        None,
    )
    .expect("runtime builds")
    .into_runtime();

    let reviewer_task = tokio::spawn(async move {
        let req = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for child permission request")
            .expect("child permission request must reach host channel");
        let actor = req.actor().clone();
        let view = PermissionReviewView::from_request(&req, 1);
        let lines = permission_review_lines(&req);
        req.approve("allowed in test").expect("approve succeeds");
        (actor, view.actor_label, lines)
    });

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Delegate network check.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(4).expect("valid loop config"),
        )
        .await
        .expect("agent loop completes");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    let (actor, actor_label, lines) = reviewer_task.await.expect("reviewer task completes");
    match actor {
        PermissionActor::Subagent {
            display_name, role, ..
        } => {
            assert_eq!(display_name.as_deref(), Some("fetcher"));
            assert_eq!(role.as_deref(), Some("coder"));
        }
        PermissionActor::MainAgent => panic!("expected Subagent permission actor, got MainAgent"),
    }
    let label = actor_label.expect("subagent actor_label must be present");
    assert!(label.contains("fetcher"), "{label}");
    assert!(label.contains("[coder]"), "{label}");
    assert!(
        lines.first().is_some_and(|line| line.starts_with("Actor:")),
        "headless prompt should display Actor on first line: {lines:?}"
    );
}
