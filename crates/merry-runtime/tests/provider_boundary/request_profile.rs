use crate::support::{
    events::{event_kind_names, pending_tool_call},
    models::{
        ScriptedModelProvider, completed_event, completed_outputs_event, model_name,
        model_tool_call,
    },
    runtime::{collect_step, record_valid_context, session_id},
    tools::{ScriptedToolExecutor, assert_tools_are_default_checkpoint_ref_plus, test_tool_spec},
};
use merry_llm::{FinishReason, ModelMessageRole, ModelOutput, testing::FakeModelProvider};
use merry_runtime::{
    ProjectRules, RegisteredTool, Runtime, SkillCatalog, SkillMetadata, TaskAnchor,
    ToolExecutionContext,
};
use std::{path::PathBuf, sync::Arc};

#[tokio::test(flavor = "current_thread")]
async fn registered_tool_specs_are_compiled_into_provider_request() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let tool = test_tool_spec("search_notes");
    let runtime = Runtime::builder(session_id("provider-registered-tool-spec"))
        .register_tool(RegisteredTool::read_only(
            tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "Use registered tools.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_tools_are_default_checkpoint_ref_plus(requests[0].tools(), &[tool]);
    assert!(
        requests[0]
            .tool_profile_hash()
            .as_str()
            .starts_with("fnv1a64:")
    );
    assert!(requests[0].continuations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn compiled_provider_request_tool_profile_hash_tracks_registered_tools() {
    let first_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let search_tool = test_tool_spec("search_notes");
    let read_tool = test_tool_spec("read_file");
    let first_runtime = Runtime::builder(session_id("provider-tool-profile-hash-first"))
        .register_tool(RegisteredTool::read_only(
            search_tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .register_tool(RegisteredTool::read_only(
            read_tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(first_provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&first_runtime, "Use registered tools.").await;
    let first_hash = first_provider.recorded_requests()[0]
        .tool_profile_hash()
        .clone();

    let reordered_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let reordered_runtime = Runtime::builder(session_id("provider-tool-profile-hash-reordered"))
        .register_tool(RegisteredTool::read_only(
            read_tool,
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .register_tool(RegisteredTool::read_only(
            search_tool,
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(reordered_provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&reordered_runtime, "Use registered tools.").await;
    assert_eq!(
        reordered_provider.recorded_requests()[0].tool_profile_hash(),
        &first_hash
    );

    let changed_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let changed_runtime = Runtime::builder(session_id("provider-tool-profile-hash-changed"))
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("read_file"),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(changed_provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&changed_runtime, "Use registered tools.").await;
    assert_ne!(
        changed_provider.recorded_requests()[0].tool_profile_hash(),
        &first_hash
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compiled_provider_request_stable_prefix_hash_tracks_base_instructions_and_tools_only() {
    let first_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let read_tool = test_tool_spec("read_file");
    let first_runtime = Runtime::builder(session_id("provider-stable-prefix-first"))
        .register_tool(RegisteredTool::read_only(
            read_tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(first_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&first_runtime, "First dynamic request.").await;
    let first_request = first_provider.recorded_requests()[0].clone();

    let dynamic_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let dynamic_runtime = Runtime::builder(session_id("provider-stable-prefix-dynamic"))
        .register_tool(RegisteredTool::read_only(
            read_tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(dynamic_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    let _snapshot = record_valid_context(&dynamic_runtime).await;
    collect_step(&dynamic_runtime, "Second dynamic request.").await;
    let dynamic_request = dynamic_provider.recorded_requests()[0].clone();

    assert_eq!(
        first_request.stable_prefix_hash(),
        dynamic_request.stable_prefix_hash()
    );
    assert_ne!(
        first_request.dynamic_context_hash(),
        dynamic_request.dynamic_context_hash()
    );

    let changed_tools_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let changed_tools_runtime = Runtime::builder(session_id("provider-stable-prefix-tool-change"))
        .model_provider(Arc::new(changed_tools_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&changed_tools_runtime, "First dynamic request.").await;
    assert_ne!(
        first_request.stable_prefix_hash(),
        changed_tools_provider.recorded_requests()[0].stable_prefix_hash()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_request_uses_xml_boundaries_for_prompt_context_blocks() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = Runtime::builder(session_id("provider-prompt-xml-boundaries"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Stable project rule sentinel.")
                .expect("valid project rules"),
        )
        .task_anchor(TaskAnchor::new("Keep the current task scoped.").expect("valid task anchor"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "Current user input sentinel.").await;
    let request = provider.recorded_requests()[0].clone();
    let stable_messages = request.stable_prefix_messages();

    assert!(
        stable_messages[0]
            .content()
            .as_text()
            .starts_with("<merry_runtime_instructions>\n")
    );
    assert!(
        stable_messages[0]
            .content()
            .as_text()
            .ends_with("\n</merry_runtime_instructions>")
    );
    assert!(
        stable_messages[1]
            .content()
            .as_text()
            .starts_with("<merry_project_rules>\n")
    );
    assert!(
        stable_messages[1]
            .content()
            .as_text()
            .ends_with("\n</merry_project_rules>")
    );

    let dynamic_messages = request.dynamic_messages();
    let task_anchor = dynamic_messages
        .iter()
        .find(|message| {
            message
                .content()
                .as_text()
                .contains("Keep the current task scoped.")
        })
        .expect("task anchor should be present in dynamic context");
    assert!(
        task_anchor
            .content()
            .as_text()
            .starts_with("<merry_task_anchor>\n")
    );
    assert!(
        task_anchor
            .content()
            .as_text()
            .ends_with("\n</merry_task_anchor>")
    );
    assert_eq!(
        dynamic_messages
            .last()
            .expect("current input should be present")
            .content()
            .as_text(),
        "Current user input sentinel."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn project_rules_enter_stable_prefix_and_affect_stable_hash() {
    let first_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let first_runtime = Runtime::builder(session_id("provider-project-rules-first"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use fixture rule A.\n").expect("valid project rules"),
        )
        .model_provider(Arc::new(first_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&first_runtime, "Inspect project.").await;
    let first_request = first_provider.recorded_requests()[0].clone();

    let changed_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let changed_runtime = Runtime::builder(session_id("provider-project-rules-changed"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use fixture rule B.\n").expect("valid project rules"),
        )
        .model_provider(Arc::new(changed_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&changed_runtime, "Inspect project.").await;
    let changed_request = changed_provider.recorded_requests()[0].clone();

    assert_eq!(first_request.stable_prefix_message_count(), 2);
    assert_eq!(first_request.stable_prefix_messages().len(), 2);
    assert_eq!(
        first_request.stable_prefix_messages()[1].role(),
        ModelMessageRole::System
    );
    assert!(
        first_request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("project-rules-source:AGENTS.md")
    );
    assert!(
        first_request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("Use fixture rule A.")
    );
    assert_ne!(
        first_request.stable_prefix_hash(),
        changed_request.stable_prefix_hash()
    );
    assert_eq!(
        first_request.dynamic_context_hash(),
        changed_request.dynamic_context_hash(),
        "same user input with changed project rules should keep dynamic body unchanged"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compiled_provider_request_skill_metadata_enters_stable_prefix_before_project_rules() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let skill_catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "frontend-design",
            "Use when building polished frontend UI.",
            PathBuf::from("skills/frontend-design/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect("valid skill metadata"),
    ])
    .expect("valid skill catalog");
    let runtime = Runtime::builder(session_id("provider-skill-prefix"))
        .skill_catalog(skill_catalog)
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use project rules sentinel.\n")
                .expect("valid project rules"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "Use the skill list.").await;
    let request = provider.recorded_requests()[0].clone();

    assert_eq!(request.stable_prefix_message_count(), 3);
    assert_eq!(request.stable_prefix_messages().len(), 3);
    assert!(
        request.stable_prefix_messages()[0]
            .content()
            .as_text()
            .contains("You are Merry")
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("## Skills")
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("read_text")
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("skills/frontend-design/SKILL.md")
    );
    assert!(
        !request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("full skill body sentinel")
    );
    assert!(
        request.stable_prefix_messages()[2]
            .content()
            .as_text()
            .contains("project-rules-source:AGENTS.md")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_exposes_skill_metadata_for_completion() {
    let catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "brainstorming",
            "Use for design discussion.",
            PathBuf::from("skills/brainstorming/SKILL.md"),
            PathBuf::from("skills"),
        )
        .expect("valid skill"),
    ])
    .expect("valid catalog");

    let runtime = Runtime::builder(session_id("runtime-skill-list"))
        .skill_catalog(catalog)
        .build()
        .expect("runtime builds");

    let skills = runtime.skills().await;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name(), "brainstorming");

    let found = runtime
        .find_skill("brainstorming")
        .await
        .expect("skill found");
    assert_eq!(found.description(), "Use for design discussion.");
}

#[tokio::test(flavor = "current_thread")]
async fn skill_metadata_changes_stable_prefix_but_not_dynamic_context() {
    let first_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let first_catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "frontend-design",
            "Use for UI work.",
            PathBuf::from("skills/frontend-design/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect("valid skill metadata"),
    ])
    .expect("valid catalog");
    let first_runtime = Runtime::builder(session_id("provider-skill-hash-first"))
        .skill_catalog(first_catalog)
        .model_provider(Arc::new(first_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&first_runtime, "Same dynamic input.").await;
    let first_request = first_provider.recorded_requests()[0].clone();

    let changed_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let changed_catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "frontend-design",
            "Use for UI and responsive layout work.",
            PathBuf::from("skills/frontend-design/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect("valid skill metadata"),
    ])
    .expect("valid catalog");
    let changed_runtime = Runtime::builder(session_id("provider-skill-hash-changed"))
        .skill_catalog(changed_catalog)
        .model_provider(Arc::new(changed_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&changed_runtime, "Same dynamic input.").await;
    let changed_request = changed_provider.recorded_requests()[0].clone();

    assert_ne!(
        first_request.stable_prefix_hash(),
        changed_request.stable_prefix_hash()
    );
    assert_eq!(
        first_request.dynamic_context_hash(),
        changed_request.dynamic_context_hash()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ledger_and_artifact_changes_do_not_change_project_rules_stable_hash() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_event())],
    ]);
    let runtime = Runtime::builder(session_id("provider-project-rules-dynamic-state"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use stable project rules.\n")
                .expect("valid project rules"),
        )
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("search_notes"),
            Arc::new(ScriptedToolExecutor::succeeding_text(
                "dynamic artifact payload\n",
            )),
        ))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Request tool result.").await;
    let first_hash = provider.recorded_requests()[0].stable_prefix_hash().clone();
    let pending = pending_tool_call(&pending_events).clone();
    runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");
    collect_step(&runtime, "Use the resolved tool result.").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].stable_prefix_message_count(), 2);
    assert_eq!(requests[1].stable_prefix_message_count(), 2);
    assert_eq!(requests[1].stable_prefix_hash(), &first_hash);
    assert_ne!(
        requests[0].dynamic_context_hash(),
        requests[1].dynamic_context_hash(),
        "tool exchange and transcript body remain dynamic"
    );
    assert!(
        requests[1]
            .stable_prefix_messages()
            .iter()
            .all(|message| !message
                .content()
                .as_text()
                .contains("dynamic artifact payload")),
        "ledger/artifact changes must not alter project-rules stable prefix"
    );
}
