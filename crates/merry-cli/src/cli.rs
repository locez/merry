use crate::coding_runtime::ProcessExecutionMode;
use crate::debug::{
    Args as DebugArgs, CodingLoopLiveSmokeArgs as DebugCodingLoopLiveSmokeArgs,
    CodingLoopSubagentLiveSmokeArgs as DebugCodingLoopSubagentLiveSmokeArgs,
    CodingLoopTaskLiveSmokeArgs as DebugCodingLoopTaskLiveSmokeArgs, OpenAiArgs as DebugOpenAiArgs,
};
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use clap::{Args, CommandFactory, Parser, Subcommand};

pub(crate) const OPENAI_ENV_HELP: &str = "\
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
    about = "Rust-first agent runtime with a streaming terminal interface.",
    disable_version_flag = true
)]
pub(crate) struct Cli {
    #[arg(
        long,
        conflicts_with_all = ["no_sandbox", "inner_sandbox"],
        help = "Run TUI/run inside Merry's outer and inner bubblewrap sandboxes"
    )]
    pub(crate) with_sandbox: bool,

    #[arg(
        long,
        conflicts_with_all = ["with_sandbox", "inner_sandbox"],
        help = "Run TUI/run directly with the host filesystem, environment, and permissions"
    )]
    pub(crate) no_sandbox: bool,

    #[arg(
        long,
        conflicts_with_all = ["with_sandbox", "no_sandbox"],
        help = "Run TUI/run with the inner action sandbox and without Merry's outer sandbox"
    )]
    pub(crate) inner_sandbox: bool,

    #[arg(
        long = "merry-sandbox-child-handoff",
        hide = true,
        value_enum,
        value_name = "PROFILE"
    )]
    pub(crate) sandbox_child_handoff: Option<SandboxChildHandoff>,

    #[command(subcommand)]
    pub(crate) command: Option<CliCommand>,
}

impl Cli {
    pub(crate) fn is_product_surface(&self) -> bool {
        matches!(
            &self.command,
            None | Some(CliCommand::Resume) | Some(CliCommand::Run(_))
        )
    }

    pub(crate) fn should_bootstrap_sandbox(&self) -> bool {
        matches!(
            self.process_execution_mode(),
            ProcessExecutionMode::OuterAndInner
        ) && (self.with_sandbox || self.is_product_surface())
    }

    pub(crate) fn process_execution_mode(&self) -> ProcessExecutionMode {
        if self.no_sandbox {
            ProcessExecutionMode::Unrestricted
        } else if self.inner_sandbox {
            ProcessExecutionMode::InnerOnly
        } else {
            ProcessExecutionMode::OuterAndInner
        }
    }

