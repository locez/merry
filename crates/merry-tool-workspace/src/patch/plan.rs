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
    WORKSPACE_PATCH_TOOL,
    errors::{
        BlockingToolError, DomainError, ERROR_FILE_NOT_FOUND, ERROR_FILE_TOO_LARGE,
        ERROR_INVALID_ARGUMENTS, ERROR_NOT_FILE, ERROR_NOT_UTF8, ERROR_PATH_DENIED,
        ERROR_PROPOSAL_MISMATCH, ERROR_READ_FAILED, PathValidationError,
        WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE, failed_outcome,
    },
    path::{
        ValidatedRelativePath, open_file_for_read, resolve_existing_path, validate_relative_path,
    },
    schema::WorkspacePatchArgs,
    state::{WorkspaceToolState, matches_any_scope_path},
};

use super::{
    apply::execute_workspace_patch_plan,
    parse::parse_workspace_patch,
    types::{
        WorkspacePatchFile, WorkspacePatchHunk, WorkspacePatchOperation, build_patch_replacement,
        stable_content_fingerprint,
    },
};

#[cfg(test)]
pub(crate) fn workspace_patch_blocking(
    state: &WorkspaceToolState,
    args: WorkspacePatchArgs,
) -> ToolExecutionOutcome {
    workspace_patch_blocking_checked(state, args, None, &|| false)
        .expect("uncancelled workspace patch should not return cancellation")
}

