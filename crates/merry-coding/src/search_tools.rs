//! Action-PATH probe for the search tools the coding prompt prefers.
//!
//! The prompt asks for modern search tools because their output is easier to
//! scope and bound than `grep -r` or `find`. Merry probes the PATH that
//! sandboxed actions inherit while it composes the coding profile, so the model
//! is told which of those tools this machine actually has instead of spending
//! an action on a binary that is not installed.

use merry_process::action_process_path;
use std::{ffi::OsStr, path::Path};

/// Program names probed on the action PATH, in prompt preference order.
const RIPGREP_PROGRAM: &str = "rg";
const FD_PROGRAMS: [&str; 2] = ["fd", "fdfind"];

/// Which preferred search tools the action PATH resolves to.
///
/// The fields are private so callers describe availability through
/// [`SearchToolAvailability::summary_line`] instead of re-deriving prompt text.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SearchToolAvailability {
    ripgrep: bool,
    fd: bool,
}

impl SearchToolAvailability {
    /// Probes `path` for the preferred search tools.
    ///
    /// `file_exists` decides whether one candidate is runnable, which keeps the
    /// probe deterministic in tests and lets callers reuse the host file check
    /// they already rely on.
    pub(crate) fn detect(path: &OsStr, mut file_exists: impl FnMut(&Path) -> bool) -> Self {
        Self {
            ripgrep: program_is_on_path(path, &[RIPGREP_PROGRAM], &mut file_exists),
            fd: program_is_on_path(path, &FD_PROGRAMS, &mut file_exists),
        }
    }

    /// Probes the PATH that sandboxed actions inherit from this process.
    pub(crate) fn probe_action_path() -> Self {
        Self::detect(&action_process_path(), is_executable_file)
    }

    /// Model-visible facts line appended to the workspace capability summary.
    ///
    /// The line always starts with the same marker so the prompt reports a
    /// definite answer for this host, including when no modern tool is present.
    pub(crate) fn summary_line(&self) -> String {
        self.summary_line_for_shell(
            merry_runtime::host_shell_name(),
            merry_runtime::host_shell_has_unix_utilities(),
            merry_runtime::host_shell_chain_separator(),
        )
    }

    pub(crate) fn summary_line_for_shell(
        &self,
        shell_name: &str,
        has_unix_utilities: bool,
        chain_separator: &str,
    ) -> String {
        if has_unix_utilities {
            return match (self.ripgrep, self.fd) {
                (true, true) => format!(
                    "Action PATH search tools: `{RIPGREP_PROGRAM}` and `fd` are installed; use them for content search and path lookup."
                ),
                (true, false) => format!(
                    "Action PATH search tools: `{RIPGREP_PROGRAM}` is installed; use it for content and file search. Neither `fd` nor `fdfind` is installed here, so use bounded `find` commands for path lookup by name."
                ),
                (false, true) => format!(
                    "Action PATH search tools: `fd` is installed; use it for path lookup by name. `{RIPGREP_PROGRAM}` is not installed here, so use bounded `grep -r` commands for content search."
                ),
                (false, false) => format!(
                    "Action PATH search tools: neither `{RIPGREP_PROGRAM}` nor `fd` is installed on the action PATH; use bounded `grep -r` and `find` commands and keep their output small."
                ),
            };
        }

        let shell_note = format!(
            "Host shell is `{shell_name}` (chain commands with `{chain_separator}`; Unix `grep`/`find`/`sed` are unavailable—use `read_file` for reading files and PowerShell cmdlets when needed)."
        );
        match (self.ripgrep, self.fd) {
            (true, true) => format!(
                "Action PATH search tools: `{RIPGREP_PROGRAM}` and `fd` are installed; use them for content search and path lookup. {shell_note}"
            ),
            (true, false) => format!(
                "Action PATH search tools: `{RIPGREP_PROGRAM}` is installed; use it for content and file search. Neither `fd` nor `fdfind` is installed here, so use `rg --files` or bounded `Get-ChildItem` for path lookup. {shell_note}"
            ),
            (false, true) => format!(
                "Action PATH search tools: `fd` is installed; use it for path lookup by name. `{RIPGREP_PROGRAM}` is not installed here, so use bounded `Select-String` commands for content search. {shell_note}"
            ),
            (false, false) => format!(
                "Action PATH search tools: neither `{RIPGREP_PROGRAM}` nor `fd` is installed on the action PATH; use bounded `Select-String` and `Get-ChildItem` commands and keep their output small. {shell_note}"
            ),
        }
    }
}

fn program_is_on_path(
    path: &OsStr,
    names: &[&str],
    file_exists: &mut impl FnMut(&Path) -> bool,
) -> bool {
    merry_core::host_paths::find_executable_in_path(path, names, file_exists).is_some()
}

/// Whether one PATH candidate is a file this process could execute.
fn is_executable_file(candidate: &Path) -> bool {
    let Ok(metadata) = candidate.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}
