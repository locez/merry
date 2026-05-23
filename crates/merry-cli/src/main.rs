//! Debug and demonstration CLI for Merry.

mod config;
mod observability;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use config::{EffectiveLogSettings, MerryConfig, XdgPaths};
use futures_util::{StreamExt, stream};
use merry_core::{
    PendingToolCall, ProviderName, RuntimeEvent, RuntimeEventKind, SessionId, ToolCallResultStatus,
    ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelCapabilities, ModelError, ModelEvent, ModelEventStream,
    ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AgentLoopStatus, ArtifactContent,
    MAX_PROCESS_OUTPUT_LIMIT_BYTES, ProcessActionIntent, ProcessEnvPolicy, ProcessExitStatus,
    ProcessRunner, ProcessRunnerContext, ProcessRunnerError, ProcessRunnerFuture,
    ProcessRunnerOutput, RegisteredTool, Runtime, StepContext, StepInput, ToolExecutionContext,
    ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture, process_command_tool,
};
use merry_tool_workspace::{
    ReadOnlyWorkspaceTools, WORKSPACE_PATCH_FILE_TOOL, WORKSPACE_READ_FILE_TOOL,
    WorkspaceToolsConfig,
};
use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fmt, fs, io,
    path::{Path, PathBuf},
    process::{ExitCode, Stdio, Termination},
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufWriter};

const DEFAULT_SESSION_ID: &str = "debug-session";
const DEFAULT_INPUT: &str = "debug step";
const DEBUG_TOOL_NAME: &str = "debug_echo";
const DEBUG_TOOL_CONTINUATION_INPUT: &str = "continue after debug tool";
const CODING_LOOP_SMOKE_SESSION_ID: &str = "coding-loop-smoke";
const CODING_LOOP_SMOKE_TOOL_NAME: &str = "run_process";
const CODING_LOOP_LIVE_SMOKE_SESSION_ID: &str = "coding-loop-live-smoke";
const CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE: &str = "unfixed";
const CODING_LOOP_LIVE_SMOKE_TARGET_VALUE: &str = "fixed-by-live-llm";
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
const SANDBOX_CHILD_HANDOFF_ARG: &str = "--merry-sandbox-child-handoff";
const SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1: &str = "cli-bwrap-v1";
const SANDBOX_HOME: &str = "/home/merry";
const SANDBOX_TMPDIR: &str = "/tmp";
// These are sandbox-child paths. Host paths are resolved separately before
// re-exec; inside bwrap, HOME is intentionally set to SANDBOX_HOME.
const SANDBOX_XDG_CONFIG_HOME: &str = "/home/merry/.config";
const SANDBOX_XDG_STATE_HOME: &str = "/home/merry/.local/state";
const SANDBOX_MERRY_CONFIG_DIR: &str = "/home/merry/.config/merry";
const SANDBOX_MERRY_LOG_DIR: &str = "/home/merry/.local/state/merry/logs";
const OPENAI_ENV_HELP: &str = "\
Environment:
  MERRY_OPENAI_DEBUG=1       Required opt-in before any network attempt
  XDG_CONFIG_HOME            Optional base for merry/config.toml

Provider/model/base URL/API key source come from
`$XDG_CONFIG_HOME/merry/config.toml` or `~/.config/merry/config.toml`.
For sandboxed live smokes, prefer a config-relative `api_key_file` such as
`secrets/openai.key` so credentials are not passed through bwrap argv.
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

    #[arg(
        long = "merry-sandbox-child-handoff",
        hide = true,
        value_enum,
        value_name = "PROFILE"
    )]
    sandbox_child_handoff: Option<SandboxChildHandoff>,

    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SandboxChildHandoff {
    #[value(name = "cli-bwrap-v1")]
    CliBwrapV1,
}

impl SandboxChildHandoff {
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::CliBwrapV1 => SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxRuntimeProfile {
    CliBwrapV1,
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
        long = "accept-local-workspace-process-risk",
        help = "Accept local workspace process risk when running inside Merry's sandbox handoff"
    )]
    accept_local_workspace_process_risk: bool,

    #[arg(
        long = "events-jsonl",
        help = "Print runtime lifecycle events as JSONL instead of command stdout/stderr"
    )]
    events_jsonl: bool,

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
    #[command(
        name = "coding-loop-smoke",
        about = "Run an opt-in sandboxed coding-loop smoke with deterministic model steps"
    )]
    CodingLoopSmoke,
    #[command(
        name = "coding-loop-live-smoke",
        about = "Run an opt-in sandboxed coding-loop smoke driven by a live OpenAI-compatible model"
    )]
    CodingLoopLiveSmoke(DebugCodingLoopLiveSmokeArgs),
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
        help = "Model name; overrides [providers.default].model"
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

#[derive(Debug, Args)]
struct DebugCodingLoopLiveSmokeArgs {
    #[arg(
        long,
        value_name = "MODEL",
        allow_hyphen_values = true,
        help = "Model name; overrides [providers.default].model"
    )]
    model: Option<String>,

    #[arg(
        long,
        value_name = "N",
        value_parser = parse_max_output_tokens,
        default_value_t = 512,
        help = "Maximum output tokens per live model step"
    )]
    max_output_tokens: u64,
}

fn main() -> CliExit {
    let argv = env::args_os().collect::<Vec<_>>();
    let cli = match Cli::try_parse_from(argv.clone()) {
        Ok(cli) => cli,
        Err(error) => return CliExit::Clap(error),
    };

    let config_paths = match XdgPaths::from_env() {
        Ok(paths) => paths,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };
    let _config = match MerryConfig::load_optional(&config_paths) {
        Ok(config) => config,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };
    if let Err(error) = validate_loaded_config(_config.as_ref(), &config_paths) {
        return CliExit::Unexpected(error.to_string());
    }
    let log_settings = match effective_log_settings(_config.as_ref(), &config_paths) {
        Ok(settings) => settings,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };

    if let Err(error) = maybe_reexec_sandbox(&cli, argv.iter().skip(1).cloned().collect()) {
        return CliExit::Unexpected(error.to_string());
    }

    let _observability_guard = match observability::init_observability(log_settings.as_ref()) {
        Ok(guard) => guard,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return CliExit::Unexpected(err.to_string()),
    };

    runtime.block_on(async_main(cli, _config))
}

