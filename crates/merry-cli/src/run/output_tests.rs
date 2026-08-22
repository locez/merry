use super::{RunExitStatus, write_agent_loop_jsonl_output, write_agent_loop_output};
use crate::coding::{
    CodingSubagentsConfig, HeadlessCodingRuntimeInput, build_headless_coding, fixed_process_backend,
};
use crate::testing::{FakeProcessRunner, ScriptedProvider, model_name, process_tool_call};
use merry::profiles::DEFAULT_CODING_AGENT_MAX_MODEL_TURNS;
use merry_core::ToolName;
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments,
};
use merry_process::ProcessSession;
use merry_runtime::{AgentLoopConfig, ProcessRunner, StepContext, StepInput};
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::io::AsyncWrite;

#[derive(Default)]
struct FlushCountingWriter {
    bytes: Vec<u8>,
    flushes: usize,
}

impl AsyncWrite for FlushCountingWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.bytes.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.flushes += 1;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn writer_prints_final_output_without_event_jsonl() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("done from run")],
            FinishReason::Stop,
            None,
        ),
    })]]);
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
    let permissioned_factory = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );
    let runtime = build_headless_coding(HeadlessCodingRuntimeInput {
        session_id: "run-writer-test",
        root: &workspace,
        process_backend: fixed_process_backend(ProcessSession::from_parts(
            merry_runtime::AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
            permissioned_factory,
        )),
        provider: Arc::new(provider),
        model: model_name(),
        extra_tools: Vec::new(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
        workspace_tool_limits: None,
    })
    .expect("runtime should build");

    let mut output = Vec::new();
    let status = write_agent_loop_output(
        &runtime,
        StepInput::user_text("finish").expect("valid input"),
        AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).expect("valid config"),
        StepContext::default(),
        &mut output,
    )
    .await
    .expect("run output should write");

    assert_eq!(status, RunExitStatus::Completed);
    let text = String::from_utf8(output).expect("output should be utf-8");
    assert_eq!(text, "done from run\n");
    assert!(!text.contains("\"type\":\"session_started\""));
    assert!(!text.contains("\"type\":\"agent_loop_result\""));
}

#[tokio::test(flavor = "current_thread")]
async fn writer_streams_progress_commentary_before_final_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let provider = ScriptedProvider::new(vec![
        vec![
            Ok(ModelEvent::OutputTextDelta {
                delta: "我先解析 baidu.com 的 DNS。".to_owned(),
            }),
            Ok(
                process_tool_call("run-progress-dns", &["getent", "hosts", "baidu.com"], None)
                    .expect("valid process call"),
            ),
        ],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("baidu.com resolves to 110.242.74.102")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let runner: Arc<dyn ProcessRunner> =
        Arc::new(FakeProcessRunner::succeeding("110.242.74.102 baidu.com\n"));
    let permissioned_factory = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );
    let runtime = build_headless_coding(HeadlessCodingRuntimeInput {
        session_id: "run-progress-writer-test",
        root: &workspace,
        process_backend: fixed_process_backend(ProcessSession::from_parts(
            merry_runtime::AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
            permissioned_factory,
        )),
        provider: Arc::new(provider),
        model: model_name(),
        extra_tools: Vec::new(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
        workspace_tool_limits: None,
    })
    .expect("runtime should build");

    let mut output = Vec::new();
    let status = write_agent_loop_output(
        &runtime,
        StepInput::user_text("ping baidu.com").expect("valid input"),
        AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).expect("valid config"),
        StepContext::default(),
        &mut output,
    )
    .await
    .expect("run output should write");

    assert_eq!(status, RunExitStatus::Completed);
    let text = String::from_utf8(output).expect("output should be utf-8");
    assert_eq!(
        text,
        "我先解析 baidu.com 的 DNS。\n\ntool: run_process getent hosts baidu.com (.)\n\nbaidu.com resolves to 110.242.74.102\n"
    );
    assert!(!text.contains("\"type\":\"tool_call_pending\""));
    assert!(!text.contains("\"type\":\"agent_loop_result\""));
}

#[tokio::test(flavor = "current_thread")]
async fn jsonl_writer_streams_agent_loop_result() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("done from run")],
            FinishReason::Stop,
            None,
        ),
    })]]);
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
    let permissioned_factory = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );
    let runtime = build_headless_coding(HeadlessCodingRuntimeInput {
        session_id: "run-jsonl-writer-test",
        root: &workspace,
        process_backend: fixed_process_backend(ProcessSession::from_parts(
            merry_runtime::AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
            permissioned_factory,
        )),
        provider: Arc::new(provider),
        model: model_name(),
        extra_tools: Vec::new(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
        workspace_tool_limits: None,
    })
    .expect("runtime should build");

    let mut output = FlushCountingWriter::default();
    let status = write_agent_loop_jsonl_output(
        &runtime,
        StepInput::user_text("finish").expect("valid input"),
        AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).expect("valid config"),
        StepContext::default(),
        &mut output,
    )
    .await
    .expect("run output should write");

    assert_eq!(status, RunExitStatus::Completed);
    assert!(
        output.flushes > 1,
        "JSONL mode should flush as events arrive"
    );
    let text = String::from_utf8(output.bytes).expect("output should be utf-8");
    assert!(text.contains("\"type\":\"session_started\""));
    assert!(text.contains("\"type\":\"agent_loop_result\""));
    assert!(text.contains("\"status\":\"completed\""));
    assert!(text.contains("done from run"));
}

#[tokio::test(flavor = "current_thread")]
async fn writer_returns_incomplete_when_agent_loop_blocks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(ModelToolCall::new(
                ModelToolCallId::new("call-read").expect("valid call id"),
                ToolName::new("workspace_read_file").expect("valid tool name"),
                ToolArguments::try_from(serde_json::json!({"path": "README.md"}))
                    .expect("valid args"),
            ))],
            FinishReason::ToolCalls,
            None,
        ),
    })]]);
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
    let permissioned_factory = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );
    let runtime = build_headless_coding(HeadlessCodingRuntimeInput {
        session_id: "run-blocked-writer-test",
        root: &workspace,
        process_backend: fixed_process_backend(ProcessSession::from_parts(
            merry_runtime::AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
            permissioned_factory,
        )),
        provider: Arc::new(provider),
        model: model_name(),
        extra_tools: Vec::new(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
        workspace_tool_limits: None,
    })
    .expect("runtime should build");

    let mut output = Vec::new();
    let status = write_agent_loop_output(
        &runtime,
        StepInput::user_text("read README").expect("valid input"),
        AgentLoopConfig::new(1).expect("valid config"),
        StepContext::default(),
        &mut output,
    )
    .await
    .expect("run output should write");

    assert_eq!(status, RunExitStatus::Incomplete);
    let text = String::from_utf8(output).expect("output should be utf-8");
    assert!(text.contains("tool: workspace_read_file path=README.md"));
    assert!(text.contains("status: blocked"));
    assert!(text.contains("reason: max model turns reached (1)"));
}
