use crate::coding::{ApprovalPolicy, ProcessExecutionMode};
use crate::config::CliDefaults;
use crate::debug::{Args as DebugArgs, OpenAiArgs as DebugOpenAiArgs};
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use clap::{Args, CommandFactory, Parser, Subcommand};

pub(crate) const OPENAI_ENV_HELP: &str = "\
Environment:
  MERRY_OPENAI_DEBUG=1       Required opt-in before any network attempt
  XDG_CONFIG_HOME            Optional base for merry/config.toml

Provider/model/base URL/API key source come from
`$XDG_CONFIG_HOME/merry/config.toml` or `~/.config/merry/config.toml`.
Set exactly one of `[providers.openai-compatible].api_key` or `api_key_file`.
For sandboxed model debugging, prefer config-relative `api_key_file =
\"secrets/openai.key\"` so credentials are not passed through process argv.
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
        long,
        value_enum,
        value_name = "POLICY",
        help = "Who reviews permission requests before a command runs [default: model_then_human]"
    )]
    pub(crate) approval_policy: Option<ApprovalPolicy>,

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

    /// The effective approval policy; `model_then_human` when no flag or
    /// configured default set one.
    pub(crate) fn approval_policy(&self) -> ApprovalPolicy {
        self.approval_policy.unwrap_or_default()
    }

    /// Whether the outer-sandbox parent should bind its controlling terminal
    /// into the child for permission review answers.
    ///
    /// Only a sandboxed `run -` needs it: the task consumes stdin, and
    /// `--new-session` leaves the child unable to open `/dev/tty`. Policies
    /// that never ask a person get nothing extra.
    pub(crate) fn hands_off_review_terminal(&self) -> bool {
        let reads_task_from_stdin = match &self.command {
            Some(CliCommand::Run(args)) => args.task == crate::run::STDIN_TASK,
            _ => false,
        };
        self.should_bootstrap_sandbox()
            && reads_task_from_stdin
            && self.approval_policy().may_ask_a_human()
    }

    pub(crate) fn clipboard_access(&self) -> crate::sandbox::ClipboardAccess {
        match &self.command {
            None | Some(CliCommand::Resume) => crate::sandbox::ClipboardAccess::Tui,
            Some(
                CliCommand::Run(_)
                | CliCommand::Cmd(_)
                | CliCommand::Debug(_)
                | CliCommand::Completions(_),
            ) => crate::sandbox::ClipboardAccess::Disabled,
        }
    }

    /// Applies `[cli]` defaults as if the matching flags preceded the
    /// subcommand.
    ///
    /// Explicit command-line flags win, and each key stands in for its own
    /// flag only. The three sandbox modes are mutually exclusive, so a mode
    /// given on the command line keeps a configured `sandbox` from applying,
    /// and an explicit `--approval-policy` keeps a configured
    /// `approval_policy` from applying. The two never affect each other: the
    /// sandbox sets the execution boundary and the approval policy sets who
    /// reviews permission requests inside it.
    pub(crate) fn apply_defaults(&mut self, defaults: CliDefaults) {
        if !(self.with_sandbox || self.no_sandbox || self.inner_sandbox) {
            match defaults.process_execution_mode() {
                Some(ProcessExecutionMode::OuterAndInner) => self.with_sandbox = true,
                Some(ProcessExecutionMode::Unrestricted) => self.no_sandbox = true,
                Some(ProcessExecutionMode::InnerOnly) => self.inner_sandbox = true,
                None => {}
            }
        }
        if self.approval_policy.is_none() {
            self.approval_policy = defaults.approval_policy();
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
    #[command(about = "Print a shell completion script for merry to stdout")]
    Completions(crate::completions::Args),
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

fn command_usage(command: &mut clap::Command) -> String {
    let mut buffer = Vec::new();
    command
        .write_help(&mut buffer)
        .expect("clap help should render");
    String::from_utf8(buffer).expect("clap help should be utf-8")
}

#[cfg(test)]
mod tests;
