use crate::cli_error::{CliError, debug_openai_usage_error, stdout_error, unexpected};
use crate::coding::{
    CodingPermissionPolicy, HeadlessCodingRuntimeInput, ProcessExecutionMode,
    action_process_runner_for_mode, build_headless_coding_composition,
    build_headless_coding_with_policy_composition, coding_agent_process_admission,
    coding_agent_requires_sandbox_error,
};
use crate::config::MerryConfig;
use crate::mcp_tools::discover_configured_mcp_tools;
use crate::provider_config::{
    RuntimePrimaryProviderConfig, RuntimeProviderBundle, runtime_provider_bundle_from_config,
};
use crate::runtime_config::{
    action_process_backend_options, automatic_compaction_config, generation_config,
    subagents_config,
};
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use crate::tool_display::format_tool_call_progress;
use futures_util::StreamExt;
use merry_core::{ErrorInfo, RuntimeEvent, ToolCallResultStatus};
use merry_runtime::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopResult, AgentLoopStatus, Runtime,
    StepContext, StepInput,
};
use std::env;
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunExitStatus {
    Completed,
    Incomplete,
}

impl RunExitStatus {
    fn from_agent_loop_result(result: &AgentLoopResult) -> Self {
        match result.status() {
            AgentLoopStatus::Completed => Self::Completed,
            AgentLoopStatus::Failed { .. }
            | AgentLoopStatus::Cancelled { .. }
            | AgentLoopStatus::Blocked { .. }
            | _ => Self::Incomplete,
        }
    }
}

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    #[arg(long, help = "Print runtime events and final result as JSONL")]
    pub(crate) events_jsonl: bool,

    #[arg(required = true, allow_hyphen_values = true, value_name = "TASK")]
    pub(crate) task: String,
}

pub(crate) async fn run(
    args: &Args,
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    merry_config: Option<&MerryConfig>,
    process_execution_mode: ProcessExecutionMode,
    fully_trusted: bool,
) -> Result<RunExitStatus, CliError> {
    let Some(_admission) =
        coding_agent_process_admission(sandbox_child_handoff, process_execution_mode).await
    else {
        return Err(coding_agent_requires_sandbox_error("run"));
    };

    let RuntimeProviderBundle {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    } = runtime_provider_bundle_from_config(merry_config, debug_openai_usage_error)?;
    let RuntimePrimaryProviderConfig { provider, model } = primary;
    let root = env::current_dir().map_err(unexpected)?;
    let backend = action_process_runner_for_mode(
        &root,
        action_process_backend_options(merry_config).map_err(unexpected)?,
        process_execution_mode,
    )?;
    let extra_tools = discover_configured_mcp_tools(merry_config).await?;
    let session_id = default_run_session_id();
    let runtime_input = HeadlessCodingRuntimeInput {
        session_id: session_id.as_str(),
        root: &root,
        provider,
        model,
        process_backend: backend,
        extra_tools,
        allow_hidden_workspace_paths: false,
        automatic_compaction: automatic_compaction_config(merry_config).map_err(unexpected)?,
        retry_policy,
        context_compaction,
        approval_review,
        skill_roots: merry_config
            .map(MerryConfig::skill_roots)
            .transpose()
            .map_err(unexpected)?
            .unwrap_or_default(),
        subagents: subagents_config(merry_config).map_err(unexpected)?.into(),
        workspace_tool_limits: None,
    };
    let coding_runtime = if fully_trusted {
        build_headless_coding_with_policy_composition(
            runtime_input,
            CodingPermissionPolicy::fully_trusted(),
        )?
    } else {
        build_headless_coding_composition(runtime_input)?
    };
    let loop_config = coding_runtime.loop_config();
    let runtime = coding_runtime.into_runtime();
    let input = StepInput::user_text(&args.task).map_err(unexpected)?;
    let context = StepContext::default()
        .with_generation_config(generation_config(merry_config).map_err(unexpected)?);
    if args.events_jsonl {
        write_agent_loop_jsonl_output(&runtime, input, loop_config, context, tokio::io::stdout())
            .await
    } else {
        write_agent_loop_output(&runtime, input, loop_config, context, tokio::io::stdout()).await
    }
}

fn default_run_session_id() -> merry_core::SessionId {
    crate::session_id::new_ephemeral_session_id()
}

pub(crate) async fn write_agent_loop_output<W>(
    runtime: &Runtime,
    input: StepInput,
    config: AgentLoopConfig,
    context: StepContext,
    writer: W,
) -> Result<RunExitStatus, CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let mut stream = runtime
        .run_agent_loop_stream(input, context, config)
        .map_err(unexpected)?;
    let mut pending_commentary = None;
    while let Some(event) = stream.next().await {
        write_human_progress_event(&event, &mut pending_commentary, &mut writer).await?;
    }
    let result = stream.result().await.map_err(unexpected)?;
    write_agent_loop_summary_to(&result, &mut writer).await?;
    writer.flush().await.map_err(stdout_error)?;
    Ok(RunExitStatus::from_agent_loop_result(&result))
}