async fn async_main(cli: Cli, merry_config: Option<MerryConfig>) -> CliExit {
    let sandbox_child_handoff = cli.sandbox_child_handoff;

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
            merry_config.as_ref(),
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
        CliCommand::Debug(DebugArgs {
            command: Some(DebugCommand::CodingLoopSmoke),
            ..
        }) => match run_debug_coding_loop_smoke(sandbox_child_handoff).await {
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
                Some(DebugCommand::CodingLoopLiveSmoke(DebugCodingLoopLiveSmokeArgs {
                    model,
                    max_output_tokens,
                })),
            ..
        }) => match run_debug_coding_loop_live_smoke(
            sandbox_child_handoff,
            model.as_deref(),
            max_output_tokens,
            merry_config.as_ref(),
        )
        .await
        {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: debug_coding_loop_live_smoke_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Shell(args) => match run_shell(args, sandbox_child_handoff).await {
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

fn validate_loaded_config(
    config: Option<&MerryConfig>,
    paths: &XdgPaths,
) -> Result<(), config::ConfigError> {
    let _ = paths.state_dir();
    let Some(config) = config else {
        return Ok(());
    };
    let _ = effective_log_settings(Some(config), paths)?;
    let _ = config.profile();
    config.validate_provider_settings_if_present()?;
    Ok(())
}

fn effective_log_settings(
    config: Option<&MerryConfig>,
    paths: &XdgPaths,
) -> Result<Option<EffectiveLogSettings>, config::ConfigError> {
    config
        .map(|config| config.effective_log_settings(paths))
        .transpose()
        .map(Option::flatten)
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
    xdg_paths: XdgPaths,
    log_settings: Option<EffectiveLogSettings>,
}

impl SandboxHost {
    fn from_env(args: Vec<OsString>) -> Result<Self, SandboxError> {
        let xdg_paths = XdgPaths::from_env().map_err(SandboxError::Config)?;
        let merry_config = MerryConfig::load_optional(&xdg_paths).map_err(SandboxError::Config)?;
        let log_settings = effective_log_settings(merry_config.as_ref(), &xdg_paths)
            .map_err(SandboxError::Config)?;
        Ok(Self {
            cwd: env::current_dir().map_err(SandboxError::CurrentDir)?,
            current_exe: env::current_exe().map_err(SandboxError::CurrentExe)?,
            args,
            path: env::var_os("PATH"),
            // This marker is only a recursion guard for self-reexec. It is
            // not a security proof that the current process is confined.
            inside_sandbox: env::var_os(MERRY_SANDBOX_ENV).as_deref() == Some(OsStr::new("1")),
            xdg_paths,
            log_settings,
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
    ensure_host_log_directory(host)?;

    Ok(SandboxBootstrap::Reexec(build_sandbox_plan(
        host, path, bwrap,
    )))
}

fn ensure_host_log_directory(host: &SandboxHost) -> Result<(), SandboxError> {
    let Some(log_settings) = host.log_settings.as_ref() else {
        return Ok(());
    };
    let Some(log_dir) = log_settings.path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(log_dir).map_err(|source| SandboxError::LogDirectory {
        path: log_dir.to_path_buf(),
        source,
    })
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
        os("--ro-bind-try"),
        host.xdg_paths.config_dir().as_os_str().to_owned(),
        os(SANDBOX_MERRY_CONFIG_DIR),
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
    ]);
    if let Some(log_settings) = host.log_settings.as_ref()
        && let Some(host_log_dir) = log_settings.path.parent()
    {
        args.extend([
            os("--bind"),
            host_log_dir.as_os_str().to_owned(),
            os(SANDBOX_MERRY_LOG_DIR),
        ]);
    }
    args.extend([
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
        os("XDG_CONFIG_HOME"),
        os(SANDBOX_XDG_CONFIG_HOME),
        os("--setenv"),
        os("XDG_STATE_HOME"),
        os(SANDBOX_XDG_STATE_HOME),
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
        os(SANDBOX_CHILD_HANDOFF_ARG),
        os(SandboxChildHandoff::CliBwrapV1.as_cli_value()),
    ]);
    args.extend(args_without_sandbox_bootstrap_flags(&host.args));

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

fn args_without_sandbox_bootstrap_flags(args: &[OsString]) -> Vec<OsString> {
    let mut removed = false;
    let mut sanitized = Vec::with_capacity(args.len());
    let mut index = 0;
    let mut scanning_root_flags = true;

    while index < args.len() {
        let arg = &args[index];

        if scanning_root_flags {
            if !removed && arg == OsStr::new("--with-sandbox") {
                removed = true;
                index += 1;
                continue;
            }

            if arg == OsStr::new(SANDBOX_CHILD_HANDOFF_ARG) {
                index += 1;
                if index < args.len() {
                    index += 1;
                }
                continue;
            }

            if is_sandbox_child_handoff_assignment(arg) {
                index += 1;
                continue;
            }

            scanning_root_flags = false;
        }

        sanitized.push(arg.clone());
        index += 1;
    }

    sanitized
}

fn is_sandbox_child_handoff_assignment(arg: &OsStr) -> bool {
    arg.to_str().is_some_and(|value| {
        value
            .strip_prefix(SANDBOX_CHILD_HANDOFF_ARG)
            .is_some_and(|suffix| suffix.starts_with('='))
    })
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
    Config(config::ConfigError),
    LogDirectory {
        path: PathBuf,
        source: io::Error,
    },
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
            SandboxError::Config(error) => write!(
                formatter,
                "failed to load Merry config before sandbox bootstrap: {error}"
            ),
            SandboxError::LogDirectory { path, source } => write!(
                formatter,
                "failed to create host log directory {} before sandbox bootstrap: {source}",
                path.display()
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
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let config = debug_openai_config(model, merry_config)?;

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

async fn run_debug_coding_loop_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-smoke",
        ));
    };

    let smoke_root = prepare_coding_loop_smoke_fixture("coding-loop-smoke")?;
    let relative_cwd = smoke_root_relative_cwd(&smoke_root)?;
    let runtime = build_coding_loop_smoke_runtime(
        &smoke_root,
        relative_cwd.as_deref(),
        admission,
        Arc::new(TokioProcessRunner),
    )?;

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Run the sandboxed coding-loop smoke.").map_err(unexpected)?,
            StepContext::default(),
            AgentLoopConfig::new(8).map_err(unexpected)?,
        )
        .await
        .map_err(unexpected)?;

    assert_coding_loop_smoke_result(&runtime, &result, &smoke_root).await?;

    let mut writer = BufWriter::new(tokio::io::stdout());
    writer
        .write_all(b"coding-loop-smoke: ok\n")
        .await
        .map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

async fn run_debug_coding_loop_live_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    model_flag: Option<&str>,
    max_output_tokens: u64,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-live-smoke",
        ));
    };
    let config = debug_openai_config(model_flag, merry_config)?;
    let smoke_root = prepare_coding_loop_smoke_fixture("coding-loop-live-smoke")?;
    let relative_cwd = smoke_root_relative_cwd(&smoke_root)?;
    let runtime = build_coding_loop_live_smoke_runtime(
        &smoke_root,
        relative_cwd.as_deref(),
        admission,
        config,
        Arc::new(TokioProcessRunner),
    )?;
    let generation_config =
        GenerationConfig::new(Some(max_output_tokens), false).map_err(debug_openai_usage_error)?;
    let context = StepContext::default().with_generation_config(generation_config);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&coding_loop_live_smoke_task(relative_cwd.as_deref()))
                .map_err(unexpected)?,
            context,
            AgentLoopConfig::new(10).map_err(unexpected)?,
        )
        .await
        .map_err(unexpected)?;

    assert_coding_loop_smoke_result(&runtime, &result, &smoke_root).await?;
    assert_coding_loop_live_smoke_tool_sequence(&runtime, result.events()).await?;

    let mut writer = BufWriter::new(tokio::io::stdout());
    writer
        .write_all(b"coding-loop-live-smoke: ok\n")
        .await
        .map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

