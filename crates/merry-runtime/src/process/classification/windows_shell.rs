//! Windows shell detection, invocation, and PowerShell security analysis.
//!
//! On Windows, Merry prefers PowerShell (`pwsh`, then `powershell.exe`) over
//! Git Bash because MSYS2 path conversion mangles `/flag` arguments passed to
//! native Windows toolchains (`MSBuild /t:Build`, `cl.exe /nologo`). Users can
//! override auto-detection via `MERRY_SHELL=pwsh|powershell|bash|cmd`.

#[cfg(windows)]
use std::{env, sync::OnceLock};
#[cfg(any(windows, test))]
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

/// Detected Windows shell backend and its command-invocation shape.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WindowsShell {
    /// PowerShell 7+ (`pwsh`).
    Pwsh,
    /// Windows PowerShell 5.1 (`powershell.exe`).
    PowerShell,
    /// Git for Windows Bash (`...\Git\bin\bash.exe`).
    GitBash(PathBuf),
    /// Windows Command Prompt (`cmd.exe`).
    Cmd,
}

#[cfg(any(windows, test))]
impl WindowsShell {
    /// Short human- and prompt-facing shell name.
    #[cfg(windows)]
    #[must_use]
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Pwsh => "pwsh",
            Self::PowerShell => "powershell",
            Self::GitBash(_) => "bash",
            Self::Cmd => "cmd.exe",
        }
    }

    /// Whether this shell supports `&&` for error-propagating command chaining.
    ///
    /// `pwsh` (PowerShell 7+) and Git Bash support `&&`; Windows PowerShell 5.1
    /// does not and requires `;`.
    #[cfg(windows)]
    #[must_use]
    pub(crate) const fn supports_chain_operator(&self) -> bool {
        matches!(self, Self::Pwsh | Self::GitBash(_))
    }

    /// Whether Unix coreutils (`grep`, `find`, `sed`, `head`, `tail`) are
    /// expected to be usable directly inside this shell.
    #[cfg(windows)]
    #[must_use]
    pub(crate) const fn has_unix_utilities(&self) -> bool {
        matches!(self, Self::GitBash(_))
    }

    /// Builds the process argv that executes `command` in this Windows shell.
    #[must_use]
    pub(crate) fn command_argv(&self, command: &str) -> Vec<String> {
        match self {
            Self::Pwsh => vec![
                "pwsh".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                command.to_owned(),
            ],
            Self::PowerShell => vec![
                "powershell.exe".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                command.to_owned(),
            ],
            Self::GitBash(path) => vec![
                path.to_string_lossy().into_owned(),
                "-c".to_owned(),
                command.to_owned(),
            ],
            Self::Cmd => vec!["cmd.exe".to_owned(), "/C".to_owned(), command.to_owned()],
        }
    }
}

/// Detects the active Windows shell once per process.
#[cfg(windows)]
#[must_use]
pub(crate) fn detect_windows_shell() -> &'static WindowsShell {
    static CACHED: OnceLock<WindowsShell> = OnceLock::new();
    CACHED.get_or_init(|| {
        let override_env = env::var("MERRY_SHELL").ok();
        let path_env = env::var_os("PATH");
        let pathext = env::var_os("PATHEXT");
        let git_bash_candidates = default_git_bash_candidates();
        detect_windows_shell_with(
            override_env.as_deref(),
            path_env.as_deref(),
            pathext.as_deref(),
            &git_bash_candidates,
            Path::is_file,
        )
    })
}

#[cfg(windows)]
fn default_git_bash_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for (var, suffix) in [
        ("PROGRAMFILES", r"Git\bin\bash.exe"),
        ("PROGRAMFILES(X86)", r"Git\bin\bash.exe"),
        ("LOCALAPPDATA", r"Programs\Git\bin\bash.exe"),
    ] {
        if let Some(root) = env::var_os(var) {
            candidates.push(PathBuf::from(root).join(suffix));
        }
    }
    candidates
}

