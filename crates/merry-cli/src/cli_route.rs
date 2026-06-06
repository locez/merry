use crate::cli::{self, Cli, CliCommand};
use crate::cli_error::CliError;
use crate::cli_exit::CliExit;
use crate::cmd;
use crate::config::MerryConfig;
use crate::debug::{
    self, Args as DebugArgs, CodingLoopLiveSmokeArgs as DebugCodingLoopLiveSmokeArgs,
    CodingLoopSubagentLiveSmokeArgs as DebugCodingLoopSubagentLiveSmokeArgs,
    CodingLoopTaskLiveSmokeArgs as DebugCodingLoopTaskLiveSmokeArgs, Command as DebugCommand,
    OpenAiArgs as DebugOpenAiArgs, PermissionNetworkSmokeArgs as DebugPermissionNetworkSmokeArgs,
};
use crate::run as run_command;

pub(crate) async fn run(cli: Cli, merry_config: Option<MerryConfig>) -> CliExit {
    let sandbox_child_handoff = cli.sandbox_child_handoff;

    match cli.command {
        CliCommand::Run(args) => map_result(
            run_command::run(&args, sandbox_child_handoff, merry_config.as_ref()).await,
            cli::run_usage,
            cli::run_usage,
        ),
        CliCommand::Cmd(args) => map_result(
            cmd::run(&args, merry_config.as_ref()).await,
            cli::cmd_usage,
            cli::cmd_usage,
        ),
        CliCommand::Debug(DebugArgs {
            session_id,
            input,
            command: None,
        }) => map_result(
            debug::basic::run(&session_id, &input, merry_config.as_ref()).await,
            cli::debug_usage,
            cli::debug_openai_usage,
        ),
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::OpenAi(DebugOpenAiArgs {
                    input,
                    model,
                    max_output_tokens,
                    debug_tool_result,
                })),
            ..
        }) => map_debug_openai_result(
            debug::openai::run(
                &input,
                model.as_deref(),
                max_output_tokens,
                debug_tool_result.as_deref(),
                merry_config.as_ref(),
            )
            .await,
        ),
        CliCommand::Debug(DebugArgs {
            command: Some(DebugCommand::Shell(args)),
            ..
        }) => map_shell_result(
            debug::shell::run(args, sandbox_child_handoff, merry_config.as_ref()).await,
        ),
        CliCommand::Debug(DebugArgs {
            command: Some(DebugCommand::CodingLoopSmoke),
            ..
        }) => map_result(
            debug::coding_loop::run_smoke(sandbox_child_handoff, merry_config.as_ref()).await,
            cli::debug_usage,
            cli::debug_openai_usage,
        ),
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::PermissionNetworkSmoke(DebugPermissionNetworkSmokeArgs {
                    model,
                    max_output_tokens,
                })),
            ..
        }) => map_result(
            debug::coding_loop::run_permission_network_smoke(
                sandbox_child_handoff,
                model.as_deref(),
                max_output_tokens,
                merry_config.as_ref(),
            )
            .await,
            cli::debug_usage,
            cli::debug_openai_usage,
        ),
        CliCommand::Debug(DebugArgs {
            command: Some(DebugCommand::CodingLoopTaskSmoke(args)),
            ..
        }) => map_result(
            debug::coding_loop::run_task_smoke(
                sandbox_child_handoff,
                args.task,
                merry_config.as_ref(),
            )
            .await,
            cli::debug_usage,
            cli::debug_openai_usage,
        ),
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::CodingLoopLiveSmoke(DebugCodingLoopLiveSmokeArgs {
                    model,
                    max_output_tokens,
                })),
            ..
        }) => map_result(
            debug::coding_loop::run_live_smoke(
                sandbox_child_handoff,
                model.as_deref(),
                max_output_tokens,
                merry_config.as_ref(),
            )
            .await,
            cli::debug_usage,
            cli::debug_coding_loop_live_smoke_usage,
        ),
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::CodingLoopTaskLiveSmoke(DebugCodingLoopTaskLiveSmokeArgs {
                    task,
                    model,
                    max_output_tokens,
                })),
            ..
        }) => map_result(
            debug::coding_loop::run_task_live_smoke(
                sandbox_child_handoff,
                task,
                model.as_deref(),
                max_output_tokens,
                merry_config.as_ref(),
            )
            .await,
            cli::debug_usage,
            cli::debug_coding_loop_task_live_smoke_usage,
        ),
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::CodingLoopSubagentLiveSmoke(DebugCodingLoopSubagentLiveSmokeArgs {
                    model,
                    max_output_tokens,
                })),
            ..
        }) => map_result(
            debug::coding_loop::run_subagent_live_smoke(
                sandbox_child_handoff,
                model.as_deref(),
                max_output_tokens,
                merry_config.as_ref(),
            )
            .await,
            cli::debug_usage,
            cli::debug_coding_loop_subagent_live_smoke_usage,
        ),
    }
}

fn map_result(
    result: Result<(), CliError>,
    debug_usage: impl FnOnce() -> String,
    debug_openai_usage: impl FnOnce() -> String,
) -> CliExit {
    match result {
        Ok(()) | Err(CliError::BrokenPipe) => CliExit::Success,
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
            usage: cli::shell_usage(),
        },
        Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
    }
}

fn map_debug_openai_result(result: Result<(), CliError>) -> CliExit {
    match result {
        Ok(()) | Err(CliError::BrokenPipe) => CliExit::Success,
        Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
            message,
            usage: cli::debug_openai_usage(),
        },
        Err(CliError::DebugUsage(message)) => CliExit::Usage {
            message,
            usage: cli::debug_usage(),
        },
        Err(CliError::ShellUsage(message)) => CliExit::Usage {
            message,
            usage: cli::shell_usage(),
        },
        Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
    }
}

fn map_shell_result(result: Result<(), CliError>) -> CliExit {
    match result {
        Ok(()) | Err(CliError::BrokenPipe) => CliExit::Success,
        Err(CliError::ShellUsage(message)) => CliExit::Usage {
            message,
            usage: cli::shell_usage(),
        },
        Err(CliError::DebugUsage(message)) => CliExit::Usage {
            message,
            usage: cli::debug_usage(),
        },
        Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
            message,
            usage: cli::debug_openai_usage(),
        },
        Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
    }
}