    pub(crate) fn clipboard_access(&self) -> crate::sandbox::ClipboardAccess {
        match &self.command {
            None | Some(CliCommand::Resume) => crate::sandbox::ClipboardAccess::Tui,
            Some(CliCommand::Run(_) | CliCommand::Cmd(_) | CliCommand::Debug(_)) => {
                crate::sandbox::ClipboardAccess::Disabled
            }
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum CliCommand {
    #[command(about = "Resume a saved Merry TUI session")]
    Resume,
    #[command(about = "Complete a coding task with Merry's headless agent")]
    Run(crate::run::Args),
    #[command(about = "Generate a shell command plan from a natural-language request")]
    Cmd(crate::cmd::Args),
    #[command(about = "Print deterministic runtime events or run opt-in provider debugging")]
    Debug(DebugArgs),
}

pub(crate) fn parse_max_output_tokens(value: &str) -> Result<u64, String> {
    let tokens = value
        .parse::<u64>()
        .map_err(|error| format!("must be a positive integer: {error}"))?;

    if tokens == 0 {
        return Err("must be greater than zero".to_owned());
    }

    Ok(tokens)
}

pub(crate) fn root_usage() -> String {
    let mut command = Cli::command();
    command_usage(&mut command)
}

pub(crate) fn debug_usage() -> String {
    let mut command = Cli::command();
    let command = command
        .find_subcommand_mut("debug")
        .expect("debug subcommand should exist");
    command.set_bin_name("merry debug");
    command_usage(command)
}

pub(crate) fn run_usage() -> String {
    let mut command = Cli::command();
    let command = command
        .find_subcommand_mut("run")
        .expect("run subcommand should exist");
    command.set_bin_name("merry run");
    command_usage(command)
}

pub(crate) fn cmd_usage() -> String {
    let mut command = Cli::command();
    let command = command
        .find_subcommand_mut("cmd")
        .expect("cmd subcommand should exist");
    command.set_bin_name("merry cmd");
    command_usage(command)
}

pub(crate) fn shell_usage() -> String {
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

pub(crate) fn debug_openai_usage() -> String {
    let mut command = DebugOpenAiArgs::augment_args(clap::Command::new("openai"))
        .bin_name("merry debug openai")
        .about("Run opt-in OpenAI-compatible model debugging")
        .after_help(OPENAI_ENV_HELP);
    command_usage(&mut command)
}

pub(crate) fn debug_coding_loop_live_smoke_usage() -> String {
    let mut command = DebugCodingLoopLiveSmokeArgs::augment_args(clap::Command::new(
        "coding-loop-live-smoke",
    ))
    .bin_name("merry debug coding-loop-live-smoke")
    .about("Run an opt-in sandboxed coding-loop smoke driven by a live OpenAI-compatible model")
    .after_help(OPENAI_ENV_HELP);
    command_usage(&mut command)
}

pub(crate) fn debug_coding_loop_task_live_smoke_usage() -> String {
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

pub(crate) fn debug_coding_loop_subagent_live_smoke_usage() -> String {
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

#[cfg(test)]
mod tests {
    use super::{
        Cli, CliCommand, ProcessExecutionMode, cmd_usage, debug_coding_loop_live_smoke_usage,
        debug_coding_loop_subagent_live_smoke_usage, debug_coding_loop_task_live_smoke_usage,
        debug_openai_usage, shell_usage,
    };
    use crate::debug::{
        CodingLoopTaskSmokeTask, Command as DebugCommand, DEFAULT_INPUT, DEFAULT_SESSION_ID,
    };
    use crate::sandbox::{
        ChildHandoff as SandboxChildHandoff, ClipboardAccess, SANDBOX_CHILD_HANDOFF_ARG,
        SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1,
    };
    use clap::Parser;

    #[test]
    fn parses_no_subcommand_as_tui_entrypoint() {
        let cli = Cli::try_parse_from(["merry"]).expect("root args should parse");

        assert!(cli.command.is_none());
        assert!(cli.should_bootstrap_sandbox());
    }

    #[test]
    fn only_tui_routes_request_clipboard_access() {
        let root = Cli::try_parse_from(["merry"]).expect("root args should parse");
        let resume = Cli::try_parse_from(["merry", "resume"]).expect("resume should parse");
        let run = Cli::try_parse_from(["merry", "run", "task"]).expect("run should parse");
        let debug = Cli::try_parse_from(["merry", "debug"]).expect("debug should parse");

        assert_eq!(root.clipboard_access(), ClipboardAccess::Tui);
        assert_eq!(resume.clipboard_access(), ClipboardAccess::Tui);
        assert_eq!(run.clipboard_access(), ClipboardAccess::Disabled);
        assert_eq!(debug.clipboard_access(), ClipboardAccess::Disabled);
    }

    #[test]
    fn no_sandbox_selects_unrestricted_host_mode() {
        let tui = Cli::try_parse_from(["merry", "--no-sandbox"]).expect("root args parse");
        let run =
            Cli::try_parse_from(["merry", "--no-sandbox", "run", "task"]).expect("run parses");

        assert!(!tui.should_bootstrap_sandbox());
        assert!(!run.should_bootstrap_sandbox());
        assert_eq!(
            tui.process_execution_mode(),
            ProcessExecutionMode::Unrestricted
        );
        assert_eq!(
            run.process_execution_mode(),
            ProcessExecutionMode::Unrestricted
        );
    }

    #[test]
    fn inner_sandbox_selects_codex_compatible_single_sandbox_mode() {
        let cli =
            Cli::try_parse_from(["merry", "--inner-sandbox"]).expect("inner sandbox args parse");

        assert!(!cli.should_bootstrap_sandbox());
        assert_eq!(
            cli.process_execution_mode(),
            ProcessExecutionMode::InnerOnly
        );
    }

    #[test]
    fn debug_sandbox_remains_explicit() {
        let plain = Cli::try_parse_from(["merry", "debug"]).expect("debug args parse");
        let sandboxed =
            Cli::try_parse_from(["merry", "--with-sandbox", "debug"]).expect("debug parses");

        assert!(!plain.should_bootstrap_sandbox());
        assert!(sandboxed.should_bootstrap_sandbox());
    }

    #[test]
    fn existing_subcommands_still_parse_after_tui_entrypoint() {
        let resume = Cli::try_parse_from(["merry", "resume"]).expect("resume parses");
        assert!(matches!(resume.command, Some(CliCommand::Resume)));

        let run = Cli::try_parse_from(["merry", "run", "fix the test"]).expect("run parses");
        assert!(matches!(run.command, Some(CliCommand::Run(_))));

        let cmd = Cli::try_parse_from(["merry", "cmd", "list files"]).expect("cmd parses");
        assert!(matches!(cmd.command, Some(CliCommand::Cmd(_))));

        let debug = Cli::try_parse_from(["merry", "debug"]).expect("debug parses");
        assert!(matches!(debug.command, Some(CliCommand::Debug(_))));
    }

    #[test]
    fn parses_run_task() {
        let cli = Cli::try_parse_from(["merry", "run", "fix the failing test"])
            .expect("run args should parse");

        match cli.command.expect("command should be present") {
            CliCommand::Run(args) => {
                assert_eq!(args.task, "fix the failing test");
                assert!(!args.events_jsonl);
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn parses_run_events_jsonl() {
        let cli = Cli::try_parse_from(["merry", "run", "--events-jsonl", "fix the failing test"])
            .expect("run args should parse");

        match cli.command.expect("command should be present") {
            CliCommand::Run(args) => {
                assert_eq!(args.task, "fix the failing test");
                assert!(args.events_jsonl);
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn parses_cmd_request_defaults() {
        let cli = Cli::try_parse_from(["merry", "cmd", "find all TypeScript tests"])
            .expect("cmd args should parse");

        match cli.command.expect("command should be present") {
            CliCommand::Cmd(args) => {
                assert_eq!(args.request, "find all TypeScript tests");
                assert!(!args.json);
                assert!(!args.no_prompt);
            }
            _ => panic!("expected cmd command"),
        }
    }

    #[test]
    fn parses_cmd_json_and_no_prompt() {
        let cli = Cli::try_parse_from([
            "merry",
            "cmd",
            "--json",
            "--no-prompt",
            "find all TypeScript tests",
        ])
        .expect("cmd args should parse");

        match cli.command.expect("command should be present") {
            CliCommand::Cmd(args) => {
                assert_eq!(args.request, "find all TypeScript tests");
                assert!(args.json);
                assert!(args.no_prompt);
            }
            _ => panic!("expected cmd command"),
        }
    }

    #[test]
    fn cmd_usage_renders_cmd_help() {
        let usage = cmd_usage();

        assert!(usage.contains("Usage: merry cmd"));
        assert!(usage.contains("--no-prompt"));
        assert!(!usage.contains("merry debug openai"));
    }

    #[test]
    fn parses_debug_defaults() {
        let cli = Cli::try_parse_from(["merry", "debug"]).expect("debug args should parse");

        match cli.command.expect("command should be present") {
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
    fn parses_debug_openai_options() {
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

        match cli.command.expect("command should be present") {
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
    fn parses_debug_coding_loop_smoke() {
        let cli = Cli::try_parse_from(["merry", "debug", "coding-loop-smoke"])
            .expect("debug coding-loop-smoke args should parse");

        match cli.command.expect("command should be present") {
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
    fn parses_debug_permission_network_smoke() {
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

        match cli.command.expect("command should be present") {
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
    fn parses_debug_coding_loop_live_smoke() {
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

        match cli.command.expect("command should be present") {
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
    fn parses_debug_coding_loop_task_smoke() {
        let cli = Cli::try_parse_from(["merry", "debug", "coding-loop-task-smoke"])
            .expect("debug coding-loop-task-smoke args should parse");

        match cli.command.expect("command should be present") {
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
    fn parses_debug_coding_loop_task_live_smoke() {
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

        match cli.command.expect("command should be present") {
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
    fn parses_debug_coding_loop_subagent_live_smoke() {
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

        match cli.command.expect("command should be present") {
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
    fn parses_shell_argv() {
        let cli = Cli::try_parse_from(["merry", "debug", "shell", "--", "rustc", "--version"])
            .expect("shell args should parse");

        match cli.command.expect("command should be present") {
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
    fn parses_shell_local_workspace_process_risk_acceptance() {
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

        match cli.command.expect("command should be present") {
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
    fn parses_hidden_sandbox_child_handoff() {
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
    fn rejects_shell_argv_without_separator() {
        let error = Cli::try_parse_from(["merry", "debug", "shell", "rustc", "--version"])
            .expect_err("shell argv should require `--` separator");

        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn shell_usage_contains_shell_usage() {
        assert!(shell_usage().contains("Usage: merry debug shell [OPTIONS] -- <ARGV>..."));
    }

    #[test]
    fn parses_root_with_sandbox_flag() {
        let cli =
            Cli::try_parse_from(["merry", "--with-sandbox", "debug"]).expect("args should parse");

        assert!(cli.with_sandbox);
    }

    #[test]
    fn live_smoke_usage_contains_openai_env_help() {
        assert!(debug_openai_usage().contains("MERRY_OPENAI_DEBUG=1"));
        assert!(debug_coding_loop_live_smoke_usage().contains("MERRY_OPENAI_DEBUG=1"));
        assert!(debug_coding_loop_task_live_smoke_usage().contains("MERRY_OPENAI_DEBUG=1"));
        assert!(debug_coding_loop_subagent_live_smoke_usage().contains("MERRY_OPENAI_DEBUG=1"));
    }
}
