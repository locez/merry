use crate::coding::{AutoFallback, PermissionMode, SandboxMode};
use crate::config::CliDefaults;
use crate::workspace_trust::TrustWorkspace;
use clap::{CommandFactory, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "merry",
    about = "Rust-first agent runtime with a streaming terminal interface.",
    disable_version_flag = true
)]
pub(crate) struct Cli {
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        help = "Workspace sandbox or host execution [default: workspace]"
    )]
    pub(crate) sandbox: Option<SandboxMode>,

    #[arg(
        long,
        conflicts_with = "permission_mode",
        help = "Run configured actions without a permission review round"
    )]
    pub(crate) always_approve: bool,

    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        help = "Who reviews permission requests [default: auto]"
    )]
    pub(crate) permission_mode: Option<PermissionMode>,

    #[arg(
        long,
        value_enum,
        value_name = "FALLBACK",
        help = "After auto denies or cannot decide: ask the person, or return the denial to the model [default: ask]"
    )]
    pub(crate) auto_fallback: Option<AutoFallback>,

    #[arg(
        long,
        value_enum,
        value_name = "CHOICE",
        help = "Remember whether the workspace sandbox may write this directory: yes or no. Ignored with --sandbox off"
    )]
    pub(crate) trust_workspace: Option<TrustWorkspace>,

    #[command(subcommand)]
    pub(crate) command: Option<CliCommand>,
}

/// Permission and sandbox flags explicitly provided on the command line.
///
/// Captured before applying `[cli]` config defaults so the TUI can preserve
/// strict precedence: `defaults < config.toml < tui-preferences.toml < CLI flags`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ExplicitCliFlags {
    pub(crate) sandbox: Option<SandboxMode>,
    pub(crate) permission_mode: Option<PermissionMode>,
    pub(crate) auto_fallback: Option<AutoFallback>,
}

impl Cli {
    pub(crate) fn explicit_flags(&self) -> ExplicitCliFlags {
        ExplicitCliFlags {
            sandbox: self.sandbox,
            permission_mode: if self.always_approve {
                Some(PermissionMode::AlwaysApprove)
            } else {
                self.permission_mode
            },
            auto_fallback: self.auto_fallback,
        }
    }

    pub(crate) fn is_product_surface(&self) -> bool {
        matches!(
            &self.command,
            None | Some(CliCommand::Resume) | Some(CliCommand::Run(_))
        )
    }

    pub(crate) fn sandbox_mode(&self) -> SandboxMode {
        self.sandbox.unwrap_or_default()
    }

    pub(crate) fn is_implicit_windows_host_fallback(&self) -> bool {
        cfg!(windows) && self.sandbox.is_none()
    }

    pub(crate) fn permission_mode(&self) -> PermissionMode {
        if self.always_approve {
            PermissionMode::AlwaysApprove
        } else {
            self.permission_mode.unwrap_or_default()
        }
    }

    pub(crate) fn auto_fallback(&self) -> AutoFallback {
        self.auto_fallback.unwrap_or_default()
    }

    pub(crate) fn trust_workspace(&self) -> Option<TrustWorkspace> {
        self.trust_workspace
    }

    /// Applies `[cli]` defaults. Explicit flags win. `auto_fallback` applies
    /// only when the effective permission mode is `auto`.
    pub(crate) fn apply_defaults(&mut self, defaults: CliDefaults) -> Result<(), String> {
        if self.sandbox.is_none() {
            self.sandbox = defaults.sandbox();
        }
        if !self.always_approve && self.permission_mode.is_none() {
            self.permission_mode = defaults.permission_mode();
        }
        let fallback_on_command_line = self.auto_fallback.is_some();
        if self.auto_fallback.is_none() {
            self.auto_fallback = defaults.auto_fallback();
        }
        if self.permission_mode() != PermissionMode::Auto {
            if fallback_on_command_line {
                return Err(
                    "auto_fallback is only valid when permission_mode is \"auto\"".to_owned(),
                );
            }
            self.auto_fallback = None;
        }
        Ok(())
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
    #[command(about = "Print a shell completion script for merry to stdout")]
    Completions(crate::completions::Args),
}

pub(crate) fn root_usage() -> String {
    let mut command = Cli::command();
    command_usage(&mut command)
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

fn command_usage(command: &mut clap::Command) -> String {
    let mut buffer = Vec::new();
    command
        .write_help(&mut buffer)
        .expect("clap help should render");
    String::from_utf8(buffer).expect("clap help should be utf-8")
}

#[cfg(test)]
mod tests;
