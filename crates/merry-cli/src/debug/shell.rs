use crate::cli_error::{CliError, shell_usage_error, stdout_error, unexpected};
use crate::coding_runtime::action_process_runner;
use crate::config::MerryConfig;
use crate::debug::{DEFAULT_SESSION_ID, ShellArgs};
use crate::runtime_events::{
    collect_runtime_step_events, first_pending_tool_call, write_runtime_events,
    write_runtime_step_events_to,
};
use crate::sandbox::{
    ChildHandoff as SandboxChildHandoff, MERRY_SANDBOX_ENV, MERRY_SANDBOX_VERSION,
    MERRY_SANDBOX_VERSION_ENV, RuntimeProfile as SandboxRuntimeProfile, read_proc_self_mountinfo,
    runtime_profile_from_evidence as sandbox_runtime_profile_from_evidence,
};
use futures_util::stream;
use merry_core::{ProviderName, RuntimeEvent, RuntimeEventKind, SessionId, ToolName};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, ArtifactContent, MAX_PROCESS_OUTPUT_LIMIT_BYTES,
    ProcessActionIntent, ProcessEnvPolicy, ProcessRunner, Runtime, StepContext, StepInput,
    TokioProcessRunner, ToolExecutionContext, process_command_tool,
};
use std::{env, ffi::OsStr, sync::Arc};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

#[cfg(test)]
mod tests;

const TOOL_NAME: &str = "shell_command";
const TOOL_CALL_ID: &str = "call-shell-command";
const STEP_INPUT: &str = "run shell command through Merry process protocol";

pub(crate) async fn run(
    args: ShellArgs,
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let sandbox_marker = env::var_os(MERRY_SANDBOX_ENV);
    let sandbox_version = env::var_os(MERRY_SANDBOX_VERSION_ENV);
    let home = env::var_os("HOME");
    let tmpdir = env::var_os("TMPDIR");
    let mountinfo = read_proc_self_mountinfo().await;
    let sandbox_runtime_profile = sandbox_runtime_profile_from_evidence(
        home.as_deref(),
        tmpdir.as_deref(),
        mountinfo.as_deref(),
    );
    let admission = runtime_admission(
        args.accept_local_workspace_process_risk,
        sandbox_child_handoff,
        sandbox_runtime_profile,
        sandbox_marker.as_deref(),
        sandbox_version.as_deref(),
    );
    let intent = process_action_intent(args.argv)?;
    let current_dir = env::current_dir().map_err(|source| {
        CliError::Unexpected(format!(
            "failed to resolve current directory for shell action sandbox: {source}"
        ))
    })?;
    let runner: Arc<dyn ProcessRunner> = if sandbox_child_handoff.is_some() {
        action_process_runner(&current_dir, merry_config)?.runner()
    } else {
        Arc::new(TokioProcessRunner::new_at_workspace_root(&current_dir))
    };
    run_to_writer(
        intent,
        admission,
        runner,
        args.events_jsonl,
        tokio::io::stdout(),
    )
    .await
}

pub(crate) async fn run_to_writer<W>(
    intent: ProcessActionIntent,
    admission: Option<AcceptedLocalWorkspaceProcessAdmission>,
    runner: Arc<dyn ProcessRunner>,
    events_jsonl: bool,
    writer: W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let session_id = SessionId::new(DEFAULT_SESSION_ID).map_err(shell_usage_error)?;
    let runtime = build_runtime(session_id, intent, admission, runner)?;
    let input = StepInput::user_text(STEP_INPUT).map_err(unexpected)?;

    let mut writer = BufWriter::new(writer);
    let events = if events_jsonl {
        write_runtime_step_events_to(&runtime, input, StepContext::default(), &mut writer).await?
    } else {
        collect_runtime_step_events(&runtime, input, StepContext::default()).await?
    };
    let Some(pending) = first_pending_tool_call(&events) else {
        writer.flush().await.map_err(stdout_error)?;
        return Err(CliError::Unexpected(format!(
            "shell tool `{TOOL_NAME}` was not called; no tool call was pending"
        )));
    };

    let actual_tool_name = pending.name().as_str();
    if actual_tool_name != TOOL_NAME {
        writer.flush().await.map_err(stdout_error)?;
        return Err(CliError::Unexpected(format!(
            "shell tool `{TOOL_NAME}` was not called; pending tool was `{actual_tool_name}`"
        )));
    }

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .map_err(unexpected)?;
    if events_jsonl {
        write_runtime_events(execution_events.clone(), &mut writer).await?;
    } else {
        write_process_output(&runtime, &execution_events, &mut writer).await?;
    }
    writer.flush().await.map_err(stdout_error)
}

