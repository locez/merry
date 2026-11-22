//! Low-level bounded Git command execution, working-tree snapshotting, and PID
//! liveness probing owned by [`super::WorktreeIsolationManager`].

use super::{
    FINALIZE_GIT_TIMEOUT, MAX_UNTRACKED_FILE_BYTES, MAX_UNTRACKED_TOTAL_BYTES, SETUP_GIT_TIMEOUT,
    WorktreeError,
};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(super) fn git_toplevel(path: &Path) -> Result<PathBuf, WorktreeError> {
    let out = run_git_stdout(path, &["rev-parse", "--show-toplevel"], SETUP_GIT_TIMEOUT).map_err(
        |error| WorktreeError::NotAGitRepository {
            workspace: path.to_path_buf(),
            detail: error.to_string(),
        },
    )?;
    let raw = PathBuf::from(out.trim());
    Ok(fs::canonicalize(&raw).unwrap_or(raw))
}

pub(super) fn git_common_dir(path: &Path) -> Result<PathBuf, WorktreeError> {
    let out = run_git_stdout(path, &["rev-parse", "--git-common-dir"], SETUP_GIT_TIMEOUT)?;
    let raw = PathBuf::from(out.trim());
    let resolved = if raw.is_absolute() {
        raw
    } else {
        path.join(raw)
    };
    Ok(fs::canonicalize(&resolved).unwrap_or(resolved))
}

pub(super) fn git_dir(path: &Path) -> Result<PathBuf, WorktreeError> {
    let out = run_git_stdout(path, &["rev-parse", "--git-dir"], SETUP_GIT_TIMEOUT)?;
    let raw = PathBuf::from(out.trim());
    let resolved = if raw.is_absolute() {
        raw
    } else {
        path.join(raw)
    };
    Ok(fs::canonicalize(&resolved).unwrap_or(resolved))
}

pub(super) fn verify_head_exists(repo_toplevel: &Path) -> Result<(), WorktreeError> {
    run_git_stdout(
        repo_toplevel,
        &["rev-parse", "--verify", "HEAD"],
        SETUP_GIT_TIMEOUT,
    )
    .map_err(|error| WorktreeError::NoHeadCommit {
        repo_root: repo_toplevel.to_path_buf(),
        detail: error.to_string(),
    })?;
    Ok(())
}

pub(super) fn validate_worktree_git_dir(
    worktree_git_dir: &Path,
    git_common_dir: &Path,
) -> Result<(), WorktreeError> {
    let expected_parent = git_common_dir.join("worktrees");
    if !worktree_git_dir.starts_with(&expected_parent) || worktree_git_dir == expected_parent {
        return Err(WorktreeError::InvalidWorktreeAdminDir {
            path: worktree_git_dir.to_path_buf(),
            common_dir: git_common_dir.to_path_buf(),
        });
    }
    Ok(())
}

pub(super) fn snapshot_working_tree(
    repo_toplevel: &Path,
    worktree_path: &Path,
) -> Result<(), WorktreeError> {
    // 1. Export tracked working-tree modifications without mutating the main tree.
    let diff = run_git_bytes(
        repo_toplevel,
        &["diff", "HEAD", "--binary"],
        SETUP_GIT_TIMEOUT,
    )?;
    if !diff.is_empty() {
        run_git_with_stdin(
            worktree_path,
            &["apply", "--binary", "--whitespace=nowarn"],
            &diff,
            SETUP_GIT_TIMEOUT,
        )?;
    }

    // 2. Copy non-ignored untracked regular files within bounded size limits.
    let untracked = run_git_bytes(
        repo_toplevel,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        SETUP_GIT_TIMEOUT,
    )?;
    let mut copied_total: u64 = 0;
    for rel in parse_nul_paths(&untracked) {
        let rel_path = Path::new(&rel);
        if rel_path.is_absolute()
            || rel_path.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            continue;
        }
        let src = repo_toplevel.join(rel_path);
        let Ok(meta) = fs::symlink_metadata(&src) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let len = meta.len();
        if len > MAX_UNTRACKED_FILE_BYTES
            || copied_total.saturating_add(len) > MAX_UNTRACKED_TOTAL_BYTES
        {
            tracing::warn!(
                path = %rel,
                bytes = len,
                "skipping oversized untracked file during worktree working_tree snapshot"
            );
            continue;
        }
        let dst = worktree_path.join(rel_path);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|source| WorktreeError::Io {
                context: format!(
                    "failed to create parent directory `{}` in worktree snapshot",
                    parent.display()
                ),
                source,
            })?;
        }
        fs::copy(&src, &dst).map_err(|source| WorktreeError::Io {
            context: format!(
                "failed to copy untracked file `{}` into worktree snapshot",
                src.display()
            ),
            source,
        })?;
        copied_total = copied_total.saturating_add(len);
    }

    // 3. Create an ephemeral unsigned baseline commit in the detached worktree so
    //    `git diff <baseline_oid>` later isolates only this child's net edits.
    let status = run_git_bytes(
        worktree_path,
        &["status", "--porcelain=v1", "-z"],
        SETUP_GIT_TIMEOUT,
    )?;
    if !status.is_empty() {
        run_git_stdout(worktree_path, &["add", "-A"], SETUP_GIT_TIMEOUT)?;
        run_git_stdout(
            worktree_path,
            &[
                "-c",
                "user.name=Merry Worktree",
                "-c",
                "user.email=merry@local",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--no-verify",
                "-m",
                "merry:worktree-baseline",
            ],
            SETUP_GIT_TIMEOUT,
        )?;
    }
    Ok(())
}

