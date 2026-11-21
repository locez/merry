//! Git worktree physical isolation engine for coding subagents.
//!
//! Creates detached Git worktrees outside both the main repository and Merry's
//! private state directories (`$XDG_CACHE_HOME/merry/worktrees/<instance_id>/...`),
//! snapshots the parent's dirty working tree onto a baseline commit without
//! mutating the main tree, computes exact net diffs on completion, enforces a
//! fail-toward-keep deletion gate, and cleans up orphaned worktrees left behind
//! by dead Merry processes.

mod git;

use git::{
    best_effort_remove_worktree, collect_diff_stat, git_common_dir, git_dir, git_toplevel,
    is_pid_alive, map_relative_path, parse_nul_paths, parse_porcelain_z, remove_clean_worktree,
    run_git_bytes, run_git_stdout, snapshot_working_tree, validate_worktree_git_dir,
    verify_head_exists,
};
use merry_core::SessionId;
use merry_runtime::{
    ChildWorkspaceFinalization, ChildWorkspaceLifecycle, PathAccess, PathAccessRule,
    PathAccessRuleSource, SubagentWorktreeBase, SubagentWorktreeView,
};
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

/// Maximum size of one untracked file copied into a `working_tree` snapshot (5 MiB).
const MAX_UNTRACKED_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Maximum total bytes of untracked files copied into one `working_tree` snapshot (20 MiB).
const MAX_UNTRACKED_TOTAL_BYTES: u64 = 20 * 1024 * 1024;
/// Per-command timeout for setup git commands.
const SETUP_GIT_TIMEOUT: Duration = Duration::from_secs(15);
/// Per-command timeout for finalization git commands (fits inside runtime teardown budget).
const FINALIZE_GIT_TIMEOUT: Duration = Duration::from_secs(4);
/// Marker file written in `<base_dir>/<instance_id>/` holding the owning process PID.
const INSTANCE_PID_FILE: &str = ".pid";
/// Suffix of sibling marker file recording the canonical main repository root for GC.
const REPO_MARKER_SUFFIX: &str = ".repo";

/// Errors raised while provisioning or reattaching an isolated Git worktree.
#[derive(Debug, Error)]
pub enum WorktreeError {
    /// The parent workspace is not inside a Git repository.
    #[error("worktree isolation requires a Git repository at `{}`: {detail}", workspace.display())]
    NotAGitRepository { workspace: PathBuf, detail: String },
    /// The Git repository has no `HEAD` commit yet.
    #[error("worktree isolation requires at least one commit (HEAD) in `{}`: {detail}", repo_root.display())]
    NoHeadCommit { repo_root: PathBuf, detail: String },
    /// A git subprocess failed or timed out.
    #[error("git command `{command}` failed: {detail}")]
    GitCommandFailed { command: String, detail: String },
    /// Filesystem I/O failed while preparing worktree directories or snapshots.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    /// The resolved worktree admin directory was outside `<git_common_dir>/worktrees`.
    #[error(
        "refusing unsafe worktree gitdir `{}` outside `{}`",
        path.display(),
        common_dir.display()
    )]
    InvalidWorktreeAdminDir { path: PathBuf, common_dir: PathBuf },
    /// Reattaching to a retained worktree directory failed.
    #[error("failed to reattach retained worktree `{}`: {reason}", path.display())]
    ReattachFailed { path: PathBuf, reason: String },
}

/// Prepared isolated worktree paths, sandbox mount points, and lifecycle hook.
pub(crate) struct PreparedWorktree {
    /// Workspace root mounted into the child's workspace tools and process runner.
    pub(crate) child_workspace_root: PathBuf,
    /// Canonical `<main_repo>/.git` common directory (mounted read-only in sandbox).
    pub(crate) git_common_dir: PathBuf,
    /// Canonical `<main_repo>/.git/worktrees/<name>` directory (mounted read-write when child is writable).
    pub(crate) worktree_git_dir: PathBuf,
    /// Lifecycle hook invoked when the child settles to collect diffs and apply fail-toward-keep.
    pub(crate) lifecycle: Arc<dyn ChildWorkspaceLifecycle>,
}

