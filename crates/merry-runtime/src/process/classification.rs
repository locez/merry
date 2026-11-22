#[cfg(windows)]
use crate::process::classification::windows_shell::{
    analyze_powershell_script, parse_plain_powershell_command_sequence,
};
use crate::process::{
    classification::shell::{
        ShellKind, command_words_have_env_injection, executable_name, executable_token_is,
        parse_bash_security_analysis, parse_plain_shell_command_sequence,
        shell_like_process_input_from_argv, shell_process_input_from_argv,
    },
    contracts::{
        AcceptedLocalWorkspaceProcessAdmission, ProcessActionIntent, ProcessEnvPolicy,
        ProcessPermissionProfileId,
    },
};
pub use shell::shell_command_for_argv;
pub(crate) use shell::{shell_command_argv, shell_process_input, stable_process_input_fingerprint};
use std::path::Path;
pub use windows_shell::{
    host_shell_chain_separator, host_shell_has_unix_utilities, host_shell_name,
};

mod shell;
mod windows_shell;

impl AcceptedLocalWorkspaceProcessAdmission {
    pub(crate) fn matches_intent(self, intent: &ProcessActionIntent) -> bool {
        let required = required_process_permission_profile_id(intent);
        (self.sandbox_profile() == super::contracts::LocalWorkspaceProcessSandboxProfile::Host
            && required.is_some())
            || required == Some(self.permission_profile_id())
            || (self.permission_profile_id() == ProcessPermissionProfileId::LOCAL_WORKSPACE_HOST
                && required == Some(ProcessPermissionProfileId::LOCAL_WORKSPACE))
    }
}

/// Coarse runtime-owned classification for a proposed process argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessIntentClass {
    /// Read-only inspection and navigation commands with no workspace effect.
    Informational,
    /// Bounded local commands expected to read/write build artifacts.
    LocalWorkspaceEffect,
    /// No specific policy class is known.
    Unknown,
    /// The argv is blocked by hard process policy.
    Forbidden,
}

/// Classifies a process intent using validated argv only.
#[must_use]
pub fn classify_process_intent(intent: &ProcessActionIntent) -> ProcessIntentClass {
    classify_process_argv(intent.argv())
}

fn classify_process_argv(argv: &[String]) -> ProcessIntentClass {
    if is_forbidden_process_argv(argv) {
        return ProcessIntentClass::Forbidden;
    }
    if is_informational_process_argv(argv) {
        return ProcessIntentClass::Informational;
    }
    if is_local_workspace_effect_process_argv(argv) {
        return ProcessIntentClass::LocalWorkspaceEffect;
    }
    ProcessIntentClass::Unknown
}

fn is_informational_process_argv(argv: &[String]) -> bool {
    if is_read_only_direct_process_argv(argv) {
        return true;
    }

    is_read_only_plain_shell_process_argv(argv)
}

