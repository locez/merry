//! Headless command setup; settlement and durable saving are owned by child modules.

use crate::{
    cli_error::{CliError, debug_openai_usage_error, unexpected, usage_error},
    coding::{
        AutoFallback, CodingPermissionPolicy, HeadlessCodingRuntimeInput, PermissionMode,
        SandboxMode, action_process_runner_for_mode, build_headless_coding_with_policy_composition,
        coding_agent_process_admission, resume_headless_coding_composition_with_loaded_session,
    },
    config::{MerryConfig, XdgPaths},
    headless_review::{HeadlessPermissionReviewer, ReviewInputChannel},
    mcp_tools::{discover_configured_mcp_tools, write_startup_warnings},
    provider_config::{
        RuntimePrimaryProviderConfig, RuntimeProviderBundle, build_coding_subagents_config,
        runtime_provider_bundle_from_config,
    },
    run::persistence::{
        HeadlessRunPersistence, headless_session_metadata, run_agent_loop_with_persistence,
    },
    runtime_config::{
        automatic_compaction_config, generation_config, prepared_action_process_backend_options,
        subagents_config,
    },
    workspace_trust::{self, TrustWorkspace},
};
use merry_core::SessionId;
use merry_runtime::{
    AgentLoopResult, AgentLoopStatus, FileSessionStore, LoadedSession, SessionReservation,
    StepContext, StepInput,
};
use tokio::io::{AsyncRead, AsyncReadExt};

mod output;

mod persistence;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunExitStatus {
    Completed,
    Incomplete,
    /// The workspace dialog was closed before the task started.
    Declined,
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
pub(crate) const STDIN_TASK: &str = "-";

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
/// Saving a session appends `journal.jsonl`, so starting a new run under an
/// existing id would mix a fresh transcript into that session. A typo or a
/// reused id has to fail here, before the run consumes its task or starts a
/// runtime. A legacy `state.json` still occupies the id.
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
    merry_config: Option<&MerryConfig>,
    sandbox_mode: SandboxMode,
    permission_mode: PermissionMode,
    auto_fallback: AutoFallback,
    trust_workspace: Option<TrustWorkspace>,
) -> Result<RunExitStatus, CliError> {
    let paths = XdgPaths::from_env().map_err(unexpected)?;
    let workspace =
        match workspace_trust::prepare_product_workspace(sandbox_mode, trust_workspace, &paths)? {
            workspace_trust::WorkspaceLaunch::Ready(workspace) => workspace,
            workspace_trust::WorkspaceLaunch::Ask(pending) => {
                let choice = crate::tui::trust_dialog::prompt_owned(pending.root()).await?;
                let Some(workspace) = pending.complete(choice)? else {
                    return Ok(RunExitStatus::Declined);
                };
                workspace
            }
        };
    let session = RunSession::from_args(args)?;
    let session_store = FileSessionStore::default_store().map_err(unexpected)?;
    let _session_reservation = reserve_run_session(&session, &session_store).await?;
    let task = resolve_task(&args.task, tokio::io::stdin()).await?;
    let _admission = coding_agent_process_admission(sandbox_mode);

    let RuntimeProviderBundle {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    } = runtime_provider_bundle_from_config(merry_config, debug_openai_usage_error)?;
    let RuntimePrimaryProviderConfig { provider, model } = primary;
    let root = workspace.root;
    let mut backend_options =
        prepared_action_process_backend_options(merry_config, sandbox_mode).await?;
    if !workspace.writable {
        backend_options = backend_options.read_only_workspace();
    }
    let backend = action_process_runner_for_mode(&root, backend_options, sandbox_mode)?;
    let headless_metadata = headless_session_metadata(&session_store, &session, &root).await?;
    let loaded_session = match &session {
        RunSession::New(_) => None,
        RunSession::Resumed(id) => Some(
            LoadedSession::load(&session_store, id)
                .await
                .map_err(|error| unexpected(format!("could not resume session {id}: {error}")))?,
        ),
    };
    let mcp = discover_configured_mcp_tools(merry_config).await?;
    write_startup_warnings(&mut tokio::io::stderr(), &mcp.warnings)
        .await
        .map_err(unexpected)?;
    let subagents_cfg = subagents_config(merry_config).map_err(unexpected)?;
    let coding_subagents = build_coding_subagents_config(
        merry_config,
        &subagents_cfg,
        &root,
        false,
        debug_openai_usage_error,
    )?;
    let runtime_input = HeadlessCodingRuntimeInput {
        session_id: session.id().as_str(),
        root: &root,
        provider,
        model,
        process_backend: backend,
        extra_tools: mcp.tools,
        automatic_compaction: automatic_compaction_config(merry_config).map_err(unexpected)?,
        retry_policy,
        context_compaction,
        approval_review,
        skill_roots: merry_config
            .map(MerryConfig::skill_roots)
            .transpose()
            .map_err(unexpected)?
            .unwrap_or_default(),
        subagents: coding_subagents,
        workspace_tool_limits: None,
        workspace_writable: workspace.writable,
    };
    let headless_reviewer = HeadlessPermissionReviewer::new();
    let permission_policy = CodingPermissionPolicy::for_permission_mode(
        permission_mode.coding_mode(),
        auto_fallback.coding_fallback(),
        Some(headless_reviewer.source()),
    )
    .map_err(unexpected)?;
    let permission_review_rules = merry_config
        .map(MerryConfig::permission_review_rules)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let max_parallel_tool_calls = merry_config
        .map(MerryConfig::max_parallel_tool_calls)
        .transpose()
        .map_err(unexpected)?
        .flatten();
    let coding_runtime = match loaded_session {
        None => build_headless_coding_with_policy_composition(
            runtime_input,
            permission_policy,
            permission_review_rules,
            Some(session_store.clone()),
            max_parallel_tool_calls,
        )?,
        Some(loaded) => resume_headless_coding_composition_with_loaded_session(
            runtime_input,
            loaded,
            permission_policy,
            permission_review_rules,
            Some(session_store.clone()),
            max_parallel_tool_calls,
        )?,
    };
    let loop_config = coding_runtime.loop_config();
    let runtime = coding_runtime.into_runtime();
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
