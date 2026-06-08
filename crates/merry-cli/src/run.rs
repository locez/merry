use crate::cli_error::{CliError, debug_openai_usage_error, stdout_error, unexpected};
use crate::coding_runtime::{
    HeadlessCodingRuntimeInput, action_process_runner, build_headless_coding_runtime,
    coding_agent_loop_config, coding_agent_requires_sandbox_error,
    coding_loop_smoke_admission_from_current_process,
};
use crate::config::MerryConfig;
use crate::provider_config::{
    RuntimePrimaryProviderConfig, RuntimeProviderBundle, openai_provider_bundle,
    openai_provider_config_bundle,
};
use crate::runtime_config::{
    action_process_backend_options, automatic_compaction_config, subagents_config,
};
use crate::runtime_events::write_runtime_event;
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ErrorInfo, PendingToolCall, RuntimeEvent, RuntimeEventKind, ToolCallResultStatus,
};
use merry_runtime::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopResult, AgentLoopStatus, Runtime,
    StepContext, StepInput,
};
use serde_json::{Map, Value};
use std::env;
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

const ASSISTANT_OUTPUT_ARTIFACT_PREFIX: &str = "assistant-output-";

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
) -> Result<RunExitStatus, CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_agent_requires_sandbox_error("run"));
    };

    let config = openai_provider_config_bundle(None, merry_config, debug_openai_usage_error)?;
    let RuntimeProviderBundle {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    } = openai_provider_bundle(config, unexpected)?;
    let RuntimePrimaryProviderConfig { provider, model } = primary;
    let root = env::current_dir().map_err(unexpected)?;
    let backend = action_process_runner(
        &root,
        action_process_backend_options(merry_config).map_err(unexpected)?,
    )?;
    let runtime = build_headless_coding_runtime(HeadlessCodingRuntimeInput {
        session_id: "run",
        root: &root,
        admission,
        provider,
        model,
        runner: backend.runner(),
        permissioned_process_runner_factory: backend.permissioned_factory(),
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
    })?;
    let input = StepInput::user_text(&args.task).map_err(unexpected)?;
    if args.events_jsonl {
        write_agent_loop_jsonl_output(
            &runtime,
            input,
            coding_agent_loop_config()?,
            tokio::io::stdout(),
        )
        .await
    } else {
        write_agent_loop_output(
            &runtime,
            input,
            coding_agent_loop_config()?,
            tokio::io::stdout(),
        )
        .await
    }
}

pub(crate) async fn write_agent_loop_output<W>(
    runtime: &Runtime,
    input: StepInput,
    config: AgentLoopConfig,
    writer: W,
) -> Result<RunExitStatus, CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let mut stream = runtime
        .run_agent_loop_stream(input, StepContext::default(), config)
        .map_err(unexpected)?;
    let mut pending_commentary = None;
    while let Some(event) = stream.next().await {
        write_human_progress_event(runtime, &event, &mut pending_commentary, &mut writer).await?;
    }
    let result = stream.result().await.ok_or_else(|| {
        CliError::Unexpected("agent loop stream closed before producing a result".to_owned())
    })?;
    write_agent_loop_summary_to(&result, &mut writer).await?;
    writer.flush().await.map_err(stdout_error)?;
    Ok(RunExitStatus::from_agent_loop_result(&result))
}

