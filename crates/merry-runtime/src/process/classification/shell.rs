//! Narrow shell-wrapper recognition, tree-sitter-bash AST normalization, and
//! Windows PowerShell detection/analysis; policy stays in the classifier.

use crate::process::contracts::ProcessActionIntent;
use tree_sitter::{Node, Parser, Tree};
use tree_sitter_bash::LANGUAGE as BASH_LANGUAGE;

#[cfg(windows)]
use super::windows_shell;

/// Shell grammar family represented by one recognized shell wrapper argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellKind {
    /// POSIX/Bash-compatible shell (`bash`, `sh`, `zsh`, Git Bash).
    Bash,
    /// PowerShell (`pwsh`, `powershell`, `powershell.exe`).
    #[cfg(windows)]
    PowerShell,
    /// Windows Command Prompt (`cmd`, `cmd.exe`).
    #[cfg(windows)]
    Cmd,
}

/// Exact shell-wrapper input plus payload-free metadata helpers.
///
/// This value recognizes only the validated wrapper shape used by the current
/// shell read-only lane. It does not authorize execution by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShellProcessInput<'a> {
    pub(super) kind: ShellKind,
    pub(super) shell: &'a str,
    pub(super) flag: &'a str,
    pub(super) script: &'a str,
}

impl<'a> ShellProcessInput<'a> {
    pub(crate) const fn kind(self) -> ShellKind {
        self.kind
    }

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
        windows_shell::detect_windows_shell().command_argv(command)
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
            [shell, no_profile, non_interactive, flag, command]
                if matches!(executable_name(shell).as_str(), "powershell" | "pwsh")
                    && no_profile.eq_ignore_ascii_case("-NoProfile")
                    && non_interactive.eq_ignore_ascii_case("-NonInteractive")
                    && flag.eq_ignore_ascii_case("-Command") =>
            {
                Some(command)
            }
            [shell, no_profile, flag, command]
                if matches!(executable_name(shell).as_str(), "powershell" | "pwsh")
                    && no_profile.eq_ignore_ascii_case("-NoProfile")
                    && flag.eq_ignore_ascii_case("-Command") =>
            {
                Some(command)
            }
            [shell, flag, command]
                if executable_name(shell) == "bash" && matches!(flag.as_str(), "-c" | "-lc") =>
            {
                Some(command)
            }
            [shell, flag, command]
                if executable_name(shell) == "cmd" && flag.eq_ignore_ascii_case("/C") =>
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
pub(super) fn shell_quote_argument(argument: &str) -> String {
    if !argument.is_empty() && argument.bytes().all(is_safe_shell_word_byte) {
        return argument.to_owned();
    }
    format!("'{}'", argument.replace('\'', "'\\''"))
}

#[cfg(windows)]
pub(super) fn shell_quote_argument(argument: &str) -> String {
    if !argument.is_empty() && argument.bytes().all(is_safe_shell_word_byte) {
        return argument.to_owned();
    }
    format!("'{}'", argument.replace('\'', "''"))
}

pub(super) fn is_safe_shell_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b'@' | b'%' | b'+'
        )
}

pub(super) fn shell_process_input_from_argv(argv: &[String]) -> Option<ShellProcessInput<'_>> {
    #[cfg(windows)]
    {
        match argv {
            [shell, no_profile, non_interactive, flag, script]
                if is_supported_plain_shell_token(shell)
                    && no_profile.eq_ignore_ascii_case("-NoProfile")
                    && non_interactive.eq_ignore_ascii_case("-NonInteractive")
                    && flag.eq_ignore_ascii_case("-Command") =>
            {
                Some(ShellProcessInput {
                    kind: ShellKind::PowerShell,
                    shell,
                    flag,
                    script,
                })
            }
            [shell, no_profile, flag, script]
                if is_supported_plain_shell_token(shell)
                    && no_profile.eq_ignore_ascii_case("-NoProfile")
                    && flag.eq_ignore_ascii_case("-Command") =>
            {
                Some(ShellProcessInput {
                    kind: ShellKind::PowerShell,
                    shell,
                    flag,
                    script,
                })
            }
            [shell, flag, script]
                if is_supported_plain_bash_token(shell)
                    && matches!(flag.as_str(), "-c" | "-lc") =>
            {
                Some(ShellProcessInput {
                    kind: ShellKind::Bash,
                    shell,
                    flag,
                    script,
                })
            }
            _ => None,
        }
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
            kind: ShellKind::Bash,
            shell,
            flag,
            script,
        })
    }
}