pub(crate) fn propose_workspace_patch_blocking_checked(
    state: &WorkspaceToolState,
    args: WorkspacePatchArgs,
    call: &PendingToolCall,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolActionPreflight, ToolExecutionError> {
    match plan_workspace_patch_blocking_checked(state, args, is_cancelled) {
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

pub(crate) fn workspace_patch_blocking_checked(
    state: &WorkspaceToolState,
    args: WorkspacePatchArgs,
    approved_proposal: Option<&WorkspacePatchProposal>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    match plan_workspace_patch_blocking_checked(state, args, is_cancelled)? {
        WorkspacePatchPlanOutcome::Planned(plan) => {
            if let Some(approved) = approved_proposal {
                if match_approved_patch_proposal(approved, &plan).is_err() {
                    return Ok(proposal_mismatch_outcome(plan.subject()));
                }
            }
            execute_workspace_patch_plan(plan, is_cancelled)
        }
        WorkspacePatchPlanOutcome::Failure(outcome) => Ok(outcome),
    }
}

fn plan_workspace_patch_blocking_checked(
    state: &WorkspaceToolState,
    args: WorkspacePatchArgs,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchPlanOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }

    if args.patch.trim().is_empty() {
        return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace patch must not be empty",
            None::<String>,
        )));
    }

    if args.patch.contains('\0') {
        return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace patch must not contain NUL bytes",
            None::<String>,
        )));
    }

    if args.patch.len() > state.limits.max_patch_bytes {
        return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace patch payload exceeds the configured byte limit; retry with the smallest unique hunk needed for the edit",
            None::<String>,
        )));
    }

    let patch = match parse_workspace_patch(&args.patch) {
        Ok(patch) => patch,
        Err(error) => {
            return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
                WORKSPACE_PATCH_TOOL,
                ERROR_INVALID_ARGUMENTS,
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
        match plan_workspace_patch_file(state, file_patch, is_cancelled) {
            Ok(change) => changes.push(change),
            Err(WorkspacePatchFilePlanError::Domain { error, path }) => {
                return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
                    WORKSPACE_PATCH_TOOL,
                    error.code,
                    error.message,
                    Some(path),
                )));
            }
            Err(WorkspacePatchFilePlanError::Path(error)) => {
                return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
                    WORKSPACE_PATCH_TOOL,
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
            [change] => format!(
                "Apply {} hunk(s) in {} ({} bytes -> {} bytes).",
                change.hunks, change.relative.display, change.bytes_before, change.bytes_after
            ),
            changes => {
                let bytes_before = changes.iter().fold(0usize, |sum, change| {
                    sum.saturating_add(change.bytes_before)
                });
                let bytes_after = changes
                    .iter()
                    .fold(0usize, |sum, change| sum.saturating_add(change.bytes_after));
                format!(
                    "Apply workspace patch to {} files ({} bytes -> {} bytes).",
                    changes.len(),
                    bytes_before,
                    bytes_after
                )
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct WorkspacePatchFilePlan {
    pub(super) relative: ValidatedRelativePath,
    pub(super) path: PathBuf,
    pub(super) content_before: String,
    pub(super) replacement: String,
    pub(super) preimage_bytes: usize,
    pub(super) replacement_bytes: usize,
    pub(super) bytes_before: usize,
    pub(super) bytes_after: usize,
    pub(super) hunks: usize,
    pub(super) max_read_bytes: usize,
}

impl WorkspacePatchFilePlan {
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
        WORKSPACE_PATCH_TOOL,
        ERROR_PROPOSAL_MISMATCH,
        WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE,
        Some(path),
    )
}

fn plan_workspace_patch_file(
    state: &WorkspaceToolState,
    file_patch: WorkspacePatchFile,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchFilePlan, WorkspacePatchFilePlanError> {
    match file_patch.operation {
        WorkspacePatchOperation::Update { hunks } => {
            let relative = validate_relative_path(&file_patch.path, state.allow_hidden)
                .map_err(WorkspacePatchFilePlanError::Path)?;
            validate_patch_write_boundary(state, &relative).map_err(|error| {
                WorkspacePatchFilePlanError::Domain {
                    error,
                    path: file_patch.path.clone(),
                }
            })?;

            for root in &state.roots {
                if is_cancelled() {
                    return Err(WorkspacePatchFilePlanError::Cancelled);
                }

                match resolve_existing_path(root, &relative) {
                    Ok(Some(resolved)) => {
                        return plan_resolved_workspace_patch_file(
                            relative,
                            resolved.path,
                            hunks,
                            state,
                            is_cancelled,
                        )
                        .map_err(|error| match error {
                            BlockingToolError::Domain(error) => {
                                WorkspacePatchFilePlanError::Domain {
                                    error,
                                    path: file_patch.path,
                                }
                            }
                            BlockingToolError::Cancelled => WorkspacePatchFilePlanError::Cancelled,
                        });
                    }
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
    }
}

fn validate_patch_write_boundary(
    state: &WorkspaceToolState,
    relative: &ValidatedRelativePath,
) -> Result<(), DomainError> {
    if matches_any_scope_path(&relative.display, &state.forbidden_paths) {
        return Err(DomainError::new(
            ERROR_PATH_DENIED,
            "workspace patch path is forbidden by the child workspace scope",
        ));
    }

    let Some(write_scope) = &state.patch_write_scope else {
        return Ok(());
    };
    if matches_any_scope_path(&relative.display, write_scope) {
        Ok(())
    } else {
        Err(DomainError::new(
            ERROR_PATH_DENIED,
            "workspace patch path is outside the child write scope",
        ))
    }
}

#[derive(Debug)]
enum WorkspacePatchFilePlanError {
    Path(PathValidationError),
    Domain { error: DomainError, path: String },
    Cancelled,
}

fn plan_resolved_workspace_patch_file(
    relative: ValidatedRelativePath,
    path: PathBuf,
    hunks: Vec<WorkspacePatchHunk>,
    state: &WorkspaceToolState,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchFilePlan, BlockingToolError> {
    let content = read_patch_preimage(&path, state, is_cancelled)?;

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let (replacement, preimage_bytes, replacement_bytes) =
        build_patch_replacement(&content, &hunks)?;
    if replacement.len() > state.limits.max_write_bytes {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace patch result exceeds the configured write limit",
        )
        .into());
    }

    Ok(WorkspacePatchFilePlan {
        relative,
        path,
        bytes_before: content.len(),
        bytes_after: replacement.len(),
        preimage_bytes,
        replacement_bytes,
        hunks: hunks.len(),
        content_before: content,
        replacement,
        max_read_bytes: state.limits.max_read_bytes,
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
