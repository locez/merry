use crate::cli_error::{CliError, debug_openai_usage_error, stdout_error, unexpected, usage_error};
use crate::coding::{
    CodingPermissionPolicy, CodingTrustMode, HeadlessCodingRuntimeInput, ProcessExecutionMode,
    action_process_runner_for_mode, build_headless_coding_with_policy_composition,
    coding_agent_process_admission, coding_agent_requires_sandbox_error,
    resume_headless_coding_composition_with_loaded_session,
};
use crate::config::MerryConfig;
use crate::headless_review::{HeadlessPermissionReviewer, ReviewInputChannel};
use crate::mcp_tools::{McpSession, discover_configured_mcp_tools, write_startup_warnings};
use crate::provider_config::{
    RuntimePrimaryProviderConfig, RuntimeProviderBundle, runtime_provider_bundle_from_config,
};
use crate::runtime_config::{
    action_process_backend_options, automatic_compaction_config, generation_config,
    subagents_config,
};
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use crate::tool_display::format_tool_call_progress;
use crate::tui::session_list::{TuiSessionMetadata, TuiSessionStore, now_unix_ms};
use merry_core::{ErrorInfo, RuntimeEvent, SessionId, ToolCallResultStatus};
use merry_runtime::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopResult, AgentLoopStatus, FileSessionStore,
    LoadedSession, Runtime, SessionReservation, StepContext, StepInput,
};
use std::{env, future::Future};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufWriter};

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

/// Runtime settlement plus the first failure encountered while presenting it.
///
/// Presentation is deliberately kept separate from settlement so stdout
/// failure cannot cancel a run that can still reach a resume-safe boundary.
#[derive(Debug)]
struct SettledRun {
    runtime_result: Result<RunExitStatus, CliError>,
    presentation_result: Result<(), CliError>,
}

#[cfg(test)]
impl SettledRun {
    fn into_output_result(self) -> Result<RunExitStatus, CliError> {
        let status = self.runtime_result?;
        self.presentation_result?;
        Ok(status)
    }
}

/// `TASK` value that reads the task text from stdin instead of argv.
const STDIN_TASK: &str = "-";

/// Recorded against every tool call a settled run never produced a result for.
const ABANDONED_TOOL_CALL_REASON: &str =
    "the headless run settled before this tool call produced a result";

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    #[arg(long, help = "Print runtime events and final result as JSONL")]
    pub(crate) events_jsonl: bool,

    #[arg(
        long,
        value_name = "SESSION_ID",
        conflicts_with = "resume",
        help = "Save the run under this session id instead of a generated one"
    )]
    pub(crate) session_id: Option<String>,

    #[arg(
        long,
        value_name = "SESSION_ID",
        help = "Continue the saved session with this id instead of starting a new one"
    )]
    pub(crate) resume: Option<String>,

    #[arg(
        required = true,
        allow_hyphen_values = true,
        value_name = "TASK",
        help = "Task text, or `-` to read the task from stdin"
    )]
    pub(crate) task: String,
}

/// Session a run writes to, and whether it continues state saved by an
/// earlier run.
#[derive(Debug, PartialEq, Eq)]
enum RunSession {
    New(SessionId),
    Resumed(SessionId),
}

impl RunSession {
    fn from_args(args: &Args) -> Result<Self, CliError> {
        match (args.session_id.as_deref(), args.resume.as_deref()) {
            (Some(_), Some(_)) => Err(usage_error(
                "--session-id and --resume cannot be used together",
            )),
            (None, Some(resumed)) => Ok(Self::Resumed(parse_session_id("--resume", resumed)?)),
            (Some(requested), None) => Ok(Self::New(parse_session_id("--session-id", requested)?)),
            (None, None) => Ok(Self::New(default_run_session_id())),
        }
    }

    const fn id(&self) -> &SessionId {
        match self {
            Self::New(id) | Self::Resumed(id) => id,
        }
    }
}

fn parse_session_id(flag: &str, value: &str) -> Result<SessionId, CliError> {
    SessionId::new(value).map_err(|error| usage_error(format!("{flag}: {error}")))
}

/// Reserves a run's session id and refuses a new run that already has state.
///
/// Saving a session is an atomic replace of its `state.json`, so starting a new
/// run under an existing id destroys that session's transcript, ledger,
/// artifacts, and checkpoints with nothing left to resume. A typo or a reused
/// id has to fail here, before the run consumes its task or starts a runtime.
async fn reserve_run_session(
    session: &RunSession,
    store: &FileSessionStore,
) -> Result<SessionReservation, CliError> {
    let reservation = store
        .reserve_session(session.id())
        .await
        .map_err(unexpected)?;
    if matches!(session, RunSession::New(_))
        && store
            .contains_session(session.id())
            .await
            .map_err(unexpected)?
    {
        return Err(usage_error(format!(
            "session {} already has saved state; pass --resume {} to continue it, \
             or choose a different --session-id",
            session.id(),
            session.id()
        )));
    }
    Ok(reservation)
}

