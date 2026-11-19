use crate::support::{
    events::{event_kind_names, pending_tool_call, resolved_tool_result},
    models::{
        ScriptedModelProvider, completed_outputs_event, completed_text_event, model_name,
        model_tool_call_with_args, model_tool_call_with_id,
    },
    runtime::{collect_step, runtime_with_registered_tool, session_id},
    tools::{ScriptedToolExecutor, path_tool_spec, test_tool_spec},
};
use merry_core::{RuntimeJournalPayload, ToolCallResultStatus, ToolName, ToolOutput};
use merry_llm::{FinishReason, ModelOutput, ModelToolCall, testing::FakeModelProvider};
use merry_runtime::{
    ProcessActionIntent, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput, RegisteredTool, Runtime,
    SkillCatalog, SkillMetadata, ToolExecutionContext, process_command_tool,
};
use serde_json::{Map, Value, json};
use std::{path::PathBuf, sync::Arc};

fn shell_process_tool_call() -> ModelToolCall {
    model_tool_call_with_args(
        "call-real-shell",
        "run_process",
        Map::from_iter([
            ("command".to_owned(), json!("echo ProcessRunner | wc -l")),
            ("cwd".to_owned(), json!(".")),
        ]),
    )
}

#[derive(Debug, Clone, Copy)]
struct ReadOnlyShellProcessRunner;

impl ProcessRunner for ReadOnlyShellProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ProcessRunnerError::Cancelled);
            }

            ProcessRunnerOutput::new(
                &intent,
                ProcessExitStatus::Exited(0),
                "1\n",
                false,
                "",
                false,
            )
            .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_only_shell_wrapper_records_input_and_result_artifacts() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(shell_process_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = Runtime::builder(session_id("provider-real-shell-runner"))
        .model_provider(Arc::new(provider), model_name())
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a shell command through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .allow_read_only_shell_process_actions(Arc::new(ReadOnlyShellProcessRunner))
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Run read-only shell pipeline.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("read-only shell wrapper should execute through the process runner");

    assert_eq!(event_kind_names(&execution_events), ["ToolCallResolved"]);
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);

    let result_content = runtime
        .read_tool_output(result.call_id())
        .await
        .expect("result output read")
        .expect("result output should be stored");
    let result_payload: Value =
        serde_json::from_str(result_content.as_str()).expect("result JSON should parse");
    assert_eq!(
        result_payload["permission_profile_id"],
        "process.shell.read_only"
    );
    assert_eq!(
        result_payload["input_evidence"]["script"],
        "echo ProcessRunner | wc -l"
    );
    assert_eq!(result_payload["stdout"]["text"], "1\n");
    assert_eq!(result_payload["stderr"]["text"], "");
}

#[tokio::test(flavor = "current_thread")]
async fn execute_registered_tool_success_records_artifact_resolves_and_compiles_continuation() {
    let call = model_tool_call_with_args(
        "call-success",
        "search_notes",
        Map::from_iter([("query".to_owned(), Value::String("alpha".to_owned()))]),
    );
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call.clone())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used tool result"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-success",
        provider.clone(),
        executor.clone(),
    );

    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    let pending = pending_tool_call(&pending_events).clone();
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");

    assert_eq!(event_kind_names(&execution_events), ["ToolCallResolved"]);
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4]
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[0].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(result.call_id(), pending.id());
    assert_eq!(
        runtime
            .read_tool_output(result.call_id())
            .await
            .expect("executor result output read"),
        Some(ToolOutput::text("search result\n"))
    );
    assert_eq!(executor.calls(), vec![pending.clone()]);

    let continuation_events = collect_step(&runtime, "Continue with result.").await;
    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert_eq!(
        continuation_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6, 7, 8]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools(), &[test_tool_spec("search_notes")]);
    assert!(requests[0].continuations().is_empty());
    assert_eq!(requests[1].tools(), &[test_tool_spec("search_notes")]);
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("tool result continuation should be compiled");
    assert_eq!(continuation.call().id().as_str(), "call-success");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(
        continuation.result().content().as_text(),
        Some("search result\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reading_catalog_skill_file_emits_skill_used_event() {
    let call = model_tool_call_with_args(
        "call-read-skill",
        "read_text",
        Map::from_iter([("path".to_owned(), Value::String("demo/SKILL.md".to_owned()))]),
    );
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "demo-skill",
            "Use for demo tasks.",
            PathBuf::from("demo/SKILL.md"),
            PathBuf::from("/skills"),
        )
        .expect("valid skill metadata"),
    ])
    .expect("valid skill catalog");
    let runtime = Runtime::builder(session_id("provider-skill-used"))
        .skill_catalog(catalog)
        .register_tool(RegisteredTool::read_only(
            path_tool_spec("read_text"),
            Arc::new(ScriptedToolExecutor::succeeding_text("# Demo\n")),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Use demo skill.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ToolCallResolved", "SkillUsed"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5]
    );
    assert!(matches!(
        &execution_events[1].payload,
        RuntimeJournalPayload::SkillUsed {
            skill_name,
            skill_md_path,
            tool_call_id,
        } if skill_name == "demo-skill"
            && skill_md_path == "demo/SKILL.md"
            && tool_call_id == pending.id()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_domain_failure_resolves_failed_without_runtime_failed() {
    let call = model_tool_call_with_id("call-domain-failure");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::failing_json("tool_lookup_failed", r#"{"ok":false}"#);
    let runtime = runtime_with_registered_tool("provider-execute-tool-failure", provider, executor);
    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("domain failure should resolve the pending call");

    assert_eq!(event_kind_names(&execution_events), ["ToolCallResolved"]);
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4]
    );
    assert!(
        execution_events
            .iter()
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::Failed { .. })),
        "tool domain failure must not emit RuntimeJournalPayload::Failed: {execution_events:?}"
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[0].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(result.call_id(), pending.id());
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should have diagnostic")
            .code(),
        "tool_lookup_failed"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(
        runtime
            .read_tool_output(result.call_id())
            .await
            .expect("domain failure output read"),
        Some(ToolOutput::json(r#"{"ok":false}"#))
    );
}