pub(super) fn collect_diff_stat(
    worktree_path: &Path,
    baseline_oid: &str,
    untracked_paths: &[String],
) -> Option<String> {
    let mut sections = Vec::new();
    if let Ok(stat) = run_git_stdout(
        worktree_path,
        &["diff", "--stat", baseline_oid],
        FINALIZE_GIT_TIMEOUT,
    ) {
        let trimmed = stat.trim();
        if !trimmed.is_empty() {
            sections.push(trimmed.to_owned());
        }
    }
    if !untracked_paths.is_empty() {
        sections.push(format!("Untracked files: {}", untracked_paths.join(", ")));
    }
    (!sections.is_empty()).then(|| sections.join("\n"))
}

pub(super) fn remove_clean_worktree(
    repo_toplevel: &Path,
    worktree_path: &Path,
    repo_marker_path: &Path,
) -> bool {
    let arg = worktree_path.to_string_lossy().into_owned();
    let ok = run_git_stdout(
        repo_toplevel,
        &["worktree", "remove", "--force", &arg],
        FINALIZE_GIT_TIMEOUT,
    )
    .is_ok();
    if ok && !worktree_path.exists() {
        let _ = fs::remove_file(repo_marker_path);
        if let Some(session_dir) = worktree_path.parent() {
            let _ = fs::remove_dir(session_dir);
        }
        true
    } else {
        false
    }
}

pub(super) fn best_effort_remove_worktree(
    repo_toplevel: &Path,
    worktree_path: &Path,
    repo_marker_path: &Path,
) {
    let arg = worktree_path.to_string_lossy().into_owned();
    let _ = run_git_stdout(
        repo_toplevel,
        &["worktree", "remove", "--force", &arg],
        FINALIZE_GIT_TIMEOUT,
    );
    let _ = fs::remove_dir_all(worktree_path);
    let _ = fs::remove_file(repo_marker_path);
}

pub(super) fn map_relative_path(
    repo_relative: &str,
    workspace_subpath: Option<&Path>,
) -> Option<String> {
    let normalized = repo_relative.trim();
    if normalized.is_empty() {
        return None;
    }
    match workspace_subpath {
        Some(sub) => Path::new(normalized)
            .strip_prefix(sub)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| Some(normalized.to_owned())),
        None => Some(normalized.to_owned()),
    }
}

pub(super) fn parse_nul_paths(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|slice| !slice.is_empty())
        .map(|slice| String::from_utf8_lossy(slice).into_owned())
        .collect()
}

pub(super) fn parse_porcelain_z(bytes: &[u8]) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut parts = bytes.split(|byte| *byte == 0);
    while let Some(raw) = parts.next() {
        if raw.len() < 4 {
            continue;
        }
        let xy = String::from_utf8_lossy(&raw[..2]).into_owned();
        let path = String::from_utf8_lossy(&raw[3..]).into_owned();
        // Rename (`R`) and copy (`C`) records emit an extra NUL-terminated source path.
        if xy.starts_with('R') || xy.starts_with('C') || xy.ends_with('R') || xy.ends_with('C') {
            let _ = parts.next();
        }
        if !path.is_empty() {
            entries.push((xy, path));
        }
    }
    entries
}

