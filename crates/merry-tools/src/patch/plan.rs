use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use merry_core::PendingToolCall;
use merry_runtime::{
    ActionProposal, ActionProposalError, ActionProposalEvidence, ToolActionKind,
    ToolActionPreflight, ToolExecutionError, ToolExecutionOutcome, WorkspacePatchChangeEvidence,
    WorkspacePatchProposal,
};

use crate::{
    APPLY_PATCH_TOOL,
    errors::{
        BlockingToolError, DomainError, ERROR_FILE_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND,
        ERROR_FILE_TOO_LARGE, ERROR_INVALID_ARGUMENTS, ERROR_NOT_FILE, ERROR_NOT_UTF8,
        ERROR_PATH_DENIED, ERROR_PROPOSAL_MISMATCH, ERROR_READ_FAILED, PathValidationError,
        WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE, failed_outcome,
    },
    path::{
        NewWorkspacePath, ValidatedToolPath, open_file_for_read, resolve_existing_path,
        resolve_new_file_path, validate_workspace_path_argument,
    },
    state::{WorkspaceToolState, matches_any_scope_path},
};

use super::{
    ApplyPatchInput,
    apply::execute_apply_patch_plan,
    envelope::{WorkspacePatchOperationKind, WorkspacePatchSuccessLine},
    parse::parse_apply_patch,
    types::{
        WorkspacePatchFile, WorkspacePatchHunk, WorkspacePatchOperation,
        build_new_file_replacement, build_patch_replacement, count_file_lines,
        stable_content_fingerprint,
    },
};

#[cfg(test)]
pub(crate) fn apply_patch_blocking(
    state: &WorkspaceToolState,
    args: ApplyPatchInput,
) -> ToolExecutionOutcome {
    apply_patch_blocking_checked(state, args, None, &|| false)
        .expect("uncancelled workspace patch should not return cancellation")
}

pub(crate) fn propose_apply_patch_blocking_checked(
    state: &WorkspaceToolState,
    args: ApplyPatchInput,
    call: &PendingToolCall,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolActionPreflight, ToolExecutionError> {
    match plan_apply_patch_blocking_checked(state, args, is_cancelled) {
        Ok(WorkspacePatchPlanOutcome::Planned(plan)) => {
            let changes = plan
                .changes
                .iter()
                .map(WorkspacePatchFilePlan::change_evidence)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            let patch = WorkspacePatchProposal::from_changes(changes)
                .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            let subject = plan.subject();
            let proposal = ActionProposal::new(
                call,
                ToolActionKind::WorkspaceWrite,
                "workspace patch",
                subject.clone(),
                plan.summary(),
                ActionProposalEvidence::WorkspacePatch(patch),
            )
            .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            Ok(ToolActionPreflight::Proposal(proposal))
        }
        Ok(WorkspacePatchPlanOutcome::Failure(outcome)) => {
            Ok(ToolActionPreflight::Outcome(outcome))
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn apply_patch_blocking_checked(
    state: &WorkspaceToolState,
    args: ApplyPatchInput,
    approved_proposal: Option<&WorkspacePatchProposal>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    match plan_apply_patch_blocking_checked(state, args, is_cancelled)? {
        WorkspacePatchPlanOutcome::Planned(plan) => {
            if let Some(approved) = approved_proposal
                && match_approved_patch_proposal(approved, &plan).is_err()
            {
                return Ok(proposal_mismatch_outcome(plan.subject()));
            }
            execute_apply_patch_plan(plan, is_cancelled)
        }
        WorkspacePatchPlanOutcome::Failure(outcome) => Ok(outcome),
    }
}

fn plan_apply_patch_blocking_checked(
    state: &WorkspaceToolState,
    args: ApplyPatchInput,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchPlanOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }

    if args.patch.trim().is_empty() {
        return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
            APPLY_PATCH_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace patch must not be empty",
            None::<String>,
        )));
    }

    if args.patch.contains('\0') {
        return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
            APPLY_PATCH_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace patch must not contain NUL bytes",
            None::<String>,
        )));
    }

    if args.patch.len() > state.limits.max_patch_bytes {
        return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
            APPLY_PATCH_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace patch payload exceeds the configured byte limit; retry with the smallest unique hunk needed for the edit",
            None::<String>,
        )));
    }

    let patch = match parse_apply_patch(&args.patch) {
        Ok(patch) => patch,
        Err(error) => {
            return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
                APPLY_PATCH_TOOL,
                error.code,
                error.message,
                error.path,
            )));
        }
    };

    let mut changes = Vec::with_capacity(patch.files.len());
    for file_patch in patch.files {
        if is_cancelled() {
            return Err(ToolExecutionError::Cancelled);
        }
        match plan_apply_patch_file(state, file_patch, is_cancelled) {
            Ok(change) => changes.push(change),
            Err(WorkspacePatchFilePlanError::Domain { error, path }) => {
                return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
                    APPLY_PATCH_TOOL,
                    error.code,
                    error.message,
                    Some(path),
                )));
            }
            Err(WorkspacePatchFilePlanError::Path(error)) => {
                return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
                    APPLY_PATCH_TOOL,
                    error.code,
                    error.message,
                    error.path,
                )));
            }
            Err(WorkspacePatchFilePlanError::Cancelled) => {
                return Err(ToolExecutionError::Cancelled);
            }
        }
    }

    Ok(WorkspacePatchPlanOutcome::Planned(WorkspacePatchPlan {
        changes,
    }))
}

