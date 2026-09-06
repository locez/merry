use super::contracts::{
    AcceptedLocalWorkspaceProcessAdmission, ProcessActionIntent, ProcessEnvPolicy,
    ProcessPermissionProfileId,
};
use std::path::Path;
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

/// Exact shell-wrapper input plus payload-free metadata helpers.
///
/// This value recognizes only the validated wrapper shape used by the current
/// shell read-only lane. It is not a shell parser and it does not authorize
/// execution by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShellProcessInput<'a> {
    shell: &'a str,
    flag: &'a str,
    script: &'a str,
}

impl<'a> ShellProcessInput<'a> {
    pub(crate) const fn shell(self) -> &'a str {
        self.shell
    }

    pub(crate) const fn flag(self) -> &'a str {
        self.flag
    }

    pub(crate) const fn script(self) -> &'a str {
        self.script
    }

    pub(crate) const fn script_bytes(self) -> usize {
        self.script.len()
    }

    pub(crate) fn script_fingerprint(self) -> String {
        stable_process_input_fingerprint(self.script.as_bytes())
    }
}

pub(crate) fn shell_process_input(intent: &ProcessActionIntent) -> Option<ShellProcessInput<'_>> {
    shell_process_input_from_argv(intent.argv())
}

/// Converts one model-facing shell command into the platform process argv
/// consumed by runtime policy and process runners.
pub(crate) fn shell_command_argv(command: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "powershell".to_owned(),
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            command.to_owned(),
        ]
    }

    #[cfg(not(windows))]
    {
        vec!["bash".to_owned(), "-lc".to_owned(), command.to_owned()]
    }
}

/// Returns the command script represented by a platform shell argv wrapper.
pub(crate) fn shell_command_from_argv(argv: &[String]) -> Option<&str> {
    #[cfg(windows)]
    {
        match argv {
            [shell, no_profile, flag, command]
                if shell == "powershell" && no_profile == "-NoProfile" && flag == "-Command" =>
            {
                Some(command)
            }
            _ => None,
        }
    }

    #[cfg(not(windows))]
    {
        match argv {
            [shell, flag, command] if shell == "bash" && matches!(flag.as_str(), "-c" | "-lc") => {
                Some(command)
            }
            _ => None,
        }
    }
}

/// Converts an argv vector into one shell command string for a model-facing
/// command field. Existing platform shell wrappers are unwrapped so their
/// script is preserved; direct argv items are quoted for the host shell.
#[must_use]
pub fn shell_command_for_argv(argv: &[String]) -> String {
    if let Some(command) = shell_command_from_argv(argv) {
        return command.to_owned();
    }

    argv.iter()
        .map(|argument| shell_quote_argument(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(not(windows))]
fn shell_quote_argument(argument: &str) -> String {
    if !argument.is_empty() && argument.bytes().all(is_safe_shell_word_byte) {
        return argument.to_owned();
    }
    format!("'{}'", argument.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn shell_quote_argument(argument: &str) -> String {
    if !argument.is_empty() && argument.bytes().all(is_safe_shell_word_byte) {
        return argument.to_owned();
    }
    format!("'{}'", argument.replace('\'', "''"))
}

fn is_safe_shell_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b'@' | b'%' | b'+'
        )
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

    parse_plain_shell_command_sequence(shell_input.script()).is_some_and(|commands| {
        !commands.is_empty()
            && commands
                .iter()
                .all(|command| is_read_only_direct_process_argv(command))
    })
}

fn shell_process_input_from_argv(argv: &[String]) -> Option<ShellProcessInput<'_>> {
    #[cfg(windows)]
    {
        let [shell, no_profile, flag, script] = argv else {
            return None;
        };
        if !is_supported_plain_shell_token(shell)
            || no_profile != "-NoProfile"
            || flag != "-Command"
        {
            return None;
        }
        Some(ShellProcessInput {
            shell,
            flag,
            script,
        })
    }

    #[cfg(not(windows))]
    {
        let [shell, flag, script] = argv else {
            return None;
        };
        if !is_supported_plain_shell_token(shell) || !matches!(flag.as_str(), "-c" | "-lc") {
            return None;
        }
        Some(ShellProcessInput {
            shell,
            flag,
            script,
        })
    }
}

fn is_supported_plain_shell_token(shell: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(executable_name(shell).as_str(), "powershell" | "pwsh")
    }

    #[cfg(not(windows))]
    matches!(shell, "bash" | "sh" | "zsh")
}