pub(super) fn run_git_stdout(
    cwd: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String, WorktreeError> {
    let bytes = run_git_bytes(cwd, args, timeout)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) fn run_git_bytes(
    cwd: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, WorktreeError> {
    run_git_raw(cwd, args, None, timeout)
}

fn run_git_with_stdin(
    cwd: &Path,
    args: &[&str],
    stdin_bytes: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, WorktreeError> {
    run_git_raw(cwd, args, Some(stdin_bytes), timeout)
}

fn run_git_raw(
    cwd: &Path,
    args: &[&str],
    stdin_bytes: Option<&[u8]>,
    timeout: Duration,
) -> Result<Vec<u8>, WorktreeError> {
    let command_label = format!("git {}", args.join(" "));
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|source| WorktreeError::GitCommandFailed {
            command: command_label.clone(),
            detail: source.to_string(),
        })?;

    let mut child_stdin = child.stdin.take();
    let mut child_stdout = child.stdout.take();
    let mut child_stderr = child.stderr.take();

    let (status, stdout, stderr) = std::thread::scope(|scope| {
        let stdin_handle = stdin_bytes.map(|bytes| {
            scope.spawn(move || {
                if let Some(mut stdin) = child_stdin.take() {
                    let _ = stdin.write_all(bytes);
                }
            })
        });
        let stdout_handle = scope.spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut out) = child_stdout.take() {
                let _ = out.read_to_end(&mut buf);
            }
            buf
        });
        let stderr_handle = scope.spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut err) = child_stderr.take() {
                let _ = err.read_to_end(&mut buf);
            }
            buf
        });

        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(format!("timed out after {}s", timeout.as_secs()));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => break Err(error.to_string()),
            }
        };
        if let Some(handle) = stdin_handle {
            let _ = handle.join();
        }
        let stdout = stdout_handle.join().unwrap_or_default();
        let stderr = stderr_handle.join().unwrap_or_default();
        (status, stdout, stderr)
    });

    match status {
        Ok(exit) if exit.success() => Ok(stdout),
        Ok(exit) => {
            let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
            Err(WorktreeError::GitCommandFailed {
                command: command_label,
                detail: if detail.is_empty() {
                    format!("exited with status {exit}")
                } else {
                    detail
                },
            })
        }
        Err(detail) => Err(WorktreeError::GitCommandFailed {
            command: command_label,
            detail,
        }),
    }
}

pub(super) fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        let stat_path = format!("/proc/{pid}/stat");
        let Ok(stat) = fs::read_to_string(&stat_path) else {
            return false;
        };
        if let Some(after_comm) = stat.rsplit_once(')') {
            let state = after_comm.1.trim_start().chars().next();
            return state != Some('Z') && state != Some('X');
        }
        true
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        match Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
        {
            Ok(output) => {
                if output.status.success() {
                    return true;
                }
                let stderr = String::from_utf8_lossy(&output.stderr);
                stderr.contains("Operation not permitted")
            }
            Err(_) => true,
        }
    }
    #[cfg(windows)]
    {
        is_windows_pid_alive(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// Checks whether a Windows process ID is still running via
/// `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` and `GetExitCodeProcess`.
///
/// Reviewed FFI boundary: Win32 exposes no safe std API for checking arbitrary
/// PID liveness.
#[cfg(windows)]
#[allow(unsafe_code)]
fn is_windows_pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };

    // SAFETY: `OpenProcess` takes only plain integer flags/PID and returns a
    // nullable OS handle that we close before returning.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // SAFETY: `GetLastError` reads thread-local Win32 error state.
        return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
    }

    let mut exit_code: u32 = 0;
    // SAFETY: `handle` is a valid non-null process handle owned by this
    // function, and `exit_code` is a valid writable `u32` pointer.
    let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    // SAFETY: `handle` was returned by `OpenProcess` above and is closed once.
    unsafe {
        CloseHandle(handle);
    }
    ok != 0 && exit_code == STILL_ACTIVE as u32
}
