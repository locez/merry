use crate::{
    coding::{
        CodingRuntimeOptions, CodingSubagentsConfig, build_coding_runtime, build_headless_coding,
        resume_headless_coding,
        tests::{headless_input, test_process_backend},
    },
    runtime_events::collect_runtime_step_events,
    testing::{FakeProcessRunner, ScriptedProvider, model_name},
};
use merry_core::SessionId;
use merry_llm::{FinishReason, ModelEvent, ModelOutput, ModelResponse};
use merry_runtime::{
    FileSessionStore, PermissionedProcessRunnerFactory, ProcessRunner, ProjectRules, Runtime,
    StepContext, StepInput,
};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn coding_projects_root_agents_in_the_stable_prefix() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::write(
        workspace.join("AGENTS.md"),
        "Use root project rule sentinel.\r\nSecond root project rule sentinel.\r\n",
    )
    .expect("write root project rules");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_runtime(
        "coding-loop-root-project-rules",
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
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect project rules.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");

    let requests = provider.recorded_requests();
    let request = &requests[0];
    let stable_text = request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(request.stable_prefix_message_count(), 4);
    assert!(stable_text.contains("project-rules-source:AGENTS.md"));
    assert!(
        stable_text
            .contains("Use root project rule sentinel.\nSecond root project rule sentinel.\n")
    );
    assert!(!stable_text.contains('\r'));
}

#[tokio::test(flavor = "current_thread")]
async fn coding_omits_project_rules_when_root_agents_is_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_runtime(
        "coding-loop-missing-root-project-rules",
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
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect project rules.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");

    let requests = provider.recorded_requests();
    let request = &requests[0];
    assert!(request.stable_prefix_messages().iter().all(|message| {
        !message
            .content()
            .as_text()
            .contains("project-rules-source:")
    }));
    assert!(request.messages().iter().all(|message| {
        !message
            .content()
            .as_text()
            .contains("AGENTS.md unavailable")
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn resumed_coding_reloads_current_root_agents() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let rules_path = workspace.join("AGENTS.md");
    std::fs::write(&rules_path, "Use saved project rule A.\n").expect("write rule A");
    let store = FileSessionStore::new(temp.path().join("sessions"));
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
    let permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory> = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );

    let first_provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("first done")],
            FinishReason::Stop,
            None,
        ),
    })]]);
    let first_runtime = build_headless_coding(headless_input(
        "headless-resume-reloads-project-rules",
        &workspace,
        Arc::new(first_provider.clone()),
        Arc::clone(&runner),
        Arc::clone(&permissioned_factory),
    ))
    .expect("first runtime should build");
    collect_runtime_step_events(
        &first_runtime,
        StepInput::user_text("Inspect rule A.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("first runtime step should complete");
    first_runtime
        .save_session_to(store.clone())
        .await
        .expect("first runtime should save");
    let first_requests = first_provider.recorded_requests();
    let first_stable_prefix_hash = first_requests[0].stable_prefix_hash().clone();

    std::fs::write(&rules_path, "Use current project rule B.\n").expect("write rule B");
    let resumed_provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("resumed done")],
            FinishReason::Stop,
            None,
        ),
    })]]);
    let resumed_runtime = resume_headless_coding(
        headless_input(
            "headless-resume-reloads-project-rules",
            &workspace,
            Arc::new(resumed_provider.clone()),
            runner,
            permissioned_factory,
        ),
        store,
    )
    .await
    .expect("runtime should resume");
    collect_runtime_step_events(
        &resumed_runtime,
        StepInput::user_text("Inspect current rules.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("resumed runtime step should complete");

    let resumed_requests = resumed_provider.recorded_requests();
    let resumed_request = &resumed_requests[0];
    let resumed_stable_text = resumed_request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(resumed_stable_text.contains("Use current project rule B."));
    assert!(!resumed_stable_text.contains("Use saved project rule A."));
    assert_ne!(
        resumed_request.stable_prefix_hash(),
        &first_stable_prefix_hash
    );
}

#[tokio::test(flavor = "current_thread")]
async fn resumed_coding_replaces_stale_project_capability_seed() {
    const SESSION_ID: &str = "headless-resume-refreshes-project-capabilities";
    const OLD_LANGUAGE_RULE: &str = "- Respond in the user's current input language by default unless the user explicitly requests another language.";
    const OLD_AGENTS_CAPABILITY: &str = "Detected AGENTS.md at the workspace root; read and follow it as project-specific instructions before substantial work.";
    const OLD_CAPABILITY_TEXT: &str = "Workspace coding profile:\n- Respond in the user's current input language by default unless the user explicitly requests another language.\nDetected AGENTS.md at the workspace root; read and follow it as project-specific instructions before substantial work.";
    const CURRENT_RULE: &str = "Use current resume rule sentinel.\n";
    const OLD_RULE: &str = "Use old persisted rule sentinel.\n";

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname = \"resume-context-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(workspace.join("AGENTS.md"), CURRENT_RULE).expect("write current rules");
    let store = FileSessionStore::new(temp.path().join("sessions"));
    let old_runtime = Runtime::builder(SessionId::new(SESSION_ID).expect("valid session id"))
        .initial_context_summary("project-capabilities", OLD_CAPABILITY_TEXT)
        .project_rules(ProjectRules::new("AGENTS.md", OLD_RULE).expect("old project rules"))
        .build()
        .expect("old runtime builds");
    old_runtime
        .save_session_to(store.clone())
        .await
        .expect("old runtime saves");

    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
    let permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory> = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );
    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("resumed done")],
            FinishReason::Stop,
            None,
        ),
    })]]);
    let resumed_runtime = resume_headless_coding(
        headless_input(
            SESSION_ID,
            &workspace,
            Arc::new(provider.clone()),
            runner,
            permissioned_factory,
        ),
        store,
    )
    .await
    .expect("coding runtime resumes");
    collect_runtime_step_events(
        &resumed_runtime,
        StepInput::user_text("Inspect current capabilities.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("resumed runtime step completes");

    let requests = provider.recorded_requests();
    let request = &requests[0];
    let request_text = request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        request_text.matches("summary:project-capabilities").count(),
        1
    );
    assert!(request_text.contains("Coding file capabilities:"));
    assert!(request_text.contains("Cargo.toml is present"));
    assert!(!request_text.contains(OLD_LANGUAGE_RULE));
    assert!(!request_text.contains(OLD_AGENTS_CAPABILITY));

    let stable_text = request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(stable_text.matches(CURRENT_RULE.trim()).count(), 1);
    assert!(!stable_text.contains(OLD_RULE.trim()));
}
