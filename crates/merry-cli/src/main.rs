//! Debug and demonstration CLI for Merry.

use clap::{Args, CommandFactory, Parser, Subcommand};
use futures_util::{StreamExt, stream};
use merry_core::{
    ErrorInfo, PendingToolCall, ProviderName, RuntimeEvent, RuntimeEventKind, SessionId,
    ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelCapabilities, ModelError, ModelEvent, ModelEventStream,
    ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use merry_runtime::{
    ActionProposal, ActionProposalEvidence, MAX_PROCESS_OUTPUT_LIMIT_BYTES, ProcessActionIntent,
    ProcessEnvPolicy, ProcessExitStatus, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
    ProcessRunnerFuture, ProcessRunnerOutput, RegisteredTool, Runtime, StepContext, StepInput,
    ToolActionKind, ToolActionProposalFuture, ToolExecutionContext, ToolExecutionError,
    ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use std::{
    env,
    ffi::{OsStr, OsString},
    fmt, io,
    path::{Path, PathBuf},
    process::{ExitCode, Stdio, Termination},
    sync::Arc,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufWriter};

const DEFAULT_SESSION_ID: &str = "debug-session";
const DEFAULT_INPUT: &str = "debug step";
const DEBUG_TOOL_NAME: &str = "debug_echo";
const DEBUG_TOOL_CONTINUATION_INPUT: &str = "continue after debug tool";
const SHELL_TOOL_NAME: &str = "shell_command";
const SHELL_TOOL_CALL_ID: &str = "call-shell-command";
const SHELL_STEP_INPUT: &str = "run shell command through Merry process protocol";
const BWRAP_PROGRAM: &str = "bwrap";
const DEFAULT_SANDBOX_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
const SANDBOX_ETC_READ_ONLY_PATHS: &[&str] = &[
    "/etc/ld.so.cache",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/ssl",
    "/etc/pki",
];
const MERRY_SANDBOX_ENV: &str = "MERRY_SANDBOX";
const MERRY_SANDBOX_VERSION_ENV: &str = "MERRY_SANDBOX_VERSION";
const MERRY_SANDBOX_VERSION: &str = "1";
const SANDBOX_HOME: &str = "/home/merry";
const SANDBOX_TMPDIR: &str = "/tmp";
const OPENAI_ENV_HELP: &str = "\
Environment:
  MERRY_OPENAI_DEBUG=1       Required opt-in before any network attempt
  MERRY_OPENAI_API_KEY       Preferred API key after opt-in
  OPENAI_API_KEY             Fallback API key when MERRY_OPENAI_API_KEY is unset
  MERRY_OPENAI_MODEL         Required when --model is omitted
  MERRY_OPENAI_BASE_URL      Optional OpenAI-compatible base URL
  OPENAI_ORG_ID              Optional organization header
  OPENAI_PROJECT_ID          Optional project header
";

#[derive(Debug, Parser)]
#[command(
    name = "merry",
    about = "Debug and demonstration CLI for Merry.",
    disable_version_flag = true
)]
struct Cli {
    #[arg(long, help = "Run the command inside Merry's bubblewrap sandbox")]
    with_sandbox: bool,

    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    #[command(about = "Print deterministic runtime events or run opt-in provider debugging")]
    Debug(DebugArgs),
    #[command(about = "Run a local command through Merry's process action protocol")]
    Shell(ShellArgs),
}

#[derive(Debug, Args)]
struct ShellArgs {
    #[arg(
        required = true,
        allow_hyphen_values = true,
        last = true,
        num_args = 1..,
        value_name = "ARGV",
        help = "Command argv to run after `shell --`; no shell string parsing is performed"
    )]
    argv: Vec<String>,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
struct DebugArgs {
    #[arg(
        long,
        value_name = "SESSION_ID",
        default_value = DEFAULT_SESSION_ID,
        allow_hyphen_values = true,
        help = "Session id to use"
    )]
    session_id: String,

    #[arg(
        long,
        value_name = "TEXT",
        default_value = DEFAULT_INPUT,
        allow_hyphen_values = true,
        help = "User text input"
    )]
    input: String,

    #[command(subcommand)]
    command: Option<DebugCommand>,
}

#[derive(Debug, Subcommand)]
enum DebugCommand {
    #[command(
        name = "openai",
        about = "Run opt-in OpenAI-compatible model debugging",
        after_help = OPENAI_ENV_HELP
    )]
    OpenAi(DebugOpenAiArgs),
}

#[derive(Debug, Args)]
struct DebugOpenAiArgs {
    #[arg(
        long,
        required = true,
        value_name = "TEXT",
        allow_hyphen_values = true,
        help = "User text input to send through Runtime::step"
    )]
    input: String,

    #[arg(
        long,
        value_name = "MODEL",
        allow_hyphen_values = true,
        help = "Model name; falls back to MERRY_OPENAI_MODEL"
    )]
    model: Option<String>,

    #[arg(
        long,
        value_name = "N",
        value_parser = parse_max_output_tokens,
        help = "Optional maximum output tokens for this step"
    )]
    max_output_tokens: Option<u64>,

    #[arg(
        long,
        value_name = "TEXT",
        allow_hyphen_values = true,
        help = "Require first step to call debug_echo; return this text"
    )]
    debug_tool_result: Option<String>,
}

fn main() -> CliExit {
    let argv = env::args_os().collect::<Vec<_>>();
    let cli = match Cli::try_parse_from(argv.clone()) {
        Ok(cli) => cli,
        Err(error) => return CliExit::Clap(error),
    };

    if let Err(error) = maybe_reexec_sandbox(&cli, argv.iter().skip(1).cloned().collect()) {
        return CliExit::Unexpected(error.to_string());
    }

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return CliExit::Unexpected(err.to_string()),
    };

    runtime.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> CliExit {
    match cli.command {
        CliCommand::Debug(DebugArgs {
            session_id,
            input,
            command: None,
        }) => match run_debug(&session_id, &input).await {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: debug_openai_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::OpenAi(DebugOpenAiArgs {
                    input,
                    model,
                    max_output_tokens,
                    debug_tool_result,
                })),
            ..
        }) => match run_debug_openai(
            &input,
            model.as_deref(),
            max_output_tokens,
            debug_tool_result.as_deref(),
        )
        .await
        {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: debug_openai_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: debug_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: shell_usage(),
            },
        },
        CliCommand::Shell(args) => match run_shell(args).await {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: debug_openai_usage(),
            },
        },
    }
}

fn parse_max_output_tokens(value: &str) -> Result<u64, String> {
    let tokens = value
        .parse::<u64>()
        .map_err(|error| format!("must be a positive integer: {error}"))?;

    if tokens == 0 {
        return Err("must be greater than zero".to_owned());
    }

    Ok(tokens)
}