pub(super) fn is_supported_plain_shell_token(shell: &str) -> bool {
    #[cfg(windows)]
    {
        !shell.contains(['/', '\\'])
            && matches!(executable_name(shell).as_str(), "powershell" | "pwsh")
    }

    #[cfg(not(windows))]
    matches!(shell, "bash" | "sh" | "zsh")
}

#[cfg(windows)]
fn is_supported_plain_bash_token(shell: &str) -> bool {
    if !shell.contains(['/', '\\']) {
        return matches!(executable_name(shell).as_str(), "bash" | "sh" | "zsh");
    }
    matches!(
        windows_shell::detect_windows_shell(),
        windows_shell::WindowsShell::GitBash(path)
            if path.to_string_lossy().eq_ignore_ascii_case(shell)
    )
}

pub(crate) fn stable_process_input_fingerprint(bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    format!("fnv1a64:{hash:016x}")
}

pub(super) fn shell_like_process_input_from_argv(argv: &[String]) -> Option<ShellProcessInput<'_>> {
    #[cfg(windows)]
    {
        match argv {
            [shell, no_profile, non_interactive, flag, script]
                if matches!(executable_name(shell).as_str(), "powershell" | "pwsh")
                    && no_profile.eq_ignore_ascii_case("-NoProfile")
                    && non_interactive.eq_ignore_ascii_case("-NonInteractive")
                    && flag.eq_ignore_ascii_case("-Command") =>
            {
                Some(ShellProcessInput {
                    kind: ShellKind::PowerShell,
                    shell,
                    flag,
                    script,
                })
            }
            [shell, no_profile, flag, script]
                if matches!(executable_name(shell).as_str(), "powershell" | "pwsh")
                    && no_profile.eq_ignore_ascii_case("-NoProfile")
                    && flag.eq_ignore_ascii_case("-Command") =>
            {
                Some(ShellProcessInput {
                    kind: ShellKind::PowerShell,
                    shell,
                    flag,
                    script,
                })
            }
            [shell, flag, script]
                if matches!(executable_name(shell).as_str(), "bash" | "sh" | "zsh")
                    && matches!(flag.as_str(), "-c" | "-lc") =>
            {
                Some(ShellProcessInput {
                    kind: ShellKind::Bash,
                    shell,
                    flag,
                    script,
                })
            }
            [shell, flag, script]
                if executable_name(shell) == "cmd" && flag.eq_ignore_ascii_case("/C") =>
            {
                Some(ShellProcessInput {
                    kind: ShellKind::Cmd,
                    shell,
                    flag,
                    script,
                })
            }
            _ => None,
        }
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
            kind: ShellKind::Bash,
            shell,
            flag,
            script,
        })
    }
}

#[cfg(not(windows))]
pub(super) fn is_supported_shell_executable_name(shell: &str) -> bool {
    matches!(executable_name(shell).as_str(), "bash" | "sh" | "zsh")
}

/// AST-derived security summary of one parsed bash script.
pub(super) struct BashSecurityAnalysis {
    /// Every command node found anywhere in the AST (including inside control
    /// flow, subshells, and `$(...)`/backtick substitutions), in source order.
    pub(super) commands: Vec<Vec<String>>,
    /// True if any inline assignment, `export`/`declare`, or `env` invocation
    /// sets an execution-injecting environment variable or uses `env -S`.
    pub(super) env_injection: bool,
    /// True if the AST contains command or process substitution (`$(...)`,
    /// `` `...` ``, `<(...)`, `>(...)`).
    pub(super) has_command_substitution: bool,
    /// Non-comment command words and redirect destinations for host-path checks.
    pub(super) path_check_tokens: Vec<String>,
}

const INJECTION_ENV_KEYS: &[&str] = &[
    "LD_PRELOAD",
    "LD_AUDIT",
    "BASH_ENV",
    "ENV",
    "IFS",
    "PATH",
    "GIT_EXTERNAL_DIFF",
    "GIT_PROXY_COMMAND",
    "PROMPT_COMMAND",
];

const INJECTION_ENV_KEY_PREFIXES: &[&str] = &["DYLD_", "GIT_CONFIG"];

pub(super) fn is_injection_env_key(key: &str) -> bool {
    INJECTION_ENV_KEYS.contains(&key)
        || INJECTION_ENV_KEY_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

fn try_parse_bash(script: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&BASH_LANGUAGE.into()).ok()?;
    parser.parse(script, None)
}

