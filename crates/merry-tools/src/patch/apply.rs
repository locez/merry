use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
};

use merry_runtime::{
    ActionExecutionEvidence, ToolExecutionError, ToolExecutionOutcome,
    WorkspacePatchChangeEvidence, WorkspacePatchExecutionEvidence,
};

#[cfg(test)]
use crate::trace::{
    maybe_run_patch_test_after_write_hook, maybe_run_patch_test_before_mutation_hook,
};
use crate::{
    APPLY_PATCH_TOOL,
    errors::{
        BlockingToolError, DomainError, ERROR_FILE_TOO_LARGE, ERROR_NOT_FILE, ERROR_READ_FAILED,
        ERROR_WRITE_FAILED, failed_outcome,
    },
    path::{open_file_for_patch, open_file_for_patch_create_new, open_file_for_read},
};

use super::{
    envelope::{WorkspacePatchSuccess, WorkspacePatchSuccessChange},
    plan::{
        WorkspacePatchFileMode, WorkspacePatchFilePlan, WorkspacePatchPlan,
        read_patch_preimage_for_path,
    },
    types::{count_file_lines, stable_content_fingerprint},
};

pub(super) fn execute_apply_patch_plan(
    plan: WorkspacePatchPlan,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }

    let mut written_changes = Vec::with_capacity(plan.changes.len());
    let mut evidence_changes = Vec::with_capacity(plan.changes.len());

    for change in plan.changes {
        if is_cancelled() {
            return Err(ToolExecutionError::Cancelled);
        }
        let relative_display = change.relative.display.clone();
        let operation = change.mode.operation_kind();
        let ignored_context_hunks = change.ignored_context_hunks;
        let lines_before = change.lines_before;
        let content_after = match execute_apply_patch_file_plan(&change, is_cancelled) {
            Ok(content_after) => content_after,
            Err(PatchFileWriteError::Outcome(outcome)) => return Ok(*outcome),
            Err(PatchFileWriteError::Cancelled) => return Err(ToolExecutionError::Cancelled),
        };
        let lines_after = count_file_lines(&content_after);
        evidence_changes.push(
            WorkspacePatchChangeEvidence::new(
                relative_display.clone(),
                change.preimage_bytes,
                change.replacement_bytes,
                change.bytes_before,
                content_after.len(),
                stable_content_fingerprint(change.content_before.as_bytes()),
                stable_content_fingerprint(content_after.as_bytes()),
            )
            .map_err(|error| {
                ToolExecutionError::infrastructure(format!(
                    "workspace patch execution evidence was invalid: {error}"
                ))
            })?,
        );
        written_changes.push(WorkspacePatchSuccessChange {
            path: relative_display,
            op: Some(operation),
            hunks: change.hunks,
            lines_before: Some(lines_before),
            lines_after: Some(lines_after),
            bytes_before: change.bytes_before,
            bytes_after: content_after.len(),
            ignored_context_hunks,
            lines: change.lines,
        });
    }

    let evidence =
        WorkspacePatchExecutionEvidence::from_changes(evidence_changes).map_err(|error| {
            ToolExecutionError::infrastructure(format!(
                "workspace patch execution evidence was invalid: {error}"
            ))
        })?;
    let payload = WorkspacePatchSuccess {
        ok: true,
        tool: APPLY_PATCH_TOOL.to_owned(),
        changes: written_changes,
    };
    Ok(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("workspace patch success envelope serializes"),
    )
    .with_execution_evidence(ActionExecutionEvidence::WorkspacePatch(evidence)))
}

enum PatchFileWriteError {
    Outcome(Box<ToolExecutionOutcome>),
    Cancelled,
}

/// Opens the planned file with the access mode its operation needs.
///
/// An update rewrites the file, so it opens read-write. A delete only reads the
/// file to verify its preimage and then unlinks it, so it opens read-only:
/// removal needs write permission on the parent directory, not on the file.
fn open_planned_patch_file(plan: &WorkspacePatchFilePlan) -> Result<fs::File, DomainError> {
    match plan.mode {
        WorkspacePatchFileMode::CreateNew => open_file_for_patch_create_new(&plan.path),
        WorkspacePatchFileMode::UpdateExisting => open_file_for_patch(&plan.path),
        WorkspacePatchFileMode::DeleteExisting => open_file_for_read(&plan.path),
    }
}

