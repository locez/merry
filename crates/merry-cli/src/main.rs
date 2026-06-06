//! Debug and demonstration CLI for Merry.

mod cmd;
mod coding_runtime;
mod config;
mod debug;
mod observability;
mod provider_config;
mod run;
mod runtime_config;
mod runtime_events;
mod sandbox;
#[cfg(test)]
mod test_support;

use clap::{Args, CommandFactory, Parser, Subcommand};
use config::{MerryConfig, XdgPaths};
use merry_tool_workspace::{
    CODING_LOOP_PROCESS_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
};
use std::{
    env, fmt, io,
    process::{ExitCode, Termination},
};

use debug::{
    Args as DebugArgs, CodingLoopLiveSmokeArgs as DebugCodingLoopLiveSmokeArgs,
    CodingLoopSubagentLiveSmokeArgs as DebugCodingLoopSubagentLiveSmokeArgs,
    CodingLoopTaskLiveSmokeArgs as DebugCodingLoopTaskLiveSmokeArgs, Command as DebugCommand,
    OpenAiArgs as DebugOpenAiArgs, PermissionNetworkSmokeArgs as DebugPermissionNetworkSmokeArgs,
};
use runtime_config::{effective_log_settings, validate_loaded_config};
use sandbox::ChildHandoff as SandboxChildHandoff;

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
    Run(run::Args),
    #[command(about = "Generate a shell command plan from a natural-language request")]
    Cmd(cmd::Args),
    #[command(about = "Print deterministic runtime events or run opt-in provider debugging")]
    Debug(DebugArgs),
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
            match run::run(&args, sandbox_child_handoff, merry_config.as_ref()).await {
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
        }) => match debug::basic::run(&session_id, &input, merry_config.as_ref()).await {
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
        }) => match debug::openai::run(
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
            command: Some(DebugCommand::Shell(args)),
            ..
        }) => match debug::shell::run(args, sandbox_child_handoff, merry_config.as_ref()).await {
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
        CliCommand::Debug(DebugArgs {
            command: Some(DebugCommand::CodingLoopSmoke),
            ..
        }) => match debug::coding_loop::run_smoke(sandbox_child_handoff, merry_config.as_ref())
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
                Some(DebugCommand::PermissionNetworkSmoke(DebugPermissionNetworkSmokeArgs {
                    model,
                    max_output_tokens,
                })),
            ..
        }) => {
            match debug::coding_loop::run_permission_network_smoke(
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
        }) => match debug::coding_loop::run_task_smoke(
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
        }) => match debug::coding_loop::run_live_smoke(
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
        }) => match debug::coding_loop::run_task_live_smoke(
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
        }) => match debug::coding_loop::run_subagent_live_smoke(
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
    let debug_command = command
        .find_subcommand_mut("debug")
        .expect("debug subcommand should exist");
    let command = debug_command
        .find_subcommand_mut("shell")
        .expect("shell subcommand should exist");
    command.set_bin_name("merry debug shell");
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
    use super::DEBUG_TOOL_NAME;
    use super::{
        Cli, CliCommand, CliError, CliExit, DEBUG_TOOL_CONTINUATION_INPUT, DEFAULT_INPUT,
        DEFAULT_SESSION_ID, DebugCommand, SandboxChildHandoff, debug_openai_usage, report_cli_exit,
        shell_usage,
    };
    use crate::debug::CodingLoopTaskSmokeTask;
    use crate::debug::coding_loop::{
        CodingLoopTaskSmokeFixture, assert_coding_loop_smoke_result,
        assert_coding_loop_task_smoke_result, assert_coding_loop_task_smoke_uses_small_patch,
        assert_permission_network_smoke_result, build_coding_loop_smoke_runtime,
        build_coding_loop_task_smoke_runtime, build_scripted_permission_network_smoke_runtime,
        coding_loop_process_call, coding_loop_smoke_initial_source,
        coding_loop_subagent_live_smoke_task, coding_loop_task_fixture_manifest,
        coding_loop_tool_call, coding_loop_workspace_call,
        run_smoke as run_debug_coding_loop_smoke, write_coding_loop_task_live_smoke_report,
        write_permission_network_smoke_report,
    };
    use crate::debug::openai::{
        echo_tool as debug_echo_tool, write_tool_events as write_debug_openai_tool_events,
    };
    use crate::debug::shell::{
        process_action_intent as shell_process_action_intent, run_to_writer as run_shell_to_writer,
        runtime_admission as shell_runtime_admission,
    };
    use crate::provider_config::MERRY_OPENAI_DEBUG_ENV;
    use crate::runtime_config::{automatic_compaction_config, effective_log_settings};
    use crate::runtime_events::{collect_runtime_step_events, first_pending_tool_call};
    use crate::sandbox::{
        Bootstrap as SandboxBootstrap, Error as SandboxError, Host as SandboxHost,
        MERRY_SANDBOX_ENV, MERRY_SANDBOX_VERSION, MERRY_SANDBOX_VERSION_ENV, Plan as SandboxPlan,
        RuntimeProfile as SandboxRuntimeProfile, SANDBOX_CHILD_HANDOFF_ARG,
        SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1, SANDBOX_HOME, SANDBOX_MERRY_CONFIG_DIR,
        SANDBOX_MERRY_LOG_DIR, SANDBOX_TMPDIR, SANDBOX_XDG_CONFIG_HOME, SANDBOX_XDG_STATE_HOME,
        args_without_sandbox_bootstrap_flags, find_bwrap_in_path, os,
        plan_bootstrap_with_file_exists as plan_sandbox_bootstrap_with_file_exists,
        runtime_profile_from_evidence as sandbox_runtime_profile_from_evidence,
    };
    use crate::test_support::{FakeProcessRunner, FakeProcessRunnerStep, ScriptedProvider};
    use clap::Parser;
    use merry_core::{
        ArtifactKind, ArtifactRef, PendingToolCall, RuntimeEvent, RuntimeEventKind, ToolCallId,
        ToolCallResult, ToolCallResultStatus, ToolName,
    };
    use merry_llm::{
        FinishReason, GenerationConfig, ModelEvent, ModelName, ModelOutput, ModelResponse,
        ModelToolCall, ModelToolCallId, ToolArguments,
    };
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, ArtifactContent, CheckpointId,
        CheckpointRef, CheckpointRefId, CheckpointRefManifest, CheckpointSequenceRange,
        CheckpointSourceKind, CheckpointValidationPolicy, CitationBackedCheckpoint,
        CompactedCheckpoint, CompactedCheckpointCandidate, MAX_PROCESS_OUTPUT_LIMIT_BYTES,
        PathAccess, PathAccessRule, PathAccessRuleSource, ProcessEnvPolicy, Runtime,
        RuntimeProfile, StepContext, StepInput, ToolExecutionContext,
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

    fn plan_args(plan: &SandboxPlan) -> Vec<String> {
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
                    DebugCommand::Shell(_)
                    | DebugCommand::CodingLoopSmoke
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
                    | DebugCommand::Shell(_)
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
                    | DebugCommand::Shell(_)
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
                    | DebugCommand::Shell(_)
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
                    | DebugCommand::Shell(_)
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
                    | DebugCommand::Shell(_)
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
                    | DebugCommand::Shell(_)
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
        let prompt = coding_loop_subagent_live_smoke_task();

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
        let fixture = CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
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
        let runtime = build_coding_loop_task_smoke_runtime(
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
        assert_coding_loop_task_smoke_result(&runtime, &result, &smoke_root, fixture)
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
        let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_tool_call(
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

    #[test]
    fn coding_loop_task_live_prompt_delegates_to_default_prompt_and_agents() {
        let fixture = CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
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
        let fixture = CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);

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
        let fixture = CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
        let manifest = coding_loop_task_fixture_manifest(fixture);

        assert!(manifest.contains("[package]\n"));
        assert!(manifest.contains("name = \"merry-coding-loop-task-status-text\""));
        assert!(
            manifest.ends_with("\n[workspace]\n"),
            "fixture manifest must opt out of parent Cargo workspaces"
        );
    }

    #[test]
    fn coding_loop_task_patch_assertion_accepts_standard_patch_envelope_alias() {
        let fixture = CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
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

        assert_coding_loop_task_smoke_uses_small_patch(&[pending, resolved], fixture)
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
        let log_settings = effective_log_settings(Some(&config), &paths)
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
            coding_loop_smoke_initial_source(),
        )
        .expect("fixture source should write");
        let runtime = build_coding_loop_smoke_runtime(
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
        assert_coding_loop_smoke_result(&runtime, &result, &smoke_root)
            .await
            .expect("coding-loop smoke result should validate");
    }

    #[test]
    fn clap_parses_shell_argv() {
        let cli = Cli::try_parse_from(["merry", "debug", "shell", "--", "rustc", "--version"])
            .expect("shell args should parse");

        match cli.command {
            CliCommand::Debug(debug) => match debug.command {
                Some(DebugCommand::Shell(shell)) => {
                    assert!(!shell.accept_local_workspace_process_risk);
                    assert_eq!(shell.argv, ["rustc", "--version"]);
                }
                _ => panic!("expected shell subcommand"),
            },
            _ => panic!("expected shell subcommand"),
        }
    }

    #[test]
    fn clap_parses_shell_local_workspace_process_risk_acceptance() {
        let cli = Cli::try_parse_from([
            "merry",
            "debug",
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
            CliCommand::Debug(debug) => match debug.command {
                Some(DebugCommand::Shell(shell)) => {
                    assert!(shell.accept_local_workspace_process_risk);
                    assert_eq!(shell.argv, ["cargo", "test", "-p", "merry-runtime"]);
                }
                _ => panic!("expected shell subcommand"),
            },
            _ => panic!("expected shell subcommand"),
        }
    }

    #[test]
    fn clap_parses_hidden_sandbox_child_handoff() {
        let cli = Cli::try_parse_from([
            "merry",
            SANDBOX_CHILD_HANDOFF_ARG,
            SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1,
            "debug",
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
        let error = Cli::try_parse_from(["merry", "debug", "shell", "rustc", "--version"])
            .expect_err("shell argv should require `--` separator");

        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn shell_usage_contains_shell_usage() {
        assert!(shell_usage().contains("Usage: merry debug shell [OPTIONS] -- <ARGV>..."));
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
        let runtime = build_scripted_permission_network_smoke_runtime(
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
        assert_permission_network_smoke_result(&runtime, &result)
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
        let auto_compaction =
            automatic_compaction_config(Some(&config)).expect("auto compaction should validate");
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