/// Parses a strictly plain, expansion-free, redirect-free command sequence
/// joined only by `;`, `|`, `||`, or `&&` (used by the narrow SP3-A shell
/// read-only lane).
pub(super) fn parse_plain_shell_command_sequence(script: &str) -> Option<Vec<Vec<String>>> {
    let tree = try_parse_bash(script)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }

    const ALLOWED_KINDS: &[&str] = &[
        "program",
        "list",
        "pipeline",
        "command",
        "command_name",
        "word",
        "string",
        "string_content",
        "raw_string",
        "number",
        "concatenation",
    ];
    const ALLOWED_PUNCT: &[&str] = &["&&", "||", ";", "|", "\"", "'"];

    let mut stack = vec![root];
    let mut command_nodes = Vec::new();
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if node.is_named() {
            if !ALLOWED_KINDS.contains(&kind) {
                return None;
            }
            if kind == "command" {
                command_nodes.push(node);
            }
        } else if !(ALLOWED_PUNCT.contains(&kind) || kind.trim().is_empty()) {
            return None;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    if command_nodes.is_empty() {
        return None;
    }
    command_nodes.sort_by_key(Node::start_byte);

    let mut commands = Vec::with_capacity(command_nodes.len());
    for command_node in command_nodes {
        let mut words = Vec::new();
        let mut cursor = command_node.walk();
        for child in command_node.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }
            let word = decode_strict_plain_word(child, script)?;
            if word.is_empty() {
                return None;
            }
            words.push(word);
        }
        if words.is_empty() {
            return None;
        }
        commands.push(words);
    }
    Some(commands)
}

fn decode_strict_plain_word(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "command_name" => {
            let child = node.named_child(0)?;
            decode_strict_plain_word(child, src)
        }
        "word" | "number" => {
            let text = node.utf8_text(src.as_bytes()).ok()?;
            if text.chars().any(shell_word_character_is_disallowed) {
                return None;
            }
            Some(text.to_owned())
        }
        "raw_string" => {
            let text = node.utf8_text(src.as_bytes()).ok()?;
            text.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
                .map(str::to_owned)
        }
        "string" => {
            let mut out = String::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !child.is_named() {
                    continue;
                }
                if child.kind() != "string_content" {
                    return None;
                }
                let text = child.utf8_text(src.as_bytes()).ok()?;
                if text.chars().any(|ch| matches!(ch, '$' | '`' | '\\' | '!')) {
                    return None;
                }
                out.push_str(text);
            }
            Some(out)
        }
        "concatenation" => {
            let mut out = String::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    out.push_str(&decode_strict_plain_word(child, src)?);
                }
            }
            Some(out)
        }
        _ => None,
    }
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

