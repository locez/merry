use crate::support::{
    events::{assert_continuation_request_body, event_kind_names},
    models::{
        ScriptedModelProvider, completed_text_event, completed_tool_call_event, model_name,
        model_tool_call, model_tool_call_with_arguments,
    },
    process::RecordingProcessRunner,
    runtime::{run_default_loop, session_id},
    tools::tool_spec,
};
use merry_core::{PendingToolCall, RuntimeJournalPayload, ToolCallResultStatus, ToolName};
use merry_runtime::{
    ActionExecutionEvidence, ActionProposal, ActionProposalEvidence, AgentLoopStatus,
    ProcessEnvPolicy, Runtime, ToolActionKind, ToolActionPreflight, ToolActionProposalFuture,
    ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
    process_command_tool,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct ProposingPatchToolExecutor {
    proposed_calls: Arc<Mutex<Vec<PendingToolCall>>>,
    executed_calls: Arc<Mutex<Vec<PendingToolCall>>>,
}

impl ProposingPatchToolExecutor {
    fn new() -> Self {
        Self {
            proposed_calls: Arc::new(Mutex::new(Vec::new())),
            executed_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn proposed_calls(&self) -> Vec<PendingToolCall> {
        self.proposed_calls
            .lock()
            .expect("proposed calls mutex should not be poisoned")
            .clone()
    }

    fn executed_calls(&self) -> Vec<PendingToolCall> {
        self.executed_calls
            .lock()
            .expect("executed calls mutex should not be poisoned")
            .clone()
    }
}

impl ToolExecutor for ProposingPatchToolExecutor {
    fn propose<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolActionProposalFuture<'a> {
        Box::pin(async move {
            self.proposed_calls
                .lock()
                .expect("proposed calls mutex should not be poisoned")
                .push(call.clone());

            let patch = WorkspacePatchProposal::new(
                "notes/proposed.txt",
                5,
                7,
                20,
                22,
                "fnv1a64:0000000000000010",
                "fnv1a64:0000000000000011",
            )
            .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            let proposal = ActionProposal::new(
                &call,
                ToolActionKind::WorkspaceWrite,
                "workspace patch",
                "notes/proposed.txt",
                "Replace one matched preimage in notes/proposed.txt",
                ActionProposalEvidence::WorkspacePatch(patch),
            )
            .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            Ok(ToolActionPreflight::Proposal(proposal))
        })
    }

    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.executed_calls
                .lock()
                .expect("executed calls mutex should not be poisoned")
                .push(call);
            let evidence = WorkspacePatchExecutionEvidence::new(
                "notes/proposed.txt",
                5,
                7,
                20,
                22,
                "fnv1a64:0000000000000010",
                "fnv1a64:0000000000000011",
            )
            .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            Ok(ToolExecutionOutcome::succeeded_text("patch applied\n")
                .with_execution_evidence(ActionExecutionEvidence::WorkspacePatch(evidence)))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_executes_opt_in_apply_patch_and_continues_to_final_completion() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-patch-success",
            "apply_patch",
        )))],
        vec![Ok(completed_text_event("final after patch"))],
    ]);
    let executor = ProposingPatchToolExecutor::new();
    let runtime = Runtime::builder(session_id("agent-loop-opt-in-workspace-patch"))
        .register_tool(
            merry_runtime::RegisteredTool::new(
                tool_spec("apply_patch"),
                Arc::new(executor.clone()),
                ToolActionKind::WorkspaceWrite,
            )
            .with_action_proposal(),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .allow_low_risk_apply_patches()
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "Patch note.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(executor.proposed_calls().len(), 1);
    assert_eq!(executor.executed_calls().len(), 1);

    let resolved = result
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("patch tool call should resolve");
    assert_eq!(resolved.status(), ToolCallResultStatus::Succeeded);
    assert!(resolved.diagnostic().is_none());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_continuation_request_body(&requests[1], "Patch note.");
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation_result = requests[1].continuations()[0].result();
    assert_eq!(
        continuation_result.status(),
        ToolCallResultStatus::Succeeded
    );
    assert!(continuation_result.diagnostic().is_none());
    assert_eq!(
        continuation_result.content().as_text(),
        Some("patch applied\n")
    );
    assert!(
        !continuation_result
            .content()
            .as_str()
            .contains("action_policy_denied"),
        "successful opt-in patch continuation must not carry policy denial content"
    );
    for forbidden in [
        "proposal",
        "audit",
        "evidence",
        "fingerprint",
        "fnv1a64",
        "preimage_bytes",
        "replacement_bytes",
        "file_fingerprint_before",
        "file_fingerprint_after",
        "file_bytes_before",
        "file_bytes_after",
    ] {
        assert!(
            !continuation_result.content().as_str().contains(forbidden),
            "successful opt-in patch continuation leaked {forbidden}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_process_command_tool_executes_and_continues() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-rustc-version",
                "run_process",
                json!({ "command": "rustc --version", "cwd": null }),
            ),
        ))],
        vec![Ok(completed_text_event("final after process"))],
    ]);
    let runner = RecordingProcessRunner::succeeding("rustc 1.85.0\n");
    let runtime = Runtime::builder(session_id("agent-loop-process-command-tool"))
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a shell command through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "Check rustc version.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let observed_intents = runner.observed_intents();
    assert_eq!(observed_intents.len(), 1);
    let intent = &observed_intents[0];
    assert_eq!(intent.argv(), ["bash", "-lc", "rustc --version"]);
    assert_eq!(intent.cwd(), None);
    assert_eq!(intent.env_policy(), ProcessEnvPolicy::Empty);
    assert!(intent.stdin_text().is_none());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .tools()
            .iter()
            .any(|tool| tool.name().as_str() == "run_process"),
        "first model request should expose the registered process command tool"
    );
    assert!(requests[0].continuations().is_empty());
    assert_continuation_request_body(&requests[1], "Check rustc version.");
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-rustc-version");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert!(continuation.result().diagnostic().is_none());
    let content = continuation
        .result()
        .content()
        .as_json()
        .expect("process result should be JSON");
    let value: Value = serde_json::from_str(content).expect("process result JSON should parse");
    assert_eq!(value["ok"], true);
    assert_eq!(value["kind"], "process_action");
    assert_eq!(value["status"], json!({ "kind": "exited", "code": 0 }));
    assert_eq!(value["intent"]["command"], "rustc --version");
    assert_eq!(value["intent"]["cwd"], Value::Null);
    assert_eq!(value["stdout"]["text"], "rustc 1.85.0\n");
    assert_eq!(value["stderr"]["text"], "");
    for forbidden in ["proposal", "audit", "evidence"] {
        assert!(
            !content.contains(forbidden),
            "process continuation leaked internal {forbidden}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_process_command_tool_executes_rg_files_and_continues() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-rg-files",
                "run_process",
                json!({ "command": "rg --files", "cwd": null }),
            ),
        ))],
        vec![Ok(completed_text_event("final after rg files"))],
    ]);
    let runner =
        RecordingProcessRunner::succeeding("Cargo.toml\ncrates/merry-runtime/src/lib.rs\n");
    let runtime = Runtime::builder(session_id("agent-loop-process-command-tool-rg-files"))
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a shell command through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "List tracked source files.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let observed_intents = runner.observed_intents();
    assert_eq!(observed_intents.len(), 1);
    let intent = &observed_intents[0];
    assert_eq!(intent.argv(), ["bash", "-lc", "rg --files"]);
    assert_eq!(intent.cwd(), None);
    assert_eq!(intent.env_policy(), ProcessEnvPolicy::Empty);
    assert!(intent.stdin_text().is_none());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].continuations().is_empty());
    assert_continuation_request_body(&requests[1], "List tracked source files.");
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-rg-files");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert!(continuation.result().diagnostic().is_none());
    let content = continuation
        .result()
        .content()
        .as_json()
        .expect("process result should be JSON");
    let value: Value = serde_json::from_str(content).expect("process result JSON should parse");
    assert_eq!(value["ok"], true);
    assert_eq!(value["kind"], "process_action");
    assert_eq!(value["intent"]["command"], "rg --files");
    assert_eq!(value["intent"]["cwd"], Value::Null);
    assert_eq!(
        value["stdout"]["text"],
        "Cargo.toml\ncrates/merry-runtime/src/lib.rs\n"
    );
    assert_eq!(value["stderr"]["text"], "");
    for forbidden in ["proposal", "audit", "evidence"] {
        assert!(
            !content.contains(forbidden),
            "process continuation leaked internal {forbidden}"
        );
    }
}
