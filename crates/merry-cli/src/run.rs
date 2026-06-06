use crate::coding_runtime::{
    HeadlessCodingRuntimeInput, action_process_runner, build_headless_coding_runtime,
    coding_agent_loop_config, coding_agent_requires_sandbox_error,
    coding_loop_smoke_admission_from_current_process,
};
use crate::config::MerryConfig;
use crate::provider_config::{
    OpenAiRuntimeConfig, openai_role_provider_config, openai_runtime_config,
};
use crate::runtime_events::write_runtime_event;
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use crate::{
    CliError, automatic_compaction_config, debug_openai_usage_error, stdout_error,
    subagents_config, unexpected,
};
use futures_util::StreamExt;
use merry_core::{ArtifactId, RuntimeEvent, RuntimeEventKind};
use merry_llm::ModelName;
use merry_provider_openai::OpenAiProvider;
use merry_runtime::{
    AgentLoopConfig, AgentLoopResult, AgentLoopStatus, Runtime, RuntimeModelRole, StepContext,
    StepInput,
};
use std::{env, sync::Arc};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

const ASSISTANT_OUTPUT_ARTIFACT_PREFIX: &str = "assistant-output-";

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
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_agent_requires_sandbox_error("run"));
    };

    let config = openai_runtime_config(None, merry_config, debug_openai_usage_error)?;
    let OpenAiRuntimeConfig {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    } = config;
    let root = env::current_dir().map_err(unexpected)?;
    let backend = action_process_runner(&root, merry_config)?;
    let runtime = build_headless_coding_runtime(HeadlessCodingRuntimeInput {
        session_id: "run",
        root: &root,
        admission,
        provider: Arc::new(OpenAiProvider::new(primary.provider)),
        model: ModelName::new(&primary.model).map_err(unexpected)?,
        runner: backend.runner(),
        permissioned_process_runner_factory: backend.permissioned_factory(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: automatic_compaction_config(merry_config).map_err(unexpected)?,
        retry_policy,
        context_compaction: context_compaction
            .map(|config| {
                openai_role_provider_config(RuntimeModelRole::ContextCompaction, config, unexpected)
            })
            .transpose()?,
        approval_review: approval_review
            .map(|config| {
                openai_role_provider_config(RuntimeModelRole::ApprovalReview, config, unexpected)
            })
            .transpose()?,
        skill_roots: merry_config
            .map(MerryConfig::skill_roots)
            .transpose()
            .map_err(unexpected)?
            .unwrap_or_default(),
        subagents: subagents_config(merry_config).map_err(unexpected)?,
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
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let mut stream = runtime
        .run_agent_loop_stream(input, StepContext::default(), config)
        .map_err(unexpected)?;
    let mut pending_commentary = None;
    while let Some(event) = stream.next().await {
        write_progress_commentary_event(runtime, &event, &mut pending_commentary, &mut writer)
            .await?;
    }
    let result = stream.result().await.ok_or_else(|| {
        CliError::Unexpected("agent loop stream closed before producing a result".to_owned())
    })?;
    write_agent_loop_summary_to(&result, &mut writer).await?;
    writer.flush().await.map_err(stdout_error)
}

pub(crate) async fn write_agent_loop_jsonl_output<W>(
    runtime: &Runtime,
    input: StepInput,
    config: AgentLoopConfig,
    writer: W,
) -> Result<(), CliError>
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
    write_agent_loop_result(&result, &mut writer).await?;
    writer.flush().await.map_err(stdout_error)
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
        writer
            .write_all(format!("status: {:?}\n", result.status()).as_bytes())
            .await
            .map_err(stdout_error)?;
    }
    Ok(())
}

async fn write_progress_commentary_event<W>(
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
        RuntimeEventKind::ToolCallPending { .. }
        | RuntimeEventKind::BridgeToolCallRequested { .. } => {
            if let Some(artifact_id) = pending_commentary.take() {
                write_progress_commentary_artifact(runtime, &artifact_id, writer).await?;
            }
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
    use super::{write_agent_loop_jsonl_output, write_agent_loop_output};
    use crate::coding_runtime::{HeadlessCodingRuntimeInput, build_headless_coding_runtime};
    use crate::debug::coding_loop::coding_loop_process_call;
    use crate::test_support::{FakeProcessRunner, ScriptedProvider, model_name};
    use merry_llm::{FinishReason, ModelEvent, ModelOutput, ModelResponse};
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
            subagents: crate::config::SubagentsConfig::default(),
        })
        .expect("runtime should build");

        let mut output = Vec::new();
        write_agent_loop_output(
            &runtime,
            StepInput::user_text("finish").expect("valid input"),
            AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).expect("valid config"),
            &mut output,
        )
        .await
        .expect("run output should write");

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
            subagents: crate::config::SubagentsConfig::default(),
        })
        .expect("runtime should build");

        let mut output = Vec::new();
        write_agent_loop_output(
            &runtime,
            StepInput::user_text("ping baidu.com").expect("valid input"),
            AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).expect("valid config"),
            &mut output,
        )
        .await
        .expect("run output should write");

        let text = String::from_utf8(output).expect("output should be utf-8");
        assert_eq!(
            text,
            "我先解析 baidu.com 的 DNS。\n\nbaidu.com resolves to 110.242.74.102\n"
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
            subagents: crate::config::SubagentsConfig::default(),
        })
        .expect("runtime should build");

        let mut output = FlushCountingWriter::default();
        write_agent_loop_jsonl_output(
            &runtime,
            StepInput::user_text("finish").expect("valid input"),
            AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).expect("valid config"),
            &mut output,
        )
        .await
        .expect("run output should write");

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
}
