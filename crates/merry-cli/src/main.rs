//! Debug and demonstration CLI for Merry.

mod cmd;
mod config;
mod observability;
mod sandbox;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use config::{EffectiveLogSettings, EffectiveOpenAiProviderConfig, MerryConfig, XdgPaths};
use futures_util::{StreamExt, stream};
use merry_core::{
    ArtifactId, PendingToolCall, ProviderName, RuntimeEvent, RuntimeEventKind, SessionId,
    ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelCapabilities, ModelError, ModelEvent, ModelEventStream,
    ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelRetryPolicy, ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AgentLoopResult, AgentLoopStatus,
    ArtifactContent, AutomaticCompactionConfig, BwrapPermissionedProcessRunnerFactory,
    BwrapProcessRunner, ChildRuntimeFactory, ChildRuntimeInput,
    DEFAULT_CODING_AGENT_MAX_MODEL_TURNS, MAX_PROCESS_OUTPUT_LIMIT_BYTES,
    PermissionedProcessRunnerFactory, ProcessActionIntent, ProcessEnvPolicy, ProcessRunner,
    RegisteredTool, Runtime, RuntimeBuilder, RuntimeModelRole, RuntimeProfile, StepContext,
    StepInput, SubagentManager, TokioProcessRunner, ToolExecutionContext, ToolExecutionOutcome,
    ToolExecutor, ToolExecutorFuture, process_command_tool, subagent_registered_tools,
};
use merry_tool_workspace::{
    CODING_LOOP_PROCESS_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
    WorkspaceCodingLoopProfile, WorkspaceRuntimeProfileBuilderExt, WorkspaceToolLimits,
    WorkspaceToolsConfig,
};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fmt, fs, io,
    path::{Path, PathBuf},
    process::{ExitCode, Termination},
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

use sandbox::{
    ChildHandoff as SandboxChildHandoff, MERRY_SANDBOX_ENV, MERRY_SANDBOX_VERSION,
    MERRY_SANDBOX_VERSION_ENV, RuntimeProfile as SandboxRuntimeProfile, read_proc_self_mountinfo,
    runtime_profile_from_evidence as sandbox_runtime_profile_from_evidence,
};

#[cfg(test)]
use sandbox::{
    Bootstrap as SandboxBootstrap, Error as SandboxError, Host as SandboxHost, Plan as SandboxPlan,
    SANDBOX_CHILD_HANDOFF_ARG, SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1, SANDBOX_HOME,
    SANDBOX_MERRY_CONFIG_DIR, SANDBOX_MERRY_LOG_DIR, SANDBOX_TMPDIR, SANDBOX_XDG_CONFIG_HOME,
    SANDBOX_XDG_STATE_HOME, args_without_sandbox_bootstrap_flags, find_bwrap_in_path, os,
    plan_bootstrap_with_file_exists as plan_sandbox_bootstrap_with_file_exists,
};

const DEFAULT_SESSION_ID: &str = "debug-session";
const DEFAULT_INPUT: &str = "debug step";
const DEBUG_TOOL_NAME: &str = "debug_echo";
const DEBUG_TOOL_CONTINUATION_INPUT: &str = "continue after debug tool";
const CODING_LOOP_SMOKE_SESSION_ID: &str = "coding-loop-smoke";
const CODING_LOOP_LIVE_SMOKE_SESSION_ID: &str = "coding-loop-live-smoke";
const CODING_LOOP_TASK_SMOKE_SESSION_ID: &str = "coding-loop-task-smoke";
const CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID: &str = "coding-loop-task-live-smoke";
const CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID: &str = "coding-loop-subagent-live-smoke";
const PERMISSION_NETWORK_SMOKE_SESSION_ID: &str = "permission-network-smoke";
const PERMISSION_NETWORK_SMOKE_ARGV: [&str; 3] = ["getent", "hosts", "example.com"];
const CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE: &str = "unfixed";
const CODING_LOOP_LIVE_SMOKE_TARGET_VALUE: &str = "fixed-by-live-llm";
const CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE: &str = "subagent-output.txt";
const CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL: &str = "status: pending\n";
const CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET: &str = "status: subagent-live-smoke-complete\n";
const CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES: usize = 256;
const ASSISTANT_OUTPUT_ARTIFACT_PREFIX: &str = "assistant-output-";
const SHELL_TOOL_NAME: &str = "shell_command";
const SHELL_TOOL_CALL_ID: &str = "call-shell-command";
const SHELL_STEP_INPUT: &str = "run shell command through Merry process protocol";
const MERRY_OPENAI_DEBUG_ENV: &str = "MERRY_OPENAI_DEBUG";
const OPENAI_ENV_HELP: &str = "\
Environment:
  MERRY_OPENAI_DEBUG=1       Required opt-in before any network attempt
  XDG_CONFIG_HOME            Optional base for merry/config.toml

Provider/model/base URL/API key source come from
`$XDG_CONFIG_HOME/merry/config.toml` or `~/.config/merry/config.toml`.
Set exactly one of `[providers.openai-compatible].api_key` or `api_key_file`.
For sandboxed live smokes, prefer config-relative `api_key_file =
\"secrets/openai.key\"` so credentials are not passed through bwrap argv.
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

#[derive(Debug, Subcommand)]
enum CliCommand {
    #[command(about = "Complete a coding task with Merry's headless agent")]
    Run(RunArgs),
    #[command(about = "Generate a shell command plan from a natural-language request")]
    Cmd(cmd::Args),
    #[command(about = "Print deterministic runtime events or run opt-in provider debugging")]
    Debug(DebugArgs),
    #[command(about = "Run a local command through Merry's process action protocol")]
    Shell(ShellArgs),
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(long, help = "Print runtime events and final result as JSONL")]
    events_jsonl: bool,

    #[arg(required = true, allow_hyphen_values = true, value_name = "TASK")]
    task: String,
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
        name = "permission-network-smoke",
        about = "Run an opt-in sandboxed permission review smoke driven by a live OpenAI-compatible model",
        after_help = OPENAI_ENV_HELP
    )]
    PermissionNetworkSmoke(DebugPermissionNetworkSmokeArgs),
    #[command(
        name = "coding-loop-live-smoke",
        about = "Run an opt-in sandboxed coding-loop smoke driven by a live OpenAI-compatible model"
    )]
    CodingLoopLiveSmoke(DebugCodingLoopLiveSmokeArgs),
    #[command(
        name = "coding-loop-task-smoke",
        about = "Run an opt-in sandboxed coding-loop task smoke with deterministic model steps"
    )]
    CodingLoopTaskSmoke(DebugCodingLoopTaskSmokeArgs),
    #[command(
        name = "coding-loop-task-live-smoke",
        about = "Run an opt-in sandboxed coding-loop task smoke driven by a live OpenAI-compatible model"
    )]
    CodingLoopTaskLiveSmoke(DebugCodingLoopTaskLiveSmokeArgs),
    #[command(
        name = "coding-loop-subagent-live-smoke",
        about = "Run an opt-in sandboxed coding-loop smoke that requires a live model to delegate to a child agent"
    )]
    CodingLoopSubagentLiveSmoke(DebugCodingLoopSubagentLiveSmokeArgs),
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

#[derive(Debug, Args)]
struct DebugPermissionNetworkSmokeArgs {
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
        default_value_t = 768,
        help = "Maximum output tokens for each live model step"
    )]
    max_output_tokens: u64,
}

#[derive(Debug, Args)]
struct DebugCodingLoopTaskSmokeArgs {
    #[arg(
        long,
        value_enum,
        default_value = "status-text",
        help = "Disposable coding task fixture to run"
    )]
    task: CodingLoopTaskSmokeTask,
}

#[derive(Debug, Args)]
struct DebugCodingLoopTaskLiveSmokeArgs {
    #[arg(
        long,
        value_enum,
        default_value = "status-text",
        help = "Disposable coding task fixture to run"
    )]
    task: CodingLoopTaskSmokeTask,

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
        default_value_t = 768,
        help = "Maximum output tokens for each live model step"
    )]
    max_output_tokens: u64,
}

#[derive(Debug, Args)]
struct DebugCodingLoopSubagentLiveSmokeArgs {
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
        default_value_t = 768,
        help = "Maximum output tokens for each live model step"
    )]
    max_output_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CodingLoopTaskSmokeTask {
    StatusText,
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

    if let Err(error) =
        sandbox::maybe_reexec(cli.with_sandbox, argv.iter().skip(1).cloned().collect())
    {
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
        CliCommand::Run(args) => {
            match run_merry_run(&args, sandbox_child_handoff, merry_config.as_ref()).await {
                Ok(()) => CliExit::Success,
                Err(CliError::BrokenPipe) => CliExit::Success,
                Err(CliError::DebugUsage(message)) => CliExit::Usage {
                    message,
                    usage: run_usage(),
                },
                Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                    message,
                    usage: run_usage(),
                },
                Err(CliError::ShellUsage(message)) => CliExit::Usage {
                    message,
                    usage: shell_usage(),
                },
                Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
            }
        }
        CliCommand::Cmd(args) => match cmd::run(&args, merry_config.as_ref()).await {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: cmd_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: cmd_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Debug(DebugArgs {
            session_id,
            input,
            command: None,
        }) => match run_debug(&session_id, &input, merry_config.as_ref()).await {
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
        }) => match run_debug_coding_loop_smoke(sandbox_child_handoff, merry_config.as_ref()).await
        {
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
                Some(DebugCommand::PermissionNetworkSmoke(DebugPermissionNetworkSmokeArgs {
                    model,
                    max_output_tokens,
                })),
            ..
        }) => {
            match run_debug_permission_network_smoke(
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
                    usage: debug_openai_usage(),
                },
                Err(CliError::ShellUsage(message)) => CliExit::Usage {
                    message,
                    usage: shell_usage(),
                },
                Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
            }
        }
        CliCommand::Debug(DebugArgs {
            command: Some(DebugCommand::CodingLoopTaskSmoke(args)),
            ..
        }) => match run_debug_coding_loop_task_smoke(
            sandbox_child_handoff,
            args.task,
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
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::CodingLoopTaskLiveSmoke(DebugCodingLoopTaskLiveSmokeArgs {
                    task,
                    model,
                    max_output_tokens,
                })),
            ..
        }) => match run_debug_coding_loop_task_live_smoke(
            sandbox_child_handoff,
            task,
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
                usage: debug_coding_loop_task_live_smoke_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::CodingLoopSubagentLiveSmoke(DebugCodingLoopSubagentLiveSmokeArgs {
                    model,
                    max_output_tokens,
                })),
            ..
        }) => match run_debug_coding_loop_subagent_live_smoke(
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
                usage: debug_coding_loop_subagent_live_smoke_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Shell(args) => {
            match run_shell(args, sandbox_child_handoff, merry_config.as_ref()).await {
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
            }
        }
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
    let _ = automatic_compaction_config(Some(config))?;
    let _ = subagents_config(Some(config))?;
    let _ = config.trusted_global_path_rules()?;
    let _ = config.skill_roots()?;
    let _ = config.runtime_models()?;
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

fn automatic_compaction_config(
    config: Option<&MerryConfig>,
) -> Result<AutomaticCompactionConfig, config::ConfigError> {
    config
        .map(MerryConfig::automatic_compaction_config)
        .transpose()
        .map(Option::unwrap_or_default)
}

fn subagents_config(
    config: Option<&MerryConfig>,
) -> Result<config::SubagentsConfig, config::ConfigError> {
    config
        .map(MerryConfig::subagents_config)
        .transpose()
        .map(Option::unwrap_or_default)
}

fn configured_runtime_builder(
    session_id: SessionId,
    config: Option<&MerryConfig>,
) -> Result<RuntimeBuilder, CliError> {
    Ok(Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction_config(config).map_err(unexpected)?))
}

fn with_workspace_coding_loop_profile(
    builder: RuntimeBuilder,
    profile: WorkspaceCodingLoopProfile,
) -> Result<RuntimeBuilder, CliError> {
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .map_err(unexpected)?
        .build()
        .map_err(unexpected)?;
    builder.with_profile(profile).map_err(unexpected)
}

fn with_workspace_coding_loop_profile_for_child(
    builder: RuntimeBuilder,
    profile: WorkspaceCodingLoopProfile,
) -> Result<RuntimeBuilder, merry_runtime::RuntimeError> {
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
            reason: "child workspace coding loop profile application failed",
        })?
        .build()
        .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
            reason: "child runtime profile build failed",
        })?;
    builder.with_profile(profile)
}

async fn run_debug(
    session_id: &str,
    input: &str,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let session_id = SessionId::new(session_id).map_err(usage_error)?;
    let runtime = configured_runtime_builder(session_id, merry_config)?
        .build()
        .map_err(unexpected)?;
    let input = StepInput::user_text(input).map_err(usage_error)?;
    write_runtime_step_events(&runtime, input, StepContext::default(), tokio::io::stdout()).await
}

async fn run_merry_run(
    args: &RunArgs,
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
        write_run_agent_loop_jsonl_output(
            &runtime,
            input,
            coding_agent_loop_config()?,
            tokio::io::stdout(),
        )
        .await
    } else {
        write_run_agent_loop_output(
            &runtime,
            input,
            coding_agent_loop_config()?,
            tokio::io::stdout(),
        )
        .await
    }
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
    let model = ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?;
    let provider = OpenAiProvider::new(config.primary.provider);
    let mut builder = configured_runtime_builder(session_id, merry_config)?
        .model_provider(Arc::new(provider), model);
    builder = apply_openai_context_compaction_provider(builder, config.context_compaction)?;
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

fn coding_agent_loop_config() -> Result<AgentLoopConfig, CliError> {
    AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).map_err(unexpected)
}

async fn run_debug_coding_loop_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-smoke",
        ));
    };

    let smoke_root = prepare_coding_loop_smoke_fixture("coding-loop-smoke")?;
    let backend = action_process_runner(&smoke_root, merry_config)?;
    let runtime = build_coding_loop_smoke_runtime(
        &smoke_root,
        None,
        admission,
        backend.runner(),
        Some(backend.permissioned_factory()),
        automatic_compaction_config(merry_config).map_err(unexpected)?,
    )?;

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Run the sandboxed coding-loop smoke.").map_err(unexpected)?,
            StepContext::default(),
            coding_agent_loop_config()?,
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

async fn run_debug_permission_network_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    model_flag: Option<&str>,
    max_output_tokens: u64,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "permission-network-smoke",
        ));
    };

    let config = debug_openai_config(model_flag, merry_config)?;
    let smoke_root = prepare_coding_loop_smoke_fixture(PERMISSION_NETWORK_SMOKE_SESSION_ID)?;
    let backend = permission_network_smoke_process_runner(&smoke_root, merry_config)?;
    let runtime = build_permission_network_smoke_runtime(
        &smoke_root,
        admission,
        config,
        backend.runner(),
        backend.permissioned_factory(),
        automatic_compaction_config(merry_config).map_err(unexpected)?,
    )?;
    let generation_config =
        GenerationConfig::new(Some(max_output_tokens), false).map_err(debug_openai_usage_error)?;
    let context = StepContext::default().with_generation_config(generation_config);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&permission_network_live_smoke_task()).map_err(unexpected)?,
            context,
            AgentLoopConfig::new(6).map_err(unexpected)?,
        )
        .await
        .map_err(unexpected)?;

    assert_permission_network_smoke_result(&runtime, &result).await?;

    let mut writer = BufWriter::new(tokio::io::stdout());
    write_permission_network_smoke_report(&runtime, result.events(), &mut writer).await?;
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
    let skill_roots = merry_config
        .map(MerryConfig::skill_roots)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let backend = action_process_runner(&smoke_root, merry_config)?;
    let runtime = build_coding_loop_live_smoke_runtime(
        &smoke_root,
        admission,
        config,
        backend.runner(),
        Some(backend.permissioned_factory()),
        CodingLoopLiveRuntimeOptions {
            automatic_compaction: automatic_compaction_config(merry_config).map_err(unexpected)?,
            skill_roots,
            subagents: subagents_config(merry_config).map_err(unexpected)?,
        },
    )?;
    let generation_config =
        GenerationConfig::new(Some(max_output_tokens), false).map_err(debug_openai_usage_error)?;
    let context = StepContext::default().with_generation_config(generation_config);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&coding_loop_live_smoke_task(None)).map_err(unexpected)?,
            context,
            coding_agent_loop_config()?,
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

async fn run_debug_coding_loop_task_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    task: CodingLoopTaskSmokeTask,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-task-smoke",
        ));
    };

    let fixture = CodingLoopTaskSmokeFixture::for_task(task);
    let smoke_root = prepare_coding_loop_task_fixture("coding-loop-task-smoke", fixture)?;
    let backend = action_process_runner(&smoke_root, merry_config)?;
    let runtime = build_coding_loop_task_smoke_runtime(
        &smoke_root,
        None,
        admission,
        backend.runner(),
        Some(backend.permissioned_factory()),
        fixture,
        automatic_compaction_config(merry_config).map_err(unexpected)?,
    )?;

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(fixture.task_prompt()).map_err(unexpected)?,
            StepContext::default(),
            coding_agent_loop_config()?,
        )
        .await
        .map_err(unexpected)?;

    assert_coding_loop_task_smoke_result(&runtime, &result, &smoke_root, fixture).await?;

    let mut writer = BufWriter::new(tokio::io::stdout());
    writer
        .write_all(b"coding-loop-task-smoke: ok\n")
        .await
        .map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