fn build_runtime(
    session_id: SessionId,
    intent: ProcessActionIntent,
    admission: Option<AcceptedLocalWorkspaceProcessAdmission>,
    runner: Arc<dyn ProcessRunner>,
) -> Result<Runtime, CliError> {
    let shell_tool = process_command_tool(
        ToolName::new(TOOL_NAME).map_err(unexpected)?,
        "Run the exact CLI argv as a Merry process action.",
    )
    .map_err(unexpected)?;
    let provider = ShellToolCallProvider::new(&intent)?;
    let mut builder = Runtime::builder(session_id)
        .register_tool(shell_tool)
        .allow_low_risk_process_actions(Arc::clone(&runner))
        .model_provider(
            Arc::new(provider),
            ModelName::new("merry-shell-debug").map_err(unexpected)?,
        );
    if let Some(admission) = admission {
        builder = builder.allow_accepted_local_workspace_process_actions(admission, runner);
    }
    builder.build().map_err(unexpected)
}

pub(crate) fn runtime_admission(
    accept_local_workspace_process_risk: bool,
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    sandbox_runtime_profile: Option<SandboxRuntimeProfile>,
    sandbox: Option<&OsStr>,
    version: Option<&OsStr>,
) -> Option<AcceptedLocalWorkspaceProcessAdmission> {
    if accept_local_workspace_process_risk
        && sandbox_child_handoff == Some(SandboxChildHandoff::CliBwrapV1)
        && sandbox_runtime_profile == Some(SandboxRuntimeProfile::CliBwrapV1)
        && sandbox == Some(OsStr::new("1"))
        && version == Some(OsStr::new(MERRY_SANDBOX_VERSION))
    {
        Some(AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1())
    } else {
        None
    }
}

pub(crate) fn process_action_intent(argv: Vec<String>) -> Result<ProcessActionIntent, CliError> {
    ProcessActionIntent::new(
        argv,
        Some(".".to_owned()),
        ProcessEnvPolicy::empty(),
        None,
        MAX_PROCESS_OUTPUT_LIMIT_BYTES,
        MAX_PROCESS_OUTPUT_LIMIT_BYTES,
    )
    .map_err(shell_usage_error)
}

async fn write_process_output<W>(
    runtime: &Runtime,
    events: &[RuntimeEvent],
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let result = events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .ok_or_else(|| {
            CliError::Unexpected("shell command did not resolve a tool call".to_owned())
        })?;
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .map_err(unexpected)?;
    let ArtifactContent::Json(content) = content else {
        return Err(CliError::Unexpected(
            "shell command result artifact was not JSON".to_owned(),
        ));
    };
    let value = serde_json::from_str::<serde_json::Value>(&content).map_err(unexpected)?;
    let stdout = value
        .pointer("/stdout/text")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            CliError::Unexpected("shell command result missing stdout text".to_owned())
        })?;
    let stderr = value
        .pointer("/stderr/text")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            CliError::Unexpected("shell command result missing stderr text".to_owned())
        })?;

    writer
        .write_all(stdout.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer
        .write_all(stderr.as_bytes())
        .await
        .map_err(stdout_error)
}

struct ShellToolCallProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    call: ModelToolCall,
}

impl ShellToolCallProvider {
    fn new(intent: &ProcessActionIntent) -> Result<Self, CliError> {
        let mut arguments = serde_json::Map::new();
        arguments.insert("argv".to_owned(), serde_json::json!(intent.argv()));
        if let Some(cwd) = intent.cwd() {
            arguments.insert("cwd".to_owned(), serde_json::Value::String(cwd.to_owned()));
        }

        Ok(Self {
            name: ProviderName::new("merry-shell-cli-provider").map_err(unexpected)?,
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .map_err(unexpected)?,
            call: ModelToolCall::new(
                ModelToolCallId::new(TOOL_CALL_ID).map_err(unexpected)?,
                ToolName::new(TOOL_NAME).map_err(unexpected)?,
                ToolArguments::new(arguments),
            ),
        })
    }
}

impl ModelProvider for ShellToolCallProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let response = ModelResponse::new(
                vec![ModelOutput::tool_call(self.call.clone())],
                FinishReason::ToolCalls,
                None,
            );
            let events = vec![Ok(ModelEvent::Completed { response })];
            Ok(Box::pin(stream::iter(events)) as ModelEventStream)
        })
    }
}