fn maybe_reexec_sandbox(cli: &Cli, args: Vec<OsString>) -> Result<(), SandboxError> {
    let host = SandboxHost::from_env(args)?;
    match plan_sandbox_bootstrap(cli.with_sandbox, &host)? {
        SandboxBootstrap::Disabled | SandboxBootstrap::AlreadyInside => Ok(()),
        SandboxBootstrap::Reexec(plan) => exec_sandbox(plan),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SandboxHost {
    cwd: PathBuf,
    current_exe: PathBuf,
    args: Vec<OsString>,
    path: Option<OsString>,
    inside_sandbox: bool,
}

impl SandboxHost {
    fn from_env(args: Vec<OsString>) -> Result<Self, SandboxError> {
        Ok(Self {
            cwd: env::current_dir().map_err(SandboxError::CurrentDir)?,
            current_exe: env::current_exe().map_err(SandboxError::CurrentExe)?,
            args,
            path: env::var_os("PATH"),
            // This marker is only a recursion guard for self-reexec. It is
            // not a security proof that the current process is confined.
            inside_sandbox: env::var_os(MERRY_SANDBOX_ENV).as_deref() == Some(OsStr::new("1")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SandboxPlan {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SandboxBootstrap {
    Disabled,
    AlreadyInside,
    Reexec(SandboxPlan),
}

fn plan_sandbox_bootstrap(
    with_sandbox: bool,
    host: &SandboxHost,
) -> Result<SandboxBootstrap, SandboxError> {
    plan_sandbox_bootstrap_with_file_exists(with_sandbox, host, Path::exists)
}

fn plan_sandbox_bootstrap_with_file_exists(
    with_sandbox: bool,
    host: &SandboxHost,
    file_exists: impl Fn(&Path) -> bool,
) -> Result<SandboxBootstrap, SandboxError> {
    if !with_sandbox {
        return Ok(SandboxBootstrap::Disabled);
    }

    if host.inside_sandbox {
        return Ok(SandboxBootstrap::AlreadyInside);
    }

    let path = sandbox_path(host);
    let bwrap = find_bwrap_in_path(&path, file_exists).ok_or(SandboxError::MissingBubblewrap)?;

    Ok(SandboxBootstrap::Reexec(build_sandbox_plan(
        host, path, bwrap,
    )))
}

fn build_sandbox_plan(host: &SandboxHost, path: OsString, bwrap: PathBuf) -> SandboxPlan {
    let cwd = host.cwd.as_os_str().to_owned();
    let current_exe = host.current_exe.as_os_str().to_owned();

    let mut args = vec![
        os("--unshare-user"),
        os("--unshare-ipc"),
        os("--unshare-pid"),
        os("--unshare-uts"),
        os("--unshare-cgroup-try"),
        os("--disable-userns"),
        os("--die-with-parent"),
        os("--new-session"),
        os("--proc"),
        os("/proc"),
        os("--dev"),
        os("/dev"),
        os("--perms"),
        os("01777"),
        os("--tmpfs"),
        os(SANDBOX_TMPDIR),
        os("--tmpfs"),
        os("/home"),
        os("--perms"),
        os("0700"),
        os("--dir"),
        os(SANDBOX_HOME),
        os("--ro-bind"),
        os("/usr"),
        os("/usr"),
        os("--ro-bind-try"),
        os("/bin"),
        os("/bin"),
        os("--ro-bind-try"),
        os("/lib"),
        os("/lib"),
        os("--ro-bind-try"),
        os("/lib64"),
        os("/lib64"),
    ];
    for path in SANDBOX_ETC_READ_ONLY_PATHS {
        args.extend([os("--ro-bind-try"), os(path), os(path)]);
    }
    args.extend([
        os("--bind"),
        cwd.clone(),
        cwd.clone(),
        os("--chdir"),
        cwd.clone(),
        os("--clearenv"),
        os("--setenv"),
        os("PATH"),
        path.clone(),
        os("--setenv"),
        os("HOME"),
        os(SANDBOX_HOME),
        os("--setenv"),
        os("TMPDIR"),
        os(SANDBOX_TMPDIR),
        os("--setenv"),
        os("PWD"),
        cwd,
        os("--setenv"),
        os(MERRY_SANDBOX_ENV),
        os("1"),
        os("--setenv"),
        os(MERRY_SANDBOX_VERSION_ENV),
        os(MERRY_SANDBOX_VERSION),
        current_exe,
    ]);
    args.extend(args_without_with_sandbox(&host.args));

    SandboxPlan {
        program: bwrap.as_os_str().to_owned(),
        args,
        env: vec![(os("PATH"), path)],
    }
}

fn find_bwrap_in_path(path: &OsStr, file_exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(BWRAP_PROGRAM))
        .find(|candidate| file_exists(candidate))
}

fn sandbox_path(host: &SandboxHost) -> OsString {
    host.path
        .as_ref()
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| os(DEFAULT_SANDBOX_PATH))
}

fn args_without_with_sandbox(args: &[OsString]) -> Vec<OsString> {
    let mut removed = false;
    args.iter()
        .filter_map(|arg| {
            if !removed && arg == OsStr::new("--with-sandbox") {
                removed = true;
                None
            } else {
                Some(arg.clone())
            }
        })
        .collect()
}

fn os(value: &str) -> OsString {
    OsString::from(value)
}

fn exec_sandbox(plan: SandboxPlan) -> Result<(), SandboxError> {
    #[cfg(target_os = "linux")]
    {
        let error = exec_sandbox_plan(&plan);
        if error.kind() == io::ErrorKind::NotFound {
            Err(SandboxError::MissingBubblewrap)
        } else {
            Err(SandboxError::Exec(error))
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = plan;
        Err(SandboxError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "linux")]
fn exec_sandbox_plan(plan: &SandboxPlan) -> io::Error {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new(&plan.program);
    command.args(&plan.args).env_clear().envs(plan.env.clone());
    command.exec()
}

#[derive(Debug)]
enum SandboxError {
    CurrentDir(io::Error),
    CurrentExe(io::Error),
    #[cfg(not(target_os = "linux"))]
    UnsupportedPlatform,
    MissingBubblewrap,
    Exec(io::Error),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SandboxError::CurrentDir(error) => write!(
                formatter,
                "failed to read current directory before sandbox bootstrap: {error}"
            ),
            SandboxError::CurrentExe(error) => write!(
                formatter,
                "failed to locate current executable before sandbox bootstrap: {error}"
            ),
            #[cfg(not(target_os = "linux"))]
            SandboxError::UnsupportedPlatform => write!(
                formatter,
                "merry --with-sandbox is only supported on Linux with bubblewrap (bwrap)"
            ),
            SandboxError::MissingBubblewrap => write!(
                formatter,
                "bubblewrap executable `bwrap` was not found in PATH; install bubblewrap or run without --with-sandbox"
            ),
            SandboxError::Exec(error) => {
                write!(
                    formatter,
                    "failed to execute bubblewrap sandbox bootstrap: {error}"
                )
            }
        }
    }
}

async fn run_debug(session_id: &str, input: &str) -> Result<(), CliError> {
    let session_id = SessionId::new(session_id).map_err(usage_error)?;
    let runtime = Runtime::builder(session_id).build().map_err(unexpected)?;
    let input = StepInput::user_text(input).map_err(usage_error)?;
    write_runtime_step_events(&runtime, input, StepContext::default(), tokio::io::stdout()).await
}

async fn run_debug_openai(
    input: &str,
    model: Option<&str>,
    max_output_tokens: Option<u64>,
    debug_tool_result: Option<&str>,
) -> Result<(), CliError> {
    let config = debug_openai_config(model)?;

    let session_id = SessionId::new(DEFAULT_SESSION_ID).map_err(debug_openai_usage_error)?;
    let model = ModelName::new(&config.model).map_err(debug_openai_usage_error)?;
    let provider = OpenAiProvider::new(config.provider);
    let mut builder = Runtime::builder(session_id).model_provider(Arc::new(provider), model);
    if let Some(result) = debug_tool_result {
        builder = builder.register_tool(debug_echo_tool(result)?);
    }
    let runtime = builder.build().map_err(unexpected)?;
    let input = StepInput::user_text(input).map_err(debug_openai_usage_error)?;
    let generation_config =
        GenerationConfig::new(max_output_tokens, false).map_err(debug_openai_usage_error)?;
    let context = StepContext::new(Default::default()).with_generation_config(generation_config);

    if debug_tool_result.is_some() {
        write_debug_openai_tool_events(&runtime, input, context, tokio::io::stdout()).await
    } else {
        write_runtime_step_events(&runtime, input, context, tokio::io::stdout()).await
    }
}

async fn run_shell(args: ShellArgs) -> Result<(), CliError> {
    let intent = shell_process_action_intent(args.argv)?;
    let session_id = SessionId::new(DEFAULT_SESSION_ID).map_err(shell_usage_error)?;
    let shell_tool = shell_command_tool(intent)?;
    let provider = ShellToolCallProvider::new()?;
    let runtime = Runtime::builder(session_id)
        .register_tool(shell_tool)
        .allow_low_risk_process_actions(Arc::new(TokioProcessRunner))
        .model_provider(
            Arc::new(provider),
            ModelName::new("merry-shell-debug").map_err(unexpected)?,
        )
        .build()
        .map_err(unexpected)?;
    let input = StepInput::user_text(SHELL_STEP_INPUT).map_err(unexpected)?;

    let mut writer = BufWriter::new(tokio::io::stdout());
    let events =
        write_runtime_step_events_to(&runtime, input, StepContext::default(), &mut writer).await?;
    let Some(pending) = first_pending_tool_call(&events) else {
        writer.flush().await.map_err(stdout_error)?;
        return Err(CliError::Unexpected(format!(
            "shell tool `{SHELL_TOOL_NAME}` was not called; no tool call was pending"
        )));
    };

    let actual_tool_name = pending.name().as_str();
    if actual_tool_name != SHELL_TOOL_NAME {
        writer.flush().await.map_err(stdout_error)?;
        return Err(CliError::Unexpected(format!(
            "shell tool `{SHELL_TOOL_NAME}` was not called; pending tool was `{actual_tool_name}`"
        )));
    }

    write_runtime_events(
        runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .map_err(unexpected)?,
        &mut writer,
    )
    .await?;
    writer.flush().await.map_err(stdout_error)
}

fn shell_process_action_intent(argv: Vec<String>) -> Result<ProcessActionIntent, CliError> {
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

async fn write_runtime_step_events<W>(
    runtime: &Runtime,
    input: StepInput,
    context: StepContext,
    writer: W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    write_runtime_step_events_to(runtime, input, context, &mut writer).await?;
    writer.flush().await.map_err(stdout_error)
}

async fn write_debug_openai_tool_events<W>(
    runtime: &Runtime,
    input: StepInput,
    context: StepContext,
    writer: W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let events = write_runtime_step_events_to(runtime, input, context.clone(), &mut writer).await?;
    let pending = first_pending_tool_call(&events);

    let Some(pending) = pending else {
        writer.flush().await.map_err(stdout_error)?;
        return Err(CliError::Unexpected(format!(
            "debug tool `{DEBUG_TOOL_NAME}` was not called on the first step; no tool call was pending"
        )));
    };

    let actual_tool_name = pending.name().as_str();
    if actual_tool_name != DEBUG_TOOL_NAME {
        writer.flush().await.map_err(stdout_error)?;
        return Err(CliError::Unexpected(format!(
            "debug tool `{DEBUG_TOOL_NAME}` was not called on the first step; pending tool was `{actual_tool_name}`"
        )));
    }

    write_runtime_events(
        runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .map_err(unexpected)?,
        &mut writer,
    )
    .await?;

    let input = StepInput::user_text(DEBUG_TOOL_CONTINUATION_INPUT).map_err(unexpected)?;
    write_runtime_step_events_to(runtime, input, context, &mut writer).await?;

    writer.flush().await.map_err(stdout_error)
}

async fn write_runtime_step_events_to<W>(
    runtime: &Runtime,
    input: StepInput,
    context: StepContext,
    writer: &mut W,
) -> Result<Vec<RuntimeEvent>, CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut events = runtime.step(input, context).map_err(unexpected)?;
    let mut written = Vec::new();
    while let Some(event) = events.next().await {
        write_runtime_event(&event, writer).await?;
        written.push(event);
    }

    Ok(written)
}

async fn write_runtime_events<W>(events: Vec<RuntimeEvent>, writer: &mut W) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    for event in events {
        write_runtime_event(&event, writer).await?;
    }
    Ok(())
}

async fn write_runtime_event<W>(event: &RuntimeEvent, writer: &mut W) -> Result<(), CliError>
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

fn first_pending_tool_call(events: &[RuntimeEvent]) -> Option<PendingToolCall> {
    events.iter().find_map(|event| match &event.kind {
        RuntimeEventKind::ToolCallPending { call } => Some(call.clone()),
        _ => None,
    })
}

fn debug_echo_tool(result: &str) -> Result<RegisteredTool, CliError> {
    if result.trim().is_empty() {
        return Err(debug_openai_usage_error(
            "--debug-tool-result must not be blank",
        ));
    }

    let schema = serde_json::from_value::<ToolInputSchema>(serde_json::json!({
        "type": "object",
        "additionalProperties": true
    }))
    .map_err(debug_openai_usage_error)?;
    let spec = ToolSpec::new(
        ToolName::new(DEBUG_TOOL_NAME).map_err(debug_openai_usage_error)?,
        "Return the fixed debug text provided by the CLI.",
        schema,
    )
    .map_err(debug_openai_usage_error)?;

    Ok(RegisteredTool::read_only(
        spec,
        Arc::new(DebugEchoExecutor {
            result: result.to_owned(),
        }),
    ))
}

struct DebugEchoExecutor {
    result: String,
}

impl ToolExecutor for DebugEchoExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(ToolExecutionOutcome::succeeded_text(self.result.clone())) })
    }
}

fn shell_command_tool(intent: ProcessActionIntent) -> Result<RegisteredTool, CliError> {
    let schema = serde_json::from_value::<ToolInputSchema>(serde_json::json!({
        "type": "object",
        "additionalProperties": false
    }))
    .map_err(unexpected)?;
    let spec = ToolSpec::new(
        ToolName::new(SHELL_TOOL_NAME).map_err(unexpected)?,
        "Propose the exact CLI argv as a Merry process action.",
        schema,
    )
    .map_err(unexpected)?;

    Ok(RegisteredTool::new(
        spec,
        Arc::new(ShellCommandExecutor { intent }),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal())
}

struct ShellCommandExecutor {
    intent: ProcessActionIntent,
}

impl ToolExecutor for ShellCommandExecutor {
    fn propose<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolActionProposalFuture<'a> {
        Box::pin(async move {
            let proposal = ActionProposal::new(
                &call,
                ToolActionKind::CommandExec,
                "shell command",
                self.intent.summary(),
                "Run proposed shell argv through the process action protocol.",
                ActionProposalEvidence::ProcessAction(self.intent.clone()),
            )
            .map_err(|source| ToolExecutionError::infrastructure(source.to_string()))?;
            Ok(Some(proposal))
        })
    }

    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let diagnostic = ErrorInfo::new(
                "shell_generic_execute_reached",
                "shell command reached generic executor instead of the process runner lane",
            )
            .expect("static shell diagnostic is valid");
            let payload = serde_json::json!({
                "ok": false,
                "error": {
                    "code": diagnostic.code(),
                    "message": diagnostic.message()
                }
            });
            Ok(ToolExecutionOutcome::failed_json(
                payload.to_string(),
                diagnostic,
            ))
        })
    }
}

