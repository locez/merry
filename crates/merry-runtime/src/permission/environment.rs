//! Bounded, read-only harness environment probes for AI permission review.
//!
//! Supplies ground-truth facts (such as `git status` cleanliness, current
//! branch, and credential-stripped `origin` remote URL) directly from the
//! runtime host so the reviewer does not have to guess whether a discard
//! destroys uncommitted work or whether a push targets a shared branch.

use super::{PermissionRequest, PermissionedAction};
use crate::process::shell_command_for_argv;
use serde_json::{Value, json};
use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// Maximum wall-clock time allowed for one read-only git metadata probe.
const GIT_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Maximum bytes of porcelain output parsed from `git status`.
const MAX_GIT_STATUS_BYTES: usize = 64 * 1024;

/// Ground-truth environment metadata gathered by the harness before AI review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionReviewEnvironmentMetadata {
    pub(crate) git: Option<GitReviewMetadata>,
}

impl PermissionReviewEnvironmentMetadata {
    pub(crate) fn is_empty(&self) -> bool {
        self.git.is_none()
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut object = serde_json::Map::new();
        if let Some(git) = &self.git {
            object.insert("git_status".to_owned(), git.to_json());
        }
        Value::Object(object)
    }
}

/// Ground-truth git repository status snapshot for the action's working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitReviewMetadata {
    pub(crate) branch: Option<String>,
    pub(crate) clean: bool,
    pub(crate) staged: usize,
    pub(crate) modified: usize,
    pub(crate) untracked: usize,
    pub(crate) origin_remote: Option<String>,
}

impl GitReviewMetadata {
    fn to_json(&self) -> Value {
        let mut payload = json!({
            "clean": self.clean,
            "staged": self.staged,
            "modified": self.modified,
            "untracked": self.untracked,
        });
        if let Some(branch) = &self.branch {
            payload["branch"] = json!(branch);
        }
        if let Some(origin_remote) = &self.origin_remote {
            payload["origin_remote"] = json!(origin_remote);
        }
        payload
    }
}

/// Gathers bounded harness environment metadata relevant to `request`.
///
/// Never fails permission review: timeouts, missing `git`, non-git
/// directories, or cancellation simply return `None`.
pub(crate) async fn probe_environment_metadata(
    workspace_root: &Path,
    request: &PermissionRequest,
    cancellation_token: &CancellationToken,
) -> Option<PermissionReviewEnvironmentMetadata> {
    if cancellation_token.is_cancelled() {
        return None;
    }

    let PermissionedAction::Process(intent) = request.action();
    let command = shell_command_for_argv(intent.argv());
    let probe_kind = classify_git_probe_need(&command);
    if probe_kind == GitProbeNeed::None {
        return None;
    }

    let target_dir = resolve_probe_directory(workspace_root, intent.cwd())?;
    let status_stdout = run_bounded_git(
        &target_dir,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "--branch",
        ],
        cancellation_token,
    )
    .await?;

    let mut git = parse_git_porcelain_v1_branch(&status_stdout);
    if probe_kind == GitProbeNeed::StatusAndRemote
        && let Some(remote_stdout) = run_bounded_git(
            &target_dir,
            &["--no-optional-locks", "remote", "get-url", "origin"],
            cancellation_token,
        )
        .await
    {
        let sanitized = sanitize_remote_url(remote_stdout.trim());
        if !sanitized.is_empty() {
            git.origin_remote = Some(sanitized);
        }
    }

    Some(PermissionReviewEnvironmentMetadata { git: Some(git) })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitProbeNeed {
    None,
    StatusOnly,
    StatusAndRemote,
}

fn classify_git_probe_need(command: &str) -> GitProbeNeed {
    let lower = command.to_ascii_lowercase();
    let has_token = |needle: &str| {
        lower
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '/' | '.')))
            .any(|token| {
                let leaf = token.rsplit(['/', '\\']).next().unwrap_or(token);
                leaf == needle
            })
    };

    if has_token("gh") {
        return GitProbeNeed::StatusAndRemote;
    }
    if has_token("git") {
        if ["push", "remote", "fetch", "pull", "clone"]
            .into_iter()
            .any(has_token)
        {
            return GitProbeNeed::StatusAndRemote;
        }
        return GitProbeNeed::StatusOnly;
    }
    if has_token("rm") {
        return GitProbeNeed::StatusOnly;
    }
    GitProbeNeed::None
}