#[derive(Debug)]
enum WorkspacePatchPlanOutcome {
    Planned(WorkspacePatchPlan),
    Failure(ToolExecutionOutcome),
}

#[derive(Debug)]
pub(super) struct WorkspacePatchPlan {
    pub(super) changes: Vec<WorkspacePatchFilePlan>,
}

impl WorkspacePatchPlan {
    fn subject(&self) -> String {
        match self.changes.as_slice() {
            [change] => change.relative.display.clone(),
            changes => format!("{} files", changes.len()),
        }
    }

    fn summary(&self) -> String {
        match self.changes.as_slice() {
            [change] => change.summary(),
            changes => {
                let bytes_before = changes.iter().fold(0usize, |sum, change| {
                    sum.saturating_add(change.bytes_before)
                });
                let bytes_after = changes
                    .iter()
                    .fold(0usize, |sum, change| sum.saturating_add(change.bytes_after));
                let lines_before = changes.iter().fold(0usize, |sum, change| {
                    sum.saturating_add(change.lines_before)
                });
                let lines_after = changes
                    .iter()
                    .fold(0usize, |sum, change| sum.saturating_add(change.lines_after));
                format!(
                    "Apply workspace patch to {} files ({} -> {} lines, {} -> {} bytes).",
                    changes.len(),
                    lines_before,
                    lines_after,
                    bytes_before,
                    bytes_after
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkspacePatchFileMode {
    CreateNew,
    UpdateExisting,
    DeleteExisting,
}

impl WorkspacePatchFileMode {
    /// Names the operation recorded in the success envelope.
    pub(super) fn operation_kind(self) -> WorkspacePatchOperationKind {
        match self {
            Self::CreateNew => WorkspacePatchOperationKind::Add,
            Self::UpdateExisting => WorkspacePatchOperationKind::Update,
            Self::DeleteExisting => WorkspacePatchOperationKind::Delete,
        }
    }
}

#[derive(Debug)]
pub(super) struct WorkspacePatchFilePlan {
    pub(super) relative: ValidatedToolPath,
    pub(super) path: PathBuf,
    pub(super) content_before: String,
    pub(super) replacement: String,
    pub(super) preimage_bytes: usize,
    pub(super) replacement_bytes: usize,
    pub(super) bytes_before: usize,
    pub(super) bytes_after: usize,
    /// Line counts of the planned preimage and replacement.
    ///
    /// They are stored beside the byte counts so every consumer of a plan
    /// reports the same numbers without rescanning the file content, and so a
    /// proposal summary cannot drift from the change it describes.
    pub(super) lines_before: usize,
    pub(super) lines_after: usize,
    pub(super) hunks: usize,
    pub(super) ignored_context_hunks: usize,
    pub(super) lines: Vec<WorkspacePatchSuccessLine>,
    pub(super) max_read_bytes: usize,
    pub(super) mode: WorkspacePatchFileMode,
}

impl WorkspacePatchFilePlan {
    /// Describes the planned change for proposal and audit text.
    ///
    /// Reviewers reason about a change in lines, so the summary leads with the
    /// line counts and keeps byte counts as the secondary file-size measure.
    fn summary(&self) -> String {
        if self.mode == WorkspacePatchFileMode::DeleteExisting {
            return format!(
                "Delete {} ({} lines, {} bytes).",
                self.relative.display, self.lines_before, self.bytes_before
            );
        }
        format!(
            "Apply {} hunk(s) in {} ({} -> {} lines, {} -> {} bytes).",
            self.hunks,
            self.relative.display,
            self.lines_before,
            self.lines_after,
            self.bytes_before,
            self.bytes_after
        )
    }

    pub(super) fn file_fingerprint_before(&self) -> String {
        stable_content_fingerprint(self.content_before.as_bytes())
    }

    pub(super) fn file_fingerprint_after(&self) -> String {
        stable_content_fingerprint(self.replacement.as_bytes())
    }

    fn change_evidence(&self) -> Result<WorkspacePatchChangeEvidence, ActionProposalError> {
        WorkspacePatchChangeEvidence::new(
            self.relative.display.clone(),
            self.preimage_bytes,
            self.replacement_bytes,
            self.bytes_before,
            self.bytes_after,
            self.file_fingerprint_before(),
            self.file_fingerprint_after(),
        )
    }
}

fn match_approved_patch_proposal(
    approved: &WorkspacePatchProposal,
    plan: &WorkspacePatchPlan,
) -> Result<(), ()> {
    let planned_changes = plan
        .changes
        .iter()
        .map(WorkspacePatchFilePlan::change_evidence)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ())?;
    (approved.changes() == planned_changes.as_slice())
        .then_some(())
        .ok_or(())
}

fn proposal_mismatch_outcome(path: String) -> ToolExecutionOutcome {
    failed_outcome(
        APPLY_PATCH_TOOL,
        ERROR_PROPOSAL_MISMATCH,
        WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE,
        Some(path),
    )
}

fn plan_apply_patch_file(
    state: &WorkspaceToolState,
    file_patch: WorkspacePatchFile,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchFilePlan, WorkspacePatchFilePlanError> {
    match file_patch.operation {
        WorkspacePatchOperation::Add { lines } => {
            let relative = validate_workspace_path_argument(&file_patch.path, &state.roots)
                .map_err(WorkspacePatchFilePlanError::Path)?;
            // Failures report the normalized workspace-relative path so a
            // section that named the file with an absolute path never sends a
            // host path back through a tool result.
            let display = relative.display.clone();
            validate_patch_write_boundary(state, &relative).map_err(|error| {
                WorkspacePatchFilePlanError::Domain {
                    error,
                    path: display.clone(),
                }
            })?;

            // Match Update's first-root-wins rule: Add creates at the first
            // root reporting Missing and refuses an existing target immediately.
            // If every root is missing the parent, fall back to the first root;
            // execution will create those parents after the same path checks.
            let mut first_parent_missing = None;
            for root in &state.roots {
                if is_cancelled() {
                    return Err(WorkspacePatchFilePlanError::Cancelled);
                }

                match resolve_new_file_path(root, &relative) {
                    Ok(NewWorkspacePath::Missing(path)) => {
                        return plan_new_apply_patch_file(
                            relative,
                            path,
                            lines,
                            state,
                            is_cancelled,
                        )
                        .map_err(|error| match error {
                            BlockingToolError::Domain(error) => {
                                WorkspacePatchFilePlanError::Domain {
                                    error,
                                    path: display.clone(),
                                }
                            }
                            BlockingToolError::Cancelled => WorkspacePatchFilePlanError::Cancelled,
                        });
                    }
                    Ok(NewWorkspacePath::Existing) => {
                        return Err(WorkspacePatchFilePlanError::Domain {
                            error: DomainError::new(
                                ERROR_FILE_ALREADY_EXISTS,
                                "workspace file already exists",
                            ),
                            path: relative.display,
                        });
                    }
                    Ok(NewWorkspacePath::ParentMissing) => {
                        first_parent_missing.get_or_insert_with(|| relative.resolved(root));
                    }
                    Err(error) => {
                        return Err(WorkspacePatchFilePlanError::Domain {
                            error,
                            path: relative.display,
                        });
                    }
                }
            }

            if let Some(path) = first_parent_missing {
                let display = relative.display.clone();
                return plan_new_apply_patch_file(relative, path, lines, state, is_cancelled)
                    .map_err(|error| match error {
                        BlockingToolError::Domain(error) => WorkspacePatchFilePlanError::Domain {
                            error,
                            path: display,
                        },
                        BlockingToolError::Cancelled => WorkspacePatchFilePlanError::Cancelled,
                    });
            }

            Err(WorkspacePatchFilePlanError::Domain {
                error: DomainError::new(
                    ERROR_FILE_NOT_FOUND,
                    "workspace file parent was not found",
                ),
                path: relative.display,
            })
        }
        WorkspacePatchOperation::Update { hunks } => {
            let (relative, path) =
                resolve_existing_patch_path(state, &file_patch.path, is_cancelled)?;
            let display = relative.display.clone();
            plan_resolved_apply_patch_file(
                relative,
                path,
                hunks,
                file_patch.ignored_context_hunks,
                state,
                is_cancelled,
            )
            .map_err(|error| file_plan_error(error, display))
        }
        WorkspacePatchOperation::Delete => {
            let (relative, path) =
                resolve_existing_patch_path(state, &file_patch.path, is_cancelled)?;
            let display = relative.display.clone();
            plan_resolved_apply_patch_delete(relative, path, state, is_cancelled)
                .map_err(|error| file_plan_error(error, display))
        }
    }
}

/// Resolves the workspace path of a file that a patch section edits or deletes.
///
/// Update and delete sections share the same path rules: the requested path is
/// validated against hidden-path and write-scope policy, then resolved through
/// the configured roots with first-root-wins semantics, and a missing file is
/// reported as `ERROR_FILE_NOT_FOUND`.
fn resolve_existing_patch_path(
    state: &WorkspaceToolState,
    requested: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(ValidatedToolPath, PathBuf), WorkspacePatchFilePlanError> {
    let relative = validate_workspace_path_argument(requested, &state.roots)
        .map_err(WorkspacePatchFilePlanError::Path)?;
    validate_patch_write_boundary(state, &relative).map_err(|error| {
        WorkspacePatchFilePlanError::Domain {
            error,
            path: relative.display.clone(),
        }
    })?;

    for root in &state.roots {
        if is_cancelled() {
            return Err(WorkspacePatchFilePlanError::Cancelled);
        }

        match resolve_existing_path(root, &relative) {
            Ok(Some(resolved)) => return Ok((relative, resolved.path)),
            Ok(None) => {}
            Err(error) => {
                return Err(WorkspacePatchFilePlanError::Domain {
                    error,
                    path: relative.display,
                });
            }
        }
    }

    Err(WorkspacePatchFilePlanError::Domain {
        error: DomainError::new(ERROR_FILE_NOT_FOUND, "workspace file was not found"),
        path: relative.display,
    })
}

/// Maps a blocking tool error onto a file-plan failure for the requested path.
fn file_plan_error(error: BlockingToolError, path: String) -> WorkspacePatchFilePlanError {
    match error {
        BlockingToolError::Domain(error) => WorkspacePatchFilePlanError::Domain { error, path },
        BlockingToolError::Cancelled => WorkspacePatchFilePlanError::Cancelled,
    }
}

fn validate_patch_write_boundary(
    state: &WorkspaceToolState,
    path: &ValidatedToolPath,
) -> Result<(), DomainError> {
    // Scope patterns are root-relative, so a target outside every configured
    // root has no scope spelling and cannot be authorized by a relative
    // pattern. That keeps a child agent inside the scope its parent gave it,
    // which is a deliberate narrowing rather than sandbox policy.
    let scope_path = state.scope_path(path);

    if let Some(scope_path) = scope_path.as_deref()
        && matches_any_scope_path(scope_path, &state.forbidden_paths)
    {
        return Err(DomainError::new(
            ERROR_PATH_DENIED,
            "workspace patch path is forbidden by the child workspace scope",
        ));
    }

    let Some(write_scope) = &state.patch_write_scope else {
        return Ok(());
    };
    match scope_path.as_deref() {
        Some(scope_path) if matches_any_scope_path(scope_path, write_scope) => Ok(()),
        _ => Err(DomainError::new(
            ERROR_PATH_DENIED,
            "workspace patch path is outside the child write scope",
        )),
    }
}

#[derive(Debug)]
enum WorkspacePatchFilePlanError {
    Path(PathValidationError),
    Domain { error: DomainError, path: String },
    Cancelled,
}

fn plan_resolved_apply_patch_file(
    relative: ValidatedToolPath,
    path: PathBuf,
    hunks: Vec<WorkspacePatchHunk>,
    ignored_context_hunks: usize,
    state: &WorkspaceToolState,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchFilePlan, BlockingToolError> {
    let content = read_patch_preimage(&path, state, is_cancelled)?;

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let replacement = build_patch_replacement(&content, &hunks)?;
    if replacement.text.len() > state.limits.max_write_bytes {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace patch result exceeds the configured write limit",
        )
        .into());
    }

    Ok(WorkspacePatchFilePlan {
        relative,
        path,
        lines_before: count_file_lines(&content),
        lines_after: count_file_lines(&replacement.text),
        bytes_before: content.len(),
        bytes_after: replacement.text.len(),
        preimage_bytes: replacement.preimage_bytes,
        replacement_bytes: replacement.replacement_bytes,
        hunks: hunks.len(),
        ignored_context_hunks,
        lines: replacement.lines,
        content_before: content,
        replacement: replacement.text,
        max_read_bytes: state.limits.max_read_bytes,
        mode: WorkspacePatchFileMode::UpdateExisting,
    })
}

/// Plans the removal of an existing file.
///
/// The plan keeps the preimage bytes and the file size so evidence, approval,
/// and write-time verification use the same contract as an update: the file is
/// read and compared before it is unlinked.
fn plan_resolved_apply_patch_delete(
    relative: ValidatedToolPath,
    path: PathBuf,
    state: &WorkspaceToolState,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchFilePlan, BlockingToolError> {
    let content = read_patch_preimage(&path, state, is_cancelled)?;

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    Ok(WorkspacePatchFilePlan {
        lines_before: count_file_lines(&content),
        lines_after: 0,
        bytes_before: content.len(),
        bytes_after: 0,
        preimage_bytes: content.len(),
        replacement_bytes: 0,
        hunks: 0,
        ignored_context_hunks: 0,
        lines: Vec::new(),
        content_before: content,
        replacement: String::new(),
        relative,
        path,
        max_read_bytes: state.limits.max_read_bytes,
        mode: WorkspacePatchFileMode::DeleteExisting,
    })
}

fn plan_new_apply_patch_file(
    relative: ValidatedToolPath,
    path: PathBuf,
    lines: Vec<String>,
    state: &WorkspaceToolState,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchFilePlan, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let replacement = build_new_file_replacement(&lines);
    if replacement.text.len() > state.limits.max_write_bytes {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace patch result exceeds the configured write limit",
        )
        .into());
    }

    Ok(WorkspacePatchFilePlan {
        relative,
        path,
        lines_before: 0,
        lines_after: count_file_lines(&replacement.text),
        bytes_before: 0,
        bytes_after: replacement.text.len(),
        preimage_bytes: replacement.preimage_bytes,
        replacement_bytes: replacement.replacement_bytes,
        hunks: 1,
        ignored_context_hunks: 0,
        lines: replacement.lines,
        content_before: String::new(),
        replacement: replacement.text,
        max_read_bytes: state.limits.max_read_bytes,
        mode: WorkspacePatchFileMode::CreateNew,
    })
}

fn read_patch_preimage(
    path: &Path,
    state: &WorkspaceToolState,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, BlockingToolError> {
    read_patch_preimage_for_path(path, state.limits.max_read_bytes, is_cancelled)
}

pub(super) fn read_patch_preimage_for_path(
    path: &Path,
    max_read_bytes: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let pre_open_metadata = fs::symlink_metadata(path).map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace file metadata",
        )
    })?;
    if pre_open_metadata.file_type().is_symlink() {
        return Err(DomainError::new(ERROR_PATH_DENIED, "workspace path uses a symlink").into());
    }
    if !pre_open_metadata.is_file() {
        return Err(
            DomainError::new(ERROR_NOT_FILE, "workspace path is not a regular file").into(),
        );
    }

    let mut file = open_file_for_read(path)?;
    let metadata = file.metadata().map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace file metadata",
        )
    })?;

    if !metadata.is_file() {
        return Err(
            DomainError::new(ERROR_NOT_FILE, "workspace path is not a regular file").into(),
        );
    }

    if metadata.len() > max_read_bytes as u64 {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
        .into());
    }

    let file_size = usize::try_from(metadata.len()).map_err(|_| {
        DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
    })?;

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let mut bytes = Vec::with_capacity(file_size);
    Read::by_ref(&mut file)
        .take(metadata.len())
        .read_to_end(&mut bytes)
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace file"))?;

    if bytes.len() > max_read_bytes {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
        .into());
    }

    if bytes.contains(&0) {
        return Err(DomainError::new(ERROR_NOT_UTF8, "workspace file appears to be binary").into());
    }

    let content = String::from_utf8(bytes)
        .map_err(|_| DomainError::new(ERROR_NOT_UTF8, "workspace file is not valid UTF-8"))?;

    Ok(content)
}
