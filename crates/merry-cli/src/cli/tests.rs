use super::{
    ApprovalPolicy, Cli, CliCommand, ProcessExecutionMode, cmd_usage, debug_openai_usage,
    shell_usage,
};
use crate::config::CliDefaults;
use crate::debug::{Command as DebugCommand, DEFAULT_INPUT, DEFAULT_SESSION_ID};
use crate::sandbox::{
    ChildHandoff as SandboxChildHandoff, ClipboardAccess, SANDBOX_CHILD_HANDOFF_ARG,
    SANDBOX_CHILD_HANDOFF_CLI_BWRAP,
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
    let run = Cli::try_parse_from(["merry", "--no-sandbox", "run", "task"]).expect("run parses");

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
    assert_eq!(tui.approval_policy(), ApprovalPolicy::ModelThenHuman);
    assert_eq!(run.approval_policy(), ApprovalPolicy::ModelThenHuman);
}

#[test]
fn approval_policy_flag_parses_every_reviewer_name() {
    for (value, policy) in [
        ("none", ApprovalPolicy::NoApproval),
        ("deny", ApprovalPolicy::Deny),
        ("model_only", ApprovalPolicy::ModelOnly),
        ("model_then_human", ApprovalPolicy::ModelThenHuman),
        ("human_only", ApprovalPolicy::HumanOnly),
    ] {
        let cli = Cli::try_parse_from(["merry", "--approval-policy", value, "run", "task"])
            .unwrap_or_else(|error| panic!("--approval-policy {value} should parse: {error}"));
        assert_eq!(cli.approval_policy(), policy, "{value}");
    }

    for value in [
        "auto",
        "trusted",
        "model",
        "human",
        "model-then-human",
        "fully-trusted",
        "--approval-policy",
        "yes",
    ] {
        assert!(
            Cli::try_parse_from(["merry", "--approval-policy", value]).is_err(),
            "{value} should be rejected"
        );
    }
    assert!(
        Cli::try_parse_from(["merry", "--fully-trusted"]).is_err(),
        "the old --fully-trusted flag should no longer parse"
    );
}

#[test]
fn none_policy_is_explicit_and_independent_from_host_execution_mode() {
    let cli = Cli::try_parse_from([
        "merry",
        "--approval-policy",
        "none",
        "--no-sandbox",
        "run",
        "task",
    ])
    .expect("no-approval run parses");

    assert_eq!(
        cli.process_execution_mode(),
        ProcessExecutionMode::Unrestricted
    );
    assert_eq!(cli.approval_policy(), ApprovalPolicy::NoApproval);
}

fn defaults(
    mode: Option<ProcessExecutionMode>,
    approval_policy: Option<ApprovalPolicy>,
) -> CliDefaults {
    CliDefaults::new(mode, approval_policy)
}

#[test]
fn config_sandbox_selects_mode_when_command_line_omits_one() {
    let mut unrestricted = Cli::try_parse_from(["merry", "run", "task"]).expect("run parses");
    unrestricted.apply_defaults(defaults(Some(ProcessExecutionMode::Unrestricted), None));
    assert_eq!(
        unrestricted.process_execution_mode(),
        ProcessExecutionMode::Unrestricted
    );
    assert!(!unrestricted.should_bootstrap_sandbox());
    assert_eq!(
        unrestricted.approval_policy(),
        ApprovalPolicy::ModelThenHuman
    );

    let mut inner = Cli::try_parse_from(["merry"]).expect("root parses");
    inner.apply_defaults(defaults(Some(ProcessExecutionMode::InnerOnly), None));
    assert_eq!(
        inner.process_execution_mode(),
        ProcessExecutionMode::InnerOnly
    );
    assert!(!inner.should_bootstrap_sandbox());

    let mut debug = Cli::try_parse_from(["merry", "debug"]).expect("debug parses");
    debug.apply_defaults(defaults(Some(ProcessExecutionMode::OuterAndInner), None));
    assert!(debug.with_sandbox);
    assert!(debug.should_bootstrap_sandbox());
}