fn resolve_probe_directory(workspace_root: &Path, cwd: Option<&str>) -> Option<PathBuf> {
    let candidate = match cwd {
        None | Some("") | Some(".") => workspace_root.to_path_buf(),
        Some(relative) => {
            let rel_path = Path::new(relative);
            if rel_path.is_absolute()
                || rel_path
                    .components()
                    .any(|component| matches!(component, Component::ParentDir))
            {
                return None;
            }
            workspace_root.join(rel_path)
        }
    };
    candidate.is_dir().then_some(candidate)
}

async fn run_bounded_git(
    dir: &Path,
    args: &[&str],
    cancellation_token: &CancellationToken,
) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.spawn().ok()?;
    let output = tokio::select! {
        biased;
        () = cancellation_token.cancelled() => return None,
        result = tokio::time::timeout(GIT_PROBE_TIMEOUT, child.wait_with_output()) => {
            result.ok()?.ok()?
        }
    };
    if !output.status.success() {
        return None;
    }
    let slice = if output.stdout.len() > MAX_GIT_STATUS_BYTES {
        &output.stdout[..MAX_GIT_STATUS_BYTES]
    } else {
        &output.stdout
    };
    Some(String::from_utf8_lossy(slice).into_owned())
}

/// Parses `git status --porcelain=v1 --branch` output into structured metadata.
pub(crate) fn parse_git_porcelain_v1_branch(output: &str) -> GitReviewMetadata {
    let mut branch = None;
    let mut staged = 0usize;
    let mut modified = 0usize;
    let mut untracked = 0usize;

    for line in output.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            branch = parse_porcelain_branch_header(header);
            continue;
        }
        let bytes = line.as_bytes();
        if bytes.len() < 2 {
            continue;
        }
        if bytes[0] == b'?' && bytes[1] == b'?' {
            untracked += 1;
            continue;
        }
        if bytes[0] != b' ' && bytes[0] != b'!' {
            staged += 1;
        }
        if bytes[1] != b' ' && bytes[1] != b'!' {
            modified += 1;
        }
    }

    GitReviewMetadata {
        branch,
        clean: staged == 0 && modified == 0 && untracked == 0,
        staged,
        modified,
        untracked,
        origin_remote: None,
    }
}

fn parse_porcelain_branch_header(header: &str) -> Option<String> {
    let trimmed = header.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("No commits yet on ") {
        let name = rest.split("...").next().unwrap_or(rest).trim();
        return (!name.is_empty()).then(|| name.to_owned());
    }
    if let Some(rest) = trimmed.strip_prefix("Initial commit on ") {
        let name = rest.split("...").next().unwrap_or(rest).trim();
        return (!name.is_empty()).then(|| name.to_owned());
    }
    if trimmed.starts_with("HEAD (no branch)") {
        return Some("HEAD (detached)".to_owned());
    }
    let name = trimmed.split("...").next().unwrap_or(trimmed).trim();
    let name = name.split_whitespace().next().unwrap_or(name);
    (!name.is_empty()).then(|| name.to_owned())
}

/// Strips embedded credentials (`user:token@`) from HTTP(S)/SSH/Git remote URLs
/// so secrets never enter reviewer prompts or logs.
pub(crate) fn sanitize_remote_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(scheme_end) = trimmed.find("://") {
        let after_scheme = &trimmed[scheme_end + 3..];
        let authority_end = after_scheme.find('/').unwrap_or(after_scheme.len());
        let authority = &after_scheme[..authority_end];
        if let Some(at_pos) = authority.rfind('@') {
            let host = &authority[at_pos + 1..];
            let rest = &after_scheme[authority_end..];
            return format!("{}{host}{rest}", &trimmed[..scheme_end + 3]);
        }
    }
    trimmed.to_owned()
}
