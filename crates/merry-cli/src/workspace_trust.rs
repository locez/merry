//! First-open choice for whether a workspace sandbox may write a directory.
//!
//! The prompt runs only on a product surface, and only when that directory has
//! no saved choice. It is a centered dialog: read-write, read-only, a separate
//! "don't ask again" toggle, and exit. The highlight starts on read-only and
//! the toggle starts at no, so confirming applies only to this launch. Turning
//! the toggle on saves that choice. Exit closes without opening and does not
//! write the store. A Git checkout is remembered at the repository root.
//! `--sandbox off` does not ask and does not read the saved choice.

use crate::cli_error::{CliError, unexpected};
use crate::coding::SandboxMode;
use crate::config::XdgPaths;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
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

/// Saved choice, or a dialog the caller still has to show.
#[derive(Debug)]
pub(crate) enum WorkspaceLaunch {
    Ready(ResolvedWorkspace),
    Ask(PendingWorkspaceTrust),
}

/// A directory that still needs the centered trust dialog.
#[derive(Debug)]
pub(crate) struct PendingWorkspaceTrust {
    root: PathBuf,
    store_path: PathBuf,
}

impl PendingWorkspaceTrust {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn complete(
        self,
        choice: PromptedChoice,
    ) -> Result<Option<ResolvedWorkspace>, CliError> {
        match choice {
            PromptedChoice::Exit => Ok(None),
            PromptedChoice::Open {
                writable,
                remember: save,
            } => {
                if save {
                    let choice = if writable {
                        TrustWorkspace::Yes
                    } else {
                        TrustWorkspace::No
                    };
                    remember(&self.store_path, &self.root, choice)?;
                }
                Ok(Some(ResolvedWorkspace {
                    root: self.root,
                    writable,
                }))
            }
        }
    }
}

/// Looks up the current directory without drawing.
///
/// [`WorkspaceLaunch::Ask`] means the caller shows the dialog. There is no
/// prompt when the process has no terminal; that failure happens when the
/// dialog opens.
pub(crate) fn prepare_product_workspace(
    sandbox: SandboxMode,
    explicit: Option<TrustWorkspace>,
    paths: &XdgPaths,
) -> Result<WorkspaceLaunch, CliError> {
    let cwd = std::env::current_dir().map_err(unexpected)?;
    prepare_workspace(cwd, sandbox, explicit, paths.state_dir())
}

fn prepare_workspace(
    cwd: PathBuf,
    sandbox: SandboxMode,
    explicit: Option<TrustWorkspace>,
    state_dir: &Path,
) -> Result<WorkspaceLaunch, CliError> {
    if !sandbox.uses_action_sandbox() {
        return Ok(WorkspaceLaunch::Ready(ResolvedWorkspace {
            root: cwd,
            writable: true,
        }));
    }
    let root = trust_root(&cwd)?;
    let store_path = state_dir.join(TRUST_FILE_NAME);
    if let Some(choice) = explicit {
        remember(&store_path, &root, choice)?;
        return Ok(WorkspaceLaunch::Ready(ResolvedWorkspace {
            root,
            writable: choice.writable(),
        }));
    }
    let stored = load_store(&store_path)?;
    if let Some(choice) = stored_choice(&stored, &root) {
        return Ok(WorkspaceLaunch::Ready(ResolvedWorkspace {
            root,
            writable: choice.writable(),
        }));
    }
    Ok(WorkspaceLaunch::Ask(PendingWorkspaceTrust {
        root,
        store_path,
    }))
}

/// Resolves `cwd` and remembers an explicit choice under `state_dir`.
///
/// `prompt` is called only when the sandbox is on, the flag is absent, and
/// this directory has no saved choice. A choice is written only when it asks
/// to be remembered. Exit cancels the launch.
pub(crate) fn resolve_workspace(
    cwd: PathBuf,
    sandbox: SandboxMode,
    explicit: Option<TrustWorkspace>,
    state_dir: &Path,
    mut prompt: impl FnMut(&Path) -> Result<PromptedChoice, CliError>,
) -> Result<ResolvedWorkspace, CliError> {
    match prepare_workspace(cwd, sandbox, explicit, state_dir)? {
        WorkspaceLaunch::Ready(workspace) => Ok(workspace),
        WorkspaceLaunch::Ask(pending) => {
            let choice = prompt(pending.root())?;
            pending
                .complete(choice)?
                .ok_or_else(|| unexpected("workspace trust prompt cancelled"))
        }
    }
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

/// Answer from the first-open dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptedChoice {
    /// Open the directory. `remember` writes the trust file.
    Open { writable: bool, remember: bool },
    /// Leave without opening it.
    Exit,
}

pub(crate) fn prompt_unavailable(root: &Path) -> CliError {
    unexpected(format!(
        "no saved choice for `{}` and no terminal to ask. Pass --trust-workspace yes or --trust-workspace no. Choices are saved in $XDG_STATE_HOME/merry/{TRUST_FILE_NAME}.",
        root.display()
    ))
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
    use super::{PromptedChoice, TrustWorkspace, prompt_unavailable, resolve_workspace};
    use crate::cli_error::CliError;
    use crate::coding::SandboxMode;
    use std::fs;
    use std::path::Path;

    #[test]
    fn missing_terminal_names_the_flag_and_the_saved_file() {
        let CliError::Unexpected(message) = prompt_unavailable(Path::new("/work/repo")) else {
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
                Ok(PromptedChoice::Open {
                    writable: false,
                    remember: true,
                })
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
            Ok(PromptedChoice::Open {
                writable: false,
                remember: true,
            })
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
    fn enter_keeps_the_directory_read_only_and_asks_again() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path().join("loose");
        fs::create_dir_all(&cwd).expect("mkdir");
        let state = temp.path().join("state");
        let resolved = resolve_workspace(cwd, SandboxMode::Workspace, None, &state, |_| {
            Ok(PromptedChoice::Open {
                writable: false,
                remember: false,
            })
        })
        .expect("prompt");
        assert!(!resolved.writable);
        assert!(!state.join("workspace-trust.json").exists());

        let mut prompted = false;
        let again = resolve_workspace(
            temp.path().join("loose"),
            SandboxMode::Workspace,
            None,
            &state,
            |_| {
                prompted = true;
                Ok(PromptedChoice::Open {
                    writable: false,
                    remember: false,
                })
            },
        )
        .expect("prompt again");
        assert!(prompted);
        assert!(!again.writable);
        assert!(!state.join("workspace-trust.json").exists());
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
