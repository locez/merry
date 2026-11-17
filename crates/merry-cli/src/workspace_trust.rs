//! First-open choice for whether a workspace sandbox may write a directory.
//!
//! The prompt runs only on a product surface, and only the first time that
//! directory is opened. A Git checkout is remembered at the repository root.
//! `--sandbox off` does not ask and does not read the saved choice.

use crate::cli_error::{CliError, unexpected};
use crate::coding::SandboxMode;
use crate::config::XdgPaths;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

const TRUST_FILE_NAME: &str = "workspace-trust.json";

/// Command-line choice for [`resolve_workspace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum TrustWorkspace {
    /// The directory is readable and writable, and `AGENTS.md` is loaded.
    Yes,
    /// The directory is read-only. The sandbox stays on.
    No,
}

impl TrustWorkspace {
    const fn writable(self) -> bool {
        matches!(self, Self::Yes)
    }

    const fn access(self) -> TrustAccess {
        match self {
            Self::Yes => TrustAccess::ReadWrite,
            Self::No => TrustAccess::ReadOnly,
        }
    }
}

/// Directory the session and the sandbox use, plus whether it is writable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedWorkspace {
    pub(crate) root: PathBuf,
    pub(crate) writable: bool,
}

/// Resolves the current directory for a product launch.
///
/// Workspace mode asks on `/dev/tty` when there is no saved choice and no
/// explicit flag. There is no prompt when the process has no terminal.
pub(crate) fn resolve_product_workspace(
    sandbox: SandboxMode,
    explicit: Option<TrustWorkspace>,
    paths: &XdgPaths,
) -> Result<ResolvedWorkspace, CliError> {
    let cwd = std::env::current_dir().map_err(unexpected)?;
    resolve_workspace(cwd, sandbox, explicit, paths.state_dir(), prompt_on_tty)
}

/// Resolves `cwd` and remembers the choice under `state_dir`.
///
/// `prompt` is called only when the sandbox is on, the flag is absent, and
/// this directory has no saved choice. Both answers are saved.
pub(crate) fn resolve_workspace(
    cwd: PathBuf,
    sandbox: SandboxMode,
    explicit: Option<TrustWorkspace>,
    state_dir: &Path,
    mut prompt: impl FnMut(&Path) -> Result<TrustWorkspace, CliError>,
) -> Result<ResolvedWorkspace, CliError> {
    if !sandbox.uses_action_sandbox() {
        return Ok(ResolvedWorkspace {
            root: cwd,
            writable: true,
        });
    }
    let root = trust_root(&cwd)?;
    let store_path = state_dir.join(TRUST_FILE_NAME);
    if let Some(choice) = explicit {
        remember(&store_path, &root, choice)?;
        return Ok(ResolvedWorkspace {
            root,
            writable: choice.writable(),
        });
    }
    let stored = load_store(&store_path)?;
    if let Some(choice) = stored_choice(&stored, &root) {
        return Ok(ResolvedWorkspace {
            root,
            writable: choice.writable(),
        });
    }
    let choice = prompt(&root)?;
    remember(&store_path, &root, choice)?;
    Ok(ResolvedWorkspace {
        root,
        writable: choice.writable(),
    })
}

fn trust_root(cwd: &Path) -> Result<PathBuf, CliError> {
    let start = cwd.canonicalize().map_err(|error| {
        unexpected(format!(
            "could not resolve the workspace directory {}: {error}",
            cwd.display()
        ))
    })?;
    let mut current = start.clone();
    loop {
        let git = current.join(".git");
        if git.is_dir() || git.is_file() {
            return Ok(current);
        }
        if !current.pop() {
            return Ok(start);
        }
    }
}

fn prompt_on_tty(root: &Path) -> Result<TrustWorkspace, CliError> {
    let tty = File::options()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| trust_prompt_unavailable(root))?;
    let mut tty = BufReader::new(tty);
    let text = prompt_text(root);
    loop {
        let file = tty.get_mut();
        file.write_all(text.as_bytes()).map_err(unexpected)?;
        file.flush().map_err(unexpected)?;
        let mut line = String::new();
        let read = tty.read_line(&mut line).map_err(unexpected)?;
        if read == 0 {
            return Err(trust_prompt_unavailable(root));
        }
        if let Some(choice) = parse_trust_answer(&line) {
            return Ok(choice);
        }
        writeln!(tty.get_mut(), "Answer y or n.").map_err(unexpected)?;
    }
}