fn is_read_only_direct_process_argv(argv: &[String]) -> bool {
    match argv {
        [executable, version]
            if executable_token_is(executable, "rustc") && version.as_str() == "--version" =>
        {
            true
        }
        [executable, rg_arg] if executable_token_is(executable, "rg") => {
            is_read_only_rg_single_argument(rg_arg)
        }
        [executable, print_flag, range, file]
            if executable_token_is(executable, "sed")
                && print_flag.as_str() == "-n"
                && is_read_only_sed_print_range(range)
                && is_workspace_relative_file_argument(file) =>
        {
            true
        }
        [executable, subcommand, args @ ..] if executable_token_is(executable, "git") => {
            is_read_only_git_command(subcommand, args)
        }
        [executable, version]
            if executable_token_is(executable, "git") && version.as_str() == "--version" =>
        {
            true
        }
        [executable] if executable_token_is(executable, "pwd") => true,
        [executable] if executable_token_is(executable, "true") => true,
        [executable] if executable_token_is(executable, "false") => true,
        [executable, args @ ..] if executable_token_is(executable, "echo") => {
            is_read_only_echo_args(args)
        }
        [executable, args @ ..] if executable_token_is(executable, "wc") => {
            is_read_only_wc_args(args)
        }
        [executable, args @ ..] if executable_token_is(executable, "head") => {
            is_read_only_head_or_tail_args(args)
        }
        [executable, args @ ..] if executable_token_is(executable, "tail") => {
            is_read_only_head_or_tail_args(args)
        }
        [executable, args @ ..] if executable_token_is(executable, "cargo") => {
            is_read_only_cargo_fmt_check_args(args)
        }
        [executable] if executable_token_is(executable, "ls") => true,
        [executable, file] if executable_token_is(executable, "ls") => {
            is_workspace_relative_file_argument(file)
        }
        [executable, file] if executable_token_is(executable, "cat") => {
            is_workspace_relative_file_argument(file)
        }
        [executable] if executable.eq_ignore_ascii_case("get-location") => true,
        [executable] if executable.eq_ignore_ascii_case("get-childitem") => true,
        [executable, file] if executable.eq_ignore_ascii_case("get-childitem") => {
            is_workspace_relative_file_argument(file)
        }
        [executable, file] if executable.eq_ignore_ascii_case("get-content") => {
            is_workspace_relative_file_argument(file)
        }
        _ => false,
    }
}

fn is_read_only_cargo_fmt_check_args(args: &[String]) -> bool {
    let mut saw_fmt = false;
    let mut saw_check = false;
    for argument in args {
        match argument.as_str() {
            "fmt" if !saw_fmt => saw_fmt = true,
            "--all" | "--check" if !saw_check || argument == "--all" => {
                if argument == "--check" {
                    saw_check = true;
                }
            }
            _ => return false,
        }
    }
    saw_fmt && saw_check
}

fn is_read_only_plain_shell_process_argv(argv: &[String]) -> bool {
    let Some(shell_input) = shell_process_input_from_argv(argv) else {
        return false;
    };

    let commands = match shell_input.kind() {
        ShellKind::Bash => parse_plain_shell_command_sequence(shell_input.script()),
        #[cfg(windows)]
        ShellKind::PowerShell => parse_plain_powershell_command_sequence(shell_input.script()),
        #[cfg(windows)]
        ShellKind::Cmd => None,
    };

    commands.is_some_and(|commands| {
        !commands.is_empty()
            && commands
                .iter()
                .all(|command| is_read_only_direct_process_argv(command))
    })
}

fn is_read_only_echo_args(args: &[String]) -> bool {
    !args.iter().any(|arg| arg.starts_with('-'))
}

fn is_read_only_wc_args(args: &[String]) -> bool {
    match args {
        [] => true,
        [flag_or_file] => {
            is_read_only_wc_flag(flag_or_file) || is_workspace_relative_file_argument(flag_or_file)
        }
        [flag, file] => is_read_only_wc_flag(flag) && is_workspace_relative_file_argument(file),
        _ => false,
    }
}

fn is_read_only_wc_flag(flag: &str) -> bool {
    matches!(flag, "-l" | "-w" | "-c" | "-m")
}

fn is_read_only_head_or_tail_args(args: &[String]) -> bool {
    match args {
        [] => true,
        [file] => is_workspace_relative_file_argument(file),
        [count_flag, count] if count_flag.as_str() == "-n" => is_positive_decimal(count),
        [count_flag, count, file] if count_flag.as_str() == "-n" => {
            is_positive_decimal(count) && is_workspace_relative_file_argument(file)
        }
        _ => false,
    }
}

fn is_read_only_rg_single_argument(argument: &str) -> bool {
    argument == "--version" || argument == "--files" || is_simple_rg_literal_pattern(argument)
}

fn is_simple_rg_literal_pattern(pattern: &str) -> bool {
    !pattern.starts_with('-') && !pattern.chars().any(is_rg_regex_metacharacter)
}