/// Pure, deterministic Windows shell detection cascade for production and tests.
#[cfg(any(windows, test))]
pub(crate) fn detect_windows_shell_with(
    merry_shell_override: Option<&str>,
    path_env: Option<&OsStr>,
    pathext: Option<&OsStr>,
    git_bash_candidates: &[PathBuf],
    mut is_file: impl FnMut(&Path) -> bool,
) -> WindowsShell {
    let find_git_bash = |is_file: &mut dyn FnMut(&Path) -> bool| -> Option<PathBuf> {
        for candidate in git_bash_candidates {
            if is_file(candidate) {
                return Some(candidate.clone());
            }
        }
        let path_env = path_env?;
        merry_core::host_paths::find_executable_in_path_for(
            path_env,
            &["bash"],
            pathext,
            true,
            |candidate| {
                candidate
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains("git")
                    && is_file(candidate)
            },
        )
    };

    if let Some(value) = merry_shell_override {
        match value.trim().to_ascii_lowercase().as_str() {
            "pwsh" => return WindowsShell::Pwsh,
            "powershell" | "powershell.exe" => return WindowsShell::PowerShell,
            "bash" | "gitbash" | "git-bash" => {
                if let Some(path) = find_git_bash(&mut is_file) {
                    return WindowsShell::GitBash(path);
                }
            }
            "cmd" | "cmd.exe" => return WindowsShell::Cmd,
            _ => {}
        }
    }

    if let Some(path_env) = path_env
        && merry_core::host_paths::find_executable_in_path_for(
            path_env,
            &["pwsh"],
            pathext,
            true,
            &mut is_file,
        )
        .is_some()
    {
        return WindowsShell::Pwsh;
    }

    let system_powershell = Path::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
    if is_file(system_powershell)
        || path_env.is_some_and(|path_env| {
            merry_core::host_paths::find_executable_in_path_for(
                path_env,
                &["powershell"],
                pathext,
                true,
                &mut is_file,
            )
            .is_some()
        })
    {
        return WindowsShell::PowerShell;
    }

    if let Some(path) = find_git_bash(&mut is_file) {
        return WindowsShell::GitBash(path);
    }

    WindowsShell::PowerShell
}

/// Returns the short name of the active host shell (`"bash"` on Unix;
/// `"pwsh"`, `"powershell"`, `"bash"`, or `"cmd.exe"` on Windows).
#[must_use]
pub fn host_shell_name() -> &'static str {
    #[cfg(windows)]
    {
        detect_windows_shell().name()
    }
    #[cfg(not(windows))]
    {
        "bash"
    }
}