async fn run_debug_coding_loop_task_live_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    task: CodingLoopTaskSmokeTask,
    model_flag: Option<&str>,
    max_output_tokens: u64,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-task-live-smoke",
        ));
    };
    let config = debug_openai_config(model_flag, merry_config)?;

    let fixture = CodingLoopTaskSmokeFixture::for_task(task);
    let smoke_root = prepare_coding_loop_task_fixture("coding-loop-task-live-smoke", fixture)?;
    let automatic_compaction = automatic_compaction_config(merry_config).map_err(unexpected)?;
    let skill_roots = merry_config
        .map(MerryConfig::skill_roots)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let backend = action_process_runner(&smoke_root, merry_config)?;
    let runtime = build_coding_loop_task_live_smoke_runtime(
        &smoke_root,
        admission,
        config,
        backend.runner(),
        Some(backend.permissioned_factory()),
        CodingLoopLiveRuntimeOptions {
            automatic_compaction,
            skill_roots,
            subagents: subagents_config(merry_config).map_err(unexpected)?,
        },
    )?;
    let generation_config =
        GenerationConfig::new(Some(max_output_tokens), false).map_err(debug_openai_usage_error)?;
    let context = StepContext::default().with_generation_config(generation_config);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&fixture.live_task_prompt(None)).map_err(unexpected)?,
            context,
            coding_agent_loop_config()?,
        )
        .await
        .map_err(unexpected)?;

    let assertion = async {
        assert_coding_loop_task_live_smoke_result(&runtime, &result, &smoke_root, fixture).await?;
        assert_coding_loop_task_live_smoke_tool_sequence(&runtime, result.events(), fixture).await
    }
    .await;

    let mut writer = BufWriter::new(tokio::io::stdout());
    write_coding_loop_task_live_smoke_report(
        &runtime,
        automatic_compaction,
        assertion.is_ok(),
        result.events(),
        &mut writer,
    )
    .await?;
    writer.flush().await.map_err(stdout_error)?;

    assertion
}

async fn run_debug_coding_loop_subagent_live_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    model_flag: Option<&str>,
    max_output_tokens: u64,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-subagent-live-smoke",
        ));
    };
    let config = debug_openai_config(model_flag, merry_config)?;

    let smoke_root = prepare_coding_loop_subagent_live_smoke_fixture()?;
    let automatic_compaction = automatic_compaction_config(merry_config).map_err(unexpected)?;
    let skill_roots = merry_config
        .map(MerryConfig::skill_roots)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let backend = action_process_runner(&smoke_root, merry_config)?;
    let runtime = build_coding_loop_subagent_live_smoke_runtime(
        &smoke_root,
        admission,
        config,
        backend.runner(),
        Some(backend.permissioned_factory()),
        CodingLoopLiveRuntimeOptions {
            automatic_compaction,
            skill_roots,
            subagents: coding_loop_subagent_live_smoke_config()?,
        },
    )?;
    let generation_config =
        GenerationConfig::new(Some(max_output_tokens), false).map_err(debug_openai_usage_error)?;
    let context = StepContext::default().with_generation_config(generation_config);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&coding_loop_subagent_live_smoke_task()).map_err(unexpected)?,
            context,
            coding_agent_loop_config()?,
        )
        .await
        .map_err(unexpected)?;

    let assertion =
        assert_coding_loop_subagent_live_smoke_result(&runtime, &result, &smoke_root).await;

    let mut writer = BufWriter::new(tokio::io::stdout());
    write_coding_loop_subagent_live_smoke_report(
        &runtime,
        assertion.is_ok(),
        result.events(),
        &smoke_root,
        &mut writer,
    )
    .await?;
    writer.flush().await.map_err(stdout_error)?;

    assertion
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

fn coding_agent_requires_sandbox_error(command: &str) -> CliError {
    CliError::DebugUsage(format!(
        "merry {command} must run via `merry --with-sandbox {command}`"
    ))
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

async fn assert_permission_network_smoke_result(
    runtime: &Runtime,
    result: &merry_runtime::AgentLoopResult,
) -> Result<(), CliError> {
    if result.status() != &AgentLoopStatus::Completed {
        return Err(CliError::Unexpected(format!(
            "permission-network-smoke did not complete: {:?}",
            result.status()
        )));
    }
    if !runtime.pending_tool_calls().await.is_empty() {
        return Err(CliError::Unexpected(
            "permission-network-smoke left pending tool calls".to_owned(),
        ));
    }

    let mut pending_by_call_id = BTreeMap::new();
    let mut saw_initial_failed_network_attempt = false;
    let mut saw_approved_successful_network_attempt = false;
    for event in result.events() {
        match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
            }
            RuntimeEventKind::ToolCallResolved { result } => {
                let call = pending_by_call_id.get(result.call_id()).ok_or_else(|| {
                    CliError::Unexpected(format!(
                        "permission-network-smoke resolved unknown tool call {}",
                        result.call_id()
                    ))
                })?;
                let content = runtime
                    .read_artifact_content(result.artifact().id())
                    .await
                    .map_err(unexpected)?;
                let Some(text) = content.as_text() else {
                    continue;
                };
                if !text.contains("\"kind\":\"process_action\"")
                    || !process_artifact_has_argv(text, PERMISSION_NETWORK_SMOKE_ARGV)
                {
                    continue;
                }

                match (call.name().as_str(), result.status()) {
                    (CODING_LOOP_PROCESS_TOOL, ToolCallResultStatus::Failed) => {
                        saw_initial_failed_network_attempt = true;
                    }
                    ("request_permissions", ToolCallResultStatus::Succeeded) => {
                        saw_approved_successful_network_attempt = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if !saw_initial_failed_network_attempt {
        return Err(CliError::Unexpected(
            "permission-network-smoke did not observe the default sandbox blocking the initial network lookup".to_owned(),
        ));
    }
    if !saw_approved_successful_network_attempt {
        return Err(CliError::Unexpected(format!(
            "permission-network-smoke did not observe the approved network lookup succeeding{}",
            failed_tool_result_summary(result.events())
                .map(|summary| format!("; {summary}"))
                .unwrap_or_default()
        )));
    }

    Ok(())
}

async fn assert_coding_loop_task_smoke_result(
    runtime: &Runtime,
    result: &merry_runtime::AgentLoopResult,
    smoke_root: &Path,
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<(), CliError> {
    if result.status() != &AgentLoopStatus::Completed {
        return Err(CliError::Unexpected(format!(
            "coding-loop-task-smoke did not complete: {:?}",
            result.status()
        )));
    }
    if !runtime.pending_tool_calls().await.is_empty() {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke left pending tool calls".to_owned(),
        ));
    }

    let statuses = result
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result.status()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if statuses.len() < 5 {
        return Err(CliError::Unexpected(format!(
            "coding-loop-task-smoke expected at least 5 resolved tool calls, saw {}",
            statuses.len()
        )));
    }
    if !statuses.contains(&ToolCallResultStatus::Failed) {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke did not observe the initial failing verification".to_owned(),
        ));
    }
    if statuses.last().copied() != Some(ToolCallResultStatus::Succeeded) {
        return Err(CliError::Unexpected(format!(
            "coding-loop-task-smoke final verification did not succeed{}",
            failed_tool_result_summary(result.events())
                .map(|summary| format!("; {summary}"))
                .unwrap_or_default()
        )));
    }

    let patched = fs::read_to_string(smoke_root.join("src/lib.rs")).map_err(unexpected)?;
    if patched != fixture.patched_source() {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke fixture was not patched as expected".to_owned(),
        ));
    }
    assert_coding_loop_task_smoke_uses_small_patch(result.events(), fixture)?;
    Ok(())
}

async fn assert_coding_loop_task_live_smoke_result(
    runtime: &Runtime,
    result: &merry_runtime::AgentLoopResult,
    smoke_root: &Path,
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<(), CliError> {
    if result.status() != &AgentLoopStatus::Completed {
        return Err(CliError::Unexpected(format!(
            "coding-loop-task-live-smoke did not complete: {:?}",
            result.status()
        )));
    }
    if !runtime.pending_tool_calls().await.is_empty() {
        return Err(CliError::Unexpected(
            "coding-loop-task-live-smoke left pending tool calls".to_owned(),
        ));
    }

    let patched = fs::read_to_string(smoke_root.join("src/lib.rs")).map_err(unexpected)?;
    if !fixture.source_satisfies_task(&patched) {
        return Err(CliError::Unexpected(
            "coding-loop-task-live-smoke fixture source does not satisfy the status-text task"
                .to_owned(),
        ));
    }
    assert_coding_loop_task_smoke_uses_small_patch(result.events(), fixture)?;
    Ok(())
}

async fn assert_coding_loop_subagent_live_smoke_result(
    runtime: &Runtime,
    result: &merry_runtime::AgentLoopResult,
    smoke_root: &Path,
) -> Result<(), CliError> {
    if result.status() != &AgentLoopStatus::Completed {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke did not complete: {:?}",
            result.status()
        )));
    }
    if !runtime.pending_tool_calls().await.is_empty() {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke left pending tool calls".to_owned(),
        ));
    }

    let patched = fs::read_to_string(smoke_root.join(CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE))
        .map_err(unexpected)?;
    if patched != CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke fixture `{CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE}` was not patched by the child"
        )));
    }

    assert_coding_loop_subagent_live_smoke_tool_sequence(runtime, result.events()).await
}

fn assert_coding_loop_task_smoke_uses_small_patch(
    events: &[RuntimeEvent],
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<(), CliError> {
    let mut pending_patch_args = BTreeMap::new();
    for event in events {
        if let RuntimeEventKind::ToolCallPending { call } = &event.kind
            && call.name().as_str() == WORKSPACE_PATCH_TOOL
        {
            let arguments = call.arguments().as_object();
            let Some(patch) = arguments.get("patch").and_then(serde_json::Value::as_str) else {
                continue;
            };
            pending_patch_args.insert(call.id().clone(), patch);
        }
    }

    let Some(patch) = events.iter().find_map(|event| {
        let RuntimeEventKind::ToolCallResolved { result } = &event.kind else {
            return None;
        };
        if result.status() != ToolCallResultStatus::Succeeded {
            return None;
        }
        pending_patch_args.get(result.call_id()).copied()
    }) else {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke did not observe successful workspace patch arguments"
                .to_owned(),
        ));
    };

    let initial_source = fixture.initial_source();
    let patched_source = fixture.patched_source();
    if patch.contains(&initial_source) || patch.contains(&patched_source) {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke used whole-file workspace patch text".to_owned(),
        ));
    }

    if !workspace_patch_envelope_is_accepted(patch)
        || !patch.contains("*** Update File: src/lib.rs\n")
    {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke did not use a workspace patch envelope".to_owned(),
        ));
    }

    if patch.len() > CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES {
        return Err(CliError::Unexpected(format!(
            "coding-loop-task-smoke patch payload was too large: {} bytes",
            patch.len()
        )));
    }

    Ok(())
}

fn workspace_patch_envelope_is_accepted(patch: &str) -> bool {
    (patch.starts_with("*** Begin Workspace Patch\n") && patch.ends_with("*** End Workspace Patch"))
        || (patch.starts_with("*** Begin Patch\n") && patch.ends_with("*** End Patch"))
}

async fn assert_coding_loop_subagent_live_smoke_tool_sequence(
    runtime: &Runtime,
    events: &[RuntimeEvent],
) -> Result<(), CliError> {
    let mut pending_by_call_id = BTreeMap::new();
    let mut resolved_tool_names = Vec::new();
    let mut resolved_read_paths = Vec::new();
    let mut spawn_call_id = None;
    let mut wait_call_id = None;
    let mut parent_patch_call_seen = false;
    for event in events {
        match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
                match call.name().as_str() {
                    "spawn_subagents" => spawn_call_id = Some(call.id().clone()),
                    "wait_subagents" => wait_call_id = Some(call.id().clone()),
                    WORKSPACE_PATCH_TOOL => parent_patch_call_seen = true,
                    _ => {}
                }
            }
            RuntimeEventKind::ToolCallResolved { result } => {
                if result.status() != ToolCallResultStatus::Succeeded {
                    return Err(CliError::Unexpected(format!(
                        "coding-loop-subagent-live-smoke tool call {} did not succeed",
                        result.call_id()
                    )));
                }
                let call = pending_by_call_id.get(result.call_id()).ok_or_else(|| {
                    CliError::Unexpected(format!(
                        "coding-loop-subagent-live-smoke resolved unknown tool call {}",
                        result.call_id()
                    ))
                })?;
                resolved_tool_names.push(call.name().as_str().to_owned());
                if call.name().as_str() == WORKSPACE_READ_FILE_TOOL
                    && result.status() == ToolCallResultStatus::Succeeded
                    && let Some(path) = call
                        .arguments()
                        .as_object()
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                {
                    resolved_read_paths.push(path.to_owned());
                }
            }
            _ => {}
        }
    }

    require_live_smoke_tool_name(&resolved_tool_names, "spawn_subagents")?;
    require_live_smoke_tool_name(&resolved_tool_names, "wait_subagents")?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_READ_FILE_TOOL)?;
    if !resolved_read_paths
        .iter()
        .any(|path| path == CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE)
    {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke did not read back the child-edited fixture".to_owned(),
        ));
    }
    if parent_patch_call_seen {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke parent should not patch the fixture".to_owned(),
        ));
    }

    let Some(spawn_call_id) = spawn_call_id else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke did not call spawn_subagents".to_owned(),
        ));
    };
    let Some(wait_call_id) = wait_call_id else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke did not call wait_subagents".to_owned(),
        ));
    };

    let spawn_call = pending_by_call_id
        .get(&spawn_call_id)
        .ok_or_else(|| CliError::Unexpected("spawn_subagents call was not recorded".to_owned()))?;
    let spawn_args = spawn_call.arguments().as_object();
    let Some(tasks) = spawn_args
        .get("tasks")
        .and_then(serde_json::Value::as_array)
    else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke spawn args did not include tasks".to_owned(),
        ));
    };
    if tasks.len() != 1 {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke expected exactly one child task, saw {}",
            tasks.len()
        )));
    }
    let task = tasks[0].as_object().ok_or_else(|| {
        CliError::Unexpected(
            "coding-loop-subagent-live-smoke child task payload was not an object".to_owned(),
        )
    })?;
    let read_scope = task
        .get("read_scope")
        .and_then(serde_json::Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !read_scope.contains(&CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE) {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke child task should be scoped to read the fixture file"
                .to_owned(),
        ));
    }
    let write_scope = task
        .get("write_scope")
        .and_then(serde_json::Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !write_scope.contains(&CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE) {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke child task should be scoped to the fixture file"
                .to_owned(),
        ));
    }

    let wait_call = pending_by_call_id
        .get(&wait_call_id)
        .ok_or_else(|| CliError::Unexpected("wait_subagents call was not recorded".to_owned()))?;
    let wait_args = wait_call.arguments().as_object();
    let Some(agent_ids) = wait_args
        .get("agent_ids")
        .and_then(serde_json::Value::as_array)
    else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke wait args did not include agent_ids".to_owned(),
        ));
    };
    if agent_ids.len() != 1 {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke expected exactly one waited child, saw {}",
            agent_ids.len()
        )));
    }
    let wait_mode = wait_args.get("mode").and_then(serde_json::Value::as_str);
    if wait_mode != Some("all") {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke wait mode was not `all`".to_owned(),
        ));
    }
    let Some(first_agent_id) = agent_ids.first().and_then(serde_json::Value::as_str) else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke wait args did not contain a child agent id".to_owned(),
        ));
    };
    if !first_agent_id.starts_with("agent-") {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke child agent id was not runtime-generated".to_owned(),
        ));
    }

    let Some(snapshot) = runtime.subagent_snapshot().await else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke runtime did not expose a subagent snapshot".to_owned(),
        ));
    };
    if snapshot.len() != 1 {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke expected one subagent in the snapshot, saw {}",
            snapshot.len()
        )));
    }
    let child = &snapshot[0];
    if child.status.as_str() != "completed" {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke child status was not completed: {:?}",
            child.status
        )));
    }
    if child.agent_id.as_str() != first_agent_id {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke snapshot child id {} did not match wait target {}",
            child.agent_id.as_str(),
            first_agent_id
        )));
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
        return Err(CliError::Unexpected(format!(
            "coding-loop-smoke had a failed tool result{}",
            failed_tool_result_summary(events)
                .map(|summary| format!("; {summary}"))
                .unwrap_or_default()
        )));
    }
    Ok(())
}