pub(crate) async fn write_agent_loop_jsonl_output<W>(
    runtime: &Runtime,
    input: StepInput,
    config: AgentLoopConfig,
    context: StepContext,
    writer: W,
) -> Result<RunExitStatus, CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let mut stream = runtime
        .run_agent_loop_stream(input, context, config)
        .map_err(unexpected)?;

    while let Some(event) = stream.next().await {
        write_public_runtime_event(&event, &mut writer).await?;
        writer.flush().await.map_err(stdout_error)?;
    }

    let result = stream.result().await.map_err(unexpected)?;
    let status = RunExitStatus::from_agent_loop_result(&result);
    write_agent_loop_result(&result, &mut writer).await?;
    writer.flush().await.map_err(stdout_error)?;
    Ok(status)
}

async fn write_agent_loop_summary_to<W>(
    result: &AgentLoopResult,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    if let Some(output) = result.final_output() {
        writer
            .write_all(output.as_bytes())
            .await
            .map_err(stdout_error)?;
        if !output.ends_with('\n') {
            writer.write_all(b"\n").await.map_err(stdout_error)?;
        }
    } else if let Some(output) = result.final_output_json() {
        writer
            .write_all(output.json().as_bytes())
            .await
            .map_err(stdout_error)?;
        writer.write_all(b"\n").await.map_err(stdout_error)?;
    } else {
        write_agent_loop_status_summary_to(result, writer).await?;
    }
    Ok(())
}

async fn write_agent_loop_status_summary_to<W>(
    result: &AgentLoopResult,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let summary = match result.status() {
        AgentLoopStatus::Completed => "status: completed\n".to_owned(),
        AgentLoopStatus::Failed { diagnostic } => format_diagnostic_status("failed", diagnostic),
        AgentLoopStatus::Cancelled { diagnostic } => {
            format_diagnostic_status("cancelled", diagnostic)
        }
        AgentLoopStatus::Blocked { reason } => {
            format!(
                "status: blocked\nreason: {}\n",
                format_blocked_reason(reason)
            )
        }
        _ => format!("status: {:?}\n", result.status()),
    };
    writer
        .write_all(summary.as_bytes())
        .await
        .map_err(stdout_error)
}

fn format_diagnostic_status(status: &str, diagnostic: &ErrorInfo) -> String {
    format!(
        "status: {status}\nerror: {}: {}\n",
        diagnostic.code(),
        diagnostic.message()
    )
}

fn format_blocked_reason(reason: &AgentLoopBlockedReason) -> String {
    match reason {
        AgentLoopBlockedReason::MaxModelTurnsReached { max_model_turns } => {
            format!("max model turns reached ({max_model_turns})")
        }
        AgentLoopBlockedReason::MultiplePendingToolCalls { pending_count } => {
            format!("multiple pending tool calls ({pending_count})")
        }
        AgentLoopBlockedReason::StepCompletedWithPendingToolCall { pending_count } => {
            format!("step completed with pending tool calls ({pending_count})")
        }
        AgentLoopBlockedReason::StepEndedWithoutTerminalEvent => {
            "step ended without a terminal event".to_owned()
        }
        AgentLoopBlockedReason::FinalOutputToolNotCalled => {
            "final output tool was not called".to_owned()
        }
        AgentLoopBlockedReason::BridgeToolCallRequested { call_id, tool_name } => {
            format!(
                "bridge tool call requested: {} ({})",
                tool_name.as_str(),
                call_id.as_str()
            )
        }
        _ => format!("{reason:?}"),
    }
}