/// Reads the task from argv, or from `reader` when `TASK` is `-`.
///
/// Reading the task from stdin keeps a large task off argv, where it would be
/// visible to every process listing on the host and bounded by the kernel's
/// per-argument limit.
async fn resolve_task<R>(task: &str, mut reader: R) -> Result<String, CliError>
where
    R: AsyncRead + Unpin,
{
    if task != STDIN_TASK {
        return Ok(task.to_owned());
    }

    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .await
        .map_err(|error| unexpected(format!("could not read the task from stdin: {error}")))?;
    if text.trim().is_empty() {
        return Err(usage_error("the task read from stdin is empty"));
    }
    Ok(text)
}

/// Chooses the channel that answers permission review for a run.
///
/// An argv task leaves stdin untouched, so review keeps reading it. A `-` task
/// consumes stdin to end-of-file before the runtime starts, so review must ask
/// somewhere else or report that it cannot ask at all.
fn review_input_channel_for_task(task: &str) -> ReviewInputChannel {
    if task == STDIN_TASK {
        ReviewInputChannel::ControllingTerminal
    } else {
        ReviewInputChannel::Stdin
    }
}

pub(crate) async fn run(
    args: &Args,
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    merry_config: Option<&MerryConfig>,
    process_execution_mode: ProcessExecutionMode,
    fully_trusted: bool,
) -> Result<RunExitStatus, CliError> {
    let session = RunSession::from_args(args)?;
    let session_store = FileSessionStore::default_store().map_err(unexpected)?;
    let _session_reservation = reserve_run_session(&session, &session_store).await?;
    let task = resolve_task(&args.task, tokio::io::stdin()).await?;
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
    let headless_metadata = headless_session_metadata(&session_store, &session, &root).await?;
    let loaded_session = match &session {
        RunSession::New(_) => None,
        RunSession::Resumed(id) => Some(
            LoadedSession::load(&session_store, id)
                .await
                .map_err(|error| unexpected(format!("could not resume session {id}: {error}")))?,
        ),
    };
    let mcp_session =
        loaded_session
            .as_ref()
            .map_or(McpSession::New, |loaded| McpSession::Resumed {
                catalog: loaded.external_tool_catalog(),
            });
    let mcp = discover_configured_mcp_tools(merry_config, mcp_session).await?;
    write_startup_warnings(&mut tokio::io::stderr(), &mcp.warnings)
        .await
        .map_err(unexpected)?;
    let runtime_input = HeadlessCodingRuntimeInput {
        session_id: session.id().as_str(),
        root: &root,
        provider,
        model,
        process_backend: backend,
        extra_tools: mcp.tools,
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
    let headless_reviewer = HeadlessPermissionReviewer::new();
    let permission_policy = CodingPermissionPolicy::for_process_boundary(
        process_execution_mode.into(),
        if fully_trusted {
            CodingTrustMode::FullyTrusted
        } else {
            CodingTrustMode::Reviewed
        },
        merry_config
            .map(MerryConfig::no_sandbox_review_mode)
            .unwrap_or_default(),
        Some(headless_reviewer.source()),
    )
    .map_err(unexpected)?;
    let coding_runtime = match loaded_session {
        None => build_headless_coding_with_policy_composition(runtime_input, permission_policy)?,
        Some(loaded) => resume_headless_coding_composition_with_loaded_session(
            runtime_input,
            loaded,
            permission_policy,
        )?,
    };
    let loop_config = coding_runtime.loop_config();
    let runtime = coding_runtime.into_runtime();
    runtime
        .save_session_to(session_store.clone())
        .await
        .map_err(unexpected)?;
    let input = StepInput::user_text(&task).map_err(unexpected)?;
    let context = StepContext::default()
        .with_generation_config(generation_config(merry_config).map_err(unexpected)?);
    let review_task = headless_reviewer.start(review_input_channel_for_task(&args.task));
    run_agent_loop_with_persistence(
        &runtime,
        input,
        tokio::io::stdout(),
        async { review_task.finish().await.map_err(unexpected) },
        HeadlessRunPersistence {
            loop_config,
            context,
            events_jsonl: args.events_jsonl,
            session_store,
            session_id: session.id(),
            metadata: headless_metadata,
        },
    )
    .await
}

/// Settles the runtime and permission reviewer before attempting persistence.
///
/// This is the production headless-run orchestration boundary. Presentation
/// failures stop the event producer at the output boundary; persistence is
/// then attempted after reviewer shutdown. Final error selection happens only
/// after that save attempt.
struct HeadlessRunPersistence<'a> {
    loop_config: AgentLoopConfig,
    context: StepContext,
    events_jsonl: bool,
    session_store: FileSessionStore,
    session_id: &'a SessionId,
    metadata: TuiSessionMetadata,
}

