use crate::{
    coding::{
        CodingRuntimeOptions, CodingSubagentsConfig, build_coding_runtime,
        tests::{resolved_tool_result, test_process_backend},
    },
    runtime_events::{collect_runtime_step_events, first_pending_tool_call},
    testing::{ScriptedProvider, model_name, workspace_tool_call},
};
use merry_core::ToolCallResultStatus;
use merry_llm::{FinishReason, ModelEvent, ModelOutput, ModelResponse};
use merry_runtime::{StepContext, StepInput, ToolExecutionContext};
use merry_tools::READ_TEXT_TOOL;
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn projects_skill_metadata_without_body() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let skill_root = temp.path().join("skills");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::create_dir_all(skill_root.join("demo")).expect("mkdir skill");
    std::fs::write(
        skill_root.join("demo/SKILL.md"),
        "---\nname: demo-skill\ndescription: Use for demo tasks.\n---\n# Demo\nbody sentinel\n",
    )
    .expect("write skill");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_runtime(
        "coding-loop-skill-prefix",
        &workspace,
        Arc::new(provider.clone()),
        model_name(),
        CodingRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: vec![skill_root.clone()],
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect skills.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");
    let request = provider.recorded_requests()[0].clone();
    let stable_text = request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    let request_text = request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(stable_text.contains("demo-skill"));
    assert!(stable_text.contains("Use for demo tasks."));
    assert!(stable_text.contains("demo/SKILL.md"));
    assert!(stable_text.contains("$skill-name"));
    assert!(request_text.contains("read_text"));
    assert!(request_text.contains("Coding file capabilities"));
    assert!(request_text.contains("user's current input language"));
    assert!(request_text.contains("configured sandbox/profile"));
    assert!(request_text.contains("network access may be intentionally restricted"));
    assert!(request_text.contains("call `request_permissions` for that exact action"));
    assert!(!stable_text.contains("body sentinel"));
}

#[tokio::test(flavor = "current_thread")]
async fn includes_skill_roots_in_workspace_read_tools() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let skill_root = temp.path().join("skills");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::create_dir_all(skill_root.join("demo")).expect("mkdir skill");
    std::fs::write(
        skill_root.join("demo/SKILL.md"),
        "---\nname: demo-skill\ndescription: Demo skill.\n---\n# Demo\n",
    )
    .expect("write skill");

    let provider = ScriptedProvider::new(vec![vec![Ok(workspace_tool_call(
        "call-read-skill",
        READ_TEXT_TOOL,
        [(
            "path",
            serde_json::Value::String("demo/SKILL.md".to_owned()),
        )],
    )
    .expect("workspace read call should build"))]]);
    let runtime = build_coding_runtime(
        "coding-loop-skill-root-read",
        &workspace,
        Arc::new(provider.clone()),
        model_name(),
        CodingRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: vec![skill_root.clone()],
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    let events = collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Read demo skill.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should collect pending skill read");
    let pending = first_pending_tool_call(&events).expect("pending skill read");
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("skill read should execute");
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
}

#[tokio::test(flavor = "current_thread")]
async fn allows_missing_default_skill_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let missing_skill_root = temp.path().join("config/merry/skills");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_runtime(
        "coding-loop-missing-default-skill-root",
        &workspace,
        Arc::new(provider.clone()),
        model_name(),
        CodingRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: vec![missing_skill_root],
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("missing default skill root should not block runtime");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Run without configured skills.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");

    let request = provider.recorded_requests()[0].clone();
    assert!(
        request
            .stable_prefix_messages()
            .iter()
            .all(|message| !message.content().as_text().contains("## Skills"))
    );
}