fn is_rg_regex_metacharacter(character: char) -> bool {
    matches!(
        character,
        '\\' | '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
    )
}

fn is_read_only_sed_print_range(range: &str) -> bool {
    let Some(line_range) = range.strip_suffix('p') else {
        return false;
    };
    if line_range.is_empty() {
        return false;
    }
    let mut parts = line_range.split(',');
    let Some(start) = parts.next() else {
        return false;
    };
    if !is_positive_decimal(start) {
        return false;
    }
    match (parts.next(), parts.next()) {
        (None, None) => true,
        (Some(end), None) => is_positive_decimal(end),
        _ => false,
    }
}

fn is_positive_decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) && value != "0"
}

fn is_workspace_relative_file_argument(argument: &str) -> bool {
    if argument.starts_with('-')
        || Path::new(argument).is_absolute()
        || has_windows_drive_prefix(argument)
    {
        return false;
    }
    #[cfg(windows)]
    {
        !argument.starts_with(['/', '\\'])
            && !argument
                .split(['/', '\\'])
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    }
    #[cfg(not(windows))]
    {
        !argument.split('/').any(|segment| {
            segment.is_empty() || segment == "." || segment == ".." || segment.contains('\\')
        })
    }
}

fn is_read_only_git_command(subcommand: &str, args: &[String]) -> bool {
    match subcommand {
        "status" => {
            let mut short = false;
            let mut branch = false;
            for argument in args {
                match argument.as_str() {
                    "--short" if !short => short = true,
                    "--branch" if !branch => branch = true,
                    _ => return false,
                }
            }
            true
        }
        "branch" => matches!(args, [arg] if arg.as_str() == "--show-current"),
        "log" => args
            .iter()
            .all(|arg| arg.as_str() == "--oneline" || is_git_count_arg(arg)),
        "diff" => is_read_only_git_diff_args(args),
        "show" => is_read_only_git_show_args(args),
        _ => false,
    }
}

fn is_git_count_arg(argument: &str) -> bool {
    let Some(count) = argument.strip_prefix('-') else {
        return false;
    };
    is_positive_decimal(count)
}

fn is_read_only_git_diff_args(args: &[String]) -> bool {
    match args {
        [] => true,
        [separator, path] if separator.as_str() == "--" => {
            is_workspace_relative_file_argument(path)
        }
        _ => false,
    }
}

fn is_read_only_git_show_args(args: &[String]) -> bool {
    match args {
        [arg] => !arg.starts_with('-') && !arg.contains(':'),
        [stat, rev] if stat.as_str() == "--stat" => !rev.starts_with('-') && !rev.contains(':'),
        _ => false,
    }
}

fn is_local_workspace_effect_process_argv(argv: &[String]) -> bool {
    matches!(
        argv,
        [cargo, command, package_flag, package]
            if executable_token_is(cargo, "cargo")
                && matches!(command.as_str(), "test" | "check")
                && (package_flag.as_str() == "-p" || package_flag.as_str() == "--package")
                && is_safe_cargo_package_token(package)
    )
}