fn execute_apply_patch_file_plan(
    plan: &WorkspacePatchFilePlan,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, PatchFileWriteError> {
    let relative_display = plan.relative.display.clone();
    if is_cancelled() {
        return Err(PatchFileWriteError::Cancelled);
    }

    #[cfg(test)]
    maybe_run_patch_test_before_mutation_hook(&plan.path);

    let mut file = match open_planned_patch_file(plan) {
        Ok(file) => file,
        Err(error) => {
            return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                APPLY_PATCH_TOOL,
                error.code,
                error.message,
                Some(relative_display),
            ))));
        }
    };
    // Re-read the preimage immediately before the mutation. This closes the
    // window between planning and acting: an edit, replacement, or removal that
    // landed after the plan was built must fail here instead of overwriting or
    // unlinking bytes the caller never saw. Updates and deletes share it so a
    // stale delete cannot report evidence for content it did not remove.
    if plan.mode != WorkspacePatchFileMode::CreateNew {
        match read_open_patch_file_before_write(&mut file, plan.max_read_bytes, is_cancelled) {
            Ok(bytes) if bytes == plan.content_before.as_bytes() => {}
            Ok(_) => {
                return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                    APPLY_PATCH_TOOL,
                    ERROR_WRITE_FAILED,
                    "workspace file changed after the patch was planned; re-read the target file and submit a fresh patch",
                    Some(relative_display),
                ))));
            }
            Err(BlockingToolError::Domain(error)) => {
                return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                    APPLY_PATCH_TOOL,
                    error.code,
                    error.message,
                    Some(relative_display),
                ))));
            }
            Err(BlockingToolError::Cancelled) => return Err(PatchFileWriteError::Cancelled),
        }
    }
    if plan.mode == WorkspacePatchFileMode::DeleteExisting {
        drop(file);
        return delete_apply_patch_file(plan, relative_display);
    }
    if file.seek(SeekFrom::Start(0)).is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            APPLY_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not seek workspace file",
            Some(relative_display),
        ))));
    }
    if file.write_all(plan.replacement.as_bytes()).is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            APPLY_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not write workspace file",
            Some(relative_display),
        ))));
    }
    if file.set_len(plan.replacement.len() as u64).is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            APPLY_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not truncate workspace file",
            Some(relative_display),
        ))));
    }
    if file.sync_all().is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            APPLY_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not sync workspace file",
            Some(relative_display),
        ))));
    }
    drop(file);

    #[cfg(test)]
    maybe_run_patch_test_after_write_hook(&plan.path);

    let content_after =
        match read_patch_preimage_for_path(&plan.path, plan.replacement.len(), &|| false) {
            Ok(content) => content,
            Err(BlockingToolError::Domain(error)) => {
                return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                    APPLY_PATCH_TOOL,
                    error.code,
                    error.message,
                    Some(relative_display),
                ))));
            }
            Err(BlockingToolError::Cancelled) => {
                unreachable!("post-write readback is not cancellable")
            }
        };

    if content_after != plan.replacement {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            APPLY_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "workspace patch verification failed after write",
            Some(relative_display),
        ))));
    }

    Ok(content_after)
}

/// Unlinks a planned file whose preimage was verified immediately before this
/// call.
///
/// Deletion is verified the same way a write is: the plan recorded the exact
/// preimage and byte count, the caller compared the current bytes against it,
/// and success requires the path to be absent afterwards. A partial or blocked
/// removal therefore reports a failure instead of claiming the file is gone.
fn delete_apply_patch_file(
    plan: &WorkspacePatchFilePlan,
    relative_display: String,
) -> Result<String, PatchFileWriteError> {
    if fs::remove_file(&plan.path).is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            APPLY_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not delete workspace file",
            Some(relative_display),
        ))));
    }

    #[cfg(test)]
    maybe_run_patch_test_after_write_hook(&plan.path);

    match fs::symlink_metadata(&plan.path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        _ => Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            APPLY_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "workspace file still exists after delete",
            Some(relative_display),
        )))),
    }
}

fn read_open_patch_file_before_write(
    file: &mut fs::File,
    max_read_bytes: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

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

    if file.seek(SeekFrom::Start(0)).is_err() {
        return Err(DomainError::new(ERROR_READ_FAILED, "could not seek workspace file").into());
    }

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).map_err(|_| {
        DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
    })?);
    Read::by_ref(file)
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
    Ok(bytes)
}