impl PreparedWorktree {
    /// Builds minimal-privilege `.git` sandbox path rules for this isolated worktree.
    ///
    /// Mounts `<main_repo>/.git` read-only so `git status`/`diff`/`log` can read
    /// objects and refs while blocking writes to hooks, refs, and config. When the
    /// child workspace is writable, mounts only `<main_repo>/.git/worktrees/<name>`
    /// read-write for this worktree's own `index`/`HEAD`.
    #[must_use]
    pub(crate) fn sandbox_path_rules(&self, workspace_writable: bool) -> Vec<PathAccessRule> {
        let mut rules = vec![PathAccessRule::new(
            self.git_common_dir.clone(),
            PathAccess::ReadOnly,
            PathAccessRuleSource::GitMetadataBaseline,
        )];
        if workspace_writable {
            rules.push(PathAccessRule::new(
                self.worktree_git_dir.clone(),
                PathAccess::ReadWrite,
                PathAccessRuleSource::GitMetadataBaseline,
            ));
        }
        rules
    }
}

/// Summary counters returned by [`cleanup_orphaned_worktrees`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorktreeGcReport {
    /// Number of dead-process instance directories removed.
    pub removed_instances: usize,
    /// Number of individual worktrees removed via `git worktree remove --force`.
    pub removed_worktrees: usize,
    /// Number of repositories where `git worktree prune` was run.
    pub pruned_repos: usize,
    /// Number of live-process instance directories left untouched.
    pub skipped_live_instances: usize,
}

/// Manages detached Git worktree creation, reattachment, and directory layout.
#[derive(Debug, Clone)]
pub struct WorktreeIsolationManager {
    base_dir: PathBuf,
    instance_id: String,
}

