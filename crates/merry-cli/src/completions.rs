//! Shell completion scripts generated from the clap command definition.

use crate::cli::Cli;
use clap::CommandFactory;
use clap_complete::{Shell, generate};
use std::io;

/// Arguments for `merry completions`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    #[arg(value_enum, help = "Shell to generate the completion script for")]
    pub(crate) shell: Shell,
}

/// Writes the completion script for `shell` to `output`.
///
/// The script is derived from the same clap definition that parses `merry`
/// arguments, so subcommands, flags, and enum values such as
/// `--approval-policy` stay in sync without a separate list.
pub(crate) fn write_completions<W: io::Write>(shell: Shell, output: &mut W) {
    let mut command = Cli::command();
    generate(shell, &mut command, "merry", output);
}

#[cfg(test)]
mod tests {
    use super::write_completions;
    use clap::ValueEnum;
    use clap_complete::Shell;

    #[test]
    fn every_shell_script_names_the_binary_and_root_flags() {
        for shell in Shell::value_variants() {
            let mut output = Vec::new();
            write_completions(*shell, &mut output);
            let script = String::from_utf8(output).expect("completion script should be utf-8");
            assert!(script.contains("merry"), "{shell}: {script}");
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
    fn bash_and_zsh_scripts_list_approval_policy_values() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let mut output = Vec::new();
            write_completions(shell, &mut output);
            let script = String::from_utf8(output).expect("completion script should be utf-8");
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
}