pub(crate) async fn write_agent_loop_jsonl_output<W>(
    runtime: &Runtime,
    input: StepInput,
    config: AgentLoopConfig,
    writer: W,
) -> Result<RunExitStatus, CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let mut stream = runtime
        .run_agent_loop_stream(input, StepContext::default(), config)
        .map_err(unexpected)?;

    while let Some(event) = stream.next().await {
        write_runtime_event(&event, &mut writer).await?;
        writer.flush().await.map_err(stdout_error)?;
    }

    let result = stream.result().await.ok_or_else(|| {
        CliError::Unexpected("agent loop stream closed before producing a result".to_owned())
    })?;
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
    runtime: &Runtime,
    event: &RuntimeEvent,
    pending_commentary: &mut Option<ArtifactId>,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    match &event.kind {
        RuntimeEventKind::ArtifactRecorded { artifact }
            if artifact
                .id()
                .as_str()
                .starts_with(ASSISTANT_OUTPUT_ARTIFACT_PREFIX) =>
        {
            *pending_commentary = Some(artifact.id().clone());
        }
        RuntimeEventKind::ToolCallPending { call } => {
            if let Some(artifact_id) = pending_commentary.take() {
                write_progress_commentary_artifact(runtime, &artifact_id, writer).await?;
            }
            write_human_progress_line(writer, format_tool_call_progress("tool", call)).await?;
        }
        RuntimeEventKind::BridgeToolCallRequested { call } => {
            if let Some(artifact_id) = pending_commentary.take() {
                write_progress_commentary_artifact(runtime, &artifact_id, writer).await?;
            }
            write_human_progress_line(writer, format_tool_call_progress("bridge tool", call))
                .await?;
        }
        RuntimeEventKind::ToolCallResolved { result }
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
        RuntimeEventKind::ModelRetryScheduled {
            attempt,
            next_attempt,
            max_attempts,
            delay_ms,
            error_kind,
        } => {
            let line = format!(
                "model retry: attempt {attempt}/{max_attempts} failed with {error_kind}; retrying attempt {next_attempt}/{max_attempts} in {}",
                format_delay_ms(*delay_ms)
            );
            write_human_progress_line(writer, line).await?;
        }
        RuntimeEventKind::ModelRetryExhausted {
            attempts_run,
            max_attempts,
            error_kind,
        } => {
            let line = format!(
                "model retry exhausted: {attempts_run}/{max_attempts} attempts failed with {error_kind}"
            );
            write_human_progress_line(writer, line).await?;
        }
        RuntimeEventKind::ArtifactRecorded { .. }
        | RuntimeEventKind::StepCompleted
        | RuntimeEventKind::Failed { .. }
        | RuntimeEventKind::Cancelled { .. } => {
            *pending_commentary = None;
        }
        _ => {}
    }

    Ok(())
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

fn format_tool_call_progress(prefix: &str, call: &PendingToolCall) -> String {
    let name = call.name().as_str();
    let detail = format_tool_call_detail(name, call.arguments().as_object());
    if name == "request_permissions" {
        return join_progress_parts("permission: request", detail.as_deref());
    }
    join_progress_parts(&format!("{prefix}: {name}"), detail.as_deref())
}

fn join_progress_parts(prefix: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) if !detail.is_empty() => format!("{prefix} {detail}"),
        _ => prefix.to_owned(),
    }
}

fn format_tool_call_detail(name: &str, arguments: &Map<String, Value>) -> Option<String> {
    match name {
        "run_process" => format_process_call_detail(arguments),
        "request_permissions" => format_permission_call_detail(arguments),
        _ => format_generic_tool_call_detail(arguments),
    }
}

fn format_process_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let argv = arguments.get("argv")?.as_array()?;
    let mut detail = format_argv(argv)?;
    if let Some(cwd) = arguments
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
    {
        detail.push_str(" (cwd: ");
        detail.push_str(&compact_inline(cwd, 80));
        detail.push(')');
    }
    Some(detail)
}

fn format_permission_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(requested) = arguments.get("requested").and_then(Value::as_object) {
        if let Some(network) = requested.get("network").and_then(Value::as_bool) {
            parts.push(format!("network={network}"));
        }
        if let Some(paths) = requested.get("paths").and_then(Value::as_array) {
            let paths = format_permission_paths(paths);
            if !paths.is_empty() {
                parts.push(format!("paths={paths}"));
            }
        }
    }
    if let Some(argv) = arguments
        .get("for_action")
        .and_then(Value::as_object)
        .and_then(|action| action.get("argv"))
        .and_then(Value::as_array)
        .and_then(|argv| format_argv(argv))
    {
        parts.push(format!("for: {argv}"));
    }
    non_empty_parts(parts)
}

fn format_permission_paths(paths: &[Value]) -> String {
    let mut formatted = paths
        .iter()
        .take(3)
        .filter_map(|path| {
            let object = path.as_object()?;
            let path = object.get("path")?.as_str()?;
            let access = object.get("access").and_then(Value::as_str);
            Some(match access {
                Some(access) => {
                    format!("{}:{access}", compact_inline(path, 80))
                }
                None => compact_inline(path, 80),
            })
        })
        .collect::<Vec<_>>();
    if paths.len() > formatted.len() {
        formatted.push(format!("+{}", paths.len() - formatted.len()));
    }
    formatted.join(",")
}

fn format_generic_tool_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let mut parts = Vec::new();
    for key in ["path", "cwd", "query", "pattern", "target", "command"] {
        if let Some(value) = arguments.get(key).and_then(format_inline_value) {
            parts.push(format!("{key}={value}"));
        }
    }
    if parts.is_empty() && !arguments.is_empty() {
        parts.push(format!("args={}", arguments.len()));
    }
    non_empty_parts(parts)
}

fn format_argv(argv: &[Value]) -> Option<String> {
    let words = argv
        .iter()
        .filter_map(Value::as_str)
        .map(compact_shell_word)
        .collect::<Vec<_>>();
    non_empty_parts(words)
}