fn failed_tool_result_summary(events: &[RuntimeEvent]) -> Option<String> {
    let mut pending_by_call_id = BTreeMap::new();
    for event in events {
        if let RuntimeEventKind::ToolCallPending { call } = &event.kind {
            pending_by_call_id.insert(call.id().clone(), call.name().clone());
        }
    }

    events.iter().find_map(|event| {
        let RuntimeEventKind::ToolCallResolved { result } = &event.kind else {
            return None;
        };
        if result.status() != ToolCallResultStatus::Failed {
            return None;
        }
        let tool_name = pending_by_call_id
            .get(result.call_id())
            .map_or("<unknown>", ToolName::as_str);
        let diagnostic = result
            .diagnostic()
            .map(|diagnostic| {
                format!(
                    "diagnostic={} message={}",
                    diagnostic.code(),
                    diagnostic.message()
                )
            })
            .unwrap_or_else(|| "diagnostic=<none>".to_owned());
        Some(format!(
            "tool={tool_name} call_id={} {diagnostic}",
            result.call_id()
        ))
    })
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

fn prepare_coding_loop_subagent_live_smoke_fixture() -> Result<PathBuf, CliError> {
    let root = prepare_coding_loop_smoke_fixture(CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID)?;
    fs::write(
        root.join(CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE),
        CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL,
    )
    .map_err(unexpected)?;
    Ok(root)
}

#[derive(Debug, Clone, Copy)]
struct CodingLoopTaskSmokeFixture {
    task: CodingLoopTaskSmokeTask,
}

impl CodingLoopTaskSmokeFixture {
    const fn for_task(task: CodingLoopTaskSmokeTask) -> Self {
        Self { task }
    }

    const fn package_name(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => "merry-coding-loop-task-status-text",
        }
    }

    const fn crate_name(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => "merry_coding_loop_task_status_text",
        }
    }

    fn initial_source(self) -> String {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => status_text_fixture_source("todo"),
        }
    }

    fn patched_source(self) -> String {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => status_text_fixture_source("done"),
        }
    }

    const fn patch_remove_line(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                "    Entry { key: \"status\", value: \"todo\" },"
            }
        }
    }

    const fn patch_add_line(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                "    Entry { key: \"status\", value: \"done\" },"
            }
        }
    }

    fn patch_text(self) -> String {
        format!(
            "*** Begin Workspace Patch\n*** Update File: src/lib.rs\n-{}\n+{}\n*** End Workspace Patch",
            self.patch_remove_line(),
            self.patch_add_line()
        )
    }

    const fn test_source(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                "Expected behavior: src/lib.rs exposes status() returning the completed status text.\n"
            }
        }
    }

    fn agents_source(self) -> String {
        format!(
            "\
# AGENTS.md

This is a disposable Rust fixture used by Merry's live coding-loop smoke.

Project rules:
- Inspect the repository before editing.
- Read `tests/status.rs` to understand the required behavior.
- Fix implementation code in `src/lib.rs`; do not edit tests or `Cargo.toml`.
- Use a localized source edit; do not rewrite whole files for small fixes.
- After editing, run `cargo check -p {package}` and `cargo test -p {package}`.
- Report the checks you ran and whether they passed.
",
            package = self.package_name()
        )
    }

    fn integration_test_source(self) -> String {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => format!(
                "use {}::{{default_status, preview_status, status}};\n\n#[test]\nfn status_returns_done_without_changing_related_entries() {{\n    assert_eq!(default_status(), \"todo\");\n    assert_eq!(status(), \"done\");\n    assert_eq!(preview_status(), \"todo\");\n}}\n",
                self.crate_name()
            ),
        }
    }

    const fn task_prompt(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                "Fix the disposable Rust fixture so status() returns done. Inspect the workspace, verify the target text is initially missing from src/lib.rs with rg, read the source, apply one constrained patch, run rg again on src/lib.rs to verify, and then report the result."
            }
        }
    }

    fn live_task_prompt(self, _relative_cwd: Option<&str>) -> String {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                "Fix this disposable Rust project so the required status-text behavior is implemented. Use the available tools to inspect, edit, and verify before reporting completion.".to_owned()
            }
        }
    }

    fn source_satisfies_task(self, source: &str) -> bool {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                source.contains("Entry { key: \"default\", value: \"todo\" },")
                    && source.contains("Entry { key: \"status\", value: \"done\" },")
                    && source.contains("Entry { key: \"preview\", value: \"todo\" },")
                    && !source.contains("Entry { key: \"status\", value: \"todo\" },")
            }
        }
    }
}

fn status_text_fixture_source(status: &str) -> String {
    let mut source = format!(
        "#[derive(Debug, Clone, Copy)]\nstruct Entry {{\n    key: &'static str,\n    value: &'static str,\n}}\n\nconst ENTRIES: &[Entry] = &[\n    Entry {{ key: \"default\", value: \"todo\" }},\n    Entry {{ key: \"status\", value: \"{status}\" }},\n    Entry {{ key: \"preview\", value: \"todo\" }},\n];\n\npub fn default_status() -> &'static str {{\n    resolve(\"default\")\n}}\n\npub fn status() -> &'static str {{\n    resolve(\"status\")\n}}\n\npub fn preview_status() -> &'static str {{\n    resolve(\"preview\")\n}}\n\nfn resolve(key: &str) -> &'static str {{\n    ENTRIES\n        .iter()\n        .find(|entry| entry.key == key)\n        .map(|entry| entry.value)\n        .unwrap_or(\"missing\")\n}}\n\n"
    );
    for index in 1..=30 {
        source.push_str(&format!(
            "pub fn fixture_note_{index:03}() -> &'static str {{ \"context-{index:03}\" }}\n"
        ));
    }
    source
}

fn prepare_coding_loop_task_fixture(
    name: &str,
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<PathBuf, CliError> {
    let root = env::current_dir()
        .map_err(unexpected)?
        .join(".merry")
        .join("local")
        .join(name);
    if root.exists() {
        fs::remove_dir_all(&root).map_err(unexpected)?;
    }
    fs::create_dir_all(root.join("src")).map_err(unexpected)?;
    fs::create_dir_all(root.join("tests")).map_err(unexpected)?;
    fs::write(
        root.join("Cargo.toml"),
        coding_loop_task_fixture_manifest(fixture),
    )
    .map_err(unexpected)?;
    fs::write(root.join("src/lib.rs"), fixture.initial_source()).map_err(unexpected)?;
    fs::write(root.join("AGENTS.md"), fixture.agents_source()).map_err(unexpected)?;
    fs::write(
        root.join("tests/status.rs"),
        fixture.integration_test_source(),
    )
    .map_err(unexpected)?;
    fs::write(root.join("tests.md"), fixture.test_source()).map_err(unexpected)?;
    Ok(root)
}

fn coding_loop_task_fixture_manifest(fixture: CodingLoopTaskSmokeFixture) -> String {
    format!(
        "[package]\nname = \"{}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[workspace]\n",
        fixture.package_name()
    )
}

fn coding_loop_smoke_initial_source() -> &'static str {
    "pub fn greeting() -> &'static str {\n    \"unfixed\"\n}\n"
}

fn coding_loop_smoke_patched_source() -> &'static str {
    "pub fn greeting() -> &'static str {\n    \"fixed-by-live-llm\"\n}\n"
}

fn build_coding_loop_smoke_runtime(
    root: &Path,
    relative_cwd: Option<&str>,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let provider = CodingLoopSmokeProvider::new(relative_cwd)?;
    build_coding_loop_runtime(
        CODING_LOOP_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new("merry-coding-loop-smoke").map_err(unexpected)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction,
            retry_policy: None,
            context_compaction: None,
            permissioned_process_runner_factory,
            skill_roots: Vec::new(),
            subagents: config::SubagentsConfig::default(),
        },
    )
}

fn build_coding_loop_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let provider = OpenAiProvider::new(config.primary.provider);
    let context_compaction = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?;
    let approval_review = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?;
    build_coding_loop_runtime(
        CODING_LOOP_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: true,
            approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: config.retry_policy,
            context_compaction,
            permissioned_process_runner_factory,
            skill_roots: options.skill_roots,
            subagents: options.subagents,
        },
    )
}

fn build_coding_loop_task_smoke_runtime(
    root: &Path,
    relative_cwd: Option<&str>,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    fixture: CodingLoopTaskSmokeFixture,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let provider = CodingLoopTaskSmokeProvider::new(relative_cwd, fixture)?;
    build_coding_loop_runtime(
        CODING_LOOP_TASK_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new("merry-coding-loop-task-smoke").map_err(unexpected)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction,
            retry_policy: None,
            context_compaction: None,
            permissioned_process_runner_factory,
            skill_roots: Vec::new(),
            subagents: config::SubagentsConfig::default(),
        },
    )
}

fn build_coding_loop_task_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let provider = OpenAiProvider::new(config.primary.provider);
    let context_compaction = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?;
    let approval_review = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?;
    build_coding_loop_runtime(
        CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: true,
            approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: config.retry_policy,
            context_compaction,
            permissioned_process_runner_factory,
            skill_roots: options.skill_roots,
            subagents: options.subagents,
        },
    )
}

fn build_coding_loop_subagent_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let provider = OpenAiProvider::new(config.primary.provider);
    let context_compaction = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?;
    let approval_review = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?;
    build_coding_loop_runtime(
        CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: config.retry_policy,
            context_compaction,
            permissioned_process_runner_factory,
            skill_roots: options.skill_roots,
            subagents: options.subagents,
        },
    )
}

fn build_permission_network_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let session_id = SessionId::new(PERMISSION_NETWORK_SMOKE_SESSION_ID).map_err(unexpected)?;
    let provider = OpenAiProvider::new(config.primary.provider);
    let mut builder = Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction)
        .model_provider(
            Arc::new(provider),
            ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        );
    if let Some(role_provider) = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?
    {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    if let Some(role_provider) = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?
    {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }

    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(root, &[]),
        false,
        false,
        None,
    )?)
    .map_err(unexpected)?
    .with_cli_bwrap_permissioned_process_runner(
        admission,
        runner,
        permissioned_process_runner_factory,
    );
    let mut builder = with_workspace_coding_loop_profile(builder, profile)?;
    if let Some(policy) = config.retry_policy {
        builder = builder.model_retry_policy(policy);
    }
    builder.build().map_err(unexpected)
}

#[cfg(test)]
fn build_scripted_permission_network_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let session_id = SessionId::new(PERMISSION_NETWORK_SMOKE_SESSION_ID).map_err(unexpected)?;
    let provider = PermissionNetworkSmokeProvider::new()?;
    let review_provider = PermissionNetworkSmokeReviewProvider::new()?;
    let builder = Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction)
        .model_provider(
            Arc::new(provider),
            ModelName::new("merry-permission-network-smoke-scripted").map_err(unexpected)?,
        )
        .model_provider_for_role(
            RuntimeModelRole::ApprovalReview,
            Arc::new(review_provider),
            ModelName::new("merry-permission-network-smoke-review-scripted").map_err(unexpected)?,
        );

    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(root, &[]),
        false,
        false,
        None,
    )?)
    .map_err(unexpected)?
    .with_cli_bwrap_permissioned_process_runner(
        admission,
        runner,
        permissioned_process_runner_factory,
    );
    with_workspace_coding_loop_profile(builder, profile)?
        .build()
        .map_err(unexpected)
}

fn coding_loop_subagent_live_smoke_config() -> Result<config::SubagentsConfig, CliError> {
    Ok(config::SubagentsConfig::enabled(
        merry_runtime::SubagentConfig::new(2, 1).map_err(unexpected)?,
    ))
}

struct CodingLoopLiveRuntimeOptions {
    automatic_compaction: AutomaticCompactionConfig,
    skill_roots: Vec<PathBuf>,
    subagents: config::SubagentsConfig,
}

struct CodingLoopRuntimeOptions {
    allow_hidden_workspace_paths: bool,
    approval_review: Option<RuntimeRoleProviderConfig>,
    automatic_compaction: AutomaticCompactionConfig,
    retry_policy: Option<ModelRetryPolicy>,
    context_compaction: Option<RuntimeRoleProviderConfig>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    skill_roots: Vec<PathBuf>,
    subagents: config::SubagentsConfig,
}

struct HeadlessCodingRuntimeInput<'a> {
    session_id: &'a str,
    root: &'a Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    allow_hidden_workspace_paths: bool,
    automatic_compaction: AutomaticCompactionConfig,
    retry_policy: Option<ModelRetryPolicy>,
    context_compaction: Option<RuntimeRoleProviderConfig>,
    approval_review: Option<RuntimeRoleProviderConfig>,
    skill_roots: Vec<PathBuf>,
    subagents: config::SubagentsConfig,
}

fn build_headless_coding_runtime(
    input: HeadlessCodingRuntimeInput<'_>,
) -> Result<Runtime, CliError> {
    build_coding_loop_runtime(
        input.session_id,
        input.root,
        input.admission,
        input.provider,
        input.model,
        input.runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: input.allow_hidden_workspace_paths,
            approval_review: input.approval_review,
            automatic_compaction: input.automatic_compaction,
            retry_policy: input.retry_policy,
            context_compaction: input.context_compaction,
            permissioned_process_runner_factory: Some(input.permissioned_process_runner_factory),
            skill_roots: input.skill_roots,
            subagents: input.subagents,
        },
    )
}

struct RuntimeRoleProviderConfig {
    role: RuntimeModelRole,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
}

#[derive(Clone)]
struct ActionProcessBackend {
    runner: Arc<dyn ProcessRunner>,
    permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
}

impl ActionProcessBackend {
    fn runner(&self) -> Arc<dyn ProcessRunner> {
        Arc::clone(&self.runner)
    }

    fn permissioned_factory(&self) -> Arc<dyn PermissionedProcessRunnerFactory> {
        Arc::clone(&self.permissioned_factory)
    }
}

#[derive(Clone)]
struct CodingLoopChildRuntimeFactory {
    root: PathBuf,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    runner: Arc<dyn ProcessRunner>,
    permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    skill_roots: Vec<PathBuf>,
    allow_hidden_workspace_paths: bool,
}

impl CodingLoopChildRuntimeFactory {
    fn new(
        root: &Path,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        process_backend: ActionProcessBackend,
        skill_roots: Vec<PathBuf>,
        allow_hidden_workspace_paths: bool,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            admission,
            provider,
            model,
            runner: process_backend.runner(),
            permissioned_factory: process_backend.permissioned_factory(),
            skill_roots,
            allow_hidden_workspace_paths,
        }
    }
}

impl ChildRuntimeFactory for CodingLoopChildRuntimeFactory {
    fn build_child(
        &self,
        input: ChildRuntimeInput,
    ) -> Result<Runtime, merry_runtime::RuntimeError> {
        let allow_patch = input.allowed_tools.is_empty()
            || input
                .allowed_tools
                .iter()
                .any(|tool| tool.as_str() == WORKSPACE_PATCH_TOOL);
        let allow_local_workspace_process = input.allowed_tools.is_empty()
            || input
                .allowed_tools
                .iter()
                .any(|tool| tool.as_str() == CODING_LOOP_PROCESS_TOOL);
        let builder = Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(Arc::clone(&self.provider), self.model.clone());
        let mut profile = WorkspaceCodingLoopProfile::new(
            workspace_tools_config(
                coding_loop_workspace_roots(&self.root, &self.skill_roots),
                self.allow_hidden_workspace_paths,
                false,
                None,
            )
            .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
                reason: "child workspace tool config was invalid",
            })?,
        )
        .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
            reason: "child workspace coding loop profile was invalid",
        })?;
        if allow_patch {
            profile = profile.with_patch_tool();
        }
        profile = if allow_local_workspace_process {
            profile.with_cli_bwrap_permissioned_process_runner(
                self.admission,
                Arc::clone(&self.runner),
                Arc::clone(&self.permissioned_factory),
            )
        } else {
            profile.with_read_only_process_runner(Arc::clone(&self.runner))
        };
        with_workspace_coding_loop_profile_for_child(builder, profile)?.build()
    }
}

fn coding_loop_workspace_roots(root: &Path, skill_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = vec![root.to_path_buf()];
    roots.extend(skill_roots.iter().filter(|root| root.is_dir()).cloned());
    roots
}

fn workspace_tools_config(
    roots: Vec<PathBuf>,
    allow_hidden_workspace_paths: bool,
    task_smoke_patch_limit: bool,
    max_patch_bytes_override: Option<usize>,
) -> Result<WorkspaceToolsConfig, CliError> {
    let max_patch_bytes = max_patch_bytes_override.unwrap_or_else(|| {
        if task_smoke_patch_limit {
            CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES
        } else {
            WorkspaceToolLimits::default().max_patch_bytes
        }
    });
    Ok(WorkspaceToolsConfig::new(roots)
        .with_allow_hidden(allow_hidden_workspace_paths)
        .with_limits(WorkspaceToolLimits {
            max_patch_bytes,
            ..WorkspaceToolLimits::default()
        }))
}