async fn run_agent_loop_with_persistence<W, F>(
    runtime: &Runtime,
    input: StepInput,
    writer: W,
    review_result: F,
    persistence: HeadlessRunPersistence<'_>,
) -> Result<RunExitStatus, CliError>
where
    W: AsyncWrite + Unpin,
    F: Future<Output = Result<(), CliError>>,
{
    let settled = if persistence.events_jsonl {
        settle_agent_loop_jsonl_output(
            runtime,
            input,
            persistence.loop_config,
            persistence.context,
            writer,
        )
        .await
    } else {
        settle_agent_loop_output(
            runtime,
            input,
            persistence.loop_config,
            persistence.context,
            writer,
        )
        .await
    };
    let review_result = review_result.await;
    finish_settled_run(
        runtime,
        persistence.session_store,
        persistence.session_id,
        persistence.metadata,
        settled,
        review_result,
    )
    .await
}

/// Persists a settled run before selecting its final result.
///
/// A persistence failure takes precedence over runtime, stdout, or reviewer
/// failures so the CLI cannot report only an earlier failure when durable state
/// was not written. When persistence succeeds, errors are returned in runtime,
/// presentation, then reviewer order.
async fn finish_settled_run(
    runtime: &Runtime,
    session_store: FileSessionStore,
    session_id: &SessionId,
    metadata: TuiSessionMetadata,
    settled: SettledRun,
    review_result: Result<(), CliError>,
) -> Result<RunExitStatus, CliError> {
    let SettledRun {
        runtime_result,
        presentation_result,
    } = settled;
    persist_settled_session(runtime, session_store, session_id, metadata).await?;
    let status = runtime_result?;
    presentation_result?;
    review_result?;
    Ok(status)
}

/// Records a result for every tool call the run left pending, then saves.
///
/// A run that fails mid-step settles with its tool call still unresolved, and
/// the session store refuses that state because it cannot be resumed. Giving
/// those calls a durable failed result is what makes a failed run's partial
/// transcript resumable on the shipped path, rather than only under a reviewer
/// that happens to submit results of its own.
async fn persist_settled_session(
    runtime: &Runtime,
    session_store: FileSessionStore,
    session_id: &SessionId,
    metadata: TuiSessionMetadata,
) -> Result<(), CliError> {
    runtime
        .abandon_pending_tool_calls(ABANDONED_TOOL_CALL_REASON)
        .await
        .map_err(|error| {
            unexpected(format!(
                "run finished but session {session_id} could not be made resume-safe: {error}"
            ))
        })?;
    runtime
        .save_session_to(session_store.clone())
        .await
        .map_err(|error| {
            unexpected(format!(
                "run finished but session {session_id} could not be saved: {error}"
            ))
        })?;
    write_headless_session_metadata(&session_store, metadata).await
}

async fn headless_session_metadata(
    store: &FileSessionStore,
    session: &RunSession,
    workspace_root: &std::path::Path,
) -> Result<TuiSessionMetadata, CliError> {
    let tui_store = TuiSessionStore::new(store.sessions_dir().to_path_buf());
    let session_id = session.id().clone();
    let existing = tokio::task::spawn_blocking({
        let tui_store = tui_store.clone();
        let session_id = session_id.clone();
        move || tui_store.read_metadata(&session_id)
    })
    .await
    .map_err(unexpected)?
    .map_err(unexpected)?;
    let mut metadata = existing.unwrap_or_else(|| {
        TuiSessionMetadata::new(
            session_id.clone(),
            workspace_root.to_path_buf(),
            now_unix_ms(),
        )
    });
    metadata.workspace_root = workspace_root.to_path_buf();
    if matches!(session, RunSession::New(_)) {
        metadata.headless = true;
        if metadata.title.is_none() {
            metadata.title = Some("Headless run".to_owned());
        }
    }
    metadata.mark_active(now_unix_ms());
    Ok(metadata)
}

async fn write_headless_session_metadata(
    session_store: &FileSessionStore,
    metadata: TuiSessionMetadata,
) -> Result<(), CliError> {
    let store = TuiSessionStore::new(session_store.sessions_dir().to_path_buf());
    tokio::task::spawn_blocking(move || store.write_metadata(&metadata))
        .await
        .map_err(unexpected)?
        .map_err(unexpected)
}

fn default_run_session_id() -> merry_core::SessionId {
    crate::session_id::new_ephemeral_session_id()
}

#[cfg(test)]
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
    settle_agent_loop_output(runtime, input, config, context, writer)
        .await
        .into_output_result()
}