fn prompt_text(root: &Path) -> String {
    let git = root.join(".git");
    let scope = if git.is_dir() || git.is_file() {
        "This is the Git repository root, so the choice covers the whole repository."
    } else {
        "This directory is not inside a Git repository."
    };
    format!(
        "\nMerry has not opened this directory before.\n\n  {path}\n\n{scope}\nYes makes it readable and writable and loads AGENTS.md as project instructions. No keeps it read-only. The sandbox stays either way. Toolchains, the Cargo registry, cache, and /tmp stay available. Network still needs review for each command.\n\nAllow read-write access to this directory? [y/N] ",
        path = root.display()
    )
}

fn trust_prompt_unavailable(root: &Path) -> CliError {
    unexpected(format!(
        "no saved choice for `{}` and no terminal to ask. Pass --trust-workspace yes or --trust-workspace no. Choices are saved in $XDG_STATE_HOME/merry/{TRUST_FILE_NAME}.",
        root.display()
    ))
}

fn parse_trust_answer(line: &str) -> Option<TrustWorkspace> {
    match line.trim().to_ascii_lowercase().as_str() {
        "" | "n" | "no" => Some(TrustWorkspace::No),
        "y" | "yes" => Some(TrustWorkspace::Yes),
        _ => None,
    }
}

fn stored_choice(file: &TrustFile, root: &Path) -> Option<TrustWorkspace> {
    file.directories.iter().find_map(|entry| {
        (entry.path == root).then_some(match entry.access {
            TrustAccess::ReadWrite => TrustWorkspace::Yes,
            TrustAccess::ReadOnly => TrustWorkspace::No,
        })
    })
}

fn remember(path: &Path, root: &Path, choice: TrustWorkspace) -> Result<(), CliError> {
    let mut file = load_store(path)?;
    if let Some(entry) = file.directories.iter_mut().find(|entry| entry.path == root) {
        entry.access = choice.access();
    } else {
        file.directories.push(TrustEntry {
            path: root.to_path_buf(),
            access: choice.access(),
        });
    }
    write_store(path, &file)
}