/// Returns whether Unix text/search utilities (`grep`, `find`, `sed`, `head`,
/// `tail`) are expected to be available in the active host shell.
#[must_use]
pub fn host_shell_has_unix_utilities() -> bool {
    #[cfg(windows)]
    {
        detect_windows_shell().has_unix_utilities()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Returns the error-propagating command chain separator for the active host
/// shell (`"&&"` on Unix, `pwsh`, and Git Bash; `";"` on Windows PowerShell
/// 5.1 and `cmd.exe`).
#[must_use]
pub fn host_shell_chain_separator() -> &'static str {
    #[cfg(windows)]
    {
        if detect_windows_shell().supports_chain_operator() {
            "&&"
        } else {
            ";"
        }
    }
    #[cfg(not(windows))]
    {
        "&&"
    }
}

/// Security summary of one parsed PowerShell command script.
#[cfg(any(windows, test))]
pub(crate) struct PowerShellSecurityAnalysis {
    /// Commands split by `;`, `|`, `&&`, `||`, or newlines, in source order.
    pub(crate) commands: Vec<Vec<String>>,
    /// True if the script mutates execution-affecting environment variables
    /// (`$env:PATH`, `$env:PATHEXT`, `$env:GIT_CONFIG*`, etc.) or invokes
    /// PowerShell dynamic execution (`iex`, `-EncodedCommand`, `&`, `.`).
    pub(crate) env_or_eval_injection: bool,
    /// True if the script contains subexpression/variable expansion (`$(...)`)
    /// or redirections that could hide host paths.
    pub(crate) has_command_substitution: bool,
    /// True if the script invokes a destructive PowerShell delete cmdlet/alias
    /// (`Remove-Item`, `ri`, `del`, `erase`, `rd`, `rmdir`) with recursive or
    /// broad targets.
    pub(crate) has_destructive_delete: bool,
    /// Non-comment command words for host-path review checks.
    pub(crate) path_check_tokens: Vec<String>,
}

/// Parses a plain, expansion-free PowerShell command sequence joined by `;`,
/// `|`, `&&`, or `||` for the read-only shell lane.
#[cfg(any(windows, test))]
pub(crate) fn parse_plain_powershell_command_sequence(script: &str) -> Option<Vec<Vec<String>>> {
    let analysis = analyze_powershell_script(script)?;
    if analysis.env_or_eval_injection
        || analysis.has_command_substitution
        || analysis.has_destructive_delete
        || analysis.commands.is_empty()
    {
        return None;
    }
    // Reject redirections or shell expansions in the strict read-only lane.
    for command in &analysis.commands {
        for word in command {
            if word.chars().any(|ch| {
                matches!(
                    ch,
                    '$' | '`' | '>' | '<' | '(' | ')' | '{' | '}' | '*' | '?'
                )
            }) {
                return None;
            }
        }
    }
    Some(analysis.commands)
}

/// Performs a fail-closed lexical/structural security analysis of a PowerShell
/// script. Returns `None` on unclosed quotes or block comments so callers fail
/// closed.
#[cfg(any(windows, test))]
pub(crate) fn analyze_powershell_script(script: &str) -> Option<PowerShellSecurityAnalysis> {
    let mut commands: Vec<Vec<String>> = Vec::new();
    let mut current_cmd: Vec<String> = Vec::new();
    let mut current_word = String::new();
    let mut has_word = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut env_or_eval_injection = false;
    let mut has_command_substitution = false;
    let mut path_check_tokens = Vec::new();

    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;

    let flush_word = |current_word: &mut String,
                      has_word: &mut bool,
                      current_cmd: &mut Vec<String>,
                      path_check_tokens: &mut Vec<String>| {
        if *has_word {
            let word = std::mem::take(current_word);
            if !word.is_empty() {
                path_check_tokens.push(word.clone());
                current_cmd.push(word);
            }
            *has_word = false;
        }
    };

    let flush_cmd = |current_cmd: &mut Vec<String>, commands: &mut Vec<Vec<String>>| {
        if !current_cmd.is_empty() {
            commands.push(std::mem::take(current_cmd));
        }
    };

    while i < chars.len() {
        let ch = chars[i];
        let next = chars.get(i + 1).copied();

        if in_single {
            if ch == '\'' {
                if next == Some('\'') {
                    current_word.push('\'');
                    i += 2;
                    continue;
                }
                in_single = false;
                i += 1;
                continue;
            }
            current_word.push(ch);
            has_word = true;
            i += 1;
            continue;
        }

        if in_double {
            if ch == '`' {
                let escaped = next?;
                current_word.push(escaped);
                has_word = true;
                i += 2;
                continue;
            }
            if ch == '$' && next == Some('(') {
                has_command_substitution = true;
            }
            if ch == '"' {
                in_double = false;
                i += 1;
                continue;
            }
            current_word.push(ch);
            has_word = true;
            i += 1;
            continue;
        }

        // Unquoted context
        if ch == '<' && next == Some('#') {
            // PowerShell block comment `<# ... #>`
            i += 2;
            let mut closed = false;
            while i + 1 < chars.len() {
                if chars[i] == '#' && chars[i + 1] == '>' {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                return None;
            }
            continue;
        }

        if ch == '#' && !has_word {
            // Line comment until newline.
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        if ch == '\'' {
            in_single = true;
            has_word = true;
            i += 1;
            continue;
        }

        if ch == '"' {
            in_double = true;
            has_word = true;
            i += 1;
            continue;
        }

        if ch == '`' {
            // Unquoted backtick escapes in PowerShell are a common obfuscation
            // vector (`I`EX, `r`m); fail closed as dynamic/eval injection.
            env_or_eval_injection = true;
            let escaped = next?;
            if escaped != '\n' && escaped != '\r' {
                current_word.push(escaped);
                has_word = true;
            }
            i += 2;
            continue;
        }

        if ch == '$' && next == Some('(') {
            has_command_substitution = true;
            current_word.push(ch);
            has_word = true;
            i += 1;
            continue;
        }

        // Command separators: `;`, `\n`, `\r`, `|`, `||`, `&&`
        if matches!(ch, ';' | '\n' | '\r') {
            flush_word(
                &mut current_word,
                &mut has_word,
                &mut current_cmd,
                &mut path_check_tokens,
            );
            flush_cmd(&mut current_cmd, &mut commands);
            i += 1;
            continue;
        }

        if ch == '|' {
            flush_word(
                &mut current_word,
                &mut has_word,
                &mut current_cmd,
                &mut path_check_tokens,
            );
            flush_cmd(&mut current_cmd, &mut commands);
            i += if next == Some('|') { 2 } else { 1 };
            continue;
        }

        if ch == '&' && next == Some('&') {
            flush_word(
                &mut current_word,
                &mut has_word,
                &mut current_cmd,
                &mut path_check_tokens,
            );
            flush_cmd(&mut current_cmd, &mut commands);
            i += 2;
            continue;
        }

        if ch.is_whitespace() {
            flush_word(
                &mut current_word,
                &mut has_word,
                &mut current_cmd,
                &mut path_check_tokens,
            );
            i += 1;
            continue;
        }

        current_word.push(ch);
        has_word = true;
        i += 1;
    }

    if in_single || in_double {
        return None;
    }

    flush_word(
        &mut current_word,
        &mut has_word,
        &mut current_cmd,
        &mut path_check_tokens,
    );
    flush_cmd(&mut current_cmd, &mut commands);

    let mut has_destructive_delete = false;
    for cmd in &commands {
        if powershell_command_has_env_or_eval_injection(cmd) {
            env_or_eval_injection = true;
        }
        if powershell_command_is_destructive_delete(cmd) {
            has_destructive_delete = true;
        }
    }

    Some(PowerShellSecurityAnalysis {
        commands,
        env_or_eval_injection,
        has_command_substitution,
        has_destructive_delete,
        path_check_tokens,
    })
}

#[cfg(any(windows, test))]
fn powershell_command_has_env_or_eval_injection(words: &[String]) -> bool {
    let Some(head_raw) = words.first() else {
        return false;
    };
    let head = head_raw.to_ascii_lowercase();
    let normalized = super::shell::executable_name(head_raw);

    // Call operator `&` and dot-sourcing `.` execute arbitrary commands/scripts.
    if matches!(head.as_str(), "&" | ".") {
        return true;
    }

    // Direct `$env:KEY = ...` or `$env:KEY=...` assignment.
    if let Some(rest) = head.strip_prefix("$env:") {
        let key = rest.split('=').next().unwrap_or(rest);
        let assigns =
            rest.contains('=') || words.get(1).is_some_and(|w| w == "=" || w.starts_with('='));
        if assigns && is_windows_injection_env_key(key) {
            return true;
        }
    }

    // PowerShell dynamic execution & process launch cmdlets/aliases.
    if matches!(
        normalized.as_str(),
        "invoke-expression"
            | "iex"
            | "invoke-command"
            | "icm"
            | "start-process"
            | "saps"
            | "start"
            | "wsl"
            | "runas"
    ) {
        return true;
    }

    // `Set-Item Env:PATH ...` / `New-Item Env:PATH ...`
    if matches!(head.as_str(), "set-item" | "si" | "new-item" | "ni")
        && words.iter().skip(1).any(|arg| {
            let lower = arg.to_ascii_lowercase();
            lower
                .strip_prefix("env:\\")
                .or_else(|| lower.strip_prefix("env:"))
                .is_some_and(is_windows_injection_env_key)
        })
    {
        return true;
    }

    // Encoded PowerShell flags anywhere in the invocation.
    if words.iter().skip(1).any(|arg| {
        let lower = arg.to_ascii_lowercase();
        matches!(
            lower.as_str(),
            "-enc" | "-ec" | "-e" | "-encodedcommand" | "/enc"
        ) || lower.starts_with("-encoded")
    }) {
        return true;
    }

    false
}

#[cfg(any(windows, test))]
fn is_windows_injection_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "PATH"
            | "PATHEXT"
            | "COMSPEC"
            | "PSMODULEPATH"
            | "LD_PRELOAD"
            | "LD_AUDIT"
            | "BASH_ENV"
            | "ENV"
            | "IFS"
            | "GIT_EXTERNAL_DIFF"
            | "GIT_PROXY_COMMAND"
            | "PROMPT_COMMAND"
    ) || upper.starts_with("DYLD_")
        || upper.starts_with("GIT_CONFIG")
}

#[cfg(any(windows, test))]
fn powershell_command_is_destructive_delete(words: &[String]) -> bool {
    let Some(head) = words.first() else {
        return false;
    };
    let normalized = super::shell::executable_name(head);
    if !matches!(
        normalized.as_str(),
        "remove-item" | "ri" | "del" | "erase" | "rd" | "rmdir"
    ) {
        return false;
    }
    let mut operands = Vec::new();
    for arg in words.iter().skip(1) {
        let lower = arg.to_ascii_lowercase();
        if lower.starts_with('-') || lower.starts_with('/') {
            if matches!(
                lower.as_str(),
                "-recurse" | "-r" | "-rec" | "-rf" | "/s" | "/q"
            ) || (lower.starts_with("-rec") && "-recurse".starts_with(&lower))
            {
                return true;
            }
            continue;
        }
        operands.push(arg.as_str());
    }
    operands.is_empty()
        || operands.iter().any(|op| {
            op.is_empty()
                || matches!(*op, "." | ".." | "/" | "\\" | "*" | "~")
                || op.ends_with("/*")
                || op.ends_with("\\*")
                || op
                    .chars()
                    .any(|ch| matches!(ch, '*' | '?' | '[' | '{' | '$' | '`'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, ffi::OsString};

    #[test]
    fn windows_shell_cascade_prefers_pwsh_then_powershell_then_git_bash() {
        let tools_dir = Path::new("/tools");
        let pwsh_exe = tools_dir.join("pwsh.exe");
        let path_env = env::join_paths([tools_dir]).expect("join paths");
        let pathext = OsString::from(".EXE;.CMD");
        let git_bash = vec![PathBuf::from(r"C:\Program Files\Git\bin\bash.exe")];

        assert_eq!(
            detect_windows_shell_with(None, Some(&path_env), Some(&pathext), &git_bash, |p| p
                == pwsh_exe
                || p == git_bash[0],),
            WindowsShell::Pwsh
        );

        assert_eq!(
            detect_windows_shell_with(None, Some(&path_env), Some(&pathext), &git_bash, |p| p
                == Path::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
                || p == git_bash[0],),
            WindowsShell::PowerShell
        );

        assert_eq!(
            detect_windows_shell_with(None, Some(&path_env), Some(&pathext), &git_bash, |p| p
                == git_bash[0],),
            WindowsShell::GitBash(git_bash[0].clone())
        );

        assert_eq!(
            detect_windows_shell_with(
                Some("bash"),
                Some(&path_env),
                Some(&pathext),
                &git_bash,
                |p| p == pwsh_exe || p == git_bash[0],
            ),
            WindowsShell::GitBash(git_bash[0].clone())
        );

        assert_eq!(
            WindowsShell::Pwsh.command_argv("cargo test"),
            vec![
                "pwsh",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "cargo test"
            ]
        );
        assert_eq!(
            WindowsShell::PowerShell.command_argv("cargo test"),
            vec![
                "powershell.exe",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "cargo test"
            ]
        );
        assert_eq!(
            WindowsShell::Cmd.command_argv("dir"),
            vec!["cmd.exe", "/C", "dir"]
        );
    }

    #[test]
    fn powershell_analysis_accepts_plain_commands_and_flags_dangerous_ones() {
        let safe = analyze_powershell_script(r"rg foo src\main.rs | Select-String bar")
            .expect("safe script parses");
        assert!(!safe.env_or_eval_injection);
        assert!(!safe.has_command_substitution);
        assert!(!safe.has_destructive_delete);
        assert_eq!(safe.commands.len(), 2);
        assert!(safe.path_check_tokens.iter().any(|t| t == r"src\main.rs"));

        let iex = analyze_powershell_script("Invoke-Expression $payload").expect("parses");
        assert!(iex.env_or_eval_injection);

        let env_hijack =
            analyze_powershell_script(r"$env:PATH = 'C:\evil;' + $env:PATH; cargo test")
                .expect("parses");
        assert!(env_hijack.env_or_eval_injection);

        let delete_tree =
            analyze_powershell_script(r"Remove-Item -Recurse -Force target").expect("parses");
        assert!(delete_tree.has_destructive_delete);

        let subexpr =
            analyze_powershell_script("echo \"$(Get-Content C:\\secret.txt)\"").expect("parses");
        assert!(subexpr.has_command_substitution);

        assert!(analyze_powershell_script("echo 'unclosed").is_none());
        let plain =
            parse_plain_powershell_command_sequence(r"Get-Content src\lib.rs | Select-String foo")
                .expect("plain sequence");
        assert_eq!(plain.len(), 2);
    }
}