/// Parses a bash script into an AST security summary. Returns `None` on syntax
/// errors (such as unclosed quotes) so callers fail closed.
pub(super) fn parse_bash_security_analysis(script: &str) -> Option<BashSecurityAnalysis> {
    let tree = try_parse_bash(script)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }

    let mut stack = vec![root];
    let mut command_nodes = Vec::new();
    let mut env_injection = false;
    let mut has_command_substitution = false;
    let mut path_check_tokens = Vec::new();

    while let Some(node) = stack.pop() {
        match node.kind() {
            "command" => command_nodes.push(node),
            "command_substitution" | "process_substitution" => {
                has_command_substitution = true;
            }
            "variable_assignment" => {
                if let Some(key) = variable_assignment_key(node, script)
                    && is_injection_env_key(key)
                {
                    env_injection = true;
                }
                if let Some(value_node) = node.child_by_field_name("value")
                    && let Ok(text) = value_node.utf8_text(script.as_bytes())
                {
                    path_check_tokens.push(text.to_owned());
                }
            }
            "file_redirect" => {
                if let Some(dest) = node.child_by_field_name("destination") {
                    path_check_tokens.push(decode_security_word(dest, script));
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    command_nodes.sort_by_key(Node::start_byte);

    let mut commands = Vec::with_capacity(command_nodes.len());
    for command_node in command_nodes {
        let mut words = Vec::new();
        let mut cursor = command_node.walk();
        for child in command_node.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }
            if matches!(
                child.kind(),
                "variable_assignment" | "file_redirect" | "heredoc_redirect" | "comment"
            ) {
                continue;
            }
            let decoded = decode_security_word(child, script);
            if !decoded.is_empty() {
                path_check_tokens.push(decoded.clone());
                words.push(decoded);
            }
        }
        if !words.is_empty() {
            if command_words_have_env_injection(&words) {
                env_injection = true;
            }
            commands.push(words);
        }
    }

    Some(BashSecurityAnalysis {
        commands,
        env_injection,
        has_command_substitution,
        path_check_tokens,
    })
}

fn variable_assignment_key<'a>(node: Node<'_>, src: &'a str) -> Option<&'a str> {
    if let Some(name_node) = node.child_by_field_name("name")
        && let Ok(text) = name_node.utf8_text(src.as_bytes())
    {
        return Some(text);
    }
    node.utf8_text(src.as_bytes())
        .ok()
        .and_then(|text| text.split('=').next())
}

/// Detects dangerous `env` invocations (`env -S`, `env LD_PRELOAD=...`, etc.)
/// even behind wrapper chains such as `timeout 5 env ...`.
pub(super) fn command_words_have_env_injection(words: &[String]) -> bool {
    let mut index = 0;
    while let Some(head) = words.get(index) {
        let name = executable_name(head);
        if name == "env" {
            let mut options_done = false;
            for arg in &words[index + 1..] {
                if !options_done && arg == "--" {
                    options_done = true;
                    continue;
                }
                if !options_done && arg.starts_with('-') {
                    // `-S` / `--split-string` rewrites argv and can smuggle commands.
                    if arg == "-S"
                        || arg.starts_with("-S")
                        || arg == "--split-string"
                        || arg.starts_with("--split-string=")
                        || (!arg.starts_with("--") && arg.contains('S'))
                    {
                        return true;
                    }
                    continue;
                }
                if let Some((key, _)) = arg.split_once('=') {
                    if is_injection_env_key(key) {
                        return true;
                    }
                    continue;
                }
                break;
            }
            return false;
        }
        if matches!(
            head.as_str(),
            "!" | "time" | "exec" | "command" | "builtin" | "nohup"
        ) {
            index += 1;
            continue;
        }
        if name == "timeout" {
            index += 1;
            while let Some(arg) = words.get(index) {
                if matches!(arg.as_str(), "-k" | "--kill-after" | "-s" | "--signal") {
                    index += 2;
                    continue;
                }
                if arg.starts_with('-') {
                    index += 1;
                    continue;
                }
                index += 1;
                break;
            }
            continue;
        }
        break;
    }
    false
}

/// Decodes a command word for security analysis. Expansion-free quotes are
/// unwrapped so `env "LD_PRELOAD=..."` or `"rm"` cannot hide behind quoting;
/// dynamic nodes keep their raw source text so `$VAR`/`$(...)` remain visible.
fn decode_security_word(node: Node<'_>, src: &str) -> String {
    match node.kind() {
        "command_name" => node
            .named_child(0)
            .map(|child| decode_security_word(child, src))
            .unwrap_or_default(),
        "raw_string" => node
            .utf8_text(src.as_bytes())
            .ok()
            .and_then(|text| {
                text.strip_prefix('\'')
                    .and_then(|rest| rest.strip_suffix('\''))
            })
            .unwrap_or_default()
            .to_owned(),
        "string" => {
            let mut out = String::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !child.is_named() {
                    continue;
                }
                if child.kind() == "string_content" {
                    if let Ok(text) = child.utf8_text(src.as_bytes()) {
                        out.push_str(text);
                    }
                } else if let Ok(text) = child.utf8_text(src.as_bytes()) {
                    out.push_str(text);
                }
            }
            out
        }
        "concatenation" => {
            let mut out = String::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    out.push_str(&decode_security_word(child, src));
                }
            }
            out
        }
        _ => node.utf8_text(src.as_bytes()).unwrap_or("").to_owned(),
    }
}

pub(super) fn executable_name(argument: &str) -> String {
    let base = argument
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argument)
        .to_ascii_lowercase();
    for suffix in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stripped) = base.strip_suffix(suffix)
            && !stripped.is_empty()
        {
            return stripped.to_owned();
        }
    }
    base
}

pub(super) fn executable_token_is(argument: &str, expected: &str) -> bool {
    #[cfg(windows)]
    {
        argument.eq_ignore_ascii_case(expected)
            || argument
                .strip_suffix(".exe")
                .or_else(|| argument.strip_suffix(".EXE"))
                .is_some_and(|stripped| stripped.eq_ignore_ascii_case(expected))
    }
    #[cfg(not(windows))]
    {
        argument == expected
    }
}