pub(crate) fn stable_process_input_fingerprint(bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    format!("fnv1a64:{hash:016x}")
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

fn parse_plain_shell_command_sequence(script: &str) -> Option<Vec<Vec<String>>> {
    let mut chars = script.chars().peekable();
    let mut commands = Vec::new();
    let mut current_command = Vec::new();
    let mut last_token_was_operator = false;

    loop {
        skip_shell_whitespace(&mut chars);
        let Some(next) = chars.peek().copied() else {
            break;
        };

        if is_shell_sequence_operator_start(next) {
            parse_shell_sequence_operator(&mut chars)?;
            if current_command.is_empty() {
                return None;
            }
            commands.push(std::mem::take(&mut current_command));
            last_token_was_operator = true;
            continue;
        }

        let word = parse_plain_shell_word(&mut chars)?;
        if word.is_empty() {
            return None;
        }
        current_command.push(word);
        last_token_was_operator = false;
    }

    if last_token_was_operator {
        return None;
    }
    if !current_command.is_empty() {
        commands.push(current_command);
    }
    if commands.is_empty() {
        return None;
    }

    Some(commands)
}

fn skip_shell_whitespace(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while chars
        .peek()
        .is_some_and(|character| character.is_whitespace())
    {
        chars.next();
    }
}

fn parse_shell_sequence_operator(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<()> {
    match chars.next()? {
        ';' | '|' if chars.peek() != Some(&'|') => Some(()),
        '|' if chars.peek() == Some(&'|') => {
            chars.next();
            Some(())
        }
        '&' if chars.peek() == Some(&'&') => {
            chars.next();
            Some(())
        }
        _ => None,
    }
}

fn parse_plain_shell_word(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    let mut word = String::new();
    while let Some(next) = chars.peek().copied() {
        if next.is_whitespace() || is_shell_sequence_operator_start(next) {
            break;
        }

        match next {
            '\'' => {
                chars.next();
                parse_plain_single_quoted_shell_fragment(chars, &mut word)?;
            }
            '"' => {
                chars.next();
                parse_plain_double_quoted_shell_fragment(chars, &mut word)?;
            }
            character if shell_word_character_is_disallowed(character) => return None,
            character => {
                chars.next();
                word.push(character);
            }
        }
    }

    Some(word)
}

fn parse_plain_single_quoted_shell_fragment(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    word: &mut String,
) -> Option<()> {
    for character in chars.by_ref() {
        if character == '\'' {
            return Some(());
        }
        word.push(character);
    }
    None
}

fn parse_plain_double_quoted_shell_fragment(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    word: &mut String,
) -> Option<()> {
    for character in chars.by_ref() {
        match character {
            '"' => return Some(()),
            '$' | '`' | '\\' | '!' => return None,
            _ => word.push(character),
        }
    }
    None
}

fn is_shell_sequence_operator_start(character: char) -> bool {
    matches!(character, ';' | '|' | '&')
}

fn shell_word_character_is_disallowed(character: char) -> bool {
    matches!(
        character,
        '$' | '`'
            | '\\'
            | '<'
            | '>'
            | '('
            | ')'
            | '{'
            | '}'
            | '['
            | ']'
            | '*'
            | '?'
            | '~'
            | '#'
            | '!'
    )
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
    !argument.starts_with('-')
        && !Path::new(argument).is_absolute()
        && !argument.split('/').any(|segment| {
            segment.is_empty() || segment == "." || segment == ".." || segment.contains('\\')
        })
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
        return shell_script_contains_forbidden_process(shell_input.script());
    }

    is_forbidden_direct_process_argv(argv)
}

fn is_forbidden_direct_process_argv(argv: &[String]) -> bool {
    let Some(executable) = argv.first().map(|argument| executable_name(argument)) else {
        return false;
    };

    if FORBIDDEN_PROCESS_EXECUTABLES.contains(&executable.as_str()) {
        return true;
    }

    executable == "git"
        && argv
            .get(1)
            .is_some_and(|subcommand| FORBIDDEN_GIT_SUBCOMMANDS.contains(&subcommand.as_str()))
}

fn shell_script_contains_forbidden_process(script: &str) -> bool {
    if let Some(commands) = parse_plain_shell_command_sequence(script) {
        return commands.iter().any(|command| {
            let command = shell_command_without_assignment_prefix(command);
            !command.is_empty() && is_forbidden_direct_process_argv(command)
        });
    }

    shell_script_contains_obvious_forbidden_text(script)
}

fn shell_like_process_input_from_argv(argv: &[String]) -> Option<ShellProcessInput<'_>> {
    #[cfg(windows)]
    {
        let [shell, no_profile, flag, script] = argv else {
            return None;
        };
        if !is_supported_shell_executable_name(shell)
            || no_profile != "-NoProfile"
            || flag != "-Command"
        {
            return None;
        }
        Some(ShellProcessInput {
            shell,
            flag,
            script,
        })
    }

    #[cfg(not(windows))]
    {
        let [shell, flag, script] = argv else {
            return None;
        };
        if !is_supported_shell_executable_name(shell) || !matches!(flag.as_str(), "-c" | "-lc") {
            return None;
        }
        Some(ShellProcessInput {
            shell,
            flag,
            script,
        })
    }
}