impl Default for WorktreeIsolationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WorktreeIsolationManager {
    /// Creates a manager rooted at the default cache directory
    /// (`$XDG_CACHE_HOME/merry/worktrees`, falling back to `~/.cache/merry/worktrees`
    /// or `std::env::temp_dir().join("merry-worktrees")`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            base_dir: default_worktree_base_dir(),
            instance_id: process_instance_id().to_owned(),
        }
    }

    /// Creates a manager rooted at an explicit base directory (primarily for tests).
    #[must_use]
    pub fn with_base_dir(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            instance_id: process_instance_id().to_owned(),
        }
    }

    /// Returns the base directory under which instance worktree trees are stored.
    #[must_use]
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Returns the process-scoped instance directory (`<base_dir>/<instance_id>`).
    #[must_use]
    pub fn instance_dir(&self) -> PathBuf {
        self.base_dir.join(&self.instance_id)
    }

    /// Prepares a fresh isolated Git worktree or reattaches to an existing retained
    /// worktree when waking a persisted child subagent.
    pub(crate) fn prepare_or_reattach(
        &self,
        workspace_root: &Path,
        session_id: &SessionId,
        agent_id: &str,
        base: SubagentWorktreeBase,
        existing: Option<&SubagentWorktreeView>,
    ) -> Result<PreparedWorktree, WorktreeError> {
        if let Some(existing) = existing
            && existing.retained
        {
            let existing_path = PathBuf::from(&existing.path);
            if existing_path.is_dir() {
                return self.reattach_worktree(
                    workspace_root,
                    &existing_path,
                    &existing.baseline_oid,
                );
            }
        }
        self.create_worktree(workspace_root, session_id, agent_id, base)
    }

    fn reattach_worktree(
        &self,
        workspace_root: &Path,
        worktree_path: &Path,
        baseline_oid: &str,
    ) -> Result<PreparedWorktree, WorktreeError> {
        let canonical_workspace =
            fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        let repo_toplevel = git_toplevel(&canonical_workspace)?;
        let workspace_subpath = canonical_workspace
            .strip_prefix(&repo_toplevel)
            .ok()
            .filter(|sub| !sub.as_os_str().is_empty())
            .map(Path::to_path_buf);

        let canonical_worktree =
            fs::canonicalize(worktree_path).unwrap_or_else(|_| worktree_path.to_path_buf());
        let git_common_dir =
            git_common_dir(&canonical_worktree).map_err(|error| WorktreeError::ReattachFailed {
                path: canonical_worktree.clone(),
                reason: error.to_string(),
            })?;
        let worktree_git_dir =
            git_dir(&canonical_worktree).map_err(|error| WorktreeError::ReattachFailed {
                path: canonical_worktree.clone(),
                reason: error.to_string(),
            })?;
        validate_worktree_git_dir(&worktree_git_dir, &git_common_dir)?;

        let child_workspace_root = match &workspace_subpath {
            Some(sub) => canonical_worktree.join(sub),
            None => canonical_worktree.clone(),
        };
        let repo_marker_path = sibling_repo_marker(&canonical_worktree);
        let lifecycle: Arc<dyn ChildWorkspaceLifecycle> = Arc::new(WorktreeLifecycle {
            repo_toplevel,
            worktree_path: canonical_worktree,
            repo_marker_path,
            baseline_oid: baseline_oid.to_owned(),
            workspace_subpath,
        });
        Ok(PreparedWorktree {
            child_workspace_root,
            git_common_dir,
            worktree_git_dir,
            lifecycle,
        })
    }

    fn create_worktree(
        &self,
        workspace_root: &Path,
        session_id: &SessionId,
        agent_id: &str,
        base: SubagentWorktreeBase,
    ) -> Result<PreparedWorktree, WorktreeError> {
        let canonical_workspace =
            fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        let repo_toplevel = git_toplevel(&canonical_workspace)?;
        let git_common_dir = git_common_dir(&repo_toplevel)?;
        verify_head_exists(&repo_toplevel)?;

        let workspace_subpath = canonical_workspace
            .strip_prefix(&repo_toplevel)
            .ok()
            .filter(|sub| !sub.as_os_str().is_empty())
            .map(Path::to_path_buf);

        let instance_dir = self.instance_dir();
        ensure_instance_pid_file(&instance_dir)?;

        let session_dir = instance_dir.join(sanitize_path_component(session_id.as_str()));
        fs::create_dir_all(&session_dir).map_err(|source| WorktreeError::Io {
            context: format!(
                "failed to create worktree session directory `{}`",
                session_dir.display()
            ),
            source,
        })?;

        let base_name = sanitize_path_component(agent_id);
        let worktree_path = allocate_unique_worktree_path(&session_dir, &base_name);
        let repo_marker_path = sibling_repo_marker(&worktree_path);

        let worktree_arg = worktree_path.to_string_lossy().into_owned();
        run_git_stdout(
            &repo_toplevel,
            &["worktree", "add", "--detach", &worktree_arg, "HEAD"],
            SETUP_GIT_TIMEOUT,
        )?;

        let setup_result = (|| -> Result<PreparedWorktree, WorktreeError> {
            let canonical_worktree =
                fs::canonicalize(&worktree_path).unwrap_or_else(|_| worktree_path.clone());
            let worktree_git_dir = git_dir(&canonical_worktree)?;
            validate_worktree_git_dir(&worktree_git_dir, &git_common_dir)?;

            // Record canonical main repo path outside the worktree so startup dead-PID GC
            // knows which repository to remove/prune without touching the worktree itself.
            let _ = fs::write(
                &repo_marker_path,
                format!("{}\n", repo_toplevel.to_string_lossy()),
            );

            if base == SubagentWorktreeBase::WorkingTree {
                snapshot_working_tree(&repo_toplevel, &canonical_worktree)?;
            }
            let baseline_oid = run_git_stdout(
                &canonical_worktree,
                &["rev-parse", "HEAD"],
                SETUP_GIT_TIMEOUT,
            )?
            .trim()
            .to_owned();

            let child_workspace_root = match &workspace_subpath {
                Some(sub) => canonical_worktree.join(sub),
                None => canonical_worktree.clone(),
            };
            let lifecycle: Arc<dyn ChildWorkspaceLifecycle> = Arc::new(WorktreeLifecycle {
                repo_toplevel: repo_toplevel.clone(),
                worktree_path: canonical_worktree,
                repo_marker_path: repo_marker_path.clone(),
                baseline_oid,
                workspace_subpath,
            });
            Ok(PreparedWorktree {
                child_workspace_root,
                git_common_dir,
                worktree_git_dir,
                lifecycle,
            })
        })();

        if setup_result.is_err() {
            best_effort_remove_worktree(&repo_toplevel, &worktree_path, &repo_marker_path);
        }
        setup_result
    }
}

/// Lifecycle handle for one isolated Git worktree owned by a child subagent.
struct WorktreeLifecycle {
    repo_toplevel: PathBuf,
    worktree_path: PathBuf,
    repo_marker_path: PathBuf,
    baseline_oid: String,
    workspace_subpath: Option<PathBuf>,
}

impl ChildWorkspaceLifecycle for WorktreeLifecycle {
    fn metadata(&self) -> SubagentWorktreeView {
        SubagentWorktreeView {
            path: self.worktree_path.to_string_lossy().into_owned(),
            baseline_oid: self.baseline_oid.clone(),
            diff_stat: None,
            retained: true,
        }
    }

