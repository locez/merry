//! Repairs for `clap_complete`'s fish completion script.
//!
//! The fish backend of `clap_complete` covers named options only, and it gates
//! every root-level candidate behind a guard that fails while a value-taking
//! option is still waiting for its value. Both gaps send fish to file
//! completion instead of the value being typed: without the repairs below,
//! `merry --approval-policy <TAB>` lists directories and `merry completions
//! <TAB>` lists files. The repairs read the same clap definition and reuse the
//! guard functions the generated script already defines.
//!
//! Each repair covers one gap in an upstream generator, so it can be deleted
//! once `clap_complete` closes that gap. The checks in `tests.rs` assert what
//! fish offers rather than which repair produced it, so they pass either way.

use super::BIN_NAME;
use clap::{Arg, Command};

#[cfg(test)]
mod tests;

/// Applies the fish repairs to `script`, which must be the fish script that
/// `clap_complete` generated for `command`.
///
/// The repairs only add candidates and relax one guard, so a `clap_complete`
/// template change that makes a repair stop matching degrades to the unpatched
/// script instead of a broken one. `tests.rs` runs the generated script in a
/// real fish and fails when either repair stops taking effect.
pub(super) fn repair_value_completion(script: &mut String, command: &Command) {
    keep_root_candidates_for_partial_options(script);
    append_positional_values(script, command);
}

/// Keeps the root candidate set available while an option waits for its value.
///
/// The generated script answers "has a subcommand been typed?" with
/// `__fish_merry_needs_command`:
///
/// ```fish
/// set -l cmd (commandline -opc)
/// set -e cmd[1]
/// argparse -s (__fish_merry_global_optspecs) -- $cmd 2>/dev/null
/// or return
/// ```
///
/// While the value of a value-taking option is missing, the typed tokens end
/// with that option, `argparse` exits non-zero, and the guard reports a
/// subcommand that was never typed. Fish then discards every candidate behind
/// that guard, including the values of the option being typed, and falls back
/// to file completion. A command line that cannot be parsed yet has no
/// subcommand, so the guard reports that instead; fish offers the option's
/// candidates only where an argument is expected, which is exactly the value
/// being typed.
fn keep_root_candidates_for_partial_options(script: &mut String) {
    let guard = format!(
        "    argparse -s (__fish_{BIN_NAME}_global_optspecs) -- $cmd 2>/dev/null\n    or return\n"
    );
    let partial_guard = format!(
        "    argparse -s (__fish_{BIN_NAME}_global_optspecs) -- $cmd 2>/dev/null\n    or return 0\n"
    );
    if script.contains(&guard) {
        *script = script.replace(&guard, &partial_guard);
    }
}

/// Appends candidates for positional arguments that have a fixed value set.
///
/// `clap_complete`'s fish backend emits no candidates for positional arguments,
/// so `merry completions <SHELL>` had nothing to offer even though the value
/// list is part of the clap definition. Each appended line reuses a guard
/// function from the generated script to scope its candidates to the command
/// that owns the argument.
fn append_positional_values(script: &mut String, command: &Command) {
    append_positional_values_in(script, command, &[]);
}

/// Walks `command` and its subcommands, appending one `complete` line per
/// positional argument with visible possible values.
fn append_positional_values_in(script: &mut String, command: &Command, parents: &[&str]) {
    let Some(guard) = guard_for(parents) else {
        return;
    };
    for arg in command.get_positionals() {
        if arg.is_hide_set() {
            continue;
        }
        let candidates = value_candidates(arg);
        if candidates.is_empty() {
            continue;
        }
        script.push_str(&format!(
            "complete -c {BIN_NAME} -n \"{guard}\" -f -a \"{candidates}\"\n"
        ));
    }
    for subcommand in command.get_subcommands() {
        let mut subcommand_parents = parents.to_vec();
        subcommand_parents.push(subcommand.get_name());
        append_positional_values_in(script, subcommand, &subcommand_parents);
    }
}

/// The condition that scopes candidates to the command reached through
/// `parents`, matching the conditions `clap_complete` generates for that
/// command's own candidates.
///
/// `None` marks a subcommand level deeper than the generated script
/// distinguishes; `clap_complete`'s fish backend stops at those levels too.
fn guard_for(parents: &[&str]) -> Option<String> {
    match parents {
        [] => Some(format!("__fish_{BIN_NAME}_needs_command")),
        [command] => Some(format!("__fish_{BIN_NAME}_using_subcommand {command}")),
        [parent, command] => Some(format!(
            "__fish_{BIN_NAME}_using_subcommand {parent}; and __fish_seen_subcommand_from {command}"
        )),
        _ => None,
    }
}

/// Renders `arg`'s visible possible values as one `value<TAB>'help'` entry per
/// line.
///
/// This is the candidate format of `clap_complete`'s fish backend, mirrored
/// here because that backend keeps its value rendering and quoting helpers
/// private. Value names are single shell tokens, as clap spells them for a
/// `ValueEnum`.
fn value_candidates(arg: &Arg) -> String {
    arg.get_possible_values()
        .iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| {
            let help = value
                .get_help()
                .map_or_else(String::new, |help| help.to_string());
            format!("{}\\t'{}'", escape(value.get_name()), escape(&help))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Escapes text for the double-quoted fish string that carries candidates.
///
/// The text comes from clap's validated value names and help rather than from
/// user input, but a backslash, quote, or dollar sign would still change how
/// fish reads the `complete -a` payload.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '\\' | '\'' | '"' | '$') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}