struct ShellToolCallProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    call: ModelToolCall,
}

impl ShellToolCallProvider {
    fn new() -> Result<Self, CliError> {
        Ok(Self {
            name: ProviderName::new("merry-shell-cli-provider").map_err(unexpected)?,
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .map_err(unexpected)?,
            call: ModelToolCall::new(
                ModelToolCallId::new(SHELL_TOOL_CALL_ID).map_err(unexpected)?,
                ToolName::new(SHELL_TOOL_NAME).map_err(unexpected)?,
                ToolArguments::new(Default::default()),
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

struct TokioProcessRunner;

impl ProcessRunner for TokioProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move { run_tokio_process(intent, context).await })
    }
}

async fn run_tokio_process(
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
    let Some((program, args)) = intent.argv().split_first() else {
        return Err(ProcessRunnerError::infrastructure(
            "validated process argv was unexpectedly empty",
        ));
    };

    if context.cancellation_token().is_cancelled() {
        return Err(ProcessRunnerError::Cancelled);
    }

    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(intent.cwd().unwrap_or("."))
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ProcessRunnerError::infrastructure(format!(
                "process executable `{program}` was not found"
            ))
        } else {
            ProcessRunnerError::infrastructure(format!(
                "failed to start process executable `{program}`: {source}"
            ))
        }
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ProcessRunnerError::infrastructure("process stdout pipe was not available")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ProcessRunnerError::infrastructure("process stderr pipe was not available")
    })?;
    let stdout_limit = intent.stdout_limit_bytes();
    let stderr_limit = intent.stderr_limit_bytes();

    let stdout_task = tokio::spawn(async move { read_bounded_output(stdout, stdout_limit).await });
    let stderr_task = tokio::spawn(async move { read_bounded_output(stderr, stderr_limit).await });

    let status = tokio::select! {
        biased;
        () = context.cancellation_token().cancelled() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(ProcessRunnerError::Cancelled);
        }
        status = child.wait() => status,
    };
    let status = status.map_err(|source| {
        ProcessRunnerError::infrastructure(format!(
            "failed to wait for process executable `{program}`: {source}"
        ))
    })?;
    let stdout = join_bounded_output(stdout_task, "stdout").await?;
    let stderr = join_bounded_output(stderr_task, "stderr").await?;
    let stdout_text = String::from_utf8(stdout.bytes).map_err(|source| {
        ProcessRunnerError::infrastructure(format!("process stdout was not UTF-8: {source}"))
    })?;
    let stderr_text = String::from_utf8(stderr.bytes).map_err(|source| {
        ProcessRunnerError::infrastructure(format!("process stderr was not UTF-8: {source}"))
    })?;
    let status = status
        .code()
        .map(ProcessExitStatus::Exited)
        .unwrap_or(ProcessExitStatus::DomainFailed);

    ProcessRunnerOutput::new(
        &intent,
        status,
        stdout_text,
        stdout.truncated,
        stderr_text,
        stderr.truncated,
    )
    .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
}