pub(crate) fn is_safe_cargo_package_token(package: &str) -> bool {
    !package.is_empty()
        && !package.starts_with('-')
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_forbidden_process_argv(argv: &[String]) -> bool {
    if let Some(shell_input) = shell_like_process_input_from_argv(argv) {
        return shell_script_contains_forbidden_process(shell_input.kind(), shell_input.script());
    }

    is_forbidden_direct_process_argv(argv)
}

fn is_forbidden_direct_process_argv(argv: &[String]) -> bool {
    if command_words_have_env_injection(argv) {
        return true;
    }
    let argv = strip_shell_command_prefixes(argv);
    let Some(executable) = argv.first().map(|argument| executable_name(argument)) else {
        return false;
    };

    if executable == "rm" {
        return rm_invocation_requires_review(argv);
    }

    if FORBIDDEN_PROCESS_EXECUTABLES.contains(&executable.as_str()) {
        return true;
    }

    if executable == "git" {
        return git_invocation_is_forbidden(argv);
    }

    false
}

fn git_invocation_is_forbidden(argv: &[String]) -> bool {
    let Some((subcommand, sub_args)) = git_subcommand_and_args(argv) else {
        return false;
    };
    if subcommand == "stash" && is_read_only_git_stash_args(sub_args) {
        return false;
    }
    FORBIDDEN_GIT_SUBCOMMANDS.contains(&subcommand)
}

/// Peels common command wrappers (`env`, `timeout`, `command`, `exec`, `nohup`,
/// `time`, `builtin`, `!`) so forbidden executables behind them are still
/// detected.
fn strip_shell_command_prefixes(mut argv: &[String]) -> &[String] {
    for _ in 0..8 {
        let Some(first) = argv.first() else {
            return argv;
        };
        if matches!(
            first.as_str(),
            "!" | "time" | "exec" | "command" | "builtin" | "nohup"
        ) {
            argv = &argv[1..];
            continue;
        }
        let head = executable_name(first);
        if head == "env" {
            let mut idx = 1;
            let mut options_done = false;
            while let Some(arg) = argv.get(idx) {
                if !options_done && arg == "--" {
                    options_done = true;
                    idx += 1;
                    continue;
                }
                if !options_done && arg.starts_with('-') {
                    idx += 1;
                    continue;
                }
                if arg.contains('=') && !arg.starts_with('=') {
                    idx += 1;
                    continue;
                }
                break;
            }
            argv = &argv[idx..];
            continue;
        }
        if head == "timeout" {
            let mut idx = 1;
            while let Some(arg) = argv.get(idx) {
                if matches!(arg.as_str(), "-k" | "--kill-after" | "-s" | "--signal") {
                    idx += 2;
                    continue;
                }
                if arg.starts_with('-') {
                    idx += 1;
                    continue;
                }
                // Consume the duration positional operand.
                idx += 1;
                break;
            }
            argv = argv.get(idx..).unwrap_or(&[]);
            continue;
        }
        break;
    }
    argv
}

/// Returns the git subcommand and its trailing arguments, skipping recognized
/// global git options before the subcommand.
fn git_subcommand_and_args(argv: &[String]) -> Option<(&str, &[String])> {
    let mut index = 1;
    while let Some(arg) = argv.get(index) {
        if matches!(
            arg.as_str(),
            "-C" | "-c"
                | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--super-prefix"
                | "--exec-path"
                | "--config-env"
        ) {
            index += 2;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        return Some((arg.as_str(), &argv[index + 1..]));
    }
    None
}

fn shell_script_contains_forbidden_process(kind: ShellKind, script: &str) -> bool {
    match kind {
        ShellKind::Bash => {
            let Some(analysis) = parse_bash_security_analysis(script) else {
                // Syntax error or unparseable shell structure: fail closed.
                return true;
            };
            analysis.env_injection
                || analysis
                    .commands
                    .iter()
                    .any(|command| is_forbidden_direct_process_argv(command))
        }
        #[cfg(windows)]
        ShellKind::PowerShell => {
            let Some(analysis) = analyze_powershell_script(script) else {
                return true;
            };
            analysis.env_or_eval_injection
                || analysis.has_destructive_delete
                || analysis
                    .commands
                    .iter()
                    .any(|command| is_forbidden_direct_process_argv(command))
        }
        #[cfg(windows)]
        ShellKind::Cmd => true,
    }
}

const FORBIDDEN_PROCESS_EXECUTABLES: &[&str] = &[
    "bash",
    "cmd",
    "powershell",
    "pwsh",
    "sh",
    "su",
    "sudo",
    "zsh",
];

const FORBIDDEN_GIT_SUBCOMMANDS: &[&str] = &[
    "add",
    "apply",
    "cherry-pick",
    "clean",
    "commit",
    "merge",
    "mv",
    "pull",
    "push",
    "rebase",
    "reset",
    "restore",
    "rm",
    "stash",
    "switch",
];

/// A plain `rm` of named files does not need a second review. Recursive deletes
/// and operands that wipe a tree still do. An unparsed shell mention of `rm`
/// stays under review because the flags are not reliable there.
fn rm_invocation_requires_review(argv: &[String]) -> bool {
    let mut operands = Vec::new();
    let mut end_of_flags = false;
    for argument in argv.iter().skip(1) {
        if !end_of_flags && argument == "--" {
            end_of_flags = true;
            continue;
        }
        if !end_of_flags && argument.starts_with('-') {
            if rm_flag_requires_review(argument) {
                return true;
            }
            continue;
        }
        operands.push(argument.as_str());
    }
    operands.is_empty()
        || operands
            .iter()
            .any(|operand| rm_operand_requires_review(operand))
}

fn rm_flag_requires_review(flag: &str) -> bool {
    if flag == "--recursive" || flag == "--dir" {
        return true;
    }
    let Some(cluster) = flag.strip_prefix('-') else {
        return false;
    };
    if cluster.starts_with('-') {
        return false;
    }
    cluster
        .chars()
        .any(|character| matches!(character, 'r' | 'R'))
}

fn rm_operand_requires_review(operand: &str) -> bool {
    operand.is_empty()
        || operand == "."
        || operand == ".."
        || operand == "/"
        || operand == "*"
        || operand == "~"
        || operand.ends_with("/*")
        || operand
            .chars()
            .any(|character| matches!(character, '*' | '?' | '[' | '{' | '$' | '`'))
}

/// Returns whether a process intent may enter the SP3-A low-risk process lane.
///
/// The first admitted lane is intentionally fail-closed. It is a small read-only
/// injected-runner allowset, not a general command risk model:
/// no inherited/supplied environment, no stdin text, and only deterministic
/// read-only argv shapes explicitly recognized by SP3-A. Future slices can
/// expand this predicate only with a real policy model and execution evidence
/// for the additional process inputs.
#[must_use]
pub fn is_low_risk_process_action_intent(intent: &ProcessActionIntent) -> bool {
    required_process_permission_profile_id(intent) == Some(ProcessPermissionProfileId::READ_ONLY)
}

/// Returns whether a process intent is a plain read-only shell-wrapper action.
///
/// This predicate is intentionally separate from the structured read-only
/// process lane. It recognizes only `bash`/`sh`/`zsh -c|-lc` scripts composed
/// of plain word commands joined by `|`, `&&`, `||`, or `;`, and it requires
/// each segment to match the direct read-only process classifier. It is not a
/// general shell parser and must be paired with an explicit shell runner
/// admission before execution.
#[must_use]
pub fn is_read_only_shell_process_action_intent(intent: &ProcessActionIntent) -> bool {
    required_process_permission_profile_id(intent)
        == Some(ProcessPermissionProfileId::SHELL_READ_ONLY)
}

pub(crate) fn required_process_permission_profile_id(
    intent: &ProcessActionIntent,
) -> Option<ProcessPermissionProfileId> {
    if intent.env_policy() != ProcessEnvPolicy::Empty || intent.stdin_text().is_some() {
        return Some(ProcessPermissionProfileId::LOCAL_WORKSPACE);
    }

    if is_read_only_plain_shell_process_argv(intent.argv()) {
        return Some(ProcessPermissionProfileId::SHELL_READ_ONLY);
    }

    match classify_process_intent(intent) {
        ProcessIntentClass::Informational => Some(ProcessPermissionProfileId::READ_ONLY),
        ProcessIntentClass::LocalWorkspaceEffect => {
            Some(ProcessPermissionProfileId::LOCAL_WORKSPACE)
        }
        ProcessIntentClass::Unknown => Some(ProcessPermissionProfileId::LOCAL_WORKSPACE),
        // Classification is a risk signal, not a command allowlist. The
        // selected runner remains the authority for capability enforcement.
        ProcessIntentClass::Forbidden => Some(ProcessPermissionProfileId::LOCAL_WORKSPACE),
    }
}

/// Returns whether the process classifier requires an independent action
/// review before execution.
///
/// Unknown commands are still admitted by the configured sandbox runner;
/// only the explicit high-risk classification triggers this second gate.
#[must_use]
pub(crate) fn requires_process_action_review(intent: &ProcessActionIntent) -> bool {
    matches!(
        classify_process_intent(intent),
        ProcessIntentClass::Forbidden
    )
}

/// Returns whether an unrestricted host process explicitly names a filesystem
/// path that is outside the validated workspace-relative command shape.
///
/// This is a review signal only. The host runner has no mount boundary, so an
/// approval here cannot be treated as an operating-system capability grant.
pub(crate) fn requires_host_process_path_review(intent: &ProcessActionIntent) -> bool {
    if let Some(shell_input) = shell_like_process_input_from_argv(intent.argv()) {
        return shell_script_contains_git_metadata_write(shell_input.kind(), shell_input.script())
            || shell_script_contains_host_path(shell_input.kind(), shell_input.script())
            || intent
                .argv()
                .first()
                .is_some_and(|argument| is_explicit_host_path_token(argument));
    }

    is_git_metadata_write_argv(intent.argv())
        || intent
            .argv()
            .iter()
            .any(|argument| is_explicit_host_path_token(argument))
}

fn shell_script_contains_host_path(kind: ShellKind, script: &str) -> bool {
    match kind {
        ShellKind::Bash => {
            let Some(analysis) = parse_bash_security_analysis(script) else {
                return true;
            };
            analysis.has_command_substitution
                || analysis.path_check_tokens.iter().any(|token| {
                    is_explicit_host_path_token(token) || contains_embedded_absolute_path(token)
                })
        }
        #[cfg(windows)]
        ShellKind::PowerShell => {
            let Some(analysis) = analyze_powershell_script(script) else {
                return true;
            };
            analysis.has_command_substitution
                || analysis.path_check_tokens.iter().any(|token| {
                    is_explicit_host_path_token(token) || contains_embedded_absolute_path(token)
                })
        }
        #[cfg(windows)]
        ShellKind::Cmd => true,
    }
}

fn contains_embedded_absolute_path(value: &str) -> bool {
    if contains_embedded_windows_drive_path(value) {
        return true;
    }
    value.char_indices().any(|(index, character)| {
        if character != '/' {
            return false;
        }
        if index == 0 {
            return true;
        }
        let Some(previous) = value[..index].chars().next_back() else {
            return false;
        };
        if previous == ':' && is_non_file_url_scheme_separator(value, index) {
            return false;
        }
        matches!(previous, '"' | '\'' | '(' | '[' | '{' | '=' | ':' | ',')
    })
}

fn contains_embedded_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 3 {
        return false;
    }
    for i in 0..=bytes.len() - 3 {
        if bytes[i].is_ascii_alphabetic()
            && bytes[i + 1] == b':'
            && matches!(bytes[i + 2], b'\\' | b'/')
            && (i == 0
                || matches!(
                    bytes[i - 1],
                    b'"' | b'\'' | b'(' | b'[' | b'{' | b'=' | b',' | b';'
                ))
        {
            return true;
        }
    }
    false
}

/// Returns whether the `/` at `slash_index` is the `://` separator of a
/// network URL scheme (such as `https://` or `http://`). `file://` is excluded
/// because it references local filesystem paths.
fn is_non_file_url_scheme_separator(value: &str, slash_index: usize) -> bool {
    if !value[slash_index..].starts_with("//") {
        return false;
    }
    let prefix = &value[..slash_index - 1];
    let scheme_start = prefix
        .rfind(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')))
        .map_or(0, |pos| pos + 1);
    let scheme = &prefix[scheme_start..];
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ws" | "wss" | "ftp" | "ftps" | "git" | "ssh"
    )
}

fn shell_script_contains_git_metadata_write(kind: ShellKind, script: &str) -> bool {
    match kind {
        ShellKind::Bash => {
            let Some(analysis) = parse_bash_security_analysis(script) else {
                return true;
            };
            analysis
                .commands
                .iter()
                .map(Vec::as_slice)
                .any(is_git_metadata_write_argv)
        }
        #[cfg(windows)]
        ShellKind::PowerShell => {
            let Some(analysis) = analyze_powershell_script(script) else {
                return true;
            };
            analysis
                .commands
                .iter()
                .map(Vec::as_slice)
                .any(is_git_metadata_write_argv)
        }
        #[cfg(windows)]
        ShellKind::Cmd => true,
    }
}

fn is_git_metadata_write_argv(argv: &[String]) -> bool {
    let argv = strip_shell_command_prefixes(argv);
    let Some(executable) = argv.first() else {
        return false;
    };
    executable_name(executable) == "git" && !is_read_only_git_argv_for_host_review(argv)
}

/// Fail-closed read-only git detector used by host-mode path/metadata review.
///
/// Kept separate from the narrow SP3-A `Informational` lane so read-only git
/// inspection (`git diff --cached`, `git status -sb`, `git rev-parse HEAD`,
/// `git log -n 5`, `git branch -a`, `git remote -v`, `git stash list`, etc.)
/// does not trigger unnecessary host-mode permission reviews while mutating
/// subcommands and config/path overrides stay reviewed.
fn is_read_only_git_argv_for_host_review(argv: &[String]) -> bool {
    if is_read_only_direct_process_argv(argv) {
        return true;
    }
    if argv.len() == 2 && matches!(argv[1].as_str(), "--version" | "-v" | "--help" | "-h") {
        return true;
    }

    // Reject global git options that inject config, override repository paths,
    // or execute external helpers before the subcommand.
    let mut index = 1;
    let subcommand = loop {
        let Some(arg) = argv.get(index) else {
            return false;
        };
        if matches!(
            arg.as_str(),
            "-c" | "--config-env"
                | "--git-dir"
                | "--work-tree"
                | "--exec-path"
                | "--namespace"
                | "--super-prefix"
                | "--bare"
        ) || arg.starts_with("--git-dir=")
            || arg.starts_with("--work-tree=")
            || arg.starts_with("--exec-path=")
            || arg.starts_with("--config-env=")
            || arg.starts_with("-c")
        {
            return false;
        }
        if arg == "-C" {
            let Some(dir) = argv.get(index + 1) else {
                return false;
            };
            if !is_workspace_relative_file_argument(dir) {
                return false;
            }
            index += 2;
            continue;
        }
        if matches!(arg.as_str(), "--no-pager" | "-p" | "--paginate") {
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            return false;
        }
        break arg.as_str();
    };
    let args = &argv[index + 1..];

    // Reject output-writing or external-command options on query subcommands.
    let has_unsafe_query_flag = args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "-o" | "--output" | "--ext-diff" | "--textconv"
        ) || arg.starts_with("--output=")
    });
    if has_unsafe_query_flag {
        return false;
    }

    match subcommand {
        "status" | "diff" | "log" | "show" | "blame" | "shortlog" | "describe" | "rev-parse"
        | "rev-list" | "ls-files" | "ls-tree" | "cat-file" | "merge-base" | "name-rev" | "grep"
        | "whatchanged" => true,
        "branch" => is_read_only_git_branch_args(args),
        "tag" => is_read_only_git_tag_args(args),
        "remote" => is_read_only_git_remote_args(args),
        "stash" => is_read_only_git_stash_args(args),
        "worktree" => matches!(args.first().map(String::as_str), Some("list")),
        "config" => is_read_only_git_config_args(args),
        _ => false,
    }
}