fn build_coding_loop_runtime(
    session_id: &str,
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    runner: Arc<dyn ProcessRunner>,
    options: CodingLoopRuntimeOptions,
) -> Result<Runtime, CliError> {
    let parent_session_id = SessionId::new(session_id).map_err(unexpected)?;
    let permissioned_factory = options
        .permissioned_process_runner_factory
        .unwrap_or_else(|| {
            Arc::new(merry_runtime::StaticPermissionedProcessRunnerFactory::new(
                Arc::clone(&runner),
            ))
        });
    let mut builder = Runtime::builder(parent_session_id.clone())
        .automatic_compaction(options.automatic_compaction)
        .model_provider(Arc::clone(&provider), model.clone());
    if let Some(role_provider) = options.context_compaction {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    if let Some(role_provider) = options.approval_review {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    if !options.skill_roots.is_empty() {
        let catalog = merry_runtime::SkillCatalog::load_from_roots(options.skill_roots.clone())
            .map_err(unexpected)?;
        let skill_names = catalog
            .skills()
            .iter()
            .map(|skill| skill.name())
            .collect::<Vec<_>>();
        let skill_paths = catalog
            .skills()
            .iter()
            .map(|skill| skill.skill_md_path().display().to_string())
            .collect::<Vec<_>>();
        tracing::info!(
            event = "runtime.skill_catalog.load",
            session_id,
            configured_root_count = options.skill_roots.len(),
            readable_root_count = options
                .skill_roots
                .iter()
                .filter(|root| root.is_dir())
                .count(),
            skill_count = catalog.skills().len(),
            warning_count = catalog.warnings().len(),
            skill_names = ?skill_names,
            skill_paths = ?skill_paths,
            "runtime skill catalog loaded"
        );
        builder = builder.skill_catalog(catalog);
    }

    if options.subagents.is_enabled() {
        let factory = CodingLoopChildRuntimeFactory::new(
            root,
            admission,
            Arc::clone(&provider),
            model.clone(),
            ActionProcessBackend {
                runner: Arc::clone(&runner),
                permissioned_factory: Arc::clone(&permissioned_factory),
            },
            options.skill_roots.clone(),
            options.allow_hidden_workspace_paths,
        );
        let manager = SubagentManager::new(
            parent_session_id.clone(),
            options.subagents.limits(),
            Arc::new(factory),
        );
        let [spawn_tool, wait_tool, cancel_tool] =
            subagent_registered_tools(manager.clone()).map_err(unexpected)?;
        builder = builder
            .subagent_manager(manager)
            .register_tool(spawn_tool)
            .register_tool(wait_tool)
            .register_tool(cancel_tool);
        tracing::info!(
            event = "runtime.subagents.enabled",
            session_id,
            max_threads = options.subagents.limits().max_threads(),
            max_depth = options.subagents.limits().max_depth(),
            "runtime subagent tools registered"
        );
    }

    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(root, &options.skill_roots),
        options.allow_hidden_workspace_paths,
        session_id == CODING_LOOP_TASK_SMOKE_SESSION_ID
            || session_id == CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID,
        None,
    )?)
    .map_err(unexpected)?
    .with_patch_tool()
    .with_cli_bwrap_permissioned_process_runner(admission, runner, permissioned_factory);
    let mut builder = with_workspace_coding_loop_profile(builder, profile)?;
    if let Some(policy) = options.retry_policy {
        builder = builder.model_retry_policy(policy);
    }
    builder.build().map_err(unexpected)
}

fn action_process_runner(
    workspace_root: &Path,
    merry_config: Option<&MerryConfig>,
) -> Result<ActionProcessBackend, CliError> {
    let path_rules = merry_config
        .map(MerryConfig::trusted_global_path_rules)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let network_allowed = merry_config
        .map(MerryConfig::permissions_network_allowed)
        .unwrap_or(false);
    let mut runner = BwrapProcessRunner::new_at_workspace_root(workspace_root)
        .with_path_rules(path_rules.clone());
    if network_allowed {
        runner = runner.allow_network();
    }
    let mut permissioned_factory =
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(workspace_root)
            .with_path_rules(path_rules);
    if network_allowed {
        permissioned_factory = permissioned_factory.allow_base_network();
    }
    Ok(ActionProcessBackend {
        runner: Arc::new(runner),
        permissioned_factory: Arc::new(permissioned_factory),
    })
}

fn permission_network_smoke_process_runner(
    workspace_root: &Path,
    merry_config: Option<&MerryConfig>,
) -> Result<ActionProcessBackend, CliError> {
    let path_rules = merry_config
        .map(MerryConfig::trusted_global_path_rules)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let runner = BwrapProcessRunner::new_at_workspace_root(workspace_root)
        .with_path_rules(path_rules.clone());
    let permissioned_factory =
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(workspace_root)
            .with_path_rules(path_rules);
    Ok(ActionProcessBackend {
        runner: Arc::new(runner),
        permissioned_factory: Arc::new(permissioned_factory),
    })
}

fn permission_network_live_smoke_task() -> String {
    format!(
        "\
You are driving Merry's live permission-network smoke.

Use the available tools, one tool call per step. Do not answer from memory.

Required sequence:
1. Call `{process_tool}` with exactly this argv: [\"{program}\", \"{arg1}\", \"{arg2}\"].
2. The first process call is expected to fail because the default inner sandbox has no network.
3. If that first process call fails, call `request_permissions` for the exact same process action with requested network access:
   - reason: explain that the exact DNS lookup failed under the default inner sandbox and network is needed only for this smoke command.
   - requested: {{\"network\": true}}
   - for_action: {{\"kind\": \"process\", \"argv\": [\"{program}\", \"{arg1}\", \"{arg2}\"]}}
4. After `request_permissions` resolves, inspect the tool result. It should execute the exact planned process action under the approved per-action network profile.
5. Return a concise final answer only after the approved process result succeeds.

Constraints:
- Do not request any filesystem path permission.
- Do not request network before the first process attempt fails.
- Do not use shell strings, scripts, pipelines, env, stdin, git, cargo, curl, wget, or any command other than the exact argv above.
- Do not call any workspace patch/write tool.
",
        process_tool = CODING_LOOP_PROCESS_TOOL,
        program = PERMISSION_NETWORK_SMOKE_ARGV[0],
        arg1 = PERMISSION_NETWORK_SMOKE_ARGV[1],
        arg2 = PERMISSION_NETWORK_SMOKE_ARGV[2],
    )
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
3. Call `{patch_tool}` with one `patch` string:
   *** Begin Workspace Patch
   *** Update File: src/lib.rs
   -    \"{initial}\"
   +    \"{target}\"
   *** End Workspace Patch
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
        process_tool = CODING_LOOP_PROCESS_TOOL,
        read_tool = WORKSPACE_READ_FILE_TOOL,
        patch_tool = WORKSPACE_PATCH_TOOL,
        initial = CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE,
        target = CODING_LOOP_LIVE_SMOKE_TARGET_VALUE,
    )
}

fn coding_loop_subagent_live_smoke_task() -> String {
    format!(
        "\
You are driving Merry's minimal live subagent smoke.

You must delegate the work to a child agent before you finish.

Required sequence:
1. Call `spawn_subagents` with exactly one child task.
2. The child task must use `workspace_read_file` and `workspace_patch` only.
3. The child task must read `{file}` and patch it from:
   {initial}to:
   {target}
4. The child task must declare `allowed_tools` as `[\"workspace_read_file\", \"workspace_patch\"]`.
5. The child task must declare `read_scope` and `write_scope` as `[\"{file}\"]`.
6. After spawning, call `wait_subagents` for the returned child id with mode `all`.
7. After the child reports completion, call `workspace_read_file` on `{file}` and verify the exact final content.
8. Return a concise final answer only after the verification read succeeds.

Constraints:
- The parent agent must not patch the fixture directly.
- Do not use more than one child task.
- Do not answer from memory.
- Keep the final result short.
",
        file = CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE,
        initial = CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL,
        target = CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET,
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

    require_live_smoke_tool_name(&resolved_tool_names, CODING_LOOP_PROCESS_TOOL)?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_READ_FILE_TOOL)?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_PATCH_TOOL)?;

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

async fn assert_coding_loop_task_live_smoke_tool_sequence(
    runtime: &Runtime,
    events: &[RuntimeEvent],
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<(), CliError> {
    let mut pending_by_call_id = BTreeMap::new();
    let mut resolved_tool_names = Vec::new();
    let mut resolved_artifacts = Vec::new();
    let mut resolved_read_paths = Vec::new();
    for event in events {
        match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
            }
            RuntimeEventKind::ToolCallResolved { result } => {
                let call = pending_by_call_id.get(result.call_id()).ok_or_else(|| {
                    CliError::Unexpected(format!(
                        "task live smoke resolved unknown tool call {}",
                        result.call_id()
                    ))
                })?;
                resolved_tool_names.push(call.name().as_str().to_owned());
                if result.status() == ToolCallResultStatus::Succeeded
                    && call.name().as_str() == WORKSPACE_READ_FILE_TOOL
                    && let Some(path) = call
                        .arguments()
                        .as_object()
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                {
                    resolved_read_paths.push(path.to_owned());
                }
                resolved_artifacts.push(result.artifact().id().clone());
            }
            _ => {}
        }
    }

    require_live_smoke_tool_name(&resolved_tool_names, CODING_LOOP_PROCESS_TOOL)?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_READ_FILE_TOOL)?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_PATCH_TOOL)?;

    if !resolved_read_paths.iter().any(|path| path == "AGENTS.md") {
        return Err(CliError::Unexpected(
            "task live smoke did not read AGENTS.md before completing".to_owned(),
        ));
    }
    if !resolved_read_paths.iter().any(|path| path == "src/lib.rs") {
        return Err(CliError::Unexpected(
            "task live smoke did not read src/lib.rs before patching".to_owned(),
        ));
    }

    let mut saw_cargo_check = false;
    let mut saw_cargo_test = false;
    for artifact_id in &resolved_artifacts {
        let Ok(content) = runtime.read_artifact_content(artifact_id).await else {
            continue;
        };
        let Some(text) = content.as_text() else {
            continue;
        };
        if !text.contains("\"kind\":\"process_action\"") || !text.contains("\"ok\":true") {
            continue;
        }
        saw_cargo_check |=
            process_artifact_has_cargo_package_argv(text, "check", fixture.package_name());
        saw_cargo_test |=
            process_artifact_has_cargo_package_argv(text, "test", fixture.package_name());
    }
    if !saw_cargo_check {
        return Err(CliError::Unexpected(
            "task live smoke did not observe a successful cargo check for the fixture package"
                .to_owned(),
        ));
    }
    if !saw_cargo_test {
        return Err(CliError::Unexpected(
            "task live smoke did not observe a successful cargo test for the fixture package"
                .to_owned(),
        ));
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

fn process_artifact_has_cargo_package_argv(text: &str, command: &str, package: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let Some(argv) = value
        .get("intent")
        .and_then(|intent| intent.get("argv"))
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    matches!(
        argv.as_slice(),
        [cargo, actual_command, package_flag, actual_package]
            if cargo.as_str() == Some("cargo")
                && actual_command.as_str() == Some(command)
                && matches!(package_flag.as_str(), Some("-p" | "--package"))
                && actual_package.as_str() == Some(package)
    )
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
                WORKSPACE_PATCH_TOOL,
                [(
                    "patch",
                    serde_json::Value::String(format!(
                        "*** Begin Workspace Patch\n*** Update File: src/lib.rs\n-    \"{CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE}\"\n+    \"{CODING_LOOP_LIVE_SMOKE_TARGET_VALUE}\"\n*** End Workspace Patch"
                    )),
                )],
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

#[cfg(test)]
struct PermissionNetworkSmokeProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Mutex<Vec<ModelEvent>>,
}

#[cfg(test)]
impl PermissionNetworkSmokeProvider {
    fn new() -> Result<Self, CliError> {
        let steps = vec![
            coding_loop_process_call(
                "permission-network-smoke-initial-network",
                &PERMISSION_NETWORK_SMOKE_ARGV,
                None,
            )?,
            permission_network_smoke_request_call()?,
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        "permission-network-smoke verified approved per-action network access",
                    )],
                    FinishReason::Stop,
                    None,
                ),
            },
        ];

        Ok(Self {
            name: ProviderName::new("merry-permission-network-smoke-provider")
                .map_err(unexpected)?,
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .map_err(unexpected)?,
            steps: Mutex::new(steps.into_iter().rev().collect()),
        })
    }
}

#[cfg(test)]
impl ModelProvider for PermissionNetworkSmokeProvider {
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
                .expect("permission network smoke steps mutex should not be poisoned")
                .pop()
                .ok_or_else(|| {
                    ModelError::invalid_request(
                        "permission-network-smoke provider has no scripted step",
                    )
                })?;
            Ok(Box::pin(stream::iter([Ok(event)])) as ModelEventStream)
        })
    }
}

#[cfg(test)]
struct PermissionNetworkSmokeReviewProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
}

#[cfg(test)]
impl PermissionNetworkSmokeReviewProvider {
    fn new() -> Result<Self, CliError> {
        Ok(Self {
            name: ProviderName::new("merry-permission-network-smoke-review-provider")
                .map_err(unexpected)?,
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .map_err(unexpected)?,
        })
    }
}

#[cfg(test)]
impl ModelProvider for PermissionNetworkSmokeReviewProvider {
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

            let response = ModelResponse::new(
                vec![ModelOutput::text(
                    r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"high","rationale":"The debug smoke requested network only for the exact DNS lookup that just failed under the inner sandbox."}"#,
                )],
                FinishReason::Stop,
                None,
            );
            Ok(
                Box::pin(stream::iter([Ok(ModelEvent::Completed { response })]))
                    as ModelEventStream,
            )
        })
    }
}

struct CodingLoopTaskSmokeProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Mutex<Vec<ModelEvent>>,
}

impl CodingLoopTaskSmokeProvider {
    fn new(
        relative_cwd: Option<&str>,
        fixture: CodingLoopTaskSmokeFixture,
    ) -> Result<Self, CliError> {
        let steps = vec![
            coding_loop_process_call(
                "coding-loop-task-smoke-rg-files",
                &["rg", "--files"],
                relative_cwd,
            )?,
            coding_loop_process_call(
                "coding-loop-task-smoke-verify-before",
                &["rg", "done", "src/lib.rs"],
                relative_cwd,
            )?,
            coding_loop_workspace_call(
                "coding-loop-task-smoke-read",
                WORKSPACE_READ_FILE_TOOL,
                [("path", serde_json::Value::String("src/lib.rs".to_owned()))],
            )?,
            coding_loop_workspace_call(
                "coding-loop-task-smoke-patch",
                WORKSPACE_PATCH_TOOL,
                [("patch", serde_json::Value::String(fixture.patch_text()))],
            )?,
            coding_loop_process_call(
                "coding-loop-task-smoke-verify-after",
                &["rg", "done", "src/lib.rs"],
                relative_cwd,
            )?,
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        "coding-loop-task-smoke fixed the fixture and verified rg done",
                    )],
                    FinishReason::Stop,
                    None,
                ),
            },
        ];

        Ok(Self {
            name: ProviderName::new("merry-coding-loop-task-smoke-provider").map_err(unexpected)?,
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .map_err(unexpected)?,
            steps: Mutex::new(steps.into_iter().rev().collect()),
        })
    }
}