async fn join_bounded_output(
    task: tokio::task::JoinHandle<Result<BoundedOutput, ProcessRunnerError>>,
    stream_name: &'static str,
) -> Result<BoundedOutput, ProcessRunnerError> {
    task.await.map_err(|source| {
        ProcessRunnerError::infrastructure(format!(
            "process {stream_name} reader task failed: {source}"
        ))
    })?
}

struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded_output<R>(
    mut reader: R,
    limit: usize,
) -> Result<BoundedOutput, ProcessRunnerError>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut scratch = [0_u8; 8192];
    let mut truncated = false;

    loop {
        let count = reader.read(&mut scratch).await.map_err(|source| {
            ProcessRunnerError::infrastructure(format!("failed to read process output: {source}"))
        })?;
        if count == 0 {
            break;
        }

        if bytes.len() < limit {
            let remaining = limit - bytes.len();
            let kept = count.min(remaining);
            bytes.extend_from_slice(&scratch[..kept]);
            truncated |= kept < count;
        } else {
            truncated = true;
        }
    }

    Ok(BoundedOutput { bytes, truncated })
}

fn debug_openai_config(model_flag: Option<&str>) -> Result<DebugOpenAiConfig, CliError> {
    if env::var("MERRY_OPENAI_DEBUG").as_deref() != Ok("1") {
        return Err(debug_openai_usage_error(
            "set MERRY_OPENAI_DEBUG=1 to enable live OpenAI-compatible debugging",
        ));
    }

    let api_key = required_openai_api_key()?;
    let model = match model_flag {
        Some(model) => model.to_owned(),
        None => required_env("MERRY_OPENAI_MODEL")?,
    };

    let mut provider = OpenAiProviderConfig::new(&api_key).map_err(debug_openai_usage_error)?;

    if let Some(base_url) = optional_env("MERRY_OPENAI_BASE_URL")? {
        provider = provider
            .with_base_url(&base_url)
            .map_err(debug_openai_usage_error)?;
    }

    if let Some(organization) = optional_env("OPENAI_ORG_ID")? {
        provider = provider
            .with_organization(&organization)
            .map_err(debug_openai_usage_error)?;
    }

    if let Some(project) = optional_env("OPENAI_PROJECT_ID")? {
        provider = provider
            .with_project(&project)
            .map_err(debug_openai_usage_error)?;
    }

    Ok(DebugOpenAiConfig { provider, model })
}

fn required_env(name: &'static str) -> Result<String, CliError> {
    match optional_env(name)? {
        Some(value) => Ok(value),
        None => Err(debug_openai_usage_error(format!("{name} must be set"))),
    }
}

