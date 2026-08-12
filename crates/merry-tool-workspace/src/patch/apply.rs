use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
};

use merry_runtime::{
    ActionExecutionEvidence, ToolExecutionError, ToolExecutionOutcome,
    WorkspacePatchChangeEvidence, WorkspacePatchExecutionEvidence,
};

#[cfg(test)]
use crate::trace::maybe_run_patch_test_after_write_hook;
use crate::{
    WORKSPACE_PATCH_TOOL,
    errors::{
        BlockingToolError, DomainError, ERROR_FILE_TOO_LARGE, ERROR_NOT_FILE, ERROR_READ_FAILED,
        ERROR_WRITE_FAILED, failed_outcome,
    },
    path::{open_file_for_patch, open_file_for_patch_create_new},
};

use super::{
    plan::{
        WorkspacePatchFileMode, WorkspacePatchFilePlan, WorkspacePatchPlan,
        read_patch_preimage_for_path,
    },
    types::{WorkspacePatchSuccess, WorkspacePatchSuccessChange, stable_content_fingerprint},
};

pub(super) fn execute_workspace_patch_plan(
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
        let content_after = match execute_workspace_patch_file_plan(&change, is_cancelled) {
            Ok(content_after) => content_after,
            Err(PatchFileWriteError::Outcome(outcome)) => return Ok(*outcome),
            Err(PatchFileWriteError::Cancelled) => return Err(ToolExecutionError::Cancelled),
        };
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
            hunks: change.hunks,
            bytes_before: change.bytes_before,
            bytes_after: content_after.len(),
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
        tool: WORKSPACE_PATCH_TOOL,
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

fn execute_workspace_patch_file_plan(
    plan: &WorkspacePatchFilePlan,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, PatchFileWriteError> {
    let relative_display = plan.relative.display.clone();
    if is_cancelled() {
        return Err(PatchFileWriteError::Cancelled);
    }
    let mut file = match if plan.mode == WorkspacePatchFileMode::CreateNew {
        open_file_for_patch_create_new(&plan.path)
    } else {
        open_file_for_patch(&plan.path)
    } {
        Ok(file) => file,
        Err(error) => {
            return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                WORKSPACE_PATCH_TOOL,
                error.code,
                error.message,
                Some(relative_display),
            ))));
        }
    };
    if plan.mode == WorkspacePatchFileMode::UpdateExisting {
        match read_open_patch_file_before_write(&mut file, plan.max_read_bytes, is_cancelled) {
            Ok(bytes) if bytes == plan.content_before.as_bytes() => {}
            Ok(_) => {
                return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                    WORKSPACE_PATCH_TOOL,
                    ERROR_WRITE_FAILED,
                    "workspace file changed before patch write",
                    Some(relative_display),
                ))));
            }
            Err(BlockingToolError::Domain(error)) => {
                return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                    WORKSPACE_PATCH_TOOL,
                    error.code,
                    error.message,
                    Some(relative_display),
                ))));
            }
            Err(BlockingToolError::Cancelled) => return Err(PatchFileWriteError::Cancelled),
        }
    }
    if file.seek(SeekFrom::Start(0)).is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not seek workspace file",
            Some(relative_display),
        ))));
    }
    if file.write_all(plan.replacement.as_bytes()).is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not write workspace file",
            Some(relative_display),
        ))));
    }
    if file.set_len(plan.replacement.len() as u64).is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not truncate workspace file",
            Some(relative_display),
        ))));
    }
    if file.sync_all().is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            WORKSPACE_PATCH_TOOL,
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
                    WORKSPACE_PATCH_TOOL,
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
            WORKSPACE_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "workspace patch verification failed after write",
            Some(relative_display),
        ))));
    }

    Ok(content_after)
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
