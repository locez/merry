//! Headless command setup; settlement and durable saving are owned by child modules.

use crate::{
    cli_error::{CliError, debug_openai_usage_error, unexpected, usage_error},
    coding::{
        CodingPermissionPolicy, CodingTrustMode, HeadlessCodingRuntimeInput, ProcessExecutionMode,
        action_process_runner_for_mode, build_headless_coding_with_policy_composition,
        coding_agent_process_admission, coding_agent_requires_sandbox_error,
        resume_headless_coding_composition_with_loaded_session,
    },
    config::MerryConfig,
    headless_review::{HeadlessPermissionReviewer, ReviewInputChannel},
    mcp_tools::{McpSession, discover_configured_mcp_tools, write_startup_warnings},
    provider_config::{
        RuntimePrimaryProviderConfig, RuntimeProviderBundle, runtime_provider_bundle_from_config,
    },
    run::persistence::{
        HeadlessRunPersistence, headless_session_metadata, run_agent_loop_with_persistence,
    },
    runtime_config::{
        automatic_compaction_config, generation_config, prepared_action_process_backend_options,
        subagents_config,
    },
    sandbox::ChildHandoff as SandboxChildHandoff,
};
use merry_core::SessionId;
use merry_runtime::{
    AgentLoopResult, AgentLoopStatus, FileSessionStore, LoadedSession, SessionReservation,
    StepContext, StepInput,
};
use std::env;
use tokio::io::{AsyncRead, AsyncReadExt};

mod output;

mod persistence;

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

/// `TASK` value that reads the task text from stdin instead of argv.
const STDIN_TASK: &str = "-";

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
        prepared_action_process_backend_options(merry_config, process_execution_mode).await?,
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

fn default_run_session_id() -> merry_core::SessionId {
    crate::session_id::new_ephemeral_session_id()
}

#[cfg(test)]
mod persistence_tests;

#[cfg(test)]
mod session_tests;

#[cfg(test)]
mod output_tests;