fn required_openai_api_key() -> Result<String, CliError> {
    match optional_env("MERRY_OPENAI_API_KEY")? {
        Some(value) => Ok(value),
        None => match optional_env("OPENAI_API_KEY")? {
            Some(value) => Ok(value),
            None => Err(debug_openai_usage_error(
                "MERRY_OPENAI_API_KEY or OPENAI_API_KEY must be set",
            )),
        },
    }
}

fn optional_env(name: &'static str) -> Result<Option<String>, CliError> {
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => Err(debug_openai_usage_error(format!(
            "{name} must not be blank"
        ))),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(debug_openai_usage_error(format!(
            "{name} must be valid UTF-8"
        ))),
    }
}

struct DebugOpenAiConfig {
    provider: OpenAiProviderConfig,
    model: String,
}

fn debug_usage() -> String {
    let mut command = Cli::command();
    let command = command
        .find_subcommand_mut("debug")
        .expect("debug subcommand should exist");
    command.set_bin_name("merry debug");
    command_usage(command)
}

fn shell_usage() -> String {
    let mut command = Cli::command();
    let command = command
        .find_subcommand_mut("shell")
        .expect("shell subcommand should exist");
    command.set_bin_name("merry shell");
    command_usage(command)
}

fn debug_openai_usage() -> String {
    let mut command = DebugOpenAiArgs::augment_args(clap::Command::new("openai"))
        .bin_name("merry debug openai")
        .about("Run opt-in OpenAI-compatible model debugging")
        .after_help(OPENAI_ENV_HELP);
    command_usage(&mut command)
}

fn command_usage(command: &mut clap::Command) -> String {
    let mut buffer = Vec::new();
    command
        .write_help(&mut buffer)
        .expect("clap help should render");
    String::from_utf8(buffer).expect("clap help should be utf-8")
}

fn unexpected(err: impl fmt::Display) -> CliError {
    CliError::Unexpected(err.to_string())
}

fn usage_error(err: impl fmt::Display) -> CliError {
    CliError::DebugUsage(err.to_string())
}

fn debug_openai_usage_error(err: impl fmt::Display) -> CliError {
    CliError::DebugOpenAiUsage(err.to_string())
}

fn shell_usage_error(err: impl fmt::Display) -> CliError {
    CliError::ShellUsage(err.to_string())
}

fn stdout_error(err: io::Error) -> CliError {
    if err.kind() == io::ErrorKind::BrokenPipe {
        CliError::BrokenPipe
    } else {
        CliError::Unexpected(err.to_string())
    }
}

enum CliError {
    BrokenPipe,
    DebugUsage(String),
    DebugOpenAiUsage(String),
    ShellUsage(String),
    Unexpected(String),
}

enum CliExit {
    Success,
    Usage { message: String, usage: String },
    Clap(clap::Error),
    Unexpected(String),
}

fn report_cli_exit<W: io::Write>(exit: CliExit, stderr: &mut W) -> ExitCode {
    match exit {
        CliExit::Success => ExitCode::SUCCESS,
        CliExit::Usage { message, usage } => {
            writeln!(stderr, "{message}\n\n{usage}").expect("failed to write usage to stderr");
            ExitCode::from(2)
        }
        CliExit::Clap(error) => {
            let exit_code = error.exit_code();
            error.print().expect("failed to write clap output");
            ExitCode::from(exit_code as u8)
        }
        CliExit::Unexpected(message) => {
            writeln!(stderr, "{message}").expect("failed to write error to stderr");
            ExitCode::FAILURE
        }
    }
}

impl Termination for CliExit {
    fn report(self) -> ExitCode {
        report_cli_exit(self, &mut io::stderr())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Cli, CliCommand, CliError, CliExit, DEBUG_TOOL_CONTINUATION_INPUT, DEFAULT_INPUT,
        DEFAULT_SESSION_ID, DebugCommand, MERRY_SANDBOX_ENV, MERRY_SANDBOX_VERSION,
        MERRY_SANDBOX_VERSION_ENV, SANDBOX_HOME, SANDBOX_TMPDIR, SandboxBootstrap, SandboxError,
        SandboxHost, args_without_with_sandbox, debug_echo_tool, debug_openai_usage,
        find_bwrap_in_path, os, plan_sandbox_bootstrap_with_file_exists, report_cli_exit,
        shell_process_action_intent, shell_usage, write_debug_openai_tool_events,
    };
    use super::{DEBUG_TOOL_NAME, write_runtime_step_events};
    use clap::Parser;
    use futures_util::stream;
    use merry_core::{ProviderName, RuntimeEvent, ToolCallResultStatus, ToolName};
    use merry_llm::{
        FinishReason, GenerationConfig, ModelCapabilities, ModelError, ModelEvent,
        ModelEventStream, ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest,
        ModelResponse, ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
    };
    use merry_runtime::{
        MAX_PROCESS_OUTPUT_LIMIT_BYTES, ProcessEnvPolicy, Runtime, StepContext, StepInput,
    };
    use serde_json::{Map, Value};
    use std::{
        ffi::OsString,
        path::{Path, PathBuf},
        process::ExitCode,
        sync::{Arc, Mutex},
    };

    fn sandbox_host() -> SandboxHost {
        SandboxHost {
            cwd: PathBuf::from("/workspace/merry"),
            current_exe: PathBuf::from("/workspace/merry/target/debug/merry"),
            args: vec![
                os("--with-sandbox"),
                os("debug"),
                os("--session-id"),
                os("custom-session"),
            ],
            path: Some(os("/custom/bin:/usr/bin")),
            inside_sandbox: false,
        }
    }

    fn path_is_fake_bwrap(path: &Path) -> bool {
        path == Path::new("/custom/bin/bwrap")
    }

    fn plan_sandbox(
        with_sandbox: bool,
        host: &SandboxHost,
    ) -> Result<SandboxBootstrap, SandboxError> {
        plan_sandbox_bootstrap_with_file_exists(with_sandbox, host, path_is_fake_bwrap)
    }