impl ModelProvider for CodingLoopTaskSmokeProvider {
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
                .expect("coding loop task smoke steps mutex should not be poisoned")
                .pop()
                .ok_or_else(|| {
                    ModelError::invalid_request(
                        "coding-loop-task-smoke provider has no scripted step",
                    )
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
    coding_loop_tool_call(call_id, CODING_LOOP_PROCESS_TOOL, arguments)
}

#[cfg(test)]
fn permission_network_smoke_request_call() -> Result<ModelEvent, CliError> {
    let mut requested = serde_json::Map::new();
    requested.insert("network".to_owned(), serde_json::Value::Bool(true));

    let mut for_action = serde_json::Map::new();
    for_action.insert(
        "kind".to_owned(),
        serde_json::Value::String("process".to_owned()),
    );
    for_action.insert(
        "argv".to_owned(),
        serde_json::Value::Array(
            PERMISSION_NETWORK_SMOKE_ARGV
                .iter()
                .map(|argument| serde_json::Value::String((*argument).to_owned()))
                .collect(),
        ),
    );

    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "reason".to_owned(),
        serde_json::Value::String(
            "The same DNS lookup failed under the default inner sandbox; request network for this exact debug smoke command."
                .to_owned(),
        ),
    );
    arguments.insert("requested".to_owned(), serde_json::Value::Object(requested));
    arguments.insert(
        "for_action".to_owned(),
        serde_json::Value::Object(for_action),
    );
    coding_loop_tool_call(
        "permission-network-smoke-request-network",
        "request_permissions",
        arguments,
    )
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
    let admission = shell_runtime_admission(
        args.accept_local_workspace_process_risk,
        sandbox_child_handoff,
        sandbox_runtime_profile,
        sandbox_marker.as_deref(),
        sandbox_version.as_deref(),
    );
    let intent = shell_process_action_intent(args.argv)?;
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
    run_shell_to_writer(
        intent,
        admission,
        runner,
        args.events_jsonl,
        tokio::io::stdout(),
    )
    .await
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

async fn write_run_agent_loop_output<W>(
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
        write_run_progress_commentary_event(runtime, &event, &mut pending_commentary, &mut writer)
            .await?;
    }
    let result = stream.result().await.ok_or_else(|| {
        CliError::Unexpected("agent loop stream closed before producing a result".to_owned())
    })?;
    write_run_agent_loop_summary_to(&result, &mut writer).await?;
    writer.flush().await.map_err(stdout_error)
}

async fn write_run_agent_loop_jsonl_output<W>(
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

async fn write_run_agent_loop_summary_to<W>(
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

async fn write_run_progress_commentary_event<W>(
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
                write_run_progress_commentary_artifact(runtime, &artifact_id, writer).await?;
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

async fn write_run_progress_commentary_artifact<W>(
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
    write_runtime_event_slice(&events, writer).await
}

async fn write_runtime_event_slice<W>(
    events: &[RuntimeEvent],
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    for event in events {
        write_runtime_event(event, writer).await?;
    }
    Ok(())
}

async fn write_coding_loop_task_live_smoke_report<W>(
    runtime: &Runtime,
    automatic_compaction: AutomaticCompactionConfig,
    passed: bool,
    events: &[RuntimeEvent],
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let header = if passed {
        b"coding-loop-task-live-smoke: ok\n".as_slice()
    } else {
        b"coding-loop-task-live-smoke: failed\n".as_slice()
    };
    writer.write_all(header).await.map_err(stdout_error)?;
    write_runtime_event_slice(events, writer).await?;
    write_compaction_config_summary(automatic_compaction, writer).await?;
    write_compaction_summary(runtime, writer).await?;
    write_process_artifact_previews(runtime, events, writer).await
}

async fn write_permission_network_smoke_report<W>(
    runtime: &Runtime,
    events: &[RuntimeEvent],
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    writer
        .write_all(b"permission-network-smoke: ok\n")
        .await
        .map_err(stdout_error)?;
    write_runtime_event_slice(events, writer).await?;
    write_process_artifact_previews(runtime, events, writer).await
}

async fn write_coding_loop_subagent_live_smoke_report<W>(
    runtime: &Runtime,
    passed: bool,
    events: &[RuntimeEvent],
    smoke_root: &Path,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let header = if passed {
        b"coding-loop-subagent-live-smoke: ok\n".as_slice()
    } else {
        b"coding-loop-subagent-live-smoke: failed\n".as_slice()
    };
    writer.write_all(header).await.map_err(stdout_error)?;
    write_runtime_event_slice(events, writer).await?;
    write_subagent_snapshot_summary(runtime, writer).await?;
    write_subagent_fixture_summary(smoke_root, writer).await
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

async fn write_subagent_snapshot_summary<W>(
    runtime: &Runtime,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let line = serde_json::json!({
        "type": "subagent_snapshot",
        "agents": runtime.subagent_snapshot().await,
    });
    let line = serde_json::to_string(&line).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)
}

async fn write_subagent_fixture_summary<W>(
    smoke_root: &Path,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let content = fs::read_to_string(smoke_root.join(CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE))
        .unwrap_or_else(|error| format!("unreadable fixture file: {error}"));
    let line = serde_json::json!({
        "type": "subagent_live_smoke_fixture",
        "path": CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE,
        "content": content,
        "target_matched": content == CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET,
    });
    let line = serde_json::to_string(&line).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)
}

async fn write_compaction_config_summary<W>(
    automatic_compaction: AutomaticCompactionConfig,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let policy = automatic_compaction.policy();
    let line = serde_json::json!({
        "type": "runtime_compaction_config_summary",
        "auto_compaction_enabled": automatic_compaction.is_enabled(),
        "target_output_tokens": policy.target_output_tokens(),
        "model_output_token_limit": policy.model_output_token_limit(),
        "max_accepted_output_bytes": policy.max_accepted_output_bytes(),
        "retained_raw_tail_items": policy.retained_raw_tail_items(),
        "max_ref_excerpt_bytes": policy.max_ref_excerpt_bytes(),
        "max_carried_prior_refs": policy.max_carried_prior_refs(),
    });
    let line = serde_json::to_string(&line).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)?;
    Ok(())
}

async fn write_compaction_summary<W>(runtime: &Runtime, writer: &mut W) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let summary = runtime.compacted_checkpoint_summary().await;
    let line = match summary {
        Some(summary) => serde_json::json!({
            "type": "runtime_compaction_summary",
            "checkpoint_present": true,
            "citation_backed": summary.citation_backed(),
            "checkpoint_id": summary.checkpoint_id().map(merry_runtime::CheckpointId::as_str),
            "claim_count": summary.claim_count(),
            "ref_count": summary.ref_count(),
        }),
        None => serde_json::json!({
            "type": "runtime_compaction_summary",
            "checkpoint_present": false,
            "citation_backed": false,
            "checkpoint_id": null,
            "claim_count": 0,
            "ref_count": 0,
        }),
    };
    let line = serde_json::to_string(&line).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)?;
    Ok(())
}

