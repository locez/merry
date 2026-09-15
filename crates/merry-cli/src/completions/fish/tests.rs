//! Value-completion behavior of the generated fish script.
//!
//! Every check sources the script into a real fish and asks that shell what it
//! would offer, so the assertions cover the artifact users install rather than
//! the shape of the Rust code that produced it. Expected values come from the
//! clap definition instead of a second hand-written list.

use super::escape;
use crate::coding::ApprovalPolicy;
use crate::completions::{BIN_NAME, write_completions};
use clap::ValueEnum;
use clap_complete::Shell;
use std::ffi::OsStr;
use std::io;
use std::process::Stdio;
use tempfile::TempDir;

/// Offers what `command_line` completes to at this host's fish, or skips the
/// calling check when no fish is installed to run the script with.
macro_rules! fish_candidates {
    ($command_line:expr) => {
        match probe_fish($command_line) {
            Some(candidates) => candidates,
            None => {
                eprintln!(
                    "skipping fish completion check: fish is not installed \
                     (the reliability-gate workflow installs it)"
                );
                return;
            }
        }
    };
}

/// Candidates that a real fish offers for `command_line`, with fish's
/// descriptions stripped, or `None` when this host has no fish.
///
/// The check runs in a temporary home with `--no-config`, so an installed
/// `merry` completion file or shell state cannot answer in place of the script
/// under test, and it keeps to the oldest fish surface there is: `fish -c`,
/// `source`, and `complete -C` with quoted words. A script that fish cannot
/// load stops the probe with its own message instead of looking like a plain
/// candidate mismatch.
fn probe_fish(command_line: &str) -> Option<Vec<String>> {
    let mut script = Vec::new();
    write_completions(Shell::Fish, &mut script).expect("fish script should render");
    let home = TempDir::new().expect("temporary fish home");
    let script_path = home.path().join("merry.fish");
    std::fs::write(&script_path, &script).expect("generated script should be written");

    let command = format!(
        "\
source {}
or begin
    echo \"fish could not source the generated completion script\" >&2
    exit 3
end
functions -q __fish_{BIN_NAME}_needs_command
or begin
    echo \"the generated completion script defines no completion helpers\" >&2
    exit 3
end
complete -C{}
",
        quote_fish(&script_path.to_string_lossy()),
        quote_fish(command_line),
    );
    let child = match std::process::Command::new("fish")
        .args([
            OsStr::new("--no-config"),
            OsStr::new("-c"),
            OsStr::new(&command),
        ])
        .env("XDG_CONFIG_HOME", home.path())
        .current_dir(home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => panic!("failed to run fish: {error}"),
    };
    let output = child.wait_with_output().expect("fish should exit");
    assert!(
        output.status.success(),
        "fish could not complete with the generated script (status {:?}): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("fish output should be utf-8");
    Some(
        stdout
            .lines()
            .map(|line| line.split('\t').next().unwrap_or_default().to_owned())
            .collect(),
    )
}

/// Quotes `text` as one fish word, so a temporary path or a probe command line
/// cannot be read as fish syntax.
fn quote_fish(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The spellings clap accepts for `T`, which is exactly the set completion has
/// to offer.
fn declared_values<T: ValueEnum>() -> Vec<String> {
    let mut names = T::value_variants()
        .iter()
        .filter_map(ValueEnum::to_possible_value)
        .map(|value| value.get_name().to_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn sorted(mut candidates: Vec<String>) -> Vec<String> {
    candidates.sort();
    candidates
}

#[test]
fn values_are_offered_while_an_option_waits_for_its_value() {
    let candidates = fish_candidates!("merry --approval-policy ");

    assert_eq!(sorted(candidates), declared_values::<ApprovalPolicy>());
}

#[test]
fn shell_values_are_offered_for_the_completions_argument() {
    let candidates = fish_candidates!("merry completions ");

    assert_eq!(sorted(candidates), declared_values::<Shell>());
}

#[test]
fn subcommands_and_their_flags_keep_completing() {
    let subcommands = fish_candidates!("merry ");
    for subcommand in ["completions", "run"] {
        assert!(
            subcommands.contains(&subcommand.to_owned()),
            "root candidates should include {subcommand}: {subcommands:?}"
        );
    }

    let flags = fish_candidates!("merry run --");
    assert!(
        flags.contains(&"--session-id".to_owned()),
        "run candidates should include --session-id: {flags:?}"
    );

    let after_value = fish_candidates!("merry --approval-policy none ");
    assert!(
        after_value.contains(&"run".to_owned()),
        "a complete option should leave the root candidates available: {after_value:?}"
    );
}

#[test]
fn candidate_text_keeps_fish_metacharacters_literal() {
    assert_eq!(escape("it's $HOME"), "it\\'s \\$HOME");
    assert_eq!(
        escape("back\\slash \"quoted\""),
        "back\\\\slash \\\"quoted\\\""
    );
}