fn is_supported_shell_executable_name(shell: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(executable_name(shell).as_str(), "powershell" | "pwsh")
    }

    #[cfg(not(windows))]
    matches!(executable_name(shell).as_str(), "bash" | "sh" | "zsh")
}

fn shell_command_without_assignment_prefix(command: &[String]) -> &[String] {
    let executable_index = command
        .iter()
        .position(|word| !is_plain_shell_assignment_word(word))
        .unwrap_or(command.len());
    &command[executable_index..]
}

fn is_plain_shell_assignment_word(word: &str) -> bool {
    let Some((name, value)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !value.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        })
}

fn shell_script_contains_obvious_forbidden_text(script: &str) -> bool {
    let words = rough_shell_words(script);
    words.iter().enumerate().any(|(index, word)| {
        let executable = executable_name(word);
        FORBIDDEN_PROCESS_EXECUTABLES.contains(&executable.as_str())
            || (executable == "git"
                && words.get(index + 1).is_some_and(|subcommand| {
                    FORBIDDEN_GIT_SUBCOMMANDS.contains(&subcommand.as_str())
                }))
    })
}

fn rough_shell_words(script: &str) -> Vec<String> {
    script
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    ';' | '|' | '&' | '<' | '>' | '(' | ')' | '{' | '}' | '[' | ']' | '\'' | '"'
                )
        })
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn executable_name(argument: &str) -> String {
    argument
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argument)
        .to_ascii_lowercase()
}

fn executable_token_is(argument: &str, expected: &str) -> bool {
    argument == expected
}

const FORBIDDEN_PROCESS_EXECUTABLES: &[&str] = &[
    "bash",
    "cmd",
    "powershell",
    "pwsh",
    "rm",
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
        return shell_script_contains_git_metadata_write(shell_input.script())
            || shell_script_contains_host_path(shell_input.script())
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

fn shell_script_contains_host_path(script: &str) -> bool {
    script.contains("$(")
        || script.contains('`')
        || script.split_whitespace().any(is_explicit_host_path_token)
        || contains_embedded_absolute_path(script)
}

fn contains_embedded_absolute_path(value: &str) -> bool {
    value.char_indices().any(|(index, character)| {
        character == '/'
            && (index == 0
                || value[..index].chars().next_back().is_some_and(|previous| {
                    matches!(previous, '"' | '\'' | '(' | '[' | '{' | '=' | ':' | ',')
                }))
    })
}

fn shell_script_contains_git_metadata_write(script: &str) -> bool {
    if let Some(commands) = parse_plain_shell_command_sequence(script) {
        return commands
            .iter()
            .map(Vec::as_slice)
            .map(shell_command_without_assignment_prefix)
            .any(is_git_metadata_write_argv);
    }

    rough_shell_words(script)
        .iter()
        .any(|word| executable_name(word) == "git")
}

fn is_git_metadata_write_argv(argv: &[String]) -> bool {
    let Some(executable) = argv.first() else {
        return false;
    };
    executable_name(executable) == "git" && !is_read_only_direct_process_argv(argv)
}

fn is_explicit_host_path_token(argument: &str) -> bool {
    let argument = argument.trim_matches(|character| matches!(character, '\'' | '"'));
    let argument = argument
        .find(['>', '<'])
        .map_or(argument, |index| &argument[index + 1..]);
    argument == ".."
        || argument.starts_with("../")
        || argument.contains("/../")
        || argument.starts_with('/')
        || argument == "~"
        || argument.starts_with("~/")
        || argument.starts_with("$HOME/")
        || argument.starts_with("${HOME}/")
        || argument
            .split_once('=')
            .is_some_and(|(_, value)| is_explicit_host_path_token(value))
}