fn load_store(path: &Path) -> Result<TrustFile, CliError> {
    if !path.exists() {
        return Ok(TrustFile {
            directories: Vec::new(),
        });
    }
    let bytes = fs::read(path).map_err(|error| {
        unexpected(format!(
            "could not read the workspace trust file {}: {error}",
            path.display()
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        unexpected(format!(
            "could not read the workspace trust file {}: {error}. Fix that file or remove it.",
            path.display()
        ))
    })
}

fn write_store(path: &Path, file: &TrustFile) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            unexpected(format!(
                "could not create the workspace trust directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&temporary).map_err(|error| {
        unexpected(format!(
            "could not write the workspace trust file {}: {error}",
            temporary.display()
        ))
    })?;
    let bytes = serde_json::to_vec_pretty(file).map_err(|error| {
        unexpected(format!(
            "could not encode the workspace trust file: {error}"
        ))
    })?;
    output.write_all(&bytes).map_err(unexpected)?;
    output.write_all(b"\n").map_err(unexpected)?;
    output.sync_all().map_err(unexpected)?;
    fs::rename(&temporary, path).map_err(|error| {
        unexpected(format!(
            "could not save the workspace trust file {}: {error}",
            path.display()
        ))
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    directories: Vec<TrustEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustEntry {
    path: PathBuf,
    access: TrustAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum TrustAccess {
    ReadWrite,
    ReadOnly,
}

#[cfg(test)]
mod tests {
    use super::{TrustWorkspace, parse_trust_answer, resolve_workspace, trust_prompt_unavailable};
    use crate::cli_error::CliError;
    use crate::coding::SandboxMode;
    use std::fs;
    use std::path::Path;

    #[test]
    fn empty_and_no_are_read_only_and_yes_is_read_write() {
        assert_eq!(parse_trust_answer(" \n"), Some(TrustWorkspace::No));
        assert_eq!(parse_trust_answer("N"), Some(TrustWorkspace::No));
        assert_eq!(parse_trust_answer("no"), Some(TrustWorkspace::No));
        assert_eq!(parse_trust_answer("y"), Some(TrustWorkspace::Yes));
        assert_eq!(parse_trust_answer("YES"), Some(TrustWorkspace::Yes));
        assert_eq!(parse_trust_answer("maybe"), None);
    }

    #[test]
    fn missing_terminal_names_the_flag_and_the_saved_file() {
        let CliError::Unexpected(message) = trust_prompt_unavailable(Path::new("/work/repo"))
        else {
            panic!("missing terminal is an unexpected error");
        };
        assert!(message.contains("--trust-workspace yes"));
        assert!(message.contains("--trust-workspace no"));
        assert!(message.contains("workspace-trust.json"));
        assert!(message.contains("/work/repo"));
    }

    #[test]
    fn sandbox_off_keeps_the_launch_directory_and_skips_the_store() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path().join("project");
        fs::create_dir_all(&cwd).expect("mkdir");
        let state = temp.path().join("state");
        let resolved = resolve_workspace(
            cwd.clone(),
            SandboxMode::Off,
            Some(TrustWorkspace::No),
            &state,
            |_| panic!("sandbox off does not prompt"),
        )
        .expect("resolve");
        assert_eq!(resolved.root, cwd);
        assert!(resolved.writable);
        assert!(!state.join("workspace-trust.json").exists());
    }

    #[test]
    fn explicit_yes_remembers_the_git_root_not_the_subdirectory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let sub = repo.join("crates").join("merry-cli");
        fs::create_dir_all(repo.join(".git")).expect("git dir");
        fs::create_dir_all(&sub).expect("subdir");
        let state = temp.path().join("state");
        let resolved = resolve_workspace(
            sub,
            SandboxMode::Workspace,
            Some(TrustWorkspace::Yes),
            &state,
            |_| panic!("an explicit choice does not prompt"),
        )
        .expect("resolve");
        let repo = repo.canonicalize().expect("canonical repo");
        assert_eq!(resolved.root, repo);
        assert!(resolved.writable);

        let mut prompted = false;
        let again = resolve_workspace(
            temp.path().join("repo").join("crates"),
            SandboxMode::Workspace,
            None,
            &state,
            |_| {
                prompted = true;
                Ok(TrustWorkspace::No)
            },
        )
        .expect("saved choice");
        assert!(!prompted);
        assert_eq!(again.root, repo);
        assert!(again.writable);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(state.join("workspace-trust.json"))
                .expect("trust file")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn a_prompted_no_is_remembered_as_read_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path().join("loose");
        fs::create_dir_all(&cwd).expect("mkdir");
        let state = temp.path().join("state");
        let resolved = resolve_workspace(cwd, SandboxMode::Workspace, None, &state, |_| {
            Ok(TrustWorkspace::No)
        })
        .expect("prompt");
        assert!(!resolved.writable);
        let again = resolve_workspace(
            temp.path().join("loose"),
            SandboxMode::Workspace,
            None,
            &state,
            |_| panic!("the saved no is reused"),
        )
        .expect("saved");
        assert_eq!(again.root, resolved.root);
        assert!(!again.writable);
    }

    #[test]
    fn a_broken_trust_file_is_not_replaced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path().join("project");
        fs::create_dir_all(&cwd).expect("mkdir");
        let state = temp.path().join("state");
        fs::create_dir_all(&state).expect("state");
        fs::write(state.join("workspace-trust.json"), b"{").expect("corrupt");
        let error = resolve_workspace(
            cwd,
            SandboxMode::Workspace,
            Some(TrustWorkspace::Yes),
            &state,
            |_| panic!("a corrupt file fails before the prompt"),
        )
        .expect_err("corrupt store");
        let CliError::Unexpected(message) = error else {
            panic!("corrupt store is unexpected");
        };
        assert!(message.contains("workspace-trust.json"), "{message}");
        assert_eq!(fs::read(state.join("workspace-trust.json")).unwrap(), b"{");
    }
}