async fn settle_agent_loop_output<W>(
    runtime: &Runtime,
    input: StepInput,
    config: AgentLoopConfig,
    context: StepContext,
    writer: W,
) -> SettledRun
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let mut stream = match runtime.run_agent_loop_stream(input, context, config) {
        Ok(stream) => stream,
        Err(error) => {
            return SettledRun {
                runtime_result: Err(unexpected(error)),
                presentation_result: Ok(()),
            };
        }
    };
    let mut pending_commentary = None;
    let mut presentation_error = None;
    loop {
        let event = match stream.next_message().await {
            Ok(Some(merry_runtime::AgentRunMessage::Event(event))) => event,
            Ok(Some(merry_runtime::AgentRunMessage::ToolInvocations { batch })) => {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Err(unexpected(format!(
                        "CLI received {} host tool invocations, but this path requires runtime-owned tools",
                        batch.calls().len()
                    ))),
                    presentation_result: Ok(()),
                };
            }
            Ok(Some(_)) => {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Err(unexpected(
                        "runtime emitted an unsupported agent run message",
                    )),
                    presentation_result: Ok(()),
                };
            }
            Ok(None) => break,
            Err(error) => {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Err(unexpected(error)),
                    presentation_result: Ok(()),
                };
            }
        };
        if let Err(error) =
            write_human_progress_event(&event, &mut pending_commentary, &mut writer).await
        {
            stream.cancel_and_wait().await;
            return SettledRun {
                runtime_result: Ok(RunExitStatus::Incomplete),
                presentation_result: Err(error),
            };
        }
    }
    let runtime_result = match stream.result().await.map_err(unexpected) {
        Ok(result) => {
            let status = RunExitStatus::from_agent_loop_result(&result);
            if presentation_error.is_none() {
                if let Err(error) = write_agent_loop_summary_to(&result, &mut writer).await {
                    presentation_error = Some(error);
                } else if let Err(error) = writer.flush().await.map_err(stdout_error) {
                    presentation_error = Some(error);
                }
            }
            Ok(status)
        }
        Err(error) => Err(error),
    };
    SettledRun {
        runtime_result,
        presentation_result: match presentation_error {
            Some(error) => Err(error),
            None => Ok(()),
        },
    }
}

#[cfg(test)]
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
    settle_agent_loop_jsonl_output(runtime, input, config, context, writer)
        .await
        .into_output_result()
}

async fn settle_agent_loop_jsonl_output<W>(
    runtime: &Runtime,
    input: StepInput,
    config: AgentLoopConfig,
    context: StepContext,
    writer: W,
) -> SettledRun
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let mut stream = match runtime.run_agent_loop_stream(input, context, config) {
        Ok(stream) => stream,
        Err(error) => {
            return SettledRun {
                runtime_result: Err(unexpected(error)),
                presentation_result: Ok(()),
            };
        }
    };
    let mut presentation_error = None;

    loop {
        let event = match stream.next_message().await {
            Ok(Some(merry_runtime::AgentRunMessage::Event(event))) => event,
            Ok(Some(merry_runtime::AgentRunMessage::ToolInvocations { batch })) => {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Err(unexpected(format!(
                        "CLI received {} host tool invocations, but this path requires runtime-owned tools",
                        batch.calls().len()
                    ))),
                    presentation_result: Ok(()),
                };
            }
            Ok(Some(_)) => {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Err(unexpected(
                        "runtime emitted an unsupported agent run message",
                    )),
                    presentation_result: Ok(()),
                };
            }
            Ok(None) => break,
            Err(error) => {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Err(unexpected(error)),
                    presentation_result: Ok(()),
                };
            }
        };
        if presentation_error.is_none() {
            if let Err(error) = write_public_runtime_event(&event, &mut writer).await {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Ok(RunExitStatus::Incomplete),
                    presentation_result: Err(error),
                };
            }
            if let Err(error) = writer.flush().await.map_err(stdout_error) {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Ok(RunExitStatus::Incomplete),
                    presentation_result: Err(error),
                };
            }
        }
    }

    let runtime_result = match stream.result().await.map_err(unexpected) {
        Ok(result) => {
            let status = RunExitStatus::from_agent_loop_result(&result);
            if presentation_error.is_none() {
                if let Err(error) = write_agent_loop_result(&result, &mut writer).await {
                    presentation_error = Some(error);
                } else if let Err(error) = writer.flush().await.map_err(stdout_error) {
                    presentation_error = Some(error);
                }
            }
            Ok(status)
        }
        Err(error) => Err(error),
    };
    SettledRun {
        runtime_result,
        presentation_result: match presentation_error {
            Some(error) => Err(error),
            None => Ok(()),
        },
    }
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
mod persistence_tests;

#[cfg(test)]
mod session_tests;

#[cfg(test)]
mod output_tests;
