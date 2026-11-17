use super::{Cli, CliCommand, cmd_usage, debug_openai_usage, shell_usage};
use crate::coding::{AutoFallback, PermissionMode, SandboxMode};
use crate::config::CliDefaults;
use crate::debug::{Command as DebugCommand, DEFAULT_INPUT, DEFAULT_SESSION_ID};
use crate::workspace_trust::TrustWorkspace;
use clap::Parser;

#[test]
fn parses_no_subcommand_as_tui_entrypoint() {
    let cli = Cli::try_parse_from(["merry"]).expect("root args should parse");

    assert!(cli.command.is_none());
    assert_eq!(cli.sandbox_mode(), SandboxMode::Workspace);
    assert_eq!(cli.permission_mode(), PermissionMode::Auto);
    assert_eq!(cli.auto_fallback(), AutoFallback::Ask);
}

#[test]
fn trust_workspace_flag_parses_yes_and_no() {
    let yes = Cli::try_parse_from(["merry", "--trust-workspace", "yes"]).expect("yes parses");
    let no = Cli::try_parse_from(["merry", "--trust-workspace", "no", "run", "task"])
        .expect("no parses");

    assert_eq!(yes.trust_workspace(), Some(TrustWorkspace::Yes));
    assert_eq!(no.trust_workspace(), Some(TrustWorkspace::No));
    assert!(Cli::try_parse_from(["merry", "--trust-workspace", "maybe"]).is_err());
}

#[test]
fn sandbox_off_selects_host_execution() {
    let tui = Cli::try_parse_from(["merry", "--sandbox", "off"]).expect("root args parse");
    let run =
        Cli::try_parse_from(["merry", "--sandbox", "off", "run", "task"]).expect("run parses");

    assert_eq!(tui.sandbox_mode(), SandboxMode::Off);
    assert_eq!(run.sandbox_mode(), SandboxMode::Off);
    assert_eq!(tui.permission_mode(), PermissionMode::Auto);
    assert_eq!(run.permission_mode(), PermissionMode::Auto);
}

#[test]
fn permission_mode_flag_parses_every_mode() {
    for (value, mode) in [
        ("auto", PermissionMode::Auto),
        ("ask", PermissionMode::Ask),
        ("always-approve", PermissionMode::AlwaysApprove),
        ("deny", PermissionMode::Deny),
    ] {
        let cli = Cli::try_parse_from(["merry", "--permission-mode", value, "run", "task"])
            .unwrap_or_else(|error| panic!("--permission-mode {value} should parse: {error}"));
        assert_eq!(cli.permission_mode(), mode, "{value}");
    }
    assert!(Cli::try_parse_from(["merry", "--permission-mode", "model_then_human"]).is_err());
    assert!(Cli::try_parse_from(["merry", "--approval-policy", "none"]).is_err());
    assert!(Cli::try_parse_from(["merry", "--no-sandbox"]).is_err());
}

#[test]
fn always_approve_is_independent_from_sandbox_off() {
    let cli = Cli::try_parse_from([
        "merry",
        "--sandbox",
        "off",
        "--always-approve",
        "run",
        "task",
    ])
    .expect("open run parses");

    assert_eq!(cli.sandbox_mode(), SandboxMode::Off);
    assert_eq!(cli.permission_mode(), PermissionMode::AlwaysApprove);
}

#[test]
fn auto_fallback_flag_is_rejected_outside_auto() {
    let error = Cli::try_parse_from([
        "merry",
        "--permission-mode",
        "ask",
        "--auto-fallback",
        "deny",
    ])
    .expect("flags parse before defaults");
    let mut cli = error;
    let failure = cli
        .apply_defaults(CliDefaults::default())
        .expect_err("fallback does not apply to ask");
    assert!(failure.contains("auto_fallback"), "{failure}");
}

#[test]
fn configured_sandbox_applies_when_the_command_line_omits_one() {
    let mut cli = Cli::try_parse_from(["merry", "run", "task"]).expect("run parses");
    cli.apply_defaults(CliDefaults::new(
        Some(SandboxMode::Off),
        Some(PermissionMode::Ask),
        None,
    ))
    .expect("defaults apply");
    assert_eq!(cli.sandbox_mode(), SandboxMode::Off);
    assert_eq!(cli.permission_mode(), PermissionMode::Ask);
}

#[test]
fn command_line_sandbox_replaces_the_configured_mode() {
    let mut cli =
        Cli::try_parse_from(["merry", "--sandbox", "workspace", "run", "task"]).expect("parses");
    cli.apply_defaults(CliDefaults::new(Some(SandboxMode::Off), None, None))
        .expect("defaults apply");
    assert_eq!(cli.sandbox_mode(), SandboxMode::Workspace);
    assert_eq!(cli.permission_mode(), PermissionMode::Auto);
}

#[test]
fn completions_subcommand_parses_every_shell_without_touching_the_sandbox() {
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let cli = Cli::try_parse_from(["merry", "completions", shell])
            .unwrap_or_else(|error| panic!("completions {shell} should parse: {error}"));
        assert!(
            matches!(cli.command, Some(CliCommand::Completions(_))),
            "{shell}"
        );
        assert!(!cli.is_product_surface(), "{shell}");
    }

    assert!(Cli::try_parse_from(["merry", "completions"]).is_err());
    assert!(Cli::try_parse_from(["merry", "completions", "tcsh"]).is_err());
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
    assert_eq!(cli.sandbox_mode(), SandboxMode::Workspace);

    match cli.command.expect("command should be present") {
        CliCommand::Debug(debug) => {
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
            Some(DebugCommand::Shell(_)) => panic!("expected debug openai subcommand"),
            None => panic!("expected debug openai subcommand"),
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
fn parses_root_sandbox_flag() {
    let cli = Cli::try_parse_from(["merry", "--sandbox", "off", "debug"]).expect("args parse");

    assert_eq!(cli.sandbox_mode(), SandboxMode::Off);
}

#[test]
fn debug_openai_usage_contains_openai_env_help() {
    assert!(debug_openai_usage().contains("MERRY_OPENAI_DEBUG=1"));
}