#[test]
fn command_line_sandbox_mode_replaces_configured_default() {
    let mut with_sandbox =
        Cli::try_parse_from(["merry", "--with-sandbox", "run", "task"]).expect("run parses");
    with_sandbox.apply_defaults(defaults(Some(ProcessExecutionMode::Unrestricted), None));
    assert_eq!(
        with_sandbox.process_execution_mode(),
        ProcessExecutionMode::OuterAndInner
    );
    assert!(with_sandbox.should_bootstrap_sandbox());
    assert!(!with_sandbox.no_sandbox);

    let mut no_sandbox = Cli::try_parse_from(["merry", "--no-sandbox"]).expect("root parses");
    no_sandbox.apply_defaults(defaults(Some(ProcessExecutionMode::InnerOnly), None));
    assert_eq!(
        no_sandbox.process_execution_mode(),
        ProcessExecutionMode::Unrestricted
    );
    assert!(!no_sandbox.inner_sandbox);
}

const SANDBOX_FLAGS: [(&str, ProcessExecutionMode); 3] = [
    ("--with-sandbox", ProcessExecutionMode::OuterAndInner),
    ("--no-sandbox", ProcessExecutionMode::Unrestricted),
    ("--inner-sandbox", ProcessExecutionMode::InnerOnly),
];

const POLICIES: [ApprovalPolicy; 5] = [
    ApprovalPolicy::NoApproval,
    ApprovalPolicy::Deny,
    ApprovalPolicy::ModelOnly,
    ApprovalPolicy::ModelThenHuman,
    ApprovalPolicy::HumanOnly,
];

/// Every approval policy combines with every sandbox mode, and a
/// combination resolves the same whether it comes from `[cli]` or from the
/// matching flags.
#[test]
fn config_and_flags_resolve_every_sandbox_and_approval_policy_combination_alike() {
    for (flag, mode) in SANDBOX_FLAGS {
        for policy in POLICIES {
            let mut configured = Cli::try_parse_from(["merry", "run", "task"]).expect("run parses");
            configured.apply_defaults(defaults(Some(mode), Some(policy)));

            let mut flagged = Cli::try_parse_from([
                "merry",
                flag,
                "--approval-policy",
                policy.name(),
                "run",
                "task",
            ])
            .expect("flags parse");
            flagged.apply_defaults(CliDefaults::default());

            for (label, cli) in [("configured", &configured), ("flagged", &flagged)] {
                assert_eq!(
                    cli.process_execution_mode(),
                    mode,
                    "{label} {flag} {policy:?}"
                );
                assert_eq!(cli.approval_policy(), policy, "{label} {flag} {policy:?}");
                assert_eq!(
                    cli.should_bootstrap_sandbox(),
                    mode == ProcessExecutionMode::OuterAndInner,
                    "{label} {flag} {policy:?}"
                );
            }
        }
    }
}

/// A command-line sandbox flag replaces only the configured sandbox; the
/// configured approval policy still applies, `none` included.
#[test]
fn command_line_sandbox_flag_leaves_the_configured_approval_policy_alone() {
    for (flag, mode) in SANDBOX_FLAGS {
        for policy in POLICIES {
            for configured_mode in [
                None,
                Some(ProcessExecutionMode::OuterAndInner),
                Some(ProcessExecutionMode::Unrestricted),
                Some(ProcessExecutionMode::InnerOnly),
            ] {
                let mut cli =
                    Cli::try_parse_from(["merry", flag, "run", "task"]).expect("flags parse");
                cli.apply_defaults(defaults(configured_mode, Some(policy)));
                assert_eq!(
                    cli.process_execution_mode(),
                    mode,
                    "{flag} {policy:?} {configured_mode:?}"
                );
                assert_eq!(
                    cli.approval_policy(),
                    policy,
                    "{flag} {policy:?} {configured_mode:?}"
                );
            }
        }
    }
}

