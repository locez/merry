//! Shell completion scripts generated from the clap command definition.

use crate::cli::Cli;
use crate::cli_exit::CliExit;
use clap::CommandFactory;
use clap_complete::{Shell, generate};
use std::io;

mod fish;

/// Binary name the generated scripts complete.
///
/// `clap_complete` derives every helper name in a script from it, so the fish
/// repairs use the same constant.
const BIN_NAME: &str = "merry";

/// Arguments for `merry completions`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    #[arg(value_enum, help = "Shell to generate the completion script for")]
    pub(crate) shell: Shell,
}

/// Prints the completion script for `shell` and reports how `merry completions`
/// should exit.
///
/// A closed stdout, as in `merry completions fish | head -1`, ends the output
/// normally; every other write failure is reported.
pub(crate) fn print_completions(shell: Shell) -> CliExit {
    match write_completions(shell, &mut io::stdout().lock()) {
        Ok(()) => CliExit::Success,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => CliExit::Success,
        Err(error) => CliExit::Unexpected(error.to_string()),
    }
}

/// Writes the completion script for `shell` to `output`.
///
/// The script is derived from the same clap definition that parses `merry`
/// arguments, so subcommands, flags, and enum values such as
/// `--approval-policy` stay in sync without a separate list. Fish gets the
/// value-completion repairs described in [`fish`].
///
/// # Errors
///
/// Returns the failure from writing `output`, so `merry completions fish >
/// ~/.config/fish/completions/merry.fish` reports a failed redirect instead of
/// leaving a truncated script behind.
pub(crate) fn write_completions<W: io::Write>(shell: Shell, output: &mut W) -> io::Result<()> {
    let mut command = Cli::command();
    let mut script = Vec::new();
    generate(shell, &mut command, BIN_NAME, &mut script);
    let mut script = String::from_utf8(script)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if matches!(shell, Shell::Fish) {
        fish::repair_value_completion(&mut script, &command);
    }
    output.write_all(script.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{BIN_NAME, write_completions};
    use clap::ValueEnum;
    use clap_complete::Shell;

    #[test]
    fn every_shell_script_names_the_binary_and_root_flags() {
        for shell in Shell::value_variants() {
            let script = script_for(*shell);
            assert!(script.contains(BIN_NAME), "{shell}: {script}");
            assert!(
                script.contains("approval-policy"),
                "{shell} script should list --approval-policy"
            );
            assert!(
                script.contains("completions"),
                "{shell} script should list the completions subcommand"
            );
        }
    }

    #[test]
    fn bash_zsh_and_fish_scripts_list_approval_policy_values() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let script = script_for(shell);
            for value in [
                "none",
                "deny",
                "model_only",
                "model_then_human",
                "human_only",
            ] {
                assert!(
                    script.contains(value),
                    "{shell} script should offer {value}"
                );
            }
        }
    }

    /// The script for `shell` rendered as text.
    fn script_for(shell: Shell) -> String {
        let mut script = Vec::new();
        write_completions(shell, &mut script).expect("completion script should render");
        String::from_utf8(script).expect("completion script should be utf-8")
    }
}