fn is_read_only_git_branch_args(args: &[String]) -> bool {
    if args.is_empty() {
        return true;
    }
    let mut index = 0;
    let mut saw_list_flag = false;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--show-current" | "-a" | "--all" | "-r" | "--remotes" | "-l" | "--list" | "-v"
            | "-vv" | "--verbose" | "--merged" | "--no-merged" => {
                saw_list_flag = true;
                index += 1;
            }
            "--contains" | "--points-at" | "--sort" | "--format" => {
                if args.get(index + 1).is_none() {
                    return false;
                }
                saw_list_flag = true;
                index += 2;
            }
            _ if arg.starts_with("--sort=")
                || arg.starts_with("--format=")
                || arg.starts_with("--contains=")
                || arg.starts_with("--points-at=") =>
            {
                saw_list_flag = true;
                index += 1;
            }
            _ => return false,
        }
    }
    saw_list_flag
}

fn is_read_only_git_tag_args(args: &[String]) -> bool {
    if args.is_empty() {
        return true;
    }
    let mut index = 0;
    let mut saw_list_flag = false;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "-l" | "--list" | "-n" => {
                saw_list_flag = true;
                index += 1;
            }
            "--contains" | "--points-at" | "--sort" | "--format" => {
                if args.get(index + 1).is_none() {
                    return false;
                }
                saw_list_flag = true;
                index += 2;
            }
            _ if arg.starts_with("--sort=")
                || arg.starts_with("--format=")
                || arg.starts_with("--contains=")
                || arg.starts_with("--points-at=") =>
            {
                saw_list_flag = true;
                index += 1;
            }
            _ => return false,
        }
    }
    saw_list_flag
}