    fn plan_args(plan: &super::SandboxPlan) -> Vec<String> {
        plan.args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn clap_parses_debug_defaults() {
        let cli = Cli::try_parse_from(["merry", "debug"]).expect("debug args should parse");

        match cli.command {
            CliCommand::Debug(debug) => {
                assert!(!cli.with_sandbox);
                assert_eq!(debug.session_id, DEFAULT_SESSION_ID);
                assert_eq!(debug.input, DEFAULT_INPUT);
                assert!(debug.command.is_none());
            }
            CliCommand::Shell(_) => panic!("expected debug subcommand"),
        }
    }

    #[test]
    fn clap_parses_debug_openai_options() {
        let cli = Cli::try_parse_from([
            "merry",
            "debug",
            "openai",
            "--input",
            "hello",
            "--model",
            "gpt-test",
            "--max-output-tokens",
            "16",
            "--debug-tool-result",
            "tool result",
        ])
        .expect("debug openai args should parse");

        match cli.command {
            CliCommand::Debug(debug) => match debug.command {
                Some(DebugCommand::OpenAi(openai)) => {
                    assert_eq!(openai.input, "hello");
                    assert_eq!(openai.model.as_deref(), Some("gpt-test"));
                    assert_eq!(openai.max_output_tokens, Some(16));
                    assert_eq!(openai.debug_tool_result.as_deref(), Some("tool result"));
                }
                None => panic!("expected debug openai subcommand"),
            },
            CliCommand::Shell(_) => panic!("expected debug subcommand"),
        }
    }

    #[test]
    fn clap_parses_shell_argv() {
        let cli = Cli::try_parse_from(["merry", "shell", "--", "rustc", "--version"])
            .expect("shell args should parse");

        match cli.command {
            CliCommand::Shell(shell) => {
                assert_eq!(shell.argv, ["rustc", "--version"]);
            }
            CliCommand::Debug(_) => panic!("expected shell subcommand"),
        }
    }

    #[test]
    fn clap_rejects_shell_argv_without_separator() {
        let error = Cli::try_parse_from(["merry", "shell", "rustc", "--version"])
            .expect_err("shell argv should require `--` separator");

        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn shell_usage_contains_shell_usage() {
        assert!(shell_usage().contains("Usage: merry shell -- <ARGV>..."));
    }

    #[test]
    fn shell_process_action_intent_uses_exact_cli_argv_and_empty_env() {
        let intent =
            match shell_process_action_intent(vec!["rustc".to_owned(), "--version".to_owned()]) {
                Ok(intent) => intent,
                Err(_) => panic!("shell process intent should be valid"),
            };

        assert_eq!(intent.argv(), ["rustc", "--version"]);
        assert_eq!(intent.cwd(), Some("."));
        assert_eq!(intent.env_policy(), ProcessEnvPolicy::empty());
        assert!(intent.stdin_text().is_none());
        assert_eq!(intent.stdout_limit_bytes(), MAX_PROCESS_OUTPUT_LIMIT_BYTES);
        assert_eq!(intent.stderr_limit_bytes(), MAX_PROCESS_OUTPUT_LIMIT_BYTES);
    }

    #[test]
    fn clap_parses_root_with_sandbox_flag() {
        let cli =
            Cli::try_parse_from(["merry", "--with-sandbox", "debug"]).expect("args should parse");

        assert!(cli.with_sandbox);
    }

    #[test]
    fn sandbox_planning_skips_when_disabled() {
        let host = sandbox_host();

        let bootstrap =
            plan_sandbox(false, &host).expect("disabled sandbox planning should succeed");

        assert_eq!(bootstrap, SandboxBootstrap::Disabled);
    }

    #[test]
    fn sandbox_planning_skips_when_already_inside() {
        let mut host = sandbox_host();
        host.inside_sandbox = true;

        let bootstrap =
            plan_sandbox(true, &host).expect("already-inside sandbox planning should succeed");

        assert_eq!(bootstrap, SandboxBootstrap::AlreadyInside);
    }

    #[test]
    fn sandbox_plan_uses_bwrap_and_required_namespace_args() {
        let host = sandbox_host();
        let bootstrap = plan_sandbox(true, &host).expect("sandbox planning should succeed");
        let SandboxBootstrap::Reexec(plan) = bootstrap else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        assert_eq!(plan.program, OsString::from("/custom/bin/bwrap"));
        for expected in [
            "--unshare-user",
            "--unshare-ipc",
            "--unshare-pid",
            "--unshare-uts",
            "--unshare-cgroup-try",
            "--disable-userns",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ] {
            assert!(args.iter().any(|arg| arg == expected), "missing {expected}");
        }
        assert!(!args.iter().any(|arg| arg == "--unshare-net"));
    }

    #[test]
    fn sandbox_plan_mounts_runtime_paths_and_workspace() {
        let host = sandbox_host();
        let SandboxBootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        assert!(contains_sequence(&args, &["--proc", "/proc"]));
        assert!(contains_sequence(&args, &["--dev", "/dev"]));
        assert!(contains_sequence(
            &args,
            &["--perms", "01777", "--tmpfs", SANDBOX_TMPDIR]
        ));
        assert!(contains_sequence(&args, &["--tmpfs", "/home"]));
        assert!(contains_sequence(
            &args,
            &["--perms", "0700", "--dir", SANDBOX_HOME]
        ));
        assert!(contains_sequence(&args, &["--ro-bind", "/usr", "/usr"]));
        assert!(contains_sequence(&args, &["--ro-bind-try", "/bin", "/bin"]));
        assert!(contains_sequence(&args, &["--ro-bind-try", "/lib", "/lib"]));
        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/lib64", "/lib64"]
        ));
        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/etc/ld.so.cache", "/etc/ld.so.cache"]
        ));
        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/etc/resolv.conf", "/etc/resolv.conf"]
        ));
        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/etc/hosts", "/etc/hosts"]
        ));
        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/etc/nsswitch.conf", "/etc/nsswitch.conf"]
        ));
        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/etc/ssl", "/etc/ssl"]
        ));
        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/etc/pki", "/etc/pki"]
        ));
        assert!(!contains_sequence(
            &args,
            &["--ro-bind-try", "/etc", "/etc"]
        ));
        assert!(contains_sequence(
            &args,
            &["--bind", "/workspace/merry", "/workspace/merry"]
        ));
        assert!(contains_sequence(&args, &["--chdir", "/workspace/merry"]));
    }

    #[test]
    fn sandbox_plan_clears_environment_and_allowlists_path_only_for_bwrap() {
        let host = sandbox_host();
        let SandboxBootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        assert_eq!(plan.env, vec![(os("PATH"), os("/custom/bin:/usr/bin"))]);
        assert!(contains_sequence(
            &args,
            &["--setenv", "PATH", "/custom/bin:/usr/bin"]
        ));
        assert!(contains_sequence(
            &args,
            &["--setenv", "HOME", SANDBOX_HOME]
        ));
        assert!(contains_sequence(
            &args,
            &["--setenv", "TMPDIR", SANDBOX_TMPDIR]
        ));
        assert!(contains_sequence(
            &args,
            &["--setenv", "PWD", "/workspace/merry"]
        ));
        assert!(contains_sequence(
            &args,
            &["--setenv", MERRY_SANDBOX_ENV, "1"]
        ));
        assert!(contains_sequence(
            &args,
            &["--setenv", MERRY_SANDBOX_VERSION_ENV, MERRY_SANDBOX_VERSION]
        ));
        assert!(!args.iter().any(|arg| arg.contains("OPENAI_API_KEY")));
        assert!(!args.iter().any(|arg| arg.contains("MERRY_OPENAI_API_KEY")));
    }

    #[test]
    fn sandbox_plan_reexecs_current_exe_with_sandbox_flag_removed() {
        let host = sandbox_host();
        let SandboxBootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        let exe_index = args
            .iter()
            .position(|arg| arg == "/workspace/merry/target/debug/merry")
            .expect("current executable should be present");
        assert_eq!(
            &args[exe_index + 1..],
            ["debug", "--session-id", "custom-session"]
        );
    }

    #[test]
    fn find_bwrap_in_path_returns_first_existing_candidate() {
        let path = os("/missing/bin:/custom/bin:/later/bin");

        let found = find_bwrap_in_path(&path, |candidate| {
            candidate == Path::new("/custom/bin/bwrap")
                || candidate == Path::new("/later/bin/bwrap")
        });

        assert_eq!(found, Some(PathBuf::from("/custom/bin/bwrap")));
    }

    #[test]
    fn sandbox_planning_errors_when_bwrap_is_missing_from_path() {
        let host = sandbox_host();

        let error = plan_sandbox_bootstrap_with_file_exists(true, &host, |_| false)
            .expect_err("missing bwrap should fail during planning");

        assert!(matches!(error, SandboxError::MissingBubblewrap));
        assert_eq!(
            error.to_string(),
            "bubblewrap executable `bwrap` was not found in PATH; install bubblewrap or run without --with-sandbox"
        );
    }

    #[test]
    fn args_without_with_sandbox_removes_only_first_marker() {
        let args = vec![
            os("--with-sandbox"),
            os("debug"),
            os("--input"),
            os("--with-sandbox"),
        ];

        assert_eq!(
            args_without_with_sandbox(&args),
            vec![os("debug"), os("--input"), os("--with-sandbox")]
        );
    }

    fn contains_sequence(args: &[String], expected: &[&str]) -> bool {
        args.windows(expected.len()).any(|window| {
            window
                .iter()
                .map(String::as_str)
                .eq(expected.iter().copied())
        })
    }

    #[test]
    fn cli_exit_unexpected_reports_failure_without_usage() {
        let mut stderr = Vec::new();

        let exit_code = report_cli_exit(
            CliExit::Unexpected(
                "debug tool `debug_echo` was not called on the first step".to_owned(),
            ),
            &mut stderr,
        );

        assert_eq!(exit_code, ExitCode::FAILURE);
        let stderr = String::from_utf8(stderr).expect("stderr should be utf-8");
        assert_eq!(
            stderr,
            "debug tool `debug_echo` was not called on the first step\n"
        );
        assert!(!stderr.contains("Usage: merry debug openai"));
    }

    #[test]
    fn cli_exit_usage_reports_exit_two_and_usage() {
        let mut stderr = Vec::new();

        let exit_code = report_cli_exit(
            CliExit::Usage {
                message: "--input requires a value".to_owned(),
                usage: debug_openai_usage(),
            },
            &mut stderr,
        );

        assert_eq!(exit_code, ExitCode::from(2));
        let stderr = String::from_utf8(stderr).expect("stderr should be utf-8");
        assert!(stderr.starts_with("--input requires a value\n\n"));
        assert!(stderr.contains("Usage: merry debug openai"));
        assert!(stderr.contains("MERRY_OPENAI_DEBUG=1"));
    }

    struct CompletingProvider {
        name: ProviderName,
        capabilities: ModelCapabilities,
    }

    impl CompletingProvider {
        fn new() -> Self {
            Self {
                name: ProviderName::new("debug-test-provider").expect("valid provider name"),
                capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                    .expect("valid capabilities"),
            }
        }
    }

    impl ModelProvider for CompletingProvider {
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
            Box::pin(async {
                let response = ModelResponse::new(
                    vec![ModelOutput::text("hidden from runtime events")],
                    FinishReason::Stop,
                    None,
                );
                let events = vec![
                    Ok(ModelEvent::Started),
                    Ok(ModelEvent::OutputTextDelta {
                        delta: "hidden".to_owned(),
                    }),
                    Ok(ModelEvent::Completed { response }),
                ];
                Ok(Box::pin(stream::iter(events)) as ModelEventStream)
            })
        }
    }

    struct RecordingProvider {
        inner: CompletingProvider,
        requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    impl RecordingProvider {
        fn new() -> Self {
            Self {
                inner: CompletingProvider::new(),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl ModelProvider for RecordingProvider {
        fn name(&self) -> &ProviderName {
            self.inner.name()
        }

        fn capabilities(&self) -> &ModelCapabilities {
            self.inner.capabilities()
        }

        fn stream_model<'a>(
            &'a self,
            request: ModelRequest,
            context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            self.requests
                .lock()
                .expect("request mutex should not be poisoned")
                .push(request.clone());
            self.inner.stream_model(request, context)
        }
    }

    type ScriptedStep = Vec<Result<ModelEvent, ModelError>>;

    #[derive(Debug, Clone)]
    struct ScriptedProvider {
        name: ProviderName,
        capabilities: ModelCapabilities,
        steps: Arc<Mutex<Vec<ScriptedStep>>>,
        requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    impl ScriptedProvider {
        fn new(scripts: Vec<ScriptedStep>) -> Self {
            Self {
                name: ProviderName::new("debug-scripted-provider").expect("valid provider name"),
                capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                    .expect("valid capabilities"),
                steps: Arc::new(Mutex::new(scripts.into_iter().rev().collect())),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn recorded_requests(&self) -> Vec<ModelRequest> {
            self.requests
                .lock()
                .expect("request mutex should not be poisoned")
                .clone()
        }
    }

    impl ModelProvider for ScriptedProvider {
        fn name(&self) -> &ProviderName {
            &self.name
        }

        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn stream_model<'a>(
            &'a self,
            request: ModelRequest,
            _context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            Box::pin(async move {
                self.requests
                    .lock()
                    .expect("request mutex should not be poisoned")
                    .push(request);

                let script = self
                    .steps
                    .lock()
                    .expect("step mutex should not be poisoned")
                    .pop()
                    .unwrap_or_default();

                Ok(Box::pin(stream::iter(script)) as ModelEventStream)
            })
        }
    }

    #[tokio::test]
    async fn debug_openai_runtime_helper_writes_runtime_lifecycle_jsonl_without_model_events() {
        let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-test").unwrap())
            .model_provider(
                Arc::new(CompletingProvider::new()),
                ModelName::new("debug-model").unwrap(),
            )
            .build()
            .expect("runtime should build");
        let input = StepInput::user_text("hello").expect("valid input");
        let mut output = Vec::new();

        write_runtime_step_events(&runtime, input, StepContext::default(), &mut output)
            .await
            .unwrap_or_else(|_| panic!("runtime events should write"));

        let text = String::from_utf8(output).expect("output should be utf-8");
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 4);
        let event_types = lines
            .iter()
            .map(|line| {
                let value = serde_json::from_str::<Value>(line).expect("line should be JSON");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            event_types,
            [
                "session_started",
                "step_started",
                "artifact_recorded",
                "step_completed"
            ]
        );
        assert!(!text.contains("hidden"));
    }

    #[tokio::test]
    async fn debug_openai_runtime_helper_passes_generation_config_to_runtime_step() {
        let provider = RecordingProvider::new();
        let requests = Arc::clone(&provider.requests);
        let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-config").unwrap())
            .model_provider(Arc::new(provider), ModelName::new("debug-model").unwrap())
            .build()
            .expect("runtime should build");
        let input = StepInput::user_text("hello").expect("valid input");
        let context = StepContext::default().with_generation_config(
            GenerationConfig::new(Some(16), false).expect("valid generation config"),
        );
        let mut output = Vec::new();

        write_runtime_step_events(&runtime, input, context, &mut output)
            .await
            .unwrap_or_else(|_| panic!("runtime events should write"));

        let requests = requests
            .lock()
            .expect("request mutex should not be poisoned")
            .clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].generation().max_output_tokens(), Some(16));
        assert!(!requests[0].generation().allow_parallel_tool_calls());
    }

    #[tokio::test]
    async fn debug_openai_tool_helper_executes_one_pending_call_and_continues() {
        let call = ModelToolCall::new(
            ModelToolCallId::new("call-debug").expect("valid tool call id"),
            ToolName::new(DEBUG_TOOL_NAME).expect("valid tool name"),
            ToolArguments::new(Map::new()),
        );
        let provider = ScriptedProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(call)],
                    FinishReason::ToolCalls,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("continued after tool")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-tool").unwrap())
            .register_tool(
                debug_echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")),
            )
            .model_provider(
                Arc::new(provider.clone()),
                ModelName::new("debug-model").unwrap(),
            )
            .build()
            .expect("runtime should build");
        let input = StepInput::user_text("please call the tool").expect("valid input");
        let context = StepContext::default().with_generation_config(
            GenerationConfig::new(Some(16), false).expect("valid generation config"),
        );
        let mut output = Vec::new();

        write_debug_openai_tool_events(&runtime, input, context, &mut output)
            .await
            .unwrap_or_else(|_| panic!("tool events should write"));

        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = text
            .lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect::<Vec<_>>();
        let event_types = events
            .iter()
            .map(|event| {
                let value = serde_json::to_value(event).expect("event should serialize");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            event_types,
            [
                "session_started",
                "step_started",
                "tool_call_pending",
                "artifact_recorded",
                "tool_call_resolved",
                "step_started",
                "artifact_recorded",
                "step_completed",
            ]
        );

        let resolved = events
            .iter()
            .find_map(|event| match &event.kind {
                merry_core::RuntimeEventKind::ToolCallResolved { result } => Some(result),
                _ => None,
            })
            .expect("tool should be resolved");
        assert_eq!(resolved.status(), ToolCallResultStatus::Succeeded);

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].tools().len(), 1);
        assert_eq!(requests[0].tools()[0].name().as_str(), DEBUG_TOOL_NAME);
        assert!(requests[0].continuations().is_empty());
        assert_eq!(requests[0].generation().max_output_tokens(), Some(16));
        assert!(!requests[0].generation().allow_parallel_tool_calls());

        assert_eq!(requests[1].tools().len(), 1);
        assert_eq!(requests[1].tools()[0].name().as_str(), DEBUG_TOOL_NAME);
        assert_eq!(requests[1].continuations().len(), 1);
        let continuation = &requests[1].continuations()[0];
        assert_eq!(continuation.call().id().as_str(), "call-debug");
        assert_eq!(
            continuation.result().status(),
            ToolCallResultStatus::Succeeded
        );
        assert_eq!(
            continuation.result().content().as_text(),
            Some("debug result")
        );
        assert!(
            requests[1]
                .messages()
                .iter()
                .any(|message| message.content().as_text() == DEBUG_TOOL_CONTINUATION_INPUT)
        );
        assert_eq!(requests[1].generation().max_output_tokens(), Some(16));
        assert!(!requests[1].generation().allow_parallel_tool_calls());
    }

    #[tokio::test]
    async fn debug_openai_tool_helper_errors_when_first_step_calls_wrong_tool() {
        let wrong_tool_name = "wrong_tool";
        let call = ModelToolCall::new(
            ModelToolCallId::new("call-wrong").expect("valid tool call id"),
            ToolName::new(wrong_tool_name).expect("valid tool name"),
            ToolArguments::new(Map::new()),
        );
        let provider = ScriptedProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(call)],
                    FinishReason::ToolCalls,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("should not continue")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime =
            Runtime::builder(merry_core::SessionId::new("debug-openai-wrong-tool").unwrap())
                .register_tool(
                    debug_echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")),
                )
                .model_provider(
                    Arc::new(provider.clone()),
                    ModelName::new("debug-model").unwrap(),
                )
                .build()
                .expect("runtime should build");
        let input = StepInput::user_text("please call the tool").expect("valid input");
        let mut output = Vec::new();

        let error =
            write_debug_openai_tool_events(&runtime, input, StepContext::default(), &mut output)
                .await
                .expect_err("wrong first-step tool call should fail");

        match error {
            CliError::Unexpected(message) => {
                assert!(message.contains(DEBUG_TOOL_NAME));
                assert!(message.contains(wrong_tool_name));
            }
            _ => panic!("expected unexpected error for wrong tool call"),
        }

        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = text
            .lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect::<Vec<_>>();
        assert!(
            !events.is_empty(),
            "first-step runtime events should be preserved"
        );
        let event_types = events
            .iter()
            .map(|event| {
                let value = serde_json::to_value(event).expect("event should serialize");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert!(event_types.iter().any(|kind| kind == "tool_call_pending"));
        assert!(!event_types.iter().any(|kind| kind == "tool_call_resolved"));
        let pending = events
            .iter()
            .find_map(|event| match &event.kind {
                merry_core::RuntimeEventKind::ToolCallPending { call } => Some(call),
                _ => None,
            })
            .expect("wrong tool call should remain pending in first-step events");
        assert_eq!(pending.name().as_str(), wrong_tool_name);

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].continuations().is_empty());
    }

    #[tokio::test]
    async fn debug_openai_tool_helper_errors_when_first_step_does_not_call_debug_echo() {
        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("completed without tool")],
                FinishReason::Stop,
                None,
            ),
        })]]);
        let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-no-tool").unwrap())
            .register_tool(
                debug_echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")),
            )
            .model_provider(
                Arc::new(provider.clone()),
                ModelName::new("debug-model").unwrap(),
            )
            .build()
            .expect("runtime should build");
        let input = StepInput::user_text("do not call the tool").expect("valid input");
        let mut output = Vec::new();

        let error =
            write_debug_openai_tool_events(&runtime, input, StepContext::default(), &mut output)
                .await
                .expect_err("missing first-step tool call should fail");

        match error {
            CliError::Unexpected(message) => {
                assert!(message.contains(DEBUG_TOOL_NAME));
                assert!(message.contains("no tool call was pending"));
            }
            _ => panic!("expected unexpected error for missing tool call"),
        }

        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = text
            .lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect::<Vec<_>>();
        assert!(
            !events.is_empty(),
            "first-step runtime events should be preserved"
        );
        let event_types = events
            .iter()
            .map(|event| {
                let value = serde_json::to_value(event).expect("event should serialize");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            event_types,
            [
                "session_started",
                "step_started",
                "artifact_recorded",
                "step_completed",
            ]
        );
        assert!(!event_types.iter().any(|kind| kind == "tool_call_pending"));
        assert!(!event_types.iter().any(|kind| kind == "tool_call_resolved"));

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].continuations().is_empty());
    }
}