/// An explicit `--approval-policy` replaces only the configured approval
/// policy; the configured sandbox still applies.
#[test]
fn explicit_approval_policy_flag_leaves_the_configured_sandbox_alone() {
    for flagged_policy in POLICIES {
        for configured_policy in [
            None,
            Some(ApprovalPolicy::Deny),
            Some(ApprovalPolicy::NoApproval),
        ] {
            for (_, configured_mode) in SANDBOX_FLAGS {
                let mut cli = Cli::try_parse_from([
                    "merry",
                    "--approval-policy",
                    flagged_policy.name(),
                    "run",
                    "task",
                ])
                .expect("flags parse");
                cli.apply_defaults(defaults(Some(configured_mode), configured_policy));
                assert_eq!(
                    cli.approval_policy(),
                    flagged_policy,
                    "{flagged_policy:?} {configured_policy:?} {configured_mode:?}"
                );
                assert_eq!(
                    cli.process_execution_mode(),
                    configured_mode,
                    "{flagged_policy:?} {configured_policy:?} {configured_mode:?}"
                );
            }
        }
    }
}

/// Only a sandboxed `run -` under a policy that may ask a person gets the
/// parent's terminal bound into the sandbox.
#[test]
fn review_terminal_handoff_is_limited_to_a_sandboxed_stdin_task_with_a_human_reviewer() {
    for policy in POLICIES {
        for (args, expected) in [
            (vec!["merry", "run", "-"], policy.may_ask_a_human()),
            (
                vec!["merry", "--with-sandbox", "run", "-"],
                policy.may_ask_a_human(),
            ),
            (vec!["merry", "--inner-sandbox", "run", "-"], false),
            (vec!["merry", "--no-sandbox", "run", "-"], false),
            (vec!["merry", "run", "fix the failing test"], false),
            (vec!["merry"], false),
            (vec!["merry", "resume"], false),
        ] {
            let mut cli = Cli::try_parse_from(args.clone()).expect("args parse");
            cli.apply_defaults(defaults(None, Some(policy)));
            assert_eq!(
                cli.hands_off_review_terminal(),
                expected,
                "{policy:?} {args:?}"
            );
        }
    }
    assert!(
        ApprovalPolicy::ModelThenHuman.may_ask_a_human()
            && ApprovalPolicy::HumanOnly.may_ask_a_human()
    );
    assert!(
        !ApprovalPolicy::NoApproval.may_ask_a_human()
            && !ApprovalPolicy::Deny.may_ask_a_human()
            && !ApprovalPolicy::ModelOnly.may_ask_a_human()
    );
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
        assert!(!cli.should_bootstrap_sandbox(), "{shell}");
        assert_eq!(cli.clipboard_access(), ClipboardAccess::Disabled, "{shell}");
    }

    assert!(Cli::try_parse_from(["merry", "completions"]).is_err());
    assert!(Cli::try_parse_from(["merry", "completions", "tcsh"]).is_err());
}

#[test]
fn inner_sandbox_selects_codex_compatible_single_sandbox_mode() {
    let cli = Cli::try_parse_from(["merry", "--inner-sandbox"]).expect("inner sandbox args parse");

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
fn parses_hidden_sandbox_child_handoff() {
    let cli = Cli::try_parse_from([
        "merry",
        SANDBOX_CHILD_HANDOFF_ARG,
        SANDBOX_CHILD_HANDOFF_CLI_BWRAP,
        "debug",
        "shell",
        "--",
        "rustc",
        "--version",
    ])
    .expect("hidden sandbox handoff args should parse");

    assert_eq!(
        cli.sandbox_child_handoff,
        Some(SandboxChildHandoff::CliBwrap)
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
    let cli = Cli::try_parse_from(["merry", "--with-sandbox", "debug"]).expect("args should parse");

    assert!(cli.with_sandbox);
}

#[test]
fn debug_openai_usage_contains_openai_env_help() {
    assert!(debug_openai_usage().contains("MERRY_OPENAI_DEBUG=1"));
}