async fn coding_loop_smoke_admission_from_current_process(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
) -> Option<AcceptedLocalWorkspaceProcessAdmission> {
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
    coding_loop_smoke_admission(
        sandbox_child_handoff,
        sandbox_runtime_profile,
        sandbox_marker.as_deref(),
        sandbox_version.as_deref(),
    )
}

fn coding_loop_smoke_requires_sandbox_error(command: &str) -> CliError {
    CliError::DebugUsage(format!(
        "{command} must run via `merry --with-sandbox debug {command}`"
    ))
}

fn smoke_root_relative_cwd(smoke_root: &Path) -> Result<Option<String>, CliError> {
    let relative_cwd = smoke_root
        .strip_prefix(env::current_dir().map_err(unexpected)?)
        .map_err(|_| {
            unexpected("coding-loop-smoke fixture must live under the current workspace")
        })?;
    path_to_process_cwd(relative_cwd)
}

async fn assert_coding_loop_smoke_result(
    runtime: &Runtime,
    result: &merry_runtime::AgentLoopResult,
    smoke_root: &Path,
) -> Result<(), CliError> {
    if result.status() != &AgentLoopStatus::Completed {
        return Err(CliError::Unexpected(format!(
            "coding-loop-smoke did not complete: {:?}",
            result.status()
        )));
    }
    if !runtime.pending_tool_calls().await.is_empty() {
        return Err(CliError::Unexpected(
            "coding-loop-smoke left pending tool calls".to_owned(),
        ));
    }
    assert_coding_loop_smoke_tool_results(result.events())?;

    let patched = fs::read_to_string(smoke_root.join("src/lib.rs")).map_err(unexpected)?;
    if patched != coding_loop_smoke_patched_source() {
        return Err(CliError::Unexpected(
            "coding-loop-smoke fixture was not patched as expected".to_owned(),
        ));
    }
    Ok(())
}

fn assert_coding_loop_smoke_tool_results(events: &[RuntimeEvent]) -> Result<(), CliError> {
    let statuses = events
        .iter()
        .filter_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result.status()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if statuses.len() != 4 {
        return Err(CliError::Unexpected(format!(
            "coding-loop-smoke expected 4 resolved tool calls, saw {}",
            statuses.len()
        )));
    }
    if statuses
        .iter()
        .any(|status| *status != ToolCallResultStatus::Succeeded)
    {
        return Err(CliError::Unexpected(
            "coding-loop-smoke had a failed tool result".to_owned(),
        ));
    }
    Ok(())
}

fn coding_loop_smoke_admission(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    sandbox_runtime_profile: Option<SandboxRuntimeProfile>,
    sandbox: Option<&OsStr>,
    version: Option<&OsStr>,
) -> Option<AcceptedLocalWorkspaceProcessAdmission> {
    shell_runtime_admission(
        true,
        sandbox_child_handoff,
        sandbox_runtime_profile,
        sandbox,
        version,
    )
}

fn prepare_coding_loop_smoke_fixture(name: &str) -> Result<PathBuf, CliError> {
    let root = env::current_dir()
        .map_err(unexpected)?
        .join(".merry")
        .join("local")
        .join(name);
    if root.exists() {
        fs::remove_dir_all(&root).map_err(unexpected)?;
    }
    fs::create_dir_all(root.join("src")).map_err(unexpected)?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"merry-coding-loop-smoke\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .map_err(unexpected)?;
    fs::write(root.join("src/lib.rs"), coding_loop_smoke_initial_source()).map_err(unexpected)?;
    Ok(root)
}

fn coding_loop_smoke_initial_source() -> &'static str {
    "pub fn greeting() -> &'static str {\n    \"unfixed\"\n}\n"
}

fn coding_loop_smoke_patched_source() -> &'static str {
    "pub fn greeting() -> &'static str {\n    \"fixed-by-live-llm\"\n}\n"
}

fn path_to_process_cwd(path: &Path) -> Result<Option<String>, CliError> {
    if path.as_os_str().is_empty() {
        return Ok(None);
    }
    let value = path.to_str().ok_or_else(|| {
        CliError::Unexpected("coding-loop-smoke fixture path must be UTF-8".to_owned())
    })?;
    Ok(Some(value.replace('\\', "/")))
}

fn build_coding_loop_smoke_runtime(
    root: &Path,
    relative_cwd: Option<&str>,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
) -> Result<Runtime, CliError> {
    let provider = CodingLoopSmokeProvider::new(relative_cwd)?;
    build_coding_loop_runtime(
        CODING_LOOP_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new("merry-coding-loop-smoke").map_err(unexpected)?,
        runner,
        false,
    )
}

fn build_coding_loop_live_smoke_runtime(
    root: &Path,
    _relative_cwd: Option<&str>,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: DebugOpenAiConfig,
    runner: Arc<dyn ProcessRunner>,
) -> Result<Runtime, CliError> {
    let provider = OpenAiProvider::new(config.provider);
    build_coding_loop_runtime(
        CODING_LOOP_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new(&config.model).map_err(debug_openai_usage_error)?,
        runner,
        true,
    )
}

fn build_coding_loop_runtime(
    session_id: &str,
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    runner: Arc<dyn ProcessRunner>,
    allow_hidden_workspace_paths: bool,
) -> Result<Runtime, CliError> {
    let workspace_tools = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![root.to_path_buf()])
            .with_allow_hidden(allow_hidden_workspace_paths),
    )
    .map_err(unexpected)?;
    let mut builder = Runtime::builder(SessionId::new(session_id).map_err(unexpected)?)
    .model_provider(provider, model)
    .allow_low_risk_workspace_patches()
    .allow_low_risk_process_actions(Arc::clone(&runner))
    .allow_accepted_local_workspace_process_actions(admission, runner)
    .register_tool(
        process_command_tool(
            ToolName::new(CODING_LOOP_SMOKE_TOOL_NAME).map_err(unexpected)?,
            "Run exact argv through Merry process policy. For this smoke use only `rg --files` and `rg fixed-by-live-llm` with the provided cwd.",
        )
        .map_err(unexpected)?,
    );
    for tool in workspace_tools.into_registered_tools_with_patch() {
        builder = builder.register_tool(tool);
    }
    builder.build().map_err(unexpected)
}

fn coding_loop_live_smoke_task(relative_cwd: Option<&str>) -> String {
    let cwd = relative_cwd.unwrap_or(".");
    format!(
        "\
You are driving Merry's minimal live coding-loop smoke.

Use the available tools, one tool call per step. Do not answer from memory.

Required sequence:
1. Call `{process_tool}` with argv `[\"rg\", \"--files\"]` and cwd `{cwd}` to inspect the fixture.
2. Call `{read_tool}` with path `src/lib.rs` to read exact source.
3. Call `{patch_tool}` with path `src/lib.rs`, old_text `\"{initial}\"`, and new_text `\"{target}\"`.
4. Call `{process_tool}` with argv `[\"rg\", \"{target}\"]` and cwd `{cwd}` to verify.
5. After verification succeeds, return a concise final answer.

Constraints:
- Do not use shell strings, scripts, pipelines, env, stdin, git, cargo, or any command except the two exact rg argv values above.
- Do not modify any file except `src/lib.rs` through `{patch_tool}`.
- The final file must equal:

pub fn greeting() -> &'static str {{
    \"{target}\"
}}
",
        process_tool = CODING_LOOP_SMOKE_TOOL_NAME,
        read_tool = WORKSPACE_READ_FILE_TOOL,
        patch_tool = WORKSPACE_PATCH_FILE_TOOL,
        initial = CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE,
        target = CODING_LOOP_LIVE_SMOKE_TARGET_VALUE,
    )
}