fn is_read_only_git_remote_args(args: &[String]) -> bool {
    match args {
        [] => true,
        [flag] if matches!(flag.as_str(), "-v" | "--verbose") => true,
        [sub, ..] if matches!(sub.as_str(), "show" | "get-url") => true,
        _ => false,
    }
}

fn is_read_only_git_stash_args(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("list" | "show"))
}

fn is_read_only_git_config_args(args: &[String]) -> bool {
    let mut saw_read_flag = false;
    for arg in args {
        if matches!(
            arg.as_str(),
            "--unset"
                | "--unset-all"
                | "--replace-all"
                | "--add"
                | "--rename-section"
                | "--remove-section"
                | "--edit"
                | "-e"
                | "--file"
                | "-f"
        ) || arg.starts_with("--file=")
        {
            return false;
        }
        if matches!(
            arg.as_str(),
            "--get" | "--get-all" | "--get-regexp" | "--list" | "-l" | "--show-origin"
        ) {
            saw_read_flag = true;
        }
    }
    saw_read_flag
}

fn is_explicit_host_path_token(argument: &str) -> bool {
    let argument = argument.trim_matches(|character| matches!(character, '\'' | '"'));
    let argument = argument
        .find(['>', '<'])
        .map_or(argument, |index| &argument[index + 1..]);
    let lower = argument.to_ascii_lowercase();
    argument == ".."
        || argument.starts_with("../")
        || argument.starts_with("..\\")
        || argument.contains("/../")
        || argument.contains("\\..\\")
        || argument.starts_with('/')
        || argument.starts_with("\\\\")
        || has_windows_drive_prefix(argument)
        || argument == "~"
        || argument.starts_with("~/")
        || argument.starts_with("~\\")
        || argument.starts_with("$HOME/")
        || argument.starts_with("${HOME}/")
        || lower.starts_with("$env:userprofile")
        || lower.starts_with("$env:appdata")
        || lower.starts_with("$env:localappdata")
        || lower.starts_with("%userprofile%")
        || lower.starts_with("%appdata%")
        || lower.starts_with("%localappdata%")
        || argument
            .split_once('=')
            .is_some_and(|(_, value)| is_explicit_host_path_token(value))
}

fn has_windows_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || matches!(bytes[2], b'\\' | b'/'))
}
