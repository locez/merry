use crate::support::{
    models::{
        ScriptedModelProvider, completed_text_event, completed_tool_call_event, model_name,
        model_tool_call_with_arguments,
    },
    process::RecordingProcessRunner,
    runtime::session_id,
    tracing::capture_traces_for,
};
use merry_core::ToolName;
use merry_runtime::{
    AgentLoopConfig, AgentLoopStatus, Runtime, StepContext, StepInput, process_command_tool,
};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_traces_loop_steps_tool_process_and_terminal_status() {
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
    let runtime = Runtime::builder(session_id("agent-loop-tracing"))
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a shell command through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .model_provider(Arc::new(provider), model_name())
        .allow_read_only_shell_process_actions(Arc::new(runner))
        .build()
        .expect("runtime should build");

    let (result, logs) = capture_traces_for(
        "agent-loop-tracing",
        runtime.run_agent_loop(
            StepInput::user_text("Check rustc version.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(8).expect("valid config"),
        ),
    )
    .await;

    let result = result.expect("agent loop should run");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(logs.contains("\"event\":\"runtime.loop.start\""));
    assert!(logs.contains("\"event\":\"runtime.step.start\""));
    assert!(logs.contains("\"event\":\"runtime.provider.request\""));
    assert!(logs.contains("\"event\":\"runtime.tool.pending\""));
    assert!(logs.contains("\"event\":\"runtime.tool.execute.start\""));
    assert!(logs.contains("\"event\":\"runtime.process.execute.start\""));
    assert!(logs.contains("\"event\":\"runtime.process.execute.finish\""));
    assert!(logs.contains("\"event\":\"runtime.artifact.record\""));
    assert!(logs.contains("\"event\":\"runtime.tool.execute.finish\""));
    assert!(logs.contains("\"event\":\"runtime.loop.finish\""));
    assert!(logs.contains("\"status\":\"completed\""));
    assert!(logs.contains("\"tool_name\":\"run_process\""));
    assert!(logs.contains("\"tool_call_id\":\"call-rustc-version\""));
    assert!(logs.contains("\"permission_profile_id\":\"process.shell.read_only\""));
    assert!(logs.contains("\"shell\":\"bash\""));
    assert!(logs.contains("\"shell_flag\":\"-lc\""));
    assert!(logs.contains("\"shell_script_bytes\":15"));
    assert!(!logs.contains("\"argv\""));
    assert!(logs.contains("\"stdout_bytes\":13"));
    assert!(logs.contains("\"stderr_bytes\":0"));
    assert!(!logs.contains("rustc 1.85.0"));
}