    fn finalize(&self, allow_cleanup: bool) -> ChildWorkspaceFinalization {
        let mut changed_set = BTreeSet::new();
        let mut untracked_paths = Vec::new();
        let mut uncertain = false;

        match run_git_bytes(
            &self.worktree_path,
            &["status", "--porcelain=v1", "-z"],
            FINALIZE_GIT_TIMEOUT,
        ) {
            Ok(bytes) => {
                for (status_xy, rel_path) in parse_porcelain_z(&bytes) {
                    if status_xy == "??" {
                        untracked_paths.push(rel_path.clone());
                    }
                    if let Some(mapped) =
                        map_relative_path(&rel_path, self.workspace_subpath.as_deref())
                    {
                        changed_set.insert(mapped);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    worktree = %self.worktree_path.display(),
                    error = %error,
                    "git status failed during worktree finalization; failing toward keep"
                );
                uncertain = true;
            }
        }

        match run_git_bytes(
            &self.worktree_path,
            &["diff", "--name-only", "-z", &self.baseline_oid],
            FINALIZE_GIT_TIMEOUT,
        ) {
            Ok(bytes) => {
                for rel_path in parse_nul_paths(&bytes) {
                    if let Some(mapped) =
                        map_relative_path(&rel_path, self.workspace_subpath.as_deref())
                    {
                        changed_set.insert(mapped);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    worktree = %self.worktree_path.display(),
                    error = %error,
                    "git diff --name-only failed during worktree finalization; failing toward keep"
                );
                uncertain = true;
            }
        }

        let head_moved = match run_git_stdout(
            &self.worktree_path,
            &["rev-parse", "HEAD"],
            FINALIZE_GIT_TIMEOUT,
        ) {
            Ok(head) => head.trim() != self.baseline_oid,
            Err(error) => {
                tracing::warn!(
                    worktree = %self.worktree_path.display(),
                    error = %error,
                    "git rev-parse HEAD failed during worktree finalization; failing toward keep"
                );
                uncertain = true;
                false
            }
        };

        let changed_paths: Vec<String> = changed_set.into_iter().collect();
        let has_changes = !changed_paths.is_empty() || head_moved;

        if uncertain || has_changes || !allow_cleanup {
            let diff_stat = if has_changes {
                collect_diff_stat(&self.worktree_path, &self.baseline_oid, &untracked_paths)
            } else {
                None
            };
            return ChildWorkspaceFinalization {
                changed_paths,
                worktree: Some(SubagentWorktreeView {
                    path: self.worktree_path.to_string_lossy().into_owned(),
                    baseline_oid: self.baseline_oid.clone(),
                    diff_stat,
                    retained: true,
                }),
            };
        }

        // Confirmed zero net changes, HEAD == baseline_oid, no errors, and cleanup allowed.
        let removed = remove_clean_worktree(
            &self.repo_toplevel,
            &self.worktree_path,
            &self.repo_marker_path,
        );
        ChildWorkspaceFinalization {
            changed_paths: Vec::new(),
            worktree: Some(SubagentWorktreeView {
                path: self.worktree_path.to_string_lossy().into_owned(),
                baseline_oid: self.baseline_oid.clone(),
                diff_stat: None,
                retained: !removed,
            }),
        }
    }
}

/// Cleans up orphaned worktree directories left behind by dead Merry processes
/// under the default cache directory.
pub fn cleanup_orphaned_worktrees_default() -> WorktreeGcReport {
    cleanup_orphaned_worktrees(&default_worktree_base_dir())
}

/// Scans `<base_dir>/<instance_id>/` and removes worktrees owned by exited PIDs
/// while sparing every instance whose `.pid` process is still alive.
pub fn cleanup_orphaned_worktrees(base_dir: &Path) -> WorktreeGcReport {
    let mut report = WorktreeGcReport::default();
    let Ok(entries) = fs::read_dir(base_dir) else {
        return report;
    };
    let mut repos_to_prune = BTreeSet::new();
    for entry in entries.flatten() {
        let instance_dir = entry.path();
        if !instance_dir.is_dir() {
            continue;
        }
        let pid_path = instance_dir.join(INSTANCE_PID_FILE);
        let Ok(pid_text) = fs::read_to_string(&pid_path) else {
            // Fail-toward-keep: never delete an instance directory missing a readable `.pid`.
            report.skipped_live_instances += 1;
            continue;
        };
        let Ok(pid) = pid_text.trim().parse::<u32>() else {
            report.skipped_live_instances += 1;
            continue;
        };
        if is_pid_alive(pid) {
            report.skipped_live_instances += 1;
            continue;
        }

        // Dead PID: unregister every recorded worktree, then remove the instance tree.
        if let Ok(session_entries) = fs::read_dir(&instance_dir) {
            for session_entry in session_entries.flatten() {
                let session_dir = session_entry.path();
                if !session_dir.is_dir() {
                    continue;
                }
                if let Ok(wt_entries) = fs::read_dir(&session_dir) {
                    for wt_entry in wt_entries.flatten() {
                        let path = wt_entry.path();
                        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                            continue;
                        };
                        let Some(worktree_name) = name.strip_suffix(REPO_MARKER_SUFFIX) else {
                            continue;
                        };
                        let Ok(repo_text) = fs::read_to_string(&path) else {
                            continue;
                        };
                        let repo_root = PathBuf::from(repo_text.trim());
                        if repo_root.is_dir() {
                            repos_to_prune.insert(repo_root.clone());
                            let worktree_dir = session_dir.join(worktree_name);
                            if worktree_dir.exists() {
                                let arg = worktree_dir.to_string_lossy().into_owned();
                                if run_git_stdout(
                                    &repo_root,
                                    &["worktree", "remove", "--force", &arg],
                                    FINALIZE_GIT_TIMEOUT,
                                )
                                .is_ok()
                                {
                                    report.removed_worktrees += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        if fs::remove_dir_all(&instance_dir).is_ok() {
            report.removed_instances += 1;
        }
    }

    for repo_root in repos_to_prune {
        if run_git_stdout(&repo_root, &["worktree", "prune"], FINALIZE_GIT_TIMEOUT).is_ok() {
            report.pruned_repos += 1;
        }
    }
    report
}

/// Returns the default base directory for isolated subagent Git worktrees.
#[must_use]
pub fn default_worktree_base_dir() -> PathBuf {
    if let Some(xdg_cache) = env::var_os("XDG_CACHE_HOME") {
        let path = PathBuf::from(xdg_cache);
        if path.is_absolute() {
            return path.join("merry").join("worktrees");
        }
    }
    if let Some(home) = env::var_os("HOME") {
        let path = PathBuf::from(home);
        if path.is_absolute() {
            return path.join(".cache").join("merry").join("worktrees");
        }
    }
    env::temp_dir().join("merry-worktrees")
}

fn process_instance_id() -> &'static str {
    static INSTANCE_ID: OnceLock<String> = OnceLock::new();
    INSTANCE_ID.get_or_init(|| {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("pid-{}-{millis}", std::process::id())
    })
}

fn ensure_instance_pid_file(instance_dir: &Path) -> Result<(), WorktreeError> {
    fs::create_dir_all(instance_dir).map_err(|source| WorktreeError::Io {
        context: format!(
            "failed to create worktree instance directory `{}`",
            instance_dir.display()
        ),
        source,
    })?;
    let pid_path = instance_dir.join(INSTANCE_PID_FILE);
    if !pid_path.exists() {
        fs::write(&pid_path, format!("{}\n", std::process::id())).map_err(|source| {
            WorktreeError::Io {
                context: format!("failed to write instance PID file `{}`", pid_path.display()),
                source,
            }
        })?;
    }
    Ok(())
}

fn allocate_unique_worktree_path(session_dir: &Path, base_name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let primary = session_dir.join(base_name);
    if !primary.exists() {
        return primary;
    }
    loop {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = session_dir.join(format!("{base_name}-{seq}"));
        if !candidate.exists() {
            return candidate;
        }
    }
}

fn sibling_repo_marker(worktree_path: &Path) -> PathBuf {
    let name = worktree_path
        .file_name()
        .map(|n| format!("{}{}", n.to_string_lossy(), REPO_MARKER_SUFFIX))
        .unwrap_or_else(|| format!("worktree{REPO_MARKER_SUFFIX}"));
    worktree_path.parent().unwrap_or(worktree_path).join(name)
}

fn sanitize_path_component(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if sanitized.is_empty() {
        "subagent".to_owned()
    } else {
        sanitized
    }
}