async fn write_process_artifact_previews<W>(
    runtime: &Runtime,
    events: &[RuntimeEvent],
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    for event in events {
        let RuntimeEventKind::ToolCallResolved { result } = &event.kind else {
            continue;
        };
        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .map_err(unexpected)?;
        let ArtifactContent::Json(content) = content else {
            continue;
        };
        let value = serde_json::from_str::<serde_json::Value>(&content).map_err(unexpected)?;
        if value.pointer("/kind").and_then(serde_json::Value::as_str) != Some("process_action") {
            continue;
        }
        let preview = serde_json::json!({
            "type": "process_artifact_preview",
            "artifact_id": result.artifact().id().as_str(),
            "call_id": result.call_id().as_str(),
            "status": value.pointer("/status"),
            "stdout": value.pointer("/stdout/text").and_then(serde_json::Value::as_str).unwrap_or(""),
            "stderr": value.pointer("/stderr/text").and_then(serde_json::Value::as_str).unwrap_or(""),
            "stdout_truncated": value.pointer("/stdout/truncated").and_then(serde_json::Value::as_bool).unwrap_or(false),
            "stderr_truncated": value.pointer("/stderr/truncated").and_then(serde_json::Value::as_bool).unwrap_or(false),
        });
        let line = serde_json::to_string(&preview).map_err(unexpected)?;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(stdout_error)?;
        writer.write_all(b"\n").await.map_err(stdout_error)?;
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

fn debug_openai_config(
    model_flag: Option<&str>,
    merry_config: Option<&MerryConfig>,
) -> Result<OpenAiRuntimeConfig, CliError> {
    debug_openai_config_with_env(model_flag, merry_config, optional_env)
}

fn debug_openai_config_with_env(
    model_flag: Option<&str>,
    merry_config: Option<&MerryConfig>,
    env_value: impl Fn(&'static str) -> Result<Option<String>, CliError>,
) -> Result<OpenAiRuntimeConfig, CliError> {
    if env_value(MERRY_OPENAI_DEBUG_ENV)?.as_deref() != Some("1") {
        return Err(debug_openai_usage_error(
            "set MERRY_OPENAI_DEBUG=1 to enable live OpenAI-compatible debugging",
        ));
    }

    openai_runtime_config(model_flag, merry_config, debug_openai_usage_error)
}

fn openai_runtime_config(
    model_flag: Option<&str>,
    merry_config: Option<&MerryConfig>,
    map_usage_error: fn(String) -> CliError,
) -> Result<OpenAiRuntimeConfig, CliError> {
    let merry_config = merry_config.ok_or_else(|| {
        map_usage_error(
            "Merry XDG provider config is required for OpenAI-compatible runtime".to_owned(),
        )
    })?;
    let provider_config = merry_config
        .openai_compatible_provider()
        .map_err(|error| map_usage_error(error.to_string()))?;
    let api_key = provider_config
        .resolve_api_key()
        .map_err(|error| map_usage_error(error.to_string()))?;
    let primary_model = match model_flag {
        Some(model) => model.to_owned(),
        None => provider_config.model.clone().ok_or_else(|| {
            map_usage_error(
                "[providers.default].model must be set or --model must be provided".to_owned(),
            )
        })?,
    };
    let primary =
        openai_provider_config(&provider_config, &api_key, primary_model, map_usage_error)?;

    let runtime_models = merry_config
        .runtime_models()
        .map_err(|error| map_usage_error(error.to_string()))?;
    let retry_policy = merry_config
        .provider_retry_policy()
        .map_err(|error| map_usage_error(error.to_string()))?;
    let context_compaction = runtime_models
        .context_compaction
        .map(|model| {
            openai_provider_config(&provider_config, &api_key, model.model, map_usage_error)
        })
        .transpose()?;
    let approval_review = runtime_models
        .approval_review
        .map(|model| {
            openai_provider_config(&provider_config, &api_key, model.model, map_usage_error)
        })
        .transpose()?;

    Ok(OpenAiRuntimeConfig {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    })
}

fn openai_provider_config(
    provider_config: &EffectiveOpenAiProviderConfig,
    api_key: &str,
    model: String,
    map_usage_error: fn(String) -> CliError,
) -> Result<OpenAiConfig, CliError> {
    let mut provider =
        OpenAiProviderConfig::new(api_key).map_err(|error| map_usage_error(error.to_string()))?;
    if let Some(base_url) = provider_config.base_url.as_deref() {
        provider = provider
            .with_base_url(base_url)
            .map_err(|error| map_usage_error(error.to_string()))?;
    }
    Ok(OpenAiConfig { provider, model })
}

fn apply_openai_context_compaction_provider(
    mut builder: RuntimeBuilder,
    context_compaction: Option<OpenAiConfig>,
) -> Result<RuntimeBuilder, CliError> {
    if let Some(config) = context_compaction {
        let role_provider = openai_context_compaction_provider(config)?;
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    Ok(builder)
}

fn openai_context_compaction_provider(
    config: OpenAiConfig,
) -> Result<RuntimeRoleProviderConfig, CliError> {
    openai_role_provider_config(
        RuntimeModelRole::ContextCompaction,
        config,
        debug_openai_usage_error,
    )
}

fn openai_approval_review_provider(
    config: OpenAiConfig,
) -> Result<RuntimeRoleProviderConfig, CliError> {
    openai_role_provider_config(
        RuntimeModelRole::ApprovalReview,
        config,
        debug_openai_usage_error,
    )
}

fn openai_role_provider_config(
    role: RuntimeModelRole,
    config: OpenAiConfig,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimeRoleProviderConfig, CliError> {
    Ok(RuntimeRoleProviderConfig {
        role,
        provider: Arc::new(OpenAiProvider::new(config.provider)),
        model: ModelName::new(&config.model).map_err(|error| map_usage_error(error.to_string()))?,
    })
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

struct OpenAiConfig {
    provider: OpenAiProviderConfig,
    model: String,
}

struct OpenAiRuntimeConfig {
    primary: OpenAiConfig,
    context_compaction: Option<OpenAiConfig>,
    approval_review: Option<OpenAiConfig>,
    retry_policy: Option<ModelRetryPolicy>,
}

fn debug_usage() -> String {
    let mut command = Cli::command();
    let command = command
        .find_subcommand_mut("debug")
        .expect("debug subcommand should exist");
    command.set_bin_name("merry debug");
    command_usage(command)
}

fn run_usage() -> String {
    let mut command = Cli::command();
    let command = command
        .find_subcommand_mut("run")
        .expect("run subcommand should exist");
    command.set_bin_name("merry run");
    command_usage(command)
}

fn cmd_usage() -> String {
    let mut command = Cli::command();
    let command = command
        .find_subcommand_mut("cmd")
        .expect("cmd subcommand should exist");
    command.set_bin_name("merry cmd");
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

fn debug_coding_loop_task_live_smoke_usage() -> String {
    let mut command = DebugCodingLoopTaskLiveSmokeArgs::augment_args(clap::Command::new(
        "coding-loop-task-live-smoke",
    ))
    .bin_name("merry debug coding-loop-task-live-smoke")
    .about(
        "Run an opt-in sandboxed coding-loop task smoke driven by a live OpenAI-compatible model",
    )
    .after_help(OPENAI_ENV_HELP);
    command_usage(&mut command)
}

fn debug_coding_loop_subagent_live_smoke_usage() -> String {
    let mut command = DebugCodingLoopSubagentLiveSmokeArgs::augment_args(clap::Command::new(
        "coding-loop-subagent-live-smoke",
    ))
    .bin_name("merry debug coding-loop-subagent-live-smoke")
    .about(
        "Run an opt-in sandboxed coding-loop smoke that requires a live model to delegate to a child agent",
    )
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
        DEFAULT_SESSION_ID, DebugCommand, MERRY_OPENAI_DEBUG_ENV, MERRY_SANDBOX_ENV,
        MERRY_SANDBOX_VERSION, MERRY_SANDBOX_VERSION_ENV, SANDBOX_CHILD_HANDOFF_ARG,
        SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1, SANDBOX_HOME, SANDBOX_MERRY_CONFIG_DIR,
        SANDBOX_MERRY_LOG_DIR, SANDBOX_TMPDIR, SANDBOX_XDG_CONFIG_HOME, SANDBOX_XDG_STATE_HOME,
        SandboxBootstrap, SandboxChildHandoff, SandboxError, SandboxHost, SandboxRuntimeProfile,
        args_without_sandbox_bootstrap_flags, coding_loop_process_call, coding_loop_workspace_call,
        collect_runtime_step_events, debug_echo_tool, debug_openai_config_with_env,
        debug_openai_usage, find_bwrap_in_path, first_pending_tool_call, os,
        plan_sandbox_bootstrap_with_file_exists, report_cli_exit, run_debug_coding_loop_smoke,
        sandbox_runtime_profile_from_evidence, shell_process_action_intent,
        shell_runtime_admission, shell_usage, write_coding_loop_task_live_smoke_report,
        write_debug_openai_tool_events, write_permission_network_smoke_report,
    };
    use super::{
        DEBUG_TOOL_NAME, DEFAULT_CODING_AGENT_MAX_MODEL_TURNS, run_shell_to_writer,
        write_runtime_step_events,
    };
    use crate::CodingLoopTaskSmokeTask;
    use crate::config::SubagentsConfig;
    use clap::Parser;
    use futures_util::stream;
    use merry_core::{
        ArtifactKind, ArtifactRef, PendingToolCall, ProviderName, RuntimeEvent, RuntimeEventKind,
        ToolCallId, ToolCallResult, ToolCallResultStatus, ToolName,
    };
    use merry_llm::{
        FinishReason, GenerationConfig, ModelCapabilities, ModelError, ModelEvent,
        ModelEventStream, ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest,
        ModelResponse, ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
    };
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AgentLoopStatus, ArtifactContent,
        CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest,
        CheckpointSequenceRange, CheckpointSourceKind, CheckpointValidationPolicy,
        CitationBackedCheckpoint, CompactedCheckpoint, CompactedCheckpointCandidate,
        MAX_PROCESS_OUTPUT_LIMIT_BYTES, PathAccess, PathAccessRule, PathAccessRuleSource,
        ProcessActionIntent, ProcessEnvPolicy, ProcessExitStatus, ProcessRunner,
        ProcessRunnerContext, ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
        Runtime, RuntimeProfile, StepContext, StepInput, SubagentConfig, ToolExecutionContext,
    };
    use merry_tool_workspace::{
        CODING_LOOP_PROCESS_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
        WorkspaceCodingLoopProfile, WorkspaceRuntimeProfileBuilderExt, WorkspaceToolsConfig,
    };
    use serde_json::{Map, Value};
    use std::{
        ffi::{OsStr, OsString},
        io,
        path::{Path, PathBuf},
        pin::Pin,
        process::ExitCode,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
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
            openai_debug: None,
            inside_sandbox: false,
            xdg_paths: super::config::XdgPaths::from_parts(
                PathBuf::from("/home/alice"),
                Some(PathBuf::from("/host/config")),
                Some(PathBuf::from("/host/state")),
            ),
            log_settings: None,
            trusted_path_rules: Vec::new(),
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
    fn clap_parses_run_task() {
        let cli = Cli::try_parse_from(["merry", "run", "fix the failing test"])
            .expect("run args should parse");

        match cli.command {
            CliCommand::Run(args) => {
                assert_eq!(args.task, "fix the failing test");
                assert!(!args.events_jsonl);
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn clap_parses_run_events_jsonl() {
        let cli = Cli::try_parse_from(["merry", "run", "--events-jsonl", "fix the failing test"])
            .expect("run args should parse");

        match cli.command {
            CliCommand::Run(args) => {
                assert_eq!(args.task, "fix the failing test");
                assert!(args.events_jsonl);
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn clap_parses_cmd_request_defaults() {
        let cli = Cli::try_parse_from(["merry", "cmd", "find all TypeScript tests"])
            .expect("cmd args should parse");

        match cli.command {
            CliCommand::Cmd(args) => {
                assert_eq!(args.request, "find all TypeScript tests");
                assert!(!args.json);
                assert!(!args.no_prompt);
            }
            _ => panic!("expected cmd command"),
        }
    }

    #[test]
    fn clap_parses_cmd_json_and_no_prompt() {
        let cli = Cli::try_parse_from([
            "merry",
            "cmd",
            "--json",
            "--no-prompt",
            "find all TypeScript tests",
        ])
        .expect("cmd args should parse");

        match cli.command {
            CliCommand::Cmd(args) => {
                assert_eq!(args.request, "find all TypeScript tests");
                assert!(args.json);
                assert!(args.no_prompt);
            }
            _ => panic!("expected cmd command"),
        }
    }

    #[test]
    fn command_plan_final_output_contract_has_described_fields() {
        let contract = super::cmd::command_plan_final_output_contract()
            .expect("command plan final output contract should build");

        assert_eq!(
            contract.tool_name().as_str(),
            merry_runtime::FINAL_OUTPUT_TOOL_NAME
        );
        let schema = serde_json::to_value(contract.tool_spec().input_schema().as_schema())
            .expect("schema should serialize");
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("schema should have object properties");

        for field in ["shell_command", "notes", "cautions"] {
            let description = properties
                .get(field)
                .and_then(|schema| schema.get("description"))
                .and_then(Value::as_str)
                .expect("field should have a description");
            assert!(!description.trim().is_empty());
        }
    }

    #[test]
    fn cmd_usage_renders_cmd_help() {
        let usage = super::cmd_usage();

        assert!(usage.contains("Usage: merry cmd"));
        assert!(usage.contains("--no-prompt"));
        assert!(!usage.contains("merry debug openai"));
    }

    #[test]
    fn command_generation_prompt_treats_file_search_as_recursive_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let environment = super::cmd::CommandGenerationEnvironment::detect(temp.path());
        let prompt = super::cmd::command_generation_prompt("列出当前目录的 rs 文件", &environment);

        assert!(prompt.contains("recursive by default"));
        assert!(prompt.contains("find -maxdepth 1 only when the user explicitly asks"));
        assert!(prompt.contains("user's current input language"));
        assert!(prompt.contains("Runtime environment:"));
        assert!(prompt.contains(super::cmd::CHECK_COMMAND_TOOL_NAME));
        assert!(prompt.contains("prefer a single shell pipeline"));
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
            _ => panic!("expected debug subcommand"),
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
                Some(
                    DebugCommand::CodingLoopSmoke
                    | DebugCommand::PermissionNetworkSmoke(_)
                    | DebugCommand::CodingLoopLiveSmoke(_)
                    | DebugCommand::CodingLoopTaskSmoke(_)
                    | DebugCommand::CodingLoopTaskLiveSmoke(_)
                    | DebugCommand::CodingLoopSubagentLiveSmoke(_),
                ) => panic!("expected debug openai subcommand"),
                None => panic!("expected debug openai subcommand"),
            },
            _ => panic!("expected debug subcommand"),
        }
    }

    #[test]
    fn clap_parses_debug_coding_loop_smoke() {
        let cli = Cli::try_parse_from(["merry", "debug", "coding-loop-smoke"])
            .expect("debug coding-loop-smoke args should parse");

        match cli.command {
            CliCommand::Debug(debug) => match debug.command {
                Some(DebugCommand::CodingLoopSmoke) => {}
                Some(
                    DebugCommand::OpenAi(_)
                    | DebugCommand::PermissionNetworkSmoke(_)
                    | DebugCommand::CodingLoopLiveSmoke(_)
                    | DebugCommand::CodingLoopTaskSmoke(_)
                    | DebugCommand::CodingLoopTaskLiveSmoke(_)
                    | DebugCommand::CodingLoopSubagentLiveSmoke(_),
                )
                | None => panic!("expected debug coding-loop-smoke subcommand"),
            },
            _ => panic!("expected debug subcommand"),
        }
    }

    #[test]
    fn clap_parses_debug_permission_network_smoke() {
        let cli = Cli::try_parse_from([
            "merry",
            "debug",
            "permission-network-smoke",
            "--model",
            "gpt-test",
            "--max-output-tokens",
            "384",
        ])
        .expect("debug permission-network-smoke args should parse");

        match cli.command {
            CliCommand::Debug(debug) => match debug.command {
                Some(DebugCommand::PermissionNetworkSmoke(smoke)) => {
                    assert_eq!(smoke.model.as_deref(), Some("gpt-test"));
                    assert_eq!(smoke.max_output_tokens, 384);
                }
                Some(
                    DebugCommand::OpenAi(_)
                    | DebugCommand::CodingLoopSmoke
                    | DebugCommand::CodingLoopLiveSmoke(_)
                    | DebugCommand::CodingLoopTaskSmoke(_)
                    | DebugCommand::CodingLoopTaskLiveSmoke(_)
                    | DebugCommand::CodingLoopSubagentLiveSmoke(_),
                )
                | None => panic!("expected debug permission-network-smoke subcommand"),
            },
            _ => panic!("expected debug subcommand"),
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
                Some(
                    DebugCommand::OpenAi(_)
                    | DebugCommand::CodingLoopSmoke
                    | DebugCommand::PermissionNetworkSmoke(_)
                    | DebugCommand::CodingLoopTaskSmoke(_)
                    | DebugCommand::CodingLoopTaskLiveSmoke(_)
                    | DebugCommand::CodingLoopSubagentLiveSmoke(_),
                )
                | None => panic!("expected debug coding-loop-live-smoke subcommand"),
            },
            _ => panic!("expected debug subcommand"),
        }
    }

    #[test]
    fn clap_parses_debug_coding_loop_task_smoke() {
        let cli = Cli::try_parse_from(["merry", "debug", "coding-loop-task-smoke"])
            .expect("debug coding-loop-task-smoke args should parse");

        match cli.command {
            CliCommand::Debug(debug) => match debug.command {
                Some(DebugCommand::CodingLoopTaskSmoke(task)) => {
                    assert_eq!(task.task, CodingLoopTaskSmokeTask::StatusText);
                }
                Some(
                    DebugCommand::OpenAi(_)
                    | DebugCommand::CodingLoopSmoke
                    | DebugCommand::PermissionNetworkSmoke(_)
                    | DebugCommand::CodingLoopLiveSmoke(_)
                    | DebugCommand::CodingLoopTaskLiveSmoke(_)
                    | DebugCommand::CodingLoopSubagentLiveSmoke(_),
                )
                | None => panic!("expected debug coding-loop-task-smoke subcommand"),
            },
            _ => panic!("expected debug subcommand"),
        }
    }

    #[test]
    fn clap_parses_debug_coding_loop_task_live_smoke() {
        let cli = Cli::try_parse_from([
            "merry",
            "debug",
            "coding-loop-task-live-smoke",
            "--task",
            "status-text",
            "--model",
            "gpt-test",
            "--max-output-tokens",
            "384",
        ])
        .expect("debug coding-loop-task-live-smoke args should parse");

        match cli.command {
            CliCommand::Debug(debug) => match debug.command {
                Some(DebugCommand::CodingLoopTaskLiveSmoke(live)) => {
                    assert_eq!(live.task, CodingLoopTaskSmokeTask::StatusText);
                    assert_eq!(live.model.as_deref(), Some("gpt-test"));
                    assert_eq!(live.max_output_tokens, 384);
                }
                Some(
                    DebugCommand::OpenAi(_)
                    | DebugCommand::CodingLoopSmoke
                    | DebugCommand::PermissionNetworkSmoke(_)
                    | DebugCommand::CodingLoopLiveSmoke(_)
                    | DebugCommand::CodingLoopTaskSmoke(_)
                    | DebugCommand::CodingLoopSubagentLiveSmoke(_),
                )
                | None => panic!("expected debug coding-loop-task-live-smoke subcommand"),
            },
            _ => panic!("expected debug subcommand"),
        }
    }

    #[test]
    fn clap_parses_debug_coding_loop_subagent_live_smoke() {
        let cli = Cli::try_parse_from([
            "merry",
            "debug",
            "coding-loop-subagent-live-smoke",
            "--model",
            "gpt-test",
            "--max-output-tokens",
            "384",
        ])
        .expect("debug coding-loop-subagent-live-smoke args should parse");

        match cli.command {
            CliCommand::Debug(debug) => match debug.command {
                Some(DebugCommand::CodingLoopSubagentLiveSmoke(live)) => {
                    assert_eq!(live.model.as_deref(), Some("gpt-test"));
                    assert_eq!(live.max_output_tokens, 384);
                }
                Some(
                    DebugCommand::OpenAi(_)
                    | DebugCommand::CodingLoopSmoke
                    | DebugCommand::PermissionNetworkSmoke(_)
                    | DebugCommand::CodingLoopLiveSmoke(_)
                    | DebugCommand::CodingLoopTaskSmoke(_)
                    | DebugCommand::CodingLoopTaskLiveSmoke(_),
                )
                | None => panic!("expected debug coding-loop-subagent-live-smoke subcommand"),
            },
            _ => panic!("expected debug subcommand"),
        }
    }

    #[test]
    fn coding_loop_subagent_live_prompt_forces_parent_delegation_and_child_patch() {
        let prompt = super::coding_loop_subagent_live_smoke_task();

        assert!(prompt.contains("Call `spawn_subagents` with exactly one child task"));
        assert!(prompt.contains("call `wait_subagents`"));
        assert!(prompt.contains("The parent agent must not patch"));
        assert!(prompt.contains("\"workspace_read_file\", \"workspace_patch\""));
        assert!(prompt.contains("\"subagent-output.txt\""));
        assert!(prompt.contains(super::CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET.trim()));
    }

    #[tokio::test]
    async fn coding_loop_smoke_admission_requires_real_sandbox_handoff() {
        let err = run_debug_coding_loop_smoke(None, None)
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
    async fn coding_loop_task_smoke_patches_fixture_and_verifies_with_fake_runner() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let fixture =
            super::CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
        let smoke_root = temp.path().join("coding-loop-task-smoke-fixture");
        std::fs::create_dir_all(smoke_root.join("src")).expect("fixture src dir should exist");
        std::fs::create_dir_all(smoke_root.join("tests")).expect("fixture tests dir should exist");
        std::fs::write(
            smoke_root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
                fixture.package_name()
            ),
        )
        .expect("fixture Cargo.toml should write");
        std::fs::write(smoke_root.join("src/lib.rs"), fixture.initial_source())
            .expect("fixture source should write");
        std::fs::write(smoke_root.join("AGENTS.md"), fixture.agents_source())
            .expect("fixture AGENTS.md should write");
        std::fs::write(
            smoke_root.join("tests/status.rs"),
            fixture.integration_test_source(),
        )
        .expect("fixture integration test should write");
        std::fs::write(smoke_root.join("tests.md"), fixture.test_source())
            .expect("fixture test note should write");

        let runner = FakeProcessRunner::scripted([
            FakeProcessRunnerStep::success(
                "AGENTS.md\nCargo.toml\nsrc/lib.rs\ntests.md\ntests/status.rs\n",
            ),
            FakeProcessRunnerStep::failure("pattern not found\n"),
            FakeProcessRunnerStep::success("src/lib.rs:    \"done\"\n"),
        ]);
        let runtime = super::build_coding_loop_task_smoke_runtime(
            &smoke_root,
            None,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(runner.clone()),
            None,
            fixture,
            merry_runtime::AutomaticCompactionConfig::default(),
        )
        .expect("coding-loop task smoke runtime should build");

        let result = runtime
            .run_agent_loop(
                StepInput::user_text(fixture.task_prompt()).expect("valid step input"),
                StepContext::default(),
                AgentLoopConfig::new(10).expect("valid loop config"),
            )
            .await
            .expect("coding-loop task smoke should run");
        super::assert_coding_loop_task_smoke_result(&runtime, &result, &smoke_root, fixture)
            .await
            .expect("coding-loop task smoke result should validate");
        assert_eq!(
            runner.observed_argv(),
            [
                vec!["rg".to_owned(), "--files".to_owned()],
                vec!["rg".to_owned(), "done".to_owned(), "src/lib.rs".to_owned()],
                vec!["rg".to_owned(), "done".to_owned(), "src/lib.rs".to_owned()],
            ]
        );
        assert_eq!(runner.observed_cwd(), [None, None, None]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coding_loop_runtime_projects_skill_metadata_without_body() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let skill_root = temp.path().join("skills");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        std::fs::create_dir_all(skill_root.join("demo")).expect("mkdir skill");
        std::fs::write(
            skill_root.join("demo/SKILL.md"),
            "---\nname: demo-skill\ndescription: Use for demo tasks.\n---\n# Demo\nbody sentinel\n",
        )
        .expect("write skill");

        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);
        let runner = Arc::new(FakeProcessRunner::succeeding(""));
        let runtime = super::build_coding_loop_runtime(
            "coding-loop-skill-prefix",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            ModelName::new("debug-model").unwrap(),
            runner,
            super::CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: vec![skill_root.clone()],
                subagents: SubagentsConfig::default(),
            },
        )
        .expect("runtime should build");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Inspect skills.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");
        let request = provider.recorded_requests()[0].clone();
        let stable_text = request
            .stable_prefix_messages()
            .iter()
            .map(|message| message.content().as_text())
            .collect::<Vec<_>>()
            .join("\n");
        let request_text = request
            .messages()
            .iter()
            .map(|message| message.content().as_text())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(stable_text.contains("demo-skill"));
        assert!(stable_text.contains("Use for demo tasks."));
        assert!(stable_text.contains("demo/SKILL.md"));
        assert!(request_text.contains("workspace_read_file"));
        assert!(request_text.contains("Workspace coding profile"));
        assert!(request_text.contains("user's current input language"));
        assert!(request_text.contains("configured sandbox/profile"));
        assert!(request_text.contains("network access may be intentionally restricted"));
        assert!(request_text.contains("call request_permissions for that exact action"));
        assert!(!stable_text.contains("body sentinel"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn headless_coding_runtime_uses_coding_agent_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);
        let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
        let permissioned_factory = Arc::new(
            merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
        );

        let runtime = super::build_headless_coding_runtime(super::HeadlessCodingRuntimeInput {
            session_id: "headless-coding-runtime-profile",
            root: &workspace,
            admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            provider: Arc::new(provider.clone()),
            model: ModelName::new("debug-model").expect("valid model name"),
            runner,
            permissioned_process_runner_factory: permissioned_factory,
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: SubagentsConfig::default(),
        })
        .expect("headless coding runtime should build");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Inspect workspace.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");

        let request = provider.recorded_requests()[0].clone();
        let request_text = request
            .messages()
            .iter()
            .map(|message| message.content().as_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(request_text.contains("Workspace coding profile"));
        assert!(request_text.contains("user's current input language"));
        assert!(
            request
                .tools()
                .iter()
                .any(|tool| tool.name().as_str() == CODING_LOOP_PROCESS_TOOL)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_writer_prints_final_output_without_event_jsonl() {
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
        let runtime = super::build_headless_coding_runtime(super::HeadlessCodingRuntimeInput {
            session_id: "run-writer-test",
            root: &workspace,
            admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            provider: Arc::new(provider),
            model: ModelName::new("debug-model").expect("valid model name"),
            runner,
            permissioned_process_runner_factory: permissioned_factory,
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: SubagentsConfig::default(),
        })
        .expect("runtime should build");

        let mut output = Vec::new();
        super::write_run_agent_loop_output(
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
    async fn run_writer_streams_progress_commentary_before_final_output() {
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
        let runtime = super::build_headless_coding_runtime(super::HeadlessCodingRuntimeInput {
            session_id: "run-progress-writer-test",
            root: &workspace,
            admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            provider: Arc::new(provider),
            model: ModelName::new("debug-model").expect("valid model name"),
            runner,
            permissioned_process_runner_factory: permissioned_factory,
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: SubagentsConfig::default(),
        })
        .expect("runtime should build");

        let mut output = Vec::new();
        super::write_run_agent_loop_output(
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
    async fn run_jsonl_writer_streams_agent_loop_result() {
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
        let runtime = super::build_headless_coding_runtime(super::HeadlessCodingRuntimeInput {
            session_id: "run-jsonl-writer-test",
            root: &workspace,
            admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            provider: Arc::new(provider),
            model: ModelName::new("debug-model").expect("valid model name"),
            runner,
            permissioned_process_runner_factory: permissioned_factory,
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: SubagentsConfig::default(),
        })
        .expect("runtime should build");

        let mut output = FlushCountingWriter::default();
        super::write_run_agent_loop_jsonl_output(
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

    #[tokio::test(flavor = "current_thread")]
    async fn command_generation_runtime_is_read_only_workspace_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);

        let runtime = super::cmd::build_runtime(super::cmd::RuntimeInput {
            session_id: "cmd-generation-runtime",
            root: &workspace,
            environment: super::cmd::CommandGenerationEnvironment::detect(&workspace),
            provider: Arc::new(provider.clone()),
            model: ModelName::new("debug-model").expect("valid model name"),
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            skill_roots: Vec::new(),
        })
        .expect("runtime should build");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Inspect workspace.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");

        let request = provider.recorded_requests()[0].clone();
        let tool_names = request
            .tools()
            .iter()
            .map(|tool| tool.name().as_str())
            .collect::<Vec<_>>();
        assert!(tool_names.contains(&WORKSPACE_READ_FILE_TOOL));
        assert!(tool_names.contains(&super::cmd::CHECK_COMMAND_TOOL_NAME));
        assert!(!tool_names.contains(&WORKSPACE_PATCH_TOOL));
        assert!(!tool_names.contains(&CODING_LOOP_PROCESS_TOOL));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_command_plan_reads_structured_final_output() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_workspace_call(
            "call-final-command-plan",
            merry_runtime::FINAL_OUTPUT_TOOL_NAME,
            [
                (
                    "shell_command",
                    Value::String("find . -name '*.rs' -print".to_owned()),
                ),
                (
                    "notes",
                    Value::Array(vec![Value::String(
                        "Searches from the current directory.".to_owned(),
                    )]),
                ),
                (
                    "cautions",
                    Value::Array(vec![Value::String("May print many paths.".to_owned())]),
                ),
            ],
        )
        .expect("final output call should build"))]]);
        let runtime = super::cmd::build_runtime(super::cmd::RuntimeInput {
            session_id: "cmd-final-output",
            root: &workspace,
            environment: super::cmd::CommandGenerationEnvironment::detect(&workspace),
            provider: Arc::new(provider),
            model: ModelName::new("debug-model").expect("valid model name"),
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            skill_roots: Vec::new(),
        })
        .expect("runtime should build");

        let plan = super::cmd::generate_command_plan(
            &runtime,
            "find rust files",
            &super::cmd::CommandGenerationEnvironment::detect(&workspace),
        )
        .await
        .expect("command plan should generate");

        assert_eq!(plan.shell_command, "find . -name '*.rs' -print");
        assert_eq!(plan.notes, ["Searches from the current directory."]);
        assert_eq!(plan.cautions, ["May print many paths."]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cmd_check_command_tool_reports_path_availability() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bin = temp.path().join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir bin");
        let program_path = bin.join("available-tool");
        std::fs::write(&program_path, "#!/bin/sh\n").expect("write tool");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&program_path)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&program_path, permissions).expect("chmod tool");
        }

        let environment = super::cmd::CommandGenerationEnvironment {
            os: "linux",
            arch: "x86_64",
            family: "unix",
            shell: "/bin/sh".to_owned(),
            cwd: temp.path().to_owned(),
            path: Some(bin.display().to_string()),
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert(
            "programs".to_owned(),
            serde_json::json!(["available-tool", "missing-tool", "printf"]),
        );
        let provider = ScriptedProvider::new(vec![vec![Ok(super::coding_loop_tool_call(
            "call-check-command",
            super::cmd::CHECK_COMMAND_TOOL_NAME,
            arguments,
        )
        .expect("check command tool call should build"))]]);
        let runtime = super::cmd::build_runtime(super::cmd::RuntimeInput {
            session_id: "cmd-check-command-tool",
            root: temp.path(),
            environment,
            provider: Arc::new(provider),
            model: ModelName::new("debug-model").expect("valid model name"),
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            skill_roots: Vec::new(),
        })
        .expect("runtime should build");
        let events = collect_runtime_step_events(
            &runtime,
            StepInput::user_text("check commands").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should collect pending call");
        let pending = first_pending_tool_call(&events).expect("pending check command call");
        let resolved = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("tool should execute");
        let artifact_id = resolved
            .iter()
            .find_map(|event| match &event.kind {
                RuntimeEventKind::ToolCallResolved { result } => {
                    Some(result.artifact().id().clone())
                }
                _ => None,
            })
            .expect("tool result artifact should be recorded");
        let content = runtime
            .read_artifact_content(&artifact_id)
            .await
            .expect("tool result artifact should be readable");
        let payload = match content {
            ArtifactContent::Json(content) => {
                serde_json::from_str::<serde_json::Value>(&content).expect("json payload")
            }
            other => panic!("expected json content, got {other:?}"),
        };

        assert_eq!(payload["ok"], true);
        assert_eq!(payload["results"][0]["available"], true);
        assert_eq!(payload["results"][1]["available"], false);
        assert_eq!(payload["results"][2]["kind"], "shell_builtin");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prompt_execute_command_plan_defaults_to_no() {
        let plan = super::cmd::CommandPlan {
            shell_command: "printf should-not-run".to_owned(),
            notes: Vec::new(),
            cautions: Vec::new(),
        };
        let mut output = Vec::new();

        let accepted = super::cmd::prompt_execute_command_plan(
            &plan,
            tokio::io::BufReader::new("".as_bytes()),
            &mut output,
        )
        .await
        .expect("prompt should read");

        assert!(!accepted);
        assert_eq!(String::from_utf8(output).expect("utf8"), "execute? [y/N] ");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execute_shell_command_to_writer_writes_complete_output() {
        let mut output = FlushCountingWriter::default();

        super::cmd::execute_shell_command_to_writer(
            "printf 'stdout-line\\n'; sleep 0.01; printf 'stderr-line\\n' >&2",
            &mut output,
        )
        .await
        .expect("shell command should execute");

        assert!(
            output.flushes > 1,
            "shell execution output should flush while streams are read"
        );
        let text = String::from_utf8(output.bytes).expect("utf8");
        assert!(text.contains("stdout-line\n"));
        assert!(text.contains("stderr-line\n"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coding_loop_runtime_includes_skill_roots_in_workspace_read_tools() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let skill_root = temp.path().join("skills");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        std::fs::create_dir_all(skill_root.join("demo")).expect("mkdir skill");
        std::fs::write(
            skill_root.join("demo/SKILL.md"),
            "---\nname: demo-skill\ndescription: Demo skill.\n---\n# Demo\n",
        )
        .expect("write skill");

        let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_workspace_call(
            "call-read-skill",
            WORKSPACE_READ_FILE_TOOL,
            [(
                "path",
                serde_json::Value::String("demo/SKILL.md".to_owned()),
            )],
        )
        .expect("workspace read call should build"))]]);
        let runner = Arc::new(FakeProcessRunner::succeeding(""));
        let runtime = super::build_coding_loop_runtime(
            "coding-loop-skill-root-read",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            ModelName::new("debug-model").unwrap(),
            runner,
            super::CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: vec![skill_root.clone()],
                subagents: SubagentsConfig::default(),
            },
        )
        .expect("runtime should build");

        let events = collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Read demo skill.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should collect pending skill read");
        let pending = first_pending_tool_call(&events).expect("pending skill read");
        let execution_events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("skill read should execute");
        let result = resolved_tool_result(&execution_events);
        assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coding_loop_runtime_allows_missing_default_skill_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let missing_skill_root = temp.path().join("config/merry/skills");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");

        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);
        let runner = Arc::new(FakeProcessRunner::succeeding(""));
        let runtime = super::build_coding_loop_runtime(
            "coding-loop-missing-default-skill-root",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            ModelName::new("debug-model").unwrap(),
            runner,
            super::CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: vec![missing_skill_root],
                subagents: SubagentsConfig::default(),
            },
        )
        .expect("missing default skill root should not block runtime");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Run without configured skills.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");

        let request = provider.recorded_requests()[0].clone();
        assert!(
            request
                .stable_prefix_messages()
                .iter()
                .all(|message| !message.content().as_text().contains("## Skills"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coding_loop_runtime_hides_subagent_tools_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");

        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);
        let runner = Arc::new(FakeProcessRunner::succeeding(""));
        let runtime = super::build_coding_loop_runtime(
            "coding-loop-subagents-default-off",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            ModelName::new("debug-model").unwrap(),
            runner,
            super::CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: Vec::new(),
                subagents: SubagentsConfig::default(),
            },
        )
        .expect("runtime should build");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Inspect available tools.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");
        let requests = provider.recorded_requests();
        let tool_names = requests[0]
            .tools()
            .iter()
            .map(|tool| tool.name().as_str())
            .collect::<Vec<_>>();

        assert!(!tool_names.contains(&"spawn_subagents"));
        assert!(!tool_names.contains(&"wait_subagents"));
        assert!(!tool_names.contains(&"cancel_subagents"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coding_loop_runtime_exposes_subagent_tools_when_enabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");

        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);
        let runner = Arc::new(FakeProcessRunner::succeeding(""));
        let runtime = super::build_coding_loop_runtime(
            "coding-loop-subagents-enabled",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            ModelName::new("debug-model").unwrap(),
            runner,
            super::CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: Vec::new(),
                subagents: SubagentsConfig::enabled_for_test(
                    SubagentConfig::new(2, 1).expect("valid subagent config"),
                ),
            },
        )
        .expect("runtime should build");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Inspect available tools.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");
        let requests = provider.recorded_requests();
        let tool_names = requests[0]
            .tools()
            .iter()
            .map(|tool| tool.name().as_str())
            .collect::<Vec<_>>();

        assert!(tool_names.contains(&"spawn_subagents"));
        assert!(tool_names.contains(&"wait_subagents"));
        assert!(tool_names.contains(&"cancel_subagents"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coding_loop_subagent_with_narrow_tools_keeps_read_only_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        std::fs::write(workspace.join("README.md"), "child fixture\n").expect("write fixture");

        let provider = ScriptedProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(ModelToolCall::new(
                        ModelToolCallId::new("call-spawn").expect("valid call id"),
                        ToolName::new("spawn_subagents").expect("valid tool name"),
                        ToolArguments::try_from(Value::Object(Map::from_iter([(
                            "tasks".to_owned(),
                            Value::Array(vec![Value::Object(Map::from_iter([
                                (
                                    "task".to_owned(),
                                    Value::String("Inspect the fixture.".to_owned()),
                                ),
                                (
                                    "max_model_turns".to_owned(),
                                    Value::Number(serde_json::Number::from(1)),
                                ),
                                (
                                    "allowed_tools".to_owned(),
                                    Value::Array(vec![Value::String(
                                        "workspace_read_file".to_owned(),
                                    )]),
                                ),
                            ]))]),
                        )])))
                        .expect("valid spawn args"),
                    ))],
                    FinishReason::ToolCalls,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("child done")],
                    FinishReason::Stop,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("parent done")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime = super::build_coding_loop_runtime(
            "coding-loop-subagent-narrow-tools",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            ModelName::new("debug-model").unwrap(),
            Arc::new(FakeProcessRunner::succeeding("")),
            super::CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: Vec::new(),
                subagents: SubagentsConfig::enabled_for_test(
                    SubagentConfig::new(2, 1).expect("valid subagent config"),
                ),
            },
        )
        .expect("runtime should build");

        let result = runtime
            .run_agent_loop(
                StepInput::user_text("Delegate fixture inspection.").expect("valid input"),
                StepContext::default(),
                AgentLoopConfig::new(3).expect("valid loop config"),
            )
            .await
            .expect("agent loop should run");

        assert_eq!(result.status(), &AgentLoopStatus::Completed);
        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 3);
        let child_request = requests
            .iter()
            .find(|request| {
                request
                    .dynamic_messages()
                    .iter()
                    .any(|message| message.content().as_text().contains("Inspect the fixture."))
            })
            .expect("child request should be recorded");
        let child_tool_names = child_request
            .tools()
            .iter()
            .map(|tool| tool.name().as_str())
            .collect::<Vec<_>>();
        assert!(child_tool_names.contains(&"workspace_read_file"));
        assert!(child_tool_names.contains(&"workspace_list_dir"));
        assert!(child_tool_names.contains(&"workspace_search_text"));
        assert!(child_tool_names.contains(&"run_process"));
        assert!(!child_tool_names.contains(&"workspace_patch"));
        assert!(!child_tool_names.contains(&"spawn_subagents"));
        assert!(!child_tool_names.contains(&"wait_subagents"));
        assert!(!child_tool_names.contains(&"cancel_subagents"));
    }

    #[test]
    fn coding_loop_task_live_prompt_delegates_to_default_prompt_and_agents() {
        let fixture =
            super::CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
        let prompt = fixture.live_task_prompt(None);

        assert!(prompt.contains("status-text behavior"));
        assert!(prompt.contains("inspect, edit, and verify"));
        assert!(!prompt.contains(super::CODING_LOOP_PROCESS_TOOL));
        assert!(!prompt.contains(super::WORKSPACE_READ_FILE_TOOL));
        assert!(!prompt.contains(super::WORKSPACE_PATCH_TOOL));
        assert!(!prompt.contains("*** Begin Workspace Patch"));
        assert!(!prompt.contains("src/lib.rs"));
        assert!(!prompt.contains("cargo check"));
        assert!(!prompt.contains("rg done"));
        assert!(!prompt.contains(".merry/local/coding-loop-task-live-smoke/src/lib.rs"));
    }

    #[test]
    fn coding_loop_task_status_text_fixture_forces_disambiguated_localized_patch() {
        let fixture =
            super::CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);

        let initial_source = fixture.initial_source();
        let patched_source = fixture.patched_source();

        assert!(initial_source.len() > super::CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES);
        assert!(patched_source.len() > super::CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES);
        assert!(fixture.patch_text().len() < super::CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES);
        assert_eq!(initial_source.matches("value: \"todo\"").count(), 3);
        assert_eq!(patched_source.matches("value: \"todo\"").count(), 2);
        assert!(patched_source.contains("Entry { key: \"default\", value: \"todo\" },"));
        assert!(patched_source.contains("Entry { key: \"status\", value: \"done\" },"));
        assert!(patched_source.contains("Entry { key: \"preview\", value: \"todo\" },"));
        assert!(fixture.source_satisfies_task(&patched_source));
        assert!(!fixture.source_satisfies_task(&initial_source));
        assert_eq!(fixture.package_name(), "merry-coding-loop-task-status-text");
        assert_eq!(fixture.crate_name(), "merry_coding_loop_task_status_text");
        let agents_source = fixture.agents_source();
        assert!(agents_source.contains("Read `tests/status.rs`"));
        assert!(agents_source.contains("cargo check -p merry-coding-loop-task-status-text"));
        assert!(agents_source.contains("cargo test -p merry-coding-loop-task-status-text"));
        let integration_test = fixture.integration_test_source();
        assert!(integration_test.contains("merry_coding_loop_task_status_text"));
        assert!(integration_test.contains("assert_eq!(status(), \"done\")"));
        assert!(
            initial_source.contains("pub fn status() -> &'static str {\n    resolve(\"status\")")
        );
        assert!(initial_source.contains(fixture.patch_remove_line()));
        assert_eq!(
            initial_source.matches(fixture.patch_remove_line()).count(),
            1
        );
        assert!(
            fixture
                .patch_text()
                .starts_with("*** Begin Workspace Patch\n")
        );
        assert!(
            fixture
                .patch_text()
                .contains("*** Update File: src/lib.rs\n")
        );
        assert_eq!(
            initial_source.replacen(fixture.patch_remove_line(), fixture.patch_add_line(), 1),
            patched_source
        );
    }

    #[test]
    fn coding_loop_task_fixture_manifest_opts_out_of_parent_workspace() {
        let fixture =
            super::CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
        let manifest = super::coding_loop_task_fixture_manifest(fixture);

        assert!(manifest.contains("[package]\n"));
        assert!(manifest.contains("name = \"merry-coding-loop-task-status-text\""));
        assert!(
            manifest.ends_with("\n[workspace]\n"),
            "fixture manifest must opt out of parent Cargo workspaces"
        );
    }

    #[test]
    fn coding_loop_task_patch_assertion_accepts_standard_patch_envelope_alias() {
        let fixture =
            super::CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
        let call_id = ToolCallId::new("call-standard-patch").expect("valid call id");
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@
-    Entry { key: \"status\", value: \"todo\" },
+    Entry { key: \"status\", value: \"done\" },
*** End Patch";
        let pending = RuntimeEvent::new(
            merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap(),
            1,
            RuntimeEventKind::ToolCallPending {
                call: PendingToolCall::new(
                    call_id.clone(),
                    ToolName::new(WORKSPACE_PATCH_TOOL).expect("valid tool name"),
                    merry_core::ToolCallArguments::new(Map::from_iter([(
                        "patch".to_owned(),
                        Value::String(patch.to_owned()),
                    )])),
                ),
            },
        );
        let resolved = RuntimeEvent::new(
            merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap(),
            2,
            RuntimeEventKind::ToolCallResolved {
                result: ToolCallResult::succeeded(
                    call_id,
                    ArtifactRef::new(
                        merry_core::ArtifactId::new("tool-result-2").unwrap(),
                        ArtifactKind::Json,
                    ),
                ),
            },
        );

        super::assert_coding_loop_task_smoke_uses_small_patch(&[pending, resolved], fixture)
            .expect("standard patch envelope alias should pass smoke patch assertion");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coding_loop_smoke_respects_configured_log_path_and_keeps_payloads_out_of_events() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let config_root = temp.path().join("config");
        let state_root = temp.path().join("state");
        let expected_log_path = state_root.join("merry/logs/merry.jsonl");
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
        assert_eq!(log_settings.path, expected_log_path);
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
            None,
            merry_runtime::AutomaticCompactionConfig::default(),
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

[models.context_compaction]
model = "gpt-compact"

[models.approval_review]
model = "gpt-review"
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
        assert_eq!(loaded.primary.model, "gpt-test");
        assert_eq!(
            loaded.primary.provider.base_url(),
            "https://api.example.test/v1"
        );
        let context_compaction = loaded
            .context_compaction
            .expect("context compaction debug config should load");
        assert_eq!(context_compaction.model, "gpt-compact");
        assert_eq!(
            context_compaction.provider.base_url(),
            "https://api.example.test/v1"
        );
        let approval_review = loaded
            .approval_review
            .expect("approval review debug config should load");
        assert_eq!(approval_review.model, "gpt-review");
        assert_eq!(
            approval_review.provider.base_url(),
            "https://api.example.test/v1"
        );
    }

    #[test]
    fn openai_debug_model_flag_overrides_only_primary_model() {
        let paths = super::config::XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = super::config::MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[providers.openai-compatible]
api_key = "sk-inline-secret"

[models.context_compaction]
provider = "openai-compatible"
model = "gpt-compact"

[models.approval_review]
provider = "openai-compatible"
model = "gpt-review"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let loaded = debug_openai_config_with_env(Some("gpt-flag"), Some(&config), |name| {
            Ok((name == "MERRY_OPENAI_DEBUG").then(|| "1".to_owned()))
        })
        .expect("debug config should load");

        assert_eq!(loaded.primary.model, "gpt-flag");
        assert_eq!(
            loaded
                .context_compaction
                .expect("context compaction debug config should load")
                .model,
            "gpt-compact"
        );
        assert_eq!(
            loaded
                .approval_review
                .expect("approval review debug config should load")
                .model,
            "gpt-review"
        );
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
            _ => panic!("expected shell subcommand"),
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
            _ => panic!("expected shell subcommand"),
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
        observed_cwd: Arc<Mutex<Vec<Option<String>>>>,
        outputs: Arc<Mutex<Vec<FakeProcessRunnerStep>>>,
    }

    impl FakeProcessRunner {
        fn succeeding(stdout: impl Into<String>) -> Self {
            Self::scripted([FakeProcessRunnerStep::success(stdout)])
        }

        fn scripted<const N: usize>(steps: [FakeProcessRunnerStep; N]) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                observed_argv: Arc::new(Mutex::new(Vec::new())),
                observed_cwd: Arc::new(Mutex::new(Vec::new())),
                outputs: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
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

        fn observed_cwd(&self) -> Vec<Option<String>> {
            self.observed_cwd
                .lock()
                .expect("observed cwd mutex should not be poisoned")
                .clone()
        }
    }

    #[derive(Clone)]
    struct FakeProcessRunnerStep {
        status: ProcessExitStatus,
        stdout: String,
        stderr: String,
    }

    impl FakeProcessRunnerStep {
        fn success(stdout: impl Into<String>) -> Self {
            Self {
                status: ProcessExitStatus::Exited(0),
                stdout: stdout.into(),
                stderr: String::new(),
            }
        }

        fn failure(stderr: impl Into<String>) -> Self {
            Self {
                status: ProcessExitStatus::Exited(1),
                stdout: String::new(),
                stderr: stderr.into(),
            }
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
                self.observed_cwd
                    .lock()
                    .expect("observed cwd mutex should not be poisoned")
                    .push(intent.cwd().map(str::to_owned));
                if context.cancellation_token().is_cancelled() {
                    return Err(ProcessRunnerError::Cancelled);
                }
                let output = self
                    .outputs
                    .lock()
                    .expect("fake process outputs mutex should not be poisoned")
                    .pop()
                    .unwrap_or_else(|| FakeProcessRunnerStep::success(String::new()));

                ProcessRunnerOutput::new(
                    &intent,
                    output.status,
                    output.stdout,
                    false,
                    output.stderr,
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
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ] {
            assert!(args.iter().any(|arg| arg == expected), "missing {expected}");
        }
        assert!(!args.iter().any(|arg| arg == "--disable-userns"));
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
        assert!(contains_sequence(&args, &["--dir", "/etc"]));
        assert!(contains_sequence(&args, &["--ro-bind", "/usr", "/usr"]));
        assert!(contains_sequence(&args, &["--ro-bind-try", "/bin", "/bin"]));
        assert!(contains_sequence(&args, &["--ro-bind-try", "/lib", "/lib"]));
        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/lib64", "/lib64"]
        ));
        assert!(contains_sequence(&args, &["--ro-bind-try", "/opt", "/opt"]));
        assert!(contains_sequence(
            &args,
            &[
                "--dir",
                "/etc",
                "--ro-bind",
                "/etc/ld.so.conf",
                "/etc/ld.so.conf"
            ]
        ));
        assert!(contains_sequence(
            &args,
            &[
                "--dir",
                "/etc",
                "--ro-bind",
                "/etc/resolv.conf",
                "/etc/resolv.conf"
            ]
        ));
        assert!(contains_sequence(
            &args,
            &["--dir", "/etc", "--ro-bind", "/etc/hosts", "/etc/hosts"]
        ));
        assert!(contains_sequence(
            &args,
            &[
                "--dir",
                "/etc",
                "--ro-bind",
                "/etc/nsswitch.conf",
                "/etc/nsswitch.conf"
            ]
        ));
        assert!(contains_sequence(
            &args,
            &[
                "--dir",
                "/etc",
                "--ro-bind",
                "/etc/ld.so.conf.d",
                "/etc/ld.so.conf.d"
            ]
        ));
        assert!(contains_sequence(
            &args,
            &["--dir", "/etc", "--ro-bind", "/etc/ssl", "/etc/ssl"]
        ));
        assert!(contains_sequence(
            &args,
            &[
                "--dir",
                "/etc",
                "--ro-bind",
                "/etc/ld.so.cache",
                "/etc/ld.so.cache"
            ]
        ));
        assert!(!contains_sequence(
            &args,
            &["--ro-bind-try", "/etc/ld.so.cache", "/etc/ld.so.cache"]
        ));
        assert!(!contains_sequence(
            &args,
            &["--ro-bind-try", "/etc/resolv.conf", "/etc/resolv.conf"]
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
    fn sandbox_plan_applies_trusted_global_path_rules_as_outer_guard() {
        let mut host = sandbox_host();
        host.trusted_path_rules = vec![
            PathAccessRule::new(
                PathBuf::from("/var/log"),
                PathAccess::ReadOnly,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
            PathAccessRule::new(
                PathBuf::from("/workspace/shared"),
                PathAccess::ReadWrite,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
            PathAccessRule::new(
                PathBuf::from("/home/alice/.ssh"),
                PathAccess::Deny,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
        ];
        let SandboxBootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/var/log", "/var/log"]
        ));
        assert!(contains_sequence(
            &args,
            &["--bind-try", "/workspace/shared", "/workspace/shared"]
        ));
        assert!(contains_sequence(&args, &["--tmpfs", "/home/alice/.ssh"]));
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
        assert!(!contains_sequence(
            &args,
            &["--setenv", MERRY_OPENAI_DEBUG_ENV, "1"]
        ));
        assert!(!args.iter().any(|arg| arg.contains("OPENAI_API_KEY")));
        assert!(!args.iter().any(|arg| arg.contains("MERRY_OPENAI_API_KEY")));
    }

    #[test]
    fn sandbox_plan_preserves_openai_debug_opt_in_without_secret_env() {
        let mut host = sandbox_host();
        host.openai_debug = Some(os("1"));
        let SandboxBootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        assert!(contains_sequence(
            &args,
            &["--setenv", MERRY_OPENAI_DEBUG_ENV, "1"]
        ));
        assert!(!args.iter().any(|arg| arg.contains("OPENAI_API_KEY")));
        assert!(!args.iter().any(|arg| arg.contains("MERRY_OPENAI_API_KEY")));
    }

    #[test]
    fn sandbox_plan_does_not_preserve_non_opt_in_openai_debug_values() {
        for value in ["0", "true", ""] {
            let mut host = sandbox_host();
            host.openai_debug = Some(os(value));
            let SandboxBootstrap::Reexec(plan) =
                plan_sandbox(true, &host).expect("sandbox planning should succeed")
            else {
                panic!("expected sandbox reexec plan");
            };
            let args = plan_args(&plan);

            assert!(!contains_sequence(
                &args,
                &["--setenv", MERRY_OPENAI_DEBUG_ENV, "1"]
            ));
        }
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

        fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
            self.capabilities = capabilities;
            self
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
    async fn configured_runtime_builder_applies_auto_compaction_config() {
        let paths = super::config::XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = super::config::MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
retained_raw_tail_items = 4
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");
        let primary = ScriptedProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("old assistant from configured builder")],
                    FinishReason::Stop,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        "tail one assistant from configured builder",
                    )],
                    FinishReason::Stop,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        "tail two assistant from configured builder",
                    )],
                    FinishReason::Stop,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("final after configured compaction")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ])
        .with_capabilities(
            ModelCapabilities::new(true, true, false, true, Some(420), Some(16))
                .expect("valid capabilities"),
        );
        let compactor = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text(
                    r#"{
                      "claims": [
                        {
                          "id": "c1",
                          "kind": "completed_action",
                          "text": "Configured builder compacted the old turn only.",
                          "refs": ["r1", "r2"]
                        }
                      ],
                      "working_intent": null
                    }"#,
                )],
                FinishReason::Stop,
                None,
            ),
        })]]);
        let runtime = super::configured_runtime_builder(
            merry_core::SessionId::new("configured-builder-auto-compaction").unwrap(),
            Some(&config),
        )
        .expect("configured builder should build")
        .model_provider(
            Arc::new(primary.clone()),
            ModelName::new("debug-model").unwrap(),
        )
        .model_provider_for_role(
            merry_runtime::RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("debug-compactor").unwrap(),
        )
        .build()
        .expect("runtime should build");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text(&"old user from configured builder ".repeat(70))
                .expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("old step should run");
        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("tail one user from configured builder").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("tail one step should run");
        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("tail two user from configured builder").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("tail two step should run");
        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("current user from configured builder").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("current step should run");

        let compactor_requests = compactor.recorded_requests();
        assert_eq!(compactor_requests.len(), 1);
        let compaction_text = compactor_requests[0]
            .messages()
            .iter()
            .map(|message| message.content().as_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(compaction_text.contains("old user from configured builder"));
        assert!(!compaction_text.contains("tail one user from configured builder"));
        assert!(!compaction_text.contains("tail two user from configured builder"));
        assert!(!compaction_text.contains("current user from configured builder"));
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

    #[tokio::test]
    async fn task_live_smoke_report_preserves_runtime_events_on_failure() {
        let runtime =
            Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
                .build()
                .expect("runtime should build");
        let event = RuntimeEvent::new(
            merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap(),
            1,
            merry_core::RuntimeEventKind::StepStarted,
        );
        let mut output = Vec::new();

        write_coding_loop_task_live_smoke_report(
            &runtime,
            merry_runtime::AutomaticCompactionConfig::default(),
            false,
            &[event],
            &mut output,
        )
        .await
        .unwrap_or_else(|_| panic!("task live smoke report should write"));

        let text = String::from_utf8(output).expect("output should be utf-8");
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("coding-loop-task-live-smoke: failed"));
        let event = lines
            .next()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("event should parse"))
            .expect("failure report should include runtime event JSONL");
        assert!(matches!(
            event.kind,
            merry_core::RuntimeEventKind::StepStarted
        ));
        let config_summary = lines
            .next()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("line should parse"))
            .expect("failure report should include compaction config summary");
        assert_eq!(
            config_summary["type"],
            serde_json::Value::String("runtime_compaction_config_summary".to_owned())
        );
        let compaction_summary = lines
            .next()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("line should parse"))
            .expect("failure report should include compaction summary");
        assert_eq!(
            compaction_summary["type"],
            serde_json::Value::String("runtime_compaction_summary".to_owned())
        );
        assert_eq!(compaction_summary["checkpoint_present"], false);
        assert!(lines.next().is_none());
    }

    #[tokio::test]
    async fn task_live_smoke_report_includes_process_artifact_preview() {
        let call_id = "call-check";
        let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_process_call(
            call_id,
            &["cargo", "check"],
            Some("."),
        )
        .expect("process call event should build"))]]);
        let runtime =
            Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
                .model_provider(Arc::new(provider), ModelName::new("debug-model").unwrap());
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let profile = WorkspaceCodingLoopProfile::new(WorkspaceToolsConfig::new(vec![
            temp.path().to_path_buf(),
        ]))
        .expect("workspace profile should build")
        .with_cli_bwrap_process_runner(
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(FakeProcessRunner::scripted([
                FakeProcessRunnerStep::failure("cargo failed\n"),
            ])),
        );
        let profile = RuntimeProfile::builder()
            .with_workspace_coding_loop(profile)
            .expect("workspace profile should apply")
            .build()
            .expect("runtime profile should build");
        let runtime = runtime
            .with_profile(profile)
            .expect("runtime profile should apply")
            .build()
            .expect("runtime should build");
        let events = collect_runtime_step_events(
            &runtime,
            StepInput::user_text("run check").expect("valid step input"),
            StepContext::default(),
        )
        .await
        .expect("step should collect process call");
        let pending = first_pending_tool_call(&events).expect("pending tool call should exist");
        let execution_events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("process call should execute");
        let mut output = Vec::new();

        write_coding_loop_task_live_smoke_report(
            &runtime,
            merry_runtime::AutomaticCompactionConfig::default(),
            false,
            &execution_events,
            &mut output,
        )
        .await
        .expect("task live smoke report should write");

        let text = String::from_utf8(output).expect("output should be utf-8");
        assert!(text.contains("\"type\":\"tool_call_resolved\""));
        assert!(text.contains("\"type\":\"process_artifact_preview\""));
        assert!(text.contains("\"call_id\":\"call-check\""));
        assert!(text.contains("\"stderr\":\"cargo failed\\n\""));
    }

    #[tokio::test]
    async fn permission_network_smoke_report_includes_tool_calls_and_process_previews() {
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let runner = Arc::new(FakeProcessRunner::scripted([
            FakeProcessRunnerStep::failure("network unreachable\n"),
            FakeProcessRunnerStep::success("93.184.216.34 example.com\n"),
        ]));
        let runtime = super::build_scripted_permission_network_smoke_runtime(
            temp.path(),
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            runner.clone(),
            Arc::new(merry_runtime::StaticPermissionedProcessRunnerFactory::new(
                runner,
            )),
            merry_runtime::AutomaticCompactionConfig::default(),
        )
        .expect("permission network smoke runtime should build");
        let result = runtime
            .run_agent_loop(
                StepInput::user_text("run permission network smoke").expect("valid step input"),
                StepContext::default(),
                AgentLoopConfig::new(6).expect("valid loop config"),
            )
            .await
            .expect("permission network smoke should run");
        super::assert_permission_network_smoke_result(&runtime, &result)
            .await
            .expect("permission network smoke assertions should pass");

        let mut output = Vec::new();
        write_permission_network_smoke_report(&runtime, result.events(), &mut output)
            .await
            .expect("permission network smoke report should write");

        let text = String::from_utf8(output).expect("output should be utf-8");
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("permission-network-smoke: ok"));
        assert!(text.contains("\"type\":\"tool_call_pending\""));
        assert!(text.contains("\"name\":\"run_process\""));
        assert!(text.contains("\"name\":\"request_permissions\""));
        assert!(text.contains("\"network\":true"));
        assert!(text.contains("\"type\":\"tool_call_resolved\""));
        assert!(text.contains("\"type\":\"process_artifact_preview\""));
        assert!(text.contains("\"call_id\":\"permission-network-smoke-initial-network\""));
        assert!(text.contains("\"call_id\":\"permission-network-smoke-request-network\""));
        assert!(text.contains("\"stderr\":\"network unreachable\\n\""));
        assert!(text.contains("\"stdout\":\"93.184.216.34 example.com\\n\""));
    }

    #[tokio::test]
    async fn task_live_smoke_report_includes_compaction_summary_without_checkpoint_text() {
        let manifest = CheckpointRefManifest::new(
            CheckpointId::new("checkpoint-task-live-smoke").expect("valid checkpoint id"),
            vec![
                CheckpointRef::new(
                    CheckpointRefId::new("r1").expect("valid ref id"),
                    CheckpointSourceKind::UserMessage,
                    "history:1",
                    CheckpointSequenceRange::new(1, 1).expect("valid sequence range"),
                    "body[0]",
                    "sensitive old task detail should not appear in smoke report",
                )
                .expect("valid ref"),
            ],
        )
        .expect("valid manifest");
        let candidate = CompactedCheckpointCandidate::from_json(
            r#"{
              "claims": [
                {
                  "id": "c1",
                  "kind": "current_state",
                  "text": "The old task window was compacted.",
                  "refs": ["r1"]
                }
              ],
              "working_intent": null
            }"#,
        )
        .expect("valid candidate");
        let citation = CitationBackedCheckpoint::from_candidate(
            CheckpointId::new("checkpoint-task-live-smoke").expect("valid checkpoint id"),
            candidate,
            manifest,
            CheckpointValidationPolicy::default(),
        )
        .expect("valid citation-backed checkpoint");
        let checkpoint =
            CompactedCheckpoint::from_citation_backed(citation).expect("valid checkpoint");
        let runtime =
            Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
                .compacted_checkpoint(checkpoint)
                .build()
                .expect("runtime should build");
        let mut output = Vec::new();

        write_coding_loop_task_live_smoke_report(
            &runtime,
            merry_runtime::AutomaticCompactionConfig::default(),
            true,
            &[],
            &mut output,
        )
        .await
        .expect("task live smoke report should write");

        let text = String::from_utf8(output).expect("output should be utf-8");
        assert!(text.contains("\"type\":\"runtime_compaction_summary\""));
        assert!(text.contains("\"checkpoint_present\":true"));
        assert!(text.contains("\"citation_backed\":true"));
        assert!(text.contains("\"checkpoint_id\":\"checkpoint-task-live-smoke\""));
        assert!(text.contains("\"claim_count\":1"));
        assert!(text.contains("\"ref_count\":1"));
        assert!(!text.contains("The old task window was compacted."));
        assert!(!text.contains("sensitive old task detail"));
    }

    #[tokio::test]
    async fn task_live_smoke_report_includes_effective_compaction_config_from_toml() {
        let paths = super::config::XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = super::config::MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
enabled = true
target_output_tokens = 144
model_output_token_limit = 233
max_accepted_output_bytes = 3456
retained_raw_tail_items = 5
max_ref_excerpt_bytes = 789
max_carried_prior_refs = 10
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");
        let auto_compaction = super::automatic_compaction_config(Some(&config))
            .expect("auto compaction should validate");
        let runtime =
            Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
                .build()
                .expect("runtime should build");
        let mut output = Vec::new();

        write_coding_loop_task_live_smoke_report(&runtime, auto_compaction, true, &[], &mut output)
            .await
            .expect("task live smoke report should write");

        let text = String::from_utf8(output).expect("output should be utf-8");
        assert!(text.contains("\"type\":\"runtime_compaction_config_summary\""));
        assert!(text.contains("\"auto_compaction_enabled\":true"));
        assert!(text.contains("\"target_output_tokens\":144"));
        assert!(text.contains("\"model_output_token_limit\":233"));
        assert!(text.contains("\"max_accepted_output_bytes\":3456"));
        assert!(text.contains("\"retained_raw_tail_items\":5"));
        assert!(text.contains("\"max_ref_excerpt_bytes\":789"));
        assert!(text.contains("\"max_carried_prior_refs\":10"));
    }
}