fn format_inline_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(compact_shell_word(value)),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Array(values) => Some(format!("[{} items]", values.len())),
        Value::Object(values) => Some(format!("{{{} fields}}", values.len())),
        Value::Null => None,
    }
}

fn non_empty_parts(parts: Vec<String>) -> Option<String> {
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn compact_shell_word(value: &str) -> String {
    let value = compact_inline(value, 120);
    if value.is_empty() {
        return "\"\"".to_owned();
    }
    if value.bytes().all(is_safe_shell_word_byte) {
        value
    } else {
        format!("{value:?}")
    }
}

fn compact_inline(value: &str, max_chars: usize) -> String {
    let mut output = value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect::<String>();
    if value.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn is_safe_shell_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b'@' | b'%' | b'+'
        )
}

fn format_delay_ms(delay_ms: u64) -> String {
    if delay_ms >= 1000 && delay_ms.is_multiple_of(1000) {
        format!("{}s", delay_ms / 1000)
    } else {
        format!("{delay_ms}ms")
    }
}

async fn write_progress_commentary_artifact<W>(
    runtime: &Runtime,
    artifact_id: &ArtifactId,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let content = runtime
        .read_artifact_content(artifact_id)
        .await
        .map_err(unexpected)?;
    let Some(text) = content.as_text() else {
        return Ok(());
    };
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }

    writer
        .write_all(text.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n\n").await.map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
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
    use super::{RunExitStatus, write_agent_loop_jsonl_output, write_agent_loop_output};
    use crate::coding_runtime::{
        CodingSubagentsConfig, HeadlessCodingRuntimeInput, build_headless_coding_runtime,
    };
    use crate::debug::coding_loop::coding_loop_process_call;
    use crate::testing::{FakeProcessRunner, ScriptedProvider, model_name};
    use merry_core::ToolName;
    use merry_llm::{
        FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
        ToolArguments,
    };
    use merry_runtime::DEFAULT_CODING_AGENT_MAX_MODEL_TURNS;
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, ProcessRunner, StepInput,
    };
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
        let runtime = build_headless_coding_runtime(HeadlessCodingRuntimeInput {
            session_id: "run-writer-test",
            root: &workspace,
            admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            provider: Arc::new(provider),
            model: model_name(),
            runner,
            permissioned_process_runner_factory: permissioned_factory,
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
        })
        .expect("runtime should build");

        let mut output = Vec::new();
        let status = write_agent_loop_output(
            &runtime,
            StepInput::user_text("finish").expect("valid input"),
            AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).expect("valid config"),
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
                Ok(coding_loop_process_call(
                    "run-progress-dns",
                    &["getent", "hosts", "baidu.com"],
                    None,
                )
                .expect("valid process call")),
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
        let runtime = build_headless_coding_runtime(HeadlessCodingRuntimeInput {
            session_id: "run-progress-writer-test",
            root: &workspace,
            admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            provider: Arc::new(provider),
            model: model_name(),
            runner,
            permissioned_process_runner_factory: permissioned_factory,
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
        })
        .expect("runtime should build");

        let mut output = Vec::new();
        let status = write_agent_loop_output(
            &runtime,
            StepInput::user_text("ping baidu.com").expect("valid input"),
            AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).expect("valid config"),
            &mut output,
        )
        .await
        .expect("run output should write");

        assert_eq!(status, RunExitStatus::Completed);
        let text = String::from_utf8(output).expect("output should be utf-8");
        assert_eq!(
            text,
            "我先解析 baidu.com 的 DNS。\n\ntool: run_process getent hosts baidu.com\n\nbaidu.com resolves to 110.242.74.102\n"
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
        let runtime = build_headless_coding_runtime(HeadlessCodingRuntimeInput {
            session_id: "run-jsonl-writer-test",
            root: &workspace,
            admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            provider: Arc::new(provider),
            model: model_name(),
            runner,
            permissioned_process_runner_factory: permissioned_factory,
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
        })
        .expect("runtime should build");

        let mut output = FlushCountingWriter::default();
        let status = write_agent_loop_jsonl_output(
            &runtime,
            StepInput::user_text("finish").expect("valid input"),
            AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).expect("valid config"),
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
        let runtime = build_headless_coding_runtime(HeadlessCodingRuntimeInput {
            session_id: "run-blocked-writer-test",
            root: &workspace,
            admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            provider: Arc::new(provider),
            model: model_name(),
            runner,
            permissioned_process_runner_factory: permissioned_factory,
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
        })
        .expect("runtime should build");

        let mut output = Vec::new();
        let status = write_agent_loop_output(
            &runtime,
            StepInput::user_text("read README").expect("valid input"),
            AgentLoopConfig::new(1).expect("valid config"),
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
