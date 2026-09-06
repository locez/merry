//! Narrow shell-wrapper recognition and lexical normalization; policy stays in the classifier.

use crate::process::contracts::ProcessActionIntent;

/// Exact shell-wrapper input plus payload-free metadata helpers.
///
/// This value recognizes only the validated wrapper shape used by the current
/// shell read-only lane. It is not a shell parser and it does not authorize
/// execution by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShellProcessInput<'a> {
    pub(super) shell: &'a str,
    pub(super) flag: &'a str,
    pub(super) script: &'a str,
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

pub(super) fn is_supported_plain_shell_token(shell: &str) -> bool {
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

pub(super) fn parse_plain_shell_command_sequence(script: &str) -> Option<Vec<Vec<String>>> {
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

pub(super) fn skip_shell_whitespace(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while chars
        .peek()
        .is_some_and(|character| character.is_whitespace())
    {
        chars.next();
    }
}

pub(super) fn parse_shell_sequence_operator(
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

pub(super) fn parse_plain_shell_word(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<String> {
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

pub(super) fn parse_plain_single_quoted_shell_fragment(
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

pub(super) fn parse_plain_double_quoted_shell_fragment(
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

pub(super) fn is_shell_sequence_operator_start(character: char) -> bool {
    matches!(character, ';' | '|' | '&')
}

pub(super) fn shell_word_character_is_disallowed(character: char) -> bool {
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

pub(super) fn shell_like_process_input_from_argv(argv: &[String]) -> Option<ShellProcessInput<'_>> {
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

pub(super) fn is_supported_shell_executable_name(shell: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(executable_name(shell).as_str(), "powershell" | "pwsh")
    }

    #[cfg(not(windows))]
    matches!(executable_name(shell).as_str(), "bash" | "sh" | "zsh")
}

pub(super) fn shell_command_without_assignment_prefix(command: &[String]) -> &[String] {
    let executable_index = command
        .iter()
        .position(|word| !is_plain_shell_assignment_word(word))
        .unwrap_or(command.len());
    &command[executable_index..]
}

pub(super) fn is_plain_shell_assignment_word(word: &str) -> bool {
    let Some((name, value)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !value.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        })
}

pub(super) fn rough_shell_words(script: &str) -> Vec<String> {
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

pub(super) fn executable_name(argument: &str) -> String {
    argument
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argument)
        .to_ascii_lowercase()
}

pub(super) fn executable_token_is(argument: &str, expected: &str) -> bool {
    argument == expected
}