async fn write_human_progress_event<W>(
    event: &RuntimeEvent,
    pending_commentary: &mut Option<String>,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    match event {
        RuntimeEvent::AssistantMessage { text, .. } => {
            *pending_commentary = Some(text.clone());
        }
        RuntimeEvent::ToolCallStarted { call, .. } => {
            if let Some(commentary) = pending_commentary.take() {
                write_progress_commentary(&commentary, writer).await?;
            }
            write_human_progress_line(writer, format_tool_call_progress("tool", call)).await?;
        }
        RuntimeEvent::ToolCallBatchStarted { batch, .. } => {
            if let Some(commentary) = pending_commentary.take() {
                write_progress_commentary(&commentary, writer).await?;
            }
            for call in batch.calls() {
                write_human_progress_line(writer, format_tool_call_progress("tool", call)).await?;
            }
        }
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.status() == ToolCallResultStatus::Failed =>
        {
            let line = result.diagnostic().map_or_else(
                || "tool failed".to_owned(),
                |diagnostic| {
                    format!(
                        "tool failed: {}: {}",
                        diagnostic.code(),
                        diagnostic.message()
                    )
                },
            );
            write_human_progress_line(writer, line).await?;
        }
        RuntimeEvent::ModelRetryScheduled {
            attempt,
            next_attempt,
            max_attempts,
            delay_ms,
            error_kind,
            ..
        } => {
            let line = format!(
                "model retry: attempt {attempt}/{max_attempts} failed with {error_kind}; retrying attempt {next_attempt}/{max_attempts} in {}",
                format_delay_ms(*delay_ms)
            );
            write_human_progress_line(writer, line).await?;
        }
        RuntimeEvent::ModelRetryExhausted {
            attempts_run,
            max_attempts,
            error_kind,
            ..
        } => {
            let line = format!(
                "model retry exhausted: {attempts_run}/{max_attempts} attempts failed with {error_kind}"
            );
            write_human_progress_line(writer, line).await?;
        }
        RuntimeEvent::StepCompleted { .. }
        | RuntimeEvent::RunFailed { .. }
        | RuntimeEvent::RunCancelled { .. }
        | RuntimeEvent::FinalOutputRecorded { .. } => {
            *pending_commentary = None;
        }
        _ => {}
    }

    Ok(())
}

async fn write_progress_commentary<W>(commentary: &str, writer: &mut W) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let commentary = commentary.trim();
    if commentary.is_empty() {
        return Ok(());
    }

    writer
        .write_all(commentary.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n\n").await.map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

async fn write_human_progress_line<W>(writer: &mut W, line: String) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }

    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n\n").await.map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

fn format_delay_ms(delay_ms: u64) -> String {
    if delay_ms >= 1000 && delay_ms.is_multiple_of(1000) {
        format!("{}s", delay_ms / 1000)
    } else {
        format!("{delay_ms}ms")
    }
}

async fn write_public_runtime_event<W>(event: &RuntimeEvent, writer: &mut W) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let line = serde_json::to_string(event).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)
}

async fn write_agent_loop_result<W>(
    result: &AgentLoopResult,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let line = match result.status() {
        AgentLoopStatus::Completed => serde_json::json!({
            "type": "agent_loop_result",
            "status": "completed",
            "model_turns_run": result.model_turns_run(),
            "final_output": result.final_output(),
            "final_output_json": result.final_output_json().map(merry_runtime::FinalOutput::json),
        }),
        AgentLoopStatus::Failed { diagnostic } => serde_json::json!({
            "type": "agent_loop_result",
            "status": "failed",
            "model_turns_run": result.model_turns_run(),
            "final_output": result.final_output(),
            "final_output_json": result.final_output_json().map(merry_runtime::FinalOutput::json),
            "diagnostic": {
                "code": diagnostic.code(),
                "message": diagnostic.message(),
            },
        }),
        AgentLoopStatus::Cancelled { diagnostic } => serde_json::json!({
            "type": "agent_loop_result",
            "status": "cancelled",
            "model_turns_run": result.model_turns_run(),
            "final_output": result.final_output(),
            "final_output_json": result.final_output_json().map(merry_runtime::FinalOutput::json),
            "diagnostic": {
                "code": diagnostic.code(),
                "message": diagnostic.message(),
            },
        }),
        AgentLoopStatus::Blocked { reason } => serde_json::json!({
            "type": "agent_loop_result",
            "status": "blocked",
            "model_turns_run": result.model_turns_run(),
            "final_output": result.final_output(),
            "final_output_json": result.final_output_json().map(merry_runtime::FinalOutput::json),
            "blocked_reason": format!("{reason:?}"),
        }),
        _ => serde_json::json!({
            "type": "agent_loop_result",
            "status": "unknown",
            "model_turns_run": result.model_turns_run(),
            "final_output": result.final_output(),
            "final_output_json": result.final_output_json().map(merry_runtime::FinalOutput::json),
        }),
    };
    let line = serde_json::to_string(&line).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)
}

#[cfg(test)]
mod tests {
    use super::{
        RunExitStatus, default_run_session_id, write_agent_loop_jsonl_output,
        write_agent_loop_output,
    };
    use crate::coding::{
        CodingSubagentsConfig, HeadlessCodingRuntimeInput, build_headless_coding,
        fixed_process_backend,
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

    #[test]
    fn default_run_session_id_is_generated() {
        let first = default_run_session_id();
        let second = default_run_session_id();

        assert_ne!(first, second);
        assert_ne!(first.as_str(), "run");
    }

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
}