async fn assert_coding_loop_live_smoke_tool_sequence(
    runtime: &Runtime,
    events: &[RuntimeEvent],
) -> Result<(), CliError> {
    let mut pending_by_call_id = BTreeMap::new();
    let mut resolved_tool_names = Vec::new();
    let mut resolved_artifacts = Vec::new();
    for event in events {
        match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
            }
            RuntimeEventKind::ToolCallResolved { result } => {
                if result.status() != ToolCallResultStatus::Succeeded {
                    return Err(CliError::Unexpected(format!(
                        "live smoke tool call {} did not succeed",
                        result.call_id()
                    )));
                }
                let call = pending_by_call_id.get(result.call_id()).ok_or_else(|| {
                    CliError::Unexpected(format!(
                        "live smoke resolved unknown tool call {}",
                        result.call_id()
                    ))
                })?;
                resolved_tool_names.push(call.name().as_str().to_owned());
                resolved_artifacts.push(result.artifact().id().clone());
            }
            _ => {}
        }
    }

    require_live_smoke_tool_name(&resolved_tool_names, CODING_LOOP_SMOKE_TOOL_NAME)?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_READ_FILE_TOOL)?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_PATCH_FILE_TOOL)?;

    let mut process_artifact_texts = Vec::new();
    for artifact_id in &resolved_artifacts {
        let Ok(content) = runtime.read_artifact_content(artifact_id).await else {
            continue;
        };
        let Some(text) = content.as_text() else {
            continue;
        };
        if text.contains("\"kind\":\"process_action\"") {
            process_artifact_texts.push(text.to_owned());
        }
    }
    let inspected = process_artifact_texts.iter().any(|text| {
        process_artifact_has_argv(text, ["rg", "--files"]) && text.contains("src/lib.rs")
    });
    let verified = process_artifact_texts.iter().any(|text| {
        process_artifact_has_argv(text, ["rg", CODING_LOOP_LIVE_SMOKE_TARGET_VALUE])
            && text.contains(CODING_LOOP_LIVE_SMOKE_TARGET_VALUE)
    });
    if !inspected {
        return Err(CliError::Unexpected(
            "live smoke did not resolve a real rg --files process call".to_owned(),
        ));
    }
    if !verified {
        return Err(CliError::Unexpected(format!(
            "live smoke did not resolve a real rg {CODING_LOOP_LIVE_SMOKE_TARGET_VALUE} verification call"
        )));
    }

    Ok(())
}

fn require_live_smoke_tool_name(names: &[String], required: &str) -> Result<(), CliError> {
    if names.iter().any(|name| name == required) {
        Ok(())
    } else {
        Err(CliError::Unexpected(format!(
            "live smoke did not resolve required tool `{required}`"
        )))
    }
}

fn process_artifact_has_argv<const N: usize>(text: &str, expected: [&str; N]) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    value
        .get("intent")
        .and_then(|intent| intent.get("argv"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|argv| {
            argv.iter()
                .filter_map(serde_json::Value::as_str)
                .eq(expected)
        })
}

struct CodingLoopSmokeProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Mutex<Vec<ModelEvent>>,
}

impl CodingLoopSmokeProvider {
    fn new(relative_cwd: Option<&str>) -> Result<Self, CliError> {
        let steps = vec![
            coding_loop_process_call(
                "coding-loop-smoke-rg-files",
                &["rg", "--files"],
                relative_cwd,
            )?,
            coding_loop_workspace_call(
                "coding-loop-smoke-read",
                WORKSPACE_READ_FILE_TOOL,
                [("path", serde_json::Value::String("src/lib.rs".to_owned()))],
            )?,
            coding_loop_workspace_call(
                "coding-loop-smoke-patch",
                WORKSPACE_PATCH_FILE_TOOL,
                [
                    ("path", serde_json::Value::String("src/lib.rs".to_owned())),
                    (
                        "old_text",
                        serde_json::Value::String(format!(
                            "\"{CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE}\""
                        )),
                    ),
                    (
                        "new_text",
                        serde_json::Value::String(format!(
                            "\"{CODING_LOOP_LIVE_SMOKE_TARGET_VALUE}\""
                        )),
                    ),
                ],
            )?,
            coding_loop_process_call(
                "coding-loop-smoke-verify",
                &["rg", CODING_LOOP_LIVE_SMOKE_TARGET_VALUE],
                relative_cwd,
            )?,
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        "coding-loop-smoke patched greeting and verified it",
                    )],
                    FinishReason::Stop,
                    None,
                ),
            },
        ];

        Ok(Self {
            name: ProviderName::new("merry-coding-loop-smoke-provider").map_err(unexpected)?,
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .map_err(unexpected)?,
            steps: Mutex::new(steps.into_iter().rev().collect()),
        })
    }
}

impl ModelProvider for CodingLoopSmokeProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            let event = self
                .steps
                .lock()
                .expect("coding loop smoke steps mutex should not be poisoned")
                .pop()
                .ok_or_else(|| {
                    ModelError::invalid_request("coding-loop-smoke provider has no scripted step")
                })?;
            Ok(Box::pin(stream::iter([Ok(event)])) as ModelEventStream)
        })
    }
}

fn coding_loop_process_call(
    call_id: &str,
    argv: &[&str],
    cwd: Option<&str>,
) -> Result<ModelEvent, CliError> {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "argv".to_owned(),
        serde_json::Value::Array(
            argv.iter()
                .map(|argument| serde_json::Value::String((*argument).to_owned()))
                .collect(),
        ),
    );
    if let Some(cwd) = cwd {
        arguments.insert("cwd".to_owned(), serde_json::Value::String(cwd.to_owned()));
    }
    coding_loop_tool_call(call_id, CODING_LOOP_SMOKE_TOOL_NAME, arguments)
}

