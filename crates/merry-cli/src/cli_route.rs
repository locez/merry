use crate::cli::{self, Cli, CliCommand};
use crate::cli_error::CliError;
use crate::cli_exit::CliExit;
use crate::cmd;
use crate::config::MerryConfig;
use crate::debug::{
    self, Args as DebugArgs, Command as DebugCommand, OpenAiArgs as DebugOpenAiArgs,
};
use crate::run as run_command;
use crate::tui;

pub(crate) async fn run(cli: Cli, merry_config: Option<MerryConfig>) -> CliExit {
    let sandbox_mode = cli.sandbox_mode();
    let permission_mode = cli.permission_mode();
    let auto_fallback = cli.auto_fallback();
    let trust_workspace = cli.trust_workspace();
    let worktree_gc = matches!(
        cli.command,
        None | Some(CliCommand::Resume) | Some(CliCommand::Run(_))
    )
    .then(|| tokio::task::spawn_blocking(merry::profiles::cleanup_orphaned_worktrees_default));

    let exit = match cli.command {
        None => map_result(
            tui::run(
                merry_config.as_ref(),
                tui::LaunchMode::New,
                sandbox_mode,
                permission_mode,
                auto_fallback,
                trust_workspace,
            )
            .await,
            cli::root_usage,
            cli::debug_openai_usage,
        ),
        Some(CliCommand::Resume) => map_result(
            tui::run(
                merry_config.as_ref(),
                tui::LaunchMode::ResumePicker,
                sandbox_mode,
                permission_mode,
                auto_fallback,
                trust_workspace,
            )
            .await,
            cli::root_usage,
            cli::debug_openai_usage,
        ),
        Some(CliCommand::Run(args)) => map_run_result(
            run_command::run(
                &args,
                merry_config.as_ref(),
                sandbox_mode,
                permission_mode,
                auto_fallback,
                trust_workspace,
            )
            .await,
        ),
        Some(CliCommand::Cmd(args)) => map_result(
            cmd::run(&args, merry_config.as_ref()).await,
            cli::cmd_usage,
            cli::cmd_usage,
        ),
        Some(CliCommand::Debug(DebugArgs {
            session_id,
            input,
            command: None,
        })) => map_result(
            debug::basic::run(&session_id, &input, merry_config.as_ref()).await,
            cli::debug_usage,
            cli::debug_openai_usage,
        ),
        Some(CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::OpenAi(DebugOpenAiArgs {
                    input,
                    model,
                    max_output_tokens,
                    debug_tool_result,
                })),
            ..
        })) => map_debug_openai_result(
            debug::openai::run(
                &input,
                model.as_deref(),
                max_output_tokens,
                debug_tool_result.as_deref(),
                merry_config.as_ref(),
            )
            .await,
        ),
        Some(CliCommand::Debug(DebugArgs {
            command: Some(DebugCommand::Shell(args)),
            ..
        })) => map_shell_result(debug::shell::run(args, merry_config.as_ref()).await),
        // `main` answers this before loading config; kept here so the route
        // stays total if that short-circuit ever moves.
        Some(CliCommand::Completions(args)) => crate::completions::print_completions(args.shell),
    };

    if let Some(gc_handle) = worktree_gc {
        match tokio::time::timeout(std::time::Duration::from_secs(2), gc_handle).await {
            Ok(Ok(report)) if report.removed_instances > 0 || report.removed_worktrees > 0 => {
                tracing::debug!(
                    removed_instances = report.removed_instances,
                    removed_worktrees = report.removed_worktrees,
                    pruned_repos = report.pruned_repos,
                    skipped_live_instances = report.skipped_live_instances,
                    "cleaned up orphaned subagent worktrees from exited processes"
                );
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "orphaned worktree GC task failed");
            }
            Err(_) => {
                tracing::debug!("orphaned worktree GC still running at CLI exit");
            }
        }
    }
    exit
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

fn map_run_result(result: Result<run_command::RunExitStatus, CliError>) -> CliExit {
    match result {
        Ok(run_command::RunExitStatus::Completed | run_command::RunExitStatus::Declined)
        | Err(CliError::BrokenPipe) => CliExit::Success,
        Ok(run_command::RunExitStatus::Incomplete) => CliExit::Failure,
        Err(CliError::DebugUsage(message)) => CliExit::Usage {
            message,
            usage: cli::run_usage(),
        },
        Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
            message,
            usage: cli::run_usage(),
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

#[cfg(test)]
mod tests {
    use super::map_run_result;
    use crate::cli_exit::CliExit;
    use crate::run::RunExitStatus;

    #[test]
    fn run_incomplete_maps_to_failure_exit() {
        assert!(matches!(
            map_run_result(Ok(RunExitStatus::Incomplete)),
            CliExit::Failure
        ));
    }

    #[test]
    fn run_completed_maps_to_success_exit() {
        assert!(matches!(
            map_run_result(Ok(RunExitStatus::Completed)),
            CliExit::Success
        ));
    }

    #[test]
    fn run_declined_maps_to_success_exit() {
        assert!(matches!(
            map_run_result(Ok(RunExitStatus::Declined)),
            CliExit::Success
        ));
    }
}