fn coding_loop_workspace_call<const N: usize>(
    call_id: &str,
    tool_name: &str,
    arguments: [(&str, serde_json::Value); N],
) -> Result<ModelEvent, CliError> {
    coding_loop_tool_call(
        call_id,
        tool_name,
        arguments
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn coding_loop_tool_call(
    call_id: &str,
    tool_name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<ModelEvent, CliError> {
    let call = ModelToolCall::new(
        ModelToolCallId::new(call_id).map_err(unexpected)?,
        ToolName::new(tool_name).map_err(unexpected)?,
        ToolArguments::new(arguments),
    );
    Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })
}

async fn run_shell(
    args: ShellArgs,
    sandbox_child_handoff: Option<SandboxChildHandoff>,
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
    let admission = shell_runtime_admission(
        args.accept_local_workspace_process_risk,
        sandbox_child_handoff,
        sandbox_runtime_profile,
        sandbox_marker.as_deref(),
        sandbox_version.as_deref(),
    );
    let intent = shell_process_action_intent(args.argv)?;
    run_shell_to_writer(
        intent,
        admission,
        Arc::new(TokioProcessRunner),
        args.events_jsonl,
        tokio::io::stdout(),
    )
    .await
}

async fn read_proc_self_mountinfo() -> Option<String> {
    tokio::task::spawn_blocking(|| std::fs::read_to_string("/proc/self/mountinfo"))
        .await
        .ok()?
        .ok()
}

async fn run_shell_to_writer<W>(
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
    let runtime = build_shell_runtime(session_id, intent, admission, runner)?;
    let input = StepInput::user_text(SHELL_STEP_INPUT).map_err(unexpected)?;

    let mut writer = BufWriter::new(writer);
    let events = if events_jsonl {
        write_runtime_step_events_to(&runtime, input, StepContext::default(), &mut writer).await?
    } else {
        collect_runtime_step_events(&runtime, input, StepContext::default()).await?
    };
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

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .map_err(unexpected)?;
    if events_jsonl {
        write_runtime_events(execution_events.clone(), &mut writer).await?;
    } else {
        write_shell_process_output(&runtime, &execution_events, &mut writer).await?;
    }
    writer.flush().await.map_err(stdout_error)
}

fn build_shell_runtime(
    session_id: SessionId,
    intent: ProcessActionIntent,
    admission: Option<AcceptedLocalWorkspaceProcessAdmission>,
    runner: Arc<dyn ProcessRunner>,
) -> Result<Runtime, CliError> {
    let shell_tool = process_command_tool(
        ToolName::new(SHELL_TOOL_NAME).map_err(unexpected)?,
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

fn shell_runtime_admission(
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

fn sandbox_runtime_profile_from_evidence(
    home: Option<&OsStr>,
    tmpdir: Option<&OsStr>,
    mountinfo: Option<&str>,
) -> Option<SandboxRuntimeProfile> {
    if home == Some(OsStr::new(SANDBOX_HOME))
        && tmpdir == Some(OsStr::new(SANDBOX_TMPDIR))
        && mountinfo_has_tmpfs_mounts(mountinfo?, ["/home", SANDBOX_TMPDIR])
    {
        Some(SandboxRuntimeProfile::CliBwrapV1)
    } else {
        None
    }
}

fn mountinfo_has_tmpfs_mounts(mountinfo: &str, mount_points: [&str; 2]) -> bool {
    mount_points
        .into_iter()
        .all(|mount_point| mountinfo_has_tmpfs_mount(mountinfo, mount_point))
}

fn mountinfo_has_tmpfs_mount(mountinfo: &str, mount_point: &str) -> bool {
    mountinfo
        .lines()
        .filter_map(parse_mountinfo_mount)
        .any(|mount| mount.mount_point == mount_point && mount.fs_type == "tmpfs")
}

fn parse_mountinfo_mount(line: &str) -> Option<MountInfoMount<'_>> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let separator_index = fields.iter().position(|field| *field == "-")?;
    if separator_index < 5 || fields.len() <= separator_index + 1 {
        return None;
    }

    Some(MountInfoMount {
        mount_point: fields[4],
        fs_type: fields[separator_index + 1],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MountInfoMount<'a> {
    mount_point: &'a str,
    fs_type: &'a str,
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
    let events = collect_runtime_step_events(runtime, input, context).await?;
    write_runtime_events(events.clone(), writer).await?;
    Ok(events)
}

async fn collect_runtime_step_events(
    runtime: &Runtime,
    input: StepInput,
    context: StepContext,
) -> Result<Vec<RuntimeEvent>, CliError> {
    let mut events = runtime.step(input, context).map_err(unexpected)?;
    let mut collected = Vec::new();
    while let Some(event) = events.next().await {
        collected.push(event);
    }

    Ok(collected)
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

async fn write_shell_process_output<W>(
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
                ModelToolCallId::new(SHELL_TOOL_CALL_ID).map_err(unexpected)?,
                ToolName::new(SHELL_TOOL_NAME).map_err(unexpected)?,
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

fn debug_openai_config(
    model_flag: Option<&str>,
    merry_config: Option<&MerryConfig>,
) -> Result<DebugOpenAiConfig, CliError> {
    debug_openai_config_with_env(model_flag, merry_config, optional_env)
}

fn debug_openai_config_with_env(
    model_flag: Option<&str>,
    merry_config: Option<&MerryConfig>,
    env_value: impl Fn(&'static str) -> Result<Option<String>, CliError>,
) -> Result<DebugOpenAiConfig, CliError> {
    if env_value("MERRY_OPENAI_DEBUG")?.as_deref() != Some("1") {
        return Err(debug_openai_usage_error(
            "set MERRY_OPENAI_DEBUG=1 to enable live OpenAI-compatible debugging",
        ));
    }

    let merry_config = merry_config.ok_or_else(|| {
        debug_openai_usage_error(
            "Merry XDG provider config is required for OpenAI-compatible debugging",
        )
    })?;
    let provider_config = merry_config
        .openai_compatible_provider()
        .map_err(debug_openai_usage_error)?;
    let api_key = provider_config
        .resolve_api_key()
        .map_err(debug_openai_usage_error)?;
    let model = match model_flag {
        Some(model) => model.to_owned(),
        None => provider_config.model.clone().ok_or_else(|| {
            debug_openai_usage_error(
                "[providers.default].model must be set or --model must be provided",
            )
        })?,
    };

    let mut provider = OpenAiProviderConfig::new(&api_key).map_err(debug_openai_usage_error)?;

    if let Some(base_url) = provider_config.base_url.as_deref() {
        provider = provider
            .with_base_url(base_url)
            .map_err(debug_openai_usage_error)?;
    }

    Ok(DebugOpenAiConfig { provider, model })
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

fn debug_coding_loop_live_smoke_usage() -> String {
    let mut command = DebugCodingLoopLiveSmokeArgs::augment_args(clap::Command::new(
        "coding-loop-live-smoke",
    ))
    .bin_name("merry debug coding-loop-live-smoke")
    .about("Run an opt-in sandboxed coding-loop smoke driven by a live OpenAI-compatible model")
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

#[derive(Debug)]
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
        MERRY_SANDBOX_VERSION_ENV, SANDBOX_CHILD_HANDOFF_ARG, SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1,
        SANDBOX_HOME, SANDBOX_MERRY_CONFIG_DIR, SANDBOX_MERRY_LOG_DIR, SANDBOX_TMPDIR,
        SANDBOX_XDG_CONFIG_HOME, SANDBOX_XDG_STATE_HOME, SandboxBootstrap, SandboxChildHandoff,
        SandboxError, SandboxHost, SandboxRuntimeProfile, args_without_sandbox_bootstrap_flags,
        debug_echo_tool, debug_openai_config_with_env, debug_openai_usage, find_bwrap_in_path, os,
        plan_sandbox_bootstrap_with_file_exists, report_cli_exit, run_debug_coding_loop_smoke,
        sandbox_runtime_profile_from_evidence, shell_process_action_intent,
        shell_runtime_admission, shell_usage, write_debug_openai_tool_events,
    };
    use super::{DEBUG_TOOL_NAME, run_shell_to_writer, write_runtime_step_events};
    use clap::Parser;
    use futures_util::stream;
    use merry_core::{ProviderName, RuntimeEvent, ToolCallResultStatus, ToolName};
    use merry_llm::{
        FinishReason, GenerationConfig, ModelCapabilities, ModelError, ModelEvent,
        ModelEventStream, ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest,
        ModelResponse, ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
    };
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, MAX_PROCESS_OUTPUT_LIMIT_BYTES,
        ProcessActionIntent, ProcessEnvPolicy, ProcessExitStatus, ProcessRunner,
        ProcessRunnerContext, ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
        Runtime, StepContext, StepInput,
    };
    use serde_json::{Map, Value};
    use std::{
        ffi::{OsStr, OsString},
        path::{Path, PathBuf},
        process::ExitCode,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
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
            xdg_paths: super::config::XdgPaths::from_parts(
                PathBuf::from("/home/alice"),
                Some(PathBuf::from("/host/config")),
                Some(PathBuf::from("/host/state")),
            ),
            log_settings: None,
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
                Some(DebugCommand::CodingLoopSmoke | DebugCommand::CodingLoopLiveSmoke(_)) => {
                    panic!("expected debug openai subcommand");
                }
                None => panic!("expected debug openai subcommand"),
            },
            CliCommand::Shell(_) => panic!("expected debug subcommand"),
        }
    }

    #[test]
    fn clap_parses_debug_coding_loop_smoke() {
        let cli = Cli::try_parse_from(["merry", "debug", "coding-loop-smoke"])
            .expect("debug coding-loop-smoke args should parse");

        match cli.command {
            CliCommand::Debug(debug) => match debug.command {
                Some(DebugCommand::CodingLoopSmoke) => {}
                Some(DebugCommand::OpenAi(_) | DebugCommand::CodingLoopLiveSmoke(_)) | None => {
                    panic!("expected debug coding-loop-smoke subcommand")
                }
            },
            CliCommand::Shell(_) => panic!("expected debug subcommand"),
        }
    }

    #[test]
    fn clap_parses_debug_coding_loop_live_smoke() {
        let cli = Cli::try_parse_from([
            "merry",
            "debug",
            "coding-loop-live-smoke",
            "--model",
            "gpt-test",
            "--max-output-tokens",
            "384",
        ])
        .expect("debug coding-loop-live-smoke args should parse");

        match cli.command {
            CliCommand::Debug(debug) => match debug.command {
                Some(DebugCommand::CodingLoopLiveSmoke(live)) => {
                    assert_eq!(live.model.as_deref(), Some("gpt-test"));
                    assert_eq!(live.max_output_tokens, 384);
                }
                Some(DebugCommand::OpenAi(_) | DebugCommand::CodingLoopSmoke) | None => {
                    panic!("expected debug coding-loop-live-smoke subcommand")
                }
            },
            CliCommand::Shell(_) => panic!("expected debug subcommand"),
        }
    }

    #[tokio::test]
    async fn coding_loop_smoke_admission_requires_real_sandbox_handoff() {
        let err = run_debug_coding_loop_smoke(None)
            .await
            .expect_err("coding-loop-smoke should require real sandbox handoff");

        match err {
            CliError::DebugUsage(message) => {
                assert!(message.contains("--with-sandbox"));
                assert!(message.contains("coding-loop-smoke"));
            }
            _ => panic!("expected debug usage error"),
        }
    }

    #[tokio::test]
    async fn coding_loop_smoke_writes_configured_json_log_records_without_payloads() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let config_root = temp.path().join("config");
        let state_root = temp.path().join("state");
        let paths = super::config::XdgPaths::from_parts(
            PathBuf::from("/home/alice"),
            Some(config_root),
            Some(state_root),
        );
        let config = super::config::MerryConfig::load_optional_from_text(
            Some("[observability.log]\nenabled = true\nlevel = \"debug\"\nformat = \"json\"\n"),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");
        let log_settings = super::effective_log_settings(Some(&config), &paths)
            .expect("log settings should validate")
            .expect("logging should be enabled");
        let log_path = log_settings.path.clone();
        let guard = super::observability::init_observability(Some(&log_settings))
            .expect("observability should initialize")
            .expect("file logging should install a guard");

        let smoke_root = temp.path().join("coding-loop-smoke-fixture");
        std::fs::create_dir_all(smoke_root.join("src")).expect("fixture src dir should exist");
        std::fs::write(
            smoke_root.join("Cargo.toml"),
            "[package]\nname = \"merry-coding-loop-smoke\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
        )
        .expect("fixture Cargo.toml should write");
        std::fs::write(
            smoke_root.join("src/lib.rs"),
            super::coding_loop_smoke_initial_source(),
        )
        .expect("fixture source should write");
        let runtime = super::build_coding_loop_smoke_runtime(
            &smoke_root,
            None,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(FakeProcessRunner::succeeding(
                "sensitive process stdout must not leak\n",
            )),
        )
        .expect("coding-loop smoke runtime should build");

        let result = runtime
            .run_agent_loop(
                StepInput::user_text("Run the sandboxed coding-loop smoke.")
                    .expect("valid step input"),
                StepContext::default(),
                AgentLoopConfig::new(8).expect("valid loop config"),
            )
            .await
            .expect("coding-loop smoke should run");
        super::assert_coding_loop_smoke_result(&runtime, &result, &smoke_root)
            .await
            .expect("coding-loop smoke result should validate");
        drop(guard);

        let log = std::fs::read_to_string(&log_path).expect("log file should be written");
        for expected in [
            "\"event\":\"runtime.loop.start\"",
            "\"event\":\"runtime.provider.request\"",
            "\"event\":\"runtime.tool.pending\"",
            "\"event\":\"runtime.tool.execute.start\"",
            "\"event\":\"runtime.workspace_tool.start\"",
            "\"event\":\"runtime.workspace_tool.finish\"",
            "\"event\":\"runtime.process.execute.start\"",
            "\"event\":\"runtime.process.execute.finish\"",
            "\"event\":\"runtime.artifact.record\"",
            "\"event\":\"runtime.tool.execute.finish\"",
            "\"event\":\"runtime.loop.finish\"",
            "\"session_id\":\"coding-loop-smoke\"",
            "\"tool_name\":\"run_process\"",
            "\"tool_name\":\"workspace_read_file\"",
            "\"tool_name\":\"workspace_patch_file\"",
            "\"status\":\"completed\"",
            "\"status\":\"succeeded\"",
            "\"diagnostic_code\"",
        ] {
            assert!(log.contains(expected), "log missing {expected}");
        }
        for forbidden in [
            "Run the sandboxed coding-loop smoke.",
            "pub fn greeting",
            "\"unfixed\"",
            "sensitive process stdout must not leak",
            "coding-loop-smoke patched greeting and verified it",
            "sk-",
            "provider_wire",
        ] {
            assert!(!log.contains(forbidden), "log leaked {forbidden}");
        }
    }

    #[test]
    fn openai_debug_config_uses_xdg_toml_provider_and_secret_file() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let config_dir = temp.path().join("config/merry");
        std::fs::create_dir_all(config_dir.join("secrets")).expect("config dir should be created");
        std::fs::write(config_dir.join("secrets/openai.key"), "sk-test\n")
            .expect("secret file should write");
        let paths = super::config::XdgPaths::from_parts(
            PathBuf::from("/home/alice"),
            Some(temp.path().join("config")),
            Some(temp.path().join("state")),
        );
        let config = super::config::MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
base_url = "https://api.example.test/v1"
api_key_file = "secrets/openai.key"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let loaded = debug_openai_config_with_env(None, Some(&config), |name| {
            Ok((name == "MERRY_OPENAI_DEBUG").then(|| "1".to_owned()))
        })
        .expect("debug config should load");
        assert_eq!(loaded.model, "gpt-test");
        assert_eq!(loaded.provider.base_url(), "https://api.example.test/v1");
    }

    #[test]
    fn clap_parses_shell_argv() {
        let cli = Cli::try_parse_from(["merry", "shell", "--", "rustc", "--version"])
            .expect("shell args should parse");

        match cli.command {
            CliCommand::Shell(shell) => {
                assert!(!shell.accept_local_workspace_process_risk);
                assert_eq!(shell.argv, ["rustc", "--version"]);
            }
            CliCommand::Debug(_) => panic!("expected shell subcommand"),
        }
    }

    #[test]
    fn clap_parses_shell_local_workspace_process_risk_acceptance() {
        let cli = Cli::try_parse_from([
            "merry",
            "shell",
            "--accept-local-workspace-process-risk",
            "--",
            "cargo",
            "test",
            "-p",
            "merry-runtime",
        ])
        .expect("shell args should parse");

        match cli.command {
            CliCommand::Shell(shell) => {
                assert!(shell.accept_local_workspace_process_risk);
                assert_eq!(shell.argv, ["cargo", "test", "-p", "merry-runtime"]);
            }
            CliCommand::Debug(_) => panic!("expected shell subcommand"),
        }
    }

    #[test]
    fn clap_parses_hidden_sandbox_child_handoff() {
        let cli = Cli::try_parse_from([
            "merry",
            SANDBOX_CHILD_HANDOFF_ARG,
            SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1,
            "shell",
            "--",
            "rustc",
            "--version",
        ])
        .expect("hidden sandbox handoff args should parse");

        assert_eq!(
            cli.sandbox_child_handoff,
            Some(SandboxChildHandoff::CliBwrapV1)
        );
    }

    #[test]
    fn clap_rejects_shell_argv_without_separator() {
        let error = Cli::try_parse_from(["merry", "shell", "rustc", "--version"])
            .expect_err("shell argv should require `--` separator");

        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn shell_usage_contains_shell_usage() {
        assert!(shell_usage().contains("Usage: merry shell [OPTIONS] -- <ARGV>..."));
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
    fn shell_runtime_admission_requires_accept_handoff_and_exact_sandbox_markers() {
        assert_eq!(
            shell_runtime_admission(
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(OsStr::new("1")),
                Some(OsStr::new("1")),
            ),
            Some(AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1())
        );

        for (accept, handoff, profile, sandbox, version) in [
            (
                false,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                Some(os("1")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                None,
                Some(os("1")),
                Some(os("1")),
            ),
            (true, None, None, None, None),
            (
                false,
                Some(SandboxChildHandoff::CliBwrapV1),
                None,
                None,
                None,
            ),
            (
                true,
                None,
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                Some(os("1")),
            ),
            (
                false,
                None,
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                Some(os("1")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                None,
                Some(os("1")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                None,
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("0")),
                Some(os("1")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                Some(os("2")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("true")),
                Some(os("1")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                Some(os("")),
            ),
        ] {
            assert_eq!(
                shell_runtime_admission(
                    accept,
                    handoff,
                    profile,
                    sandbox.as_deref(),
                    version.as_deref(),
                ),
                None
            );
        }
    }

    #[test]
    fn sandbox_runtime_profile_requires_tmpfs_home_tmp_and_expected_env() {
        let mountinfo = "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /home rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
";

        assert_eq!(
            sandbox_runtime_profile_from_evidence(
                Some(OsStr::new(SANDBOX_HOME)),
                Some(OsStr::new(SANDBOX_TMPDIR)),
                Some(mountinfo),
            ),
            Some(SandboxRuntimeProfile::CliBwrapV1)
        );

        for (home, tmpdir, mountinfo) in [
            (
                Some(OsStr::new("/home/locez")),
                Some(OsStr::new(SANDBOX_TMPDIR)),
                Some(mountinfo),
            ),
            (
                Some(OsStr::new(SANDBOX_HOME)),
                Some(OsStr::new("/var/tmp")),
                Some(mountinfo),
            ),
            (
                Some(OsStr::new(SANDBOX_HOME)),
                Some(OsStr::new(SANDBOX_TMPDIR)),
                Some(
                    "\
26 24 0:22 / / rw,relatime - overlay overlay rw
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
",
                ),
            ),
            (
                Some(OsStr::new(SANDBOX_HOME)),
                Some(OsStr::new(SANDBOX_TMPDIR)),
                Some(
                    "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /home rw,relatime - ext4 /dev/sda1 rw
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
",
                ),
            ),
            (
                Some(OsStr::new(SANDBOX_HOME)),
                Some(OsStr::new(SANDBOX_TMPDIR)),
                Some(
                    "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /home rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
",
                ),
            ),
            (
                Some(OsStr::new(SANDBOX_HOME)),
                Some(OsStr::new(SANDBOX_TMPDIR)),
                None,
            ),
        ] {
            assert_eq!(
                sandbox_runtime_profile_from_evidence(home, tmpdir, mountinfo),
                None
            );
        }
    }

    #[tokio::test]
    async fn shell_helper_simulated_sandbox_runs_local_workspace_effect_with_fake_runner() {
        let intent = shell_process_action_intent(vec![
            "cargo".to_owned(),
            "test".to_owned(),
            "-p".to_owned(),
            "merry-runtime".to_owned(),
        ])
        .unwrap_or_else(|_| panic!("shell process intent should be valid"));
        let admission = shell_runtime_admission(
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(OsStr::new("1")),
            Some(OsStr::new("1")),
        );
        let runner = FakeProcessRunner::succeeding("simulated cargo success\n");
        let mut output = Vec::new();

        run_shell_to_writer(
            intent,
            admission,
            Arc::new(runner.clone()),
            false,
            &mut output,
        )
        .await
        .unwrap_or_else(|_| panic!("accepted local workspace shell command should resolve"));

        assert_eq!(runner.call_count(), 1);
        assert_eq!(
            runner.observed_argv(),
            vec![vec!["cargo", "test", "-p", "merry-runtime"]]
        );
        let text = String::from_utf8(output).expect("output should be utf-8");
        assert_eq!(text, "simulated cargo success\n");
    }

    #[tokio::test]
    async fn shell_helper_simulated_sandbox_marker_still_denies_forbidden_command() {
        let intent = shell_process_action_intent(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "echo bad".to_owned(),
        ])
        .unwrap_or_else(|_| panic!("shell process intent should be valid"));
        let admission = shell_runtime_admission(
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(OsStr::new("1")),
            Some(OsStr::new("1")),
        );
        let runner = FakeProcessRunner::succeeding("bad\n");
        let mut output = Vec::new();

        run_shell_to_writer(
            intent,
            admission,
            Arc::new(runner.clone()),
            true,
            &mut output,
        )
        .await
        .unwrap_or_else(|_| panic!("forbidden command should resolve as a policy denial"));

        assert_eq!(runner.call_count(), 0);
        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = parse_runtime_events(&text);
        let resolved = resolved_tool_result(&events);
        assert_eq!(resolved.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            resolved
                .diagnostic()
                .expect("denied result should include a diagnostic")
                .code(),
            "action_policy_denied"
        );
    }

    #[derive(Clone)]
    struct FakeProcessRunner {
        calls: Arc<AtomicUsize>,
        observed_argv: Arc<Mutex<Vec<Vec<String>>>>,
        stdout: String,
        stderr: String,
        status: ProcessExitStatus,
    }

    impl FakeProcessRunner {
        fn succeeding(stdout: impl Into<String>) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                observed_argv: Arc::new(Mutex::new(Vec::new())),
                stdout: stdout.into(),
                stderr: String::new(),
                status: ProcessExitStatus::Exited(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn observed_argv(&self) -> Vec<Vec<String>> {
            self.observed_argv
                .lock()
                .expect("observed argv mutex should not be poisoned")
                .clone()
        }
    }

    impl ProcessRunner for FakeProcessRunner {
        fn run<'a>(
            &'a self,
            intent: ProcessActionIntent,
            context: ProcessRunnerContext,
        ) -> ProcessRunnerFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.observed_argv
                    .lock()
                    .expect("observed argv mutex should not be poisoned")
                    .push(intent.argv().to_vec());
                if context.cancellation_token().is_cancelled() {
                    return Err(ProcessRunnerError::Cancelled);
                }

                ProcessRunnerOutput::new(
                    &intent,
                    self.status,
                    self.stdout.clone(),
                    false,
                    self.stderr.clone(),
                    false,
                )
                .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
            })
        }
    }

    fn parse_runtime_events(text: &str) -> Vec<RuntimeEvent> {
        assert!(
            text.ends_with('\n'),
            "runtime JSONL should end with newline"
        );
        text.lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect()
    }

    fn resolved_tool_result(events: &[RuntimeEvent]) -> &merry_core::ToolCallResult {
        events
            .iter()
            .find_map(|event| match &event.kind {
                merry_core::RuntimeEventKind::ToolCallResolved { result } => Some(result),
                _ => None,
            })
            .expect("shell command should resolve a tool call")
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
    fn sandbox_plan_mounts_merry_config_dir_read_only_and_sets_xdg_config_home() {
        let host = sandbox_host();
        let SandboxBootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        assert!(contains_sequence(
            &args,
            &[
                "--ro-bind-try",
                "/host/config/merry",
                SANDBOX_MERRY_CONFIG_DIR
            ]
        ));
        assert!(contains_sequence(
            &args,
            &["--setenv", "XDG_CONFIG_HOME", SANDBOX_XDG_CONFIG_HOME]
        ));
    }

    #[test]
    fn sandbox_plan_does_not_mount_log_dir_when_logging_is_disabled() {
        let host = sandbox_host();
        let SandboxBootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        assert!(!contains_sequence(
            &args,
            &["--bind", "/host/state/merry/logs", SANDBOX_MERRY_LOG_DIR]
        ));
    }

    #[test]
    fn sandbox_plan_mounts_log_dir_read_write_when_file_logging_is_enabled() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let host_log_dir = temp.path().join("state/merry/logs");
        let host_log_dir_string = host_log_dir.to_string_lossy().into_owned();
        let mut host = sandbox_host();
        host.log_settings = Some(super::config::EffectiveLogSettings {
            level: super::config::LogLevel::Info,
            format: super::config::LogFormat::Json,
            path: host_log_dir.join("merry.jsonl"),
        });
        let SandboxBootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        assert!(contains_sequence(
            &args,
            &["--bind", &host_log_dir_string, SANDBOX_MERRY_LOG_DIR]
        ));
        assert!(contains_sequence(
            &args,
            &["--setenv", "XDG_STATE_HOME", SANDBOX_XDG_STATE_HOME]
        ));
        assert!(host_log_dir.exists());
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
    fn sandbox_plan_reexecs_current_exe_with_hidden_handoff_and_sandbox_flag_removed() {
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
            [
                SANDBOX_CHILD_HANDOFF_ARG,
                SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1,
                "debug",
                "--session-id",
                "custom-session",
            ]
        );
    }

    #[test]
    fn sandbox_plan_strips_host_provided_hidden_handoff_before_injecting_its_own() {
        let mut host = sandbox_host();
        host.args = vec![
            os("--with-sandbox"),
            os(SANDBOX_CHILD_HANDOFF_ARG),
            os(SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1),
            os("debug"),
            os("--session-id"),
            os("custom-session"),
        ];
        let SandboxBootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);
        let handoff_positions = args
            .iter()
            .enumerate()
            .filter_map(|(index, arg)| (arg == SANDBOX_CHILD_HANDOFF_ARG).then_some(index))
            .collect::<Vec<_>>();

        assert_eq!(handoff_positions.len(), 1);
        let handoff_index = handoff_positions[0];
        assert_eq!(args[handoff_index + 1], SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1);
        assert!(contains_sequence(
            &args,
            &[
                "/workspace/merry/target/debug/merry",
                SANDBOX_CHILD_HANDOFF_ARG,
                SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1,
                "debug",
                "--session-id",
                "custom-session",
            ],
        ));
    }

    #[test]
    fn sandbox_plan_strips_host_provided_hidden_handoff_assignment_before_injecting_its_own() {
        let mut host = sandbox_host();
        host.args = vec![
            os("--with-sandbox"),
            os("--merry-sandbox-child-handoff=cli-bwrap-v1"),
            os("debug"),
            os("--session-id"),
            os("custom-session"),
        ];
        let SandboxBootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        assert_eq!(
            args.iter()
                .filter(|arg| arg.as_str() == SANDBOX_CHILD_HANDOFF_ARG)
                .count(),
            1
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--merry-sandbox-child-handoff=cli-bwrap-v1")
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
    fn args_without_sandbox_bootstrap_flags_removes_only_first_sandbox_marker() {
        let args = vec![
            os("--with-sandbox"),
            os("debug"),
            os("--input"),
            os("--with-sandbox"),
        ];

        assert_eq!(
            args_without_sandbox_bootstrap_flags(&args),
            vec![os("debug"), os("--input"), os("--with-sandbox")]
        );
    }

    #[test]
    fn args_without_sandbox_bootstrap_flags_preserves_shell_trailing_argv() {
        let args = vec![
            os("--with-sandbox"),
            os("shell"),
            os("--"),
            os("--with-sandbox"),
            os(SANDBOX_CHILD_HANDOFF_ARG),
            os(SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1),
        ];

        assert_eq!(
            args_without_sandbox_bootstrap_flags(&args),
            vec![
                os("shell"),
                os("--"),
                os("--with-sandbox"),
                os(SANDBOX_CHILD_HANDOFF_ARG),
                os(SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1),
            ]
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
