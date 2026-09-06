use crate::tool::proposal::ActionProposalError;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

/// Per-file metadata for a constrained workspace patch change.
///
/// This stores only relative workspace identity, byte counts, and stable
/// non-cryptographic content fingerprints. It does not store old or new text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePatchChangeEvidence {
    pub(super) relative_path: String,
    pub(super) preimage_bytes: usize,
    pub(super) replacement_bytes: usize,
    pub(super) file_bytes_before: usize,
    pub(super) file_bytes_after: usize,
    pub(super) file_fingerprint_before: String,
    pub(super) file_fingerprint_after: String,
}

impl WorkspacePatchChangeEvidence {
    /// Creates validated metadata for one file change in a workspace patch.
    pub fn new(
        relative_path: impl Into<String>,
        preimage_bytes: usize,
        replacement_bytes: usize,
        file_bytes_before: usize,
        file_bytes_after: usize,
        file_fingerprint_before: impl Into<String>,
        file_fingerprint_after: impl Into<String>,
    ) -> Result<Self, ActionProposalError> {
        let relative_path = validate_apply_patch_relative_path(relative_path.into())?;
        validate_apply_patch_counts(
            preimage_bytes,
            replacement_bytes,
            file_bytes_before,
            file_bytes_after,
        )?;
        let file_fingerprint_before = validate_apply_patch_fingerprint(
            "file_fingerprint_before",
            file_fingerprint_before.into(),
        )?;
        let file_fingerprint_after = validate_apply_patch_fingerprint(
            "file_fingerprint_after",
            file_fingerprint_after.into(),
        )?;

        Ok(Self {
            relative_path,
            preimage_bytes,
            replacement_bytes,
            file_bytes_before,
            file_bytes_after,
            file_fingerprint_before,
            file_fingerprint_after,
        })
    }

    /// Returns the workspace-relative path using `/` separators.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Returns the byte length of the matched preimage.
    #[must_use]
    pub fn preimage_bytes(&self) -> usize {
        self.preimage_bytes
    }

    /// Returns the byte length of the replacement text.
    #[must_use]
    pub fn replacement_bytes(&self) -> usize {
        self.replacement_bytes
    }

    /// Returns the file size immediately before replacement.
    #[must_use]
    pub fn file_bytes_before(&self) -> usize {
        self.file_bytes_before
    }

    /// Returns the file size observed after replacement was written and read back.
    #[must_use]
    pub fn file_bytes_after(&self) -> usize {
        self.file_bytes_after
    }

    /// Returns the stable non-cryptographic fingerprint before replacement.
    #[must_use]
    pub fn file_fingerprint_before(&self) -> &str {
        &self.file_fingerprint_before
    }

    /// Returns the stable non-cryptographic fingerprint after replacement.
    #[must_use]
    pub fn file_fingerprint_after(&self) -> &str {
        &self.file_fingerprint_after
    }
}

/// Execute-time metadata for a constrained workspace patch.
///
/// This stores one or more file changes. It intentionally does not store old or
/// new text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePatchExecutionEvidence {
    pub(super) changes: Vec<WorkspacePatchChangeEvidence>,
}

impl WorkspacePatchExecutionEvidence {
    /// Creates validated execute-time metadata for a single workspace patch.
    pub fn new(
        relative_path: impl Into<String>,
        preimage_bytes: usize,
        replacement_bytes: usize,
        file_bytes_before: usize,
        file_bytes_after: usize,
        file_fingerprint_before: impl Into<String>,
        file_fingerprint_after: impl Into<String>,
    ) -> Result<Self, ActionProposalError> {
        Self::from_changes(vec![WorkspacePatchChangeEvidence::new(
            relative_path,
            preimage_bytes,
            replacement_bytes,
            file_bytes_before,
            file_bytes_after,
            file_fingerprint_before,
            file_fingerprint_after,
        )?])
    }

    /// Creates execute-time metadata for a multi-file workspace patch.
    pub fn from_changes(
        changes: Vec<WorkspacePatchChangeEvidence>,
    ) -> Result<Self, ActionProposalError> {
        validate_apply_patch_changes(&changes)?;
        Ok(Self { changes })
    }

    /// Returns all file changes included in this patch.
    #[must_use]
    pub fn changes(&self) -> &[WorkspacePatchChangeEvidence] {
        &self.changes
    }

    pub(super) fn first_change(&self) -> &WorkspacePatchChangeEvidence {
        self.changes
            .first()
            .expect("workspace patch execution evidence always has at least one change")
    }

    /// Returns the first workspace-relative path using `/` separators.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        self.first_change().relative_path()
    }

    /// Returns the byte length of the first matched preimage.
    #[must_use]
    pub fn preimage_bytes(&self) -> usize {
        self.first_change().preimage_bytes()
    }

    /// Returns the byte length of the first replacement text.
    #[must_use]
    pub fn replacement_bytes(&self) -> usize {
        self.first_change().replacement_bytes()
    }

    /// Returns the first file size immediately before replacement.
    #[must_use]
    pub fn file_bytes_before(&self) -> usize {
        self.first_change().file_bytes_before()
    }

    /// Returns the first file size observed after replacement was written and read back.
    #[must_use]
    pub fn file_bytes_after(&self) -> usize {
        self.first_change().file_bytes_after()
    }

    /// Returns the first stable non-cryptographic fingerprint before replacement.
    #[must_use]
    pub fn file_fingerprint_before(&self) -> &str {
        self.first_change().file_fingerprint_before()
    }

    /// Returns the first stable non-cryptographic fingerprint after replacement.
    #[must_use]
    pub fn file_fingerprint_after(&self) -> &str {
        self.first_change().file_fingerprint_after()
    }
}

/// Deterministic metadata for a constrained workspace patch proposal.
///
/// This stores one or more file changes needed for a future edit decision. It
/// does not store old text, new text, host absolute paths, or provider wire data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePatchProposal {
    pub(super) changes: Vec<WorkspacePatchChangeEvidence>,
}

impl WorkspacePatchProposal {
    /// Creates validated metadata for a workspace patch file change. New-file
    /// changes use an empty preimage and a zero-byte file state before writing.
    pub fn new(
        relative_path: impl Into<String>,
        preimage_bytes: usize,
        replacement_bytes: usize,
        file_bytes_before: usize,
        file_bytes_after: usize,
        file_fingerprint_before: impl Into<String>,
        file_fingerprint_after: impl Into<String>,
    ) -> Result<Self, ActionProposalError> {
        Self::from_changes(vec![WorkspacePatchChangeEvidence::new(
            relative_path,
            preimage_bytes,
            replacement_bytes,
            file_bytes_before,
            file_bytes_after,
            file_fingerprint_before,
            file_fingerprint_after,
        )?])
    }

    /// Creates proposal metadata for a multi-file workspace patch.
    pub fn from_changes(
        changes: Vec<WorkspacePatchChangeEvidence>,
    ) -> Result<Self, ActionProposalError> {
        validate_apply_patch_changes(&changes)?;
        Ok(Self { changes })
    }

    /// Returns all file changes included in this patch.
    #[must_use]
    pub fn changes(&self) -> &[WorkspacePatchChangeEvidence] {
        &self.changes
    }

    pub(super) fn first_change(&self) -> &WorkspacePatchChangeEvidence {
        self.changes
            .first()
            .expect("workspace patch proposal always has at least one change")
    }

    /// Returns the first workspace-relative path using `/` separators.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        self.first_change().relative_path()
    }

    /// Returns the byte length of the first matched preimage.
    #[must_use]
    pub fn preimage_bytes(&self) -> usize {
        self.first_change().preimage_bytes()
    }

    /// Returns the byte length of the first replacement text.
    #[must_use]
    pub fn replacement_bytes(&self) -> usize {
        self.first_change().replacement_bytes()
    }

    /// Returns the first file size before replacement.
    #[must_use]
    pub fn file_bytes_before(&self) -> usize {
        self.first_change().file_bytes_before()
    }

    /// Returns the first projected file size after replacement.
    #[must_use]
    pub fn file_bytes_after(&self) -> usize {
        self.first_change().file_bytes_after()
    }

    /// Returns the first stable non-cryptographic fingerprint before replacement.
    #[must_use]
    pub fn file_fingerprint_before(&self) -> &str {
        self.first_change().file_fingerprint_before()
    }

    /// Returns the first projected stable non-cryptographic fingerprint after replacement.
    #[must_use]
    pub fn file_fingerprint_after(&self) -> &str {
        self.first_change().file_fingerprint_after()
    }
}

pub(super) fn validate_apply_patch_changes(
    changes: &[WorkspacePatchChangeEvidence],
) -> Result<(), ActionProposalError> {
    if changes.is_empty() {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "changes",
            reason: "must contain at least one file change",
        });
    }
    Ok(())
}

pub(super) fn validate_apply_patch_counts(
    preimage_bytes: usize,
    replacement_bytes: usize,
    file_bytes_before: usize,
    file_bytes_after: usize,
) -> Result<(), ActionProposalError> {
    if preimage_bytes == 0 && file_bytes_before != 0 {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "preimage_bytes",
            reason: "must be greater than zero unless the file is new",
        });
    }

    let expected_after = file_bytes_before
        .checked_sub(preimage_bytes)
        .and_then(|unchanged| unchanged.checked_add(replacement_bytes))
        .ok_or(ActionProposalError::InvalidWorkspacePatch {
            field: "file_bytes_after",
            reason: "must be consistent with before, preimage, and replacement byte counts",
        })?;
    if expected_after != file_bytes_after {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "file_bytes_after",
            reason: "must equal file_bytes_before - preimage_bytes + replacement_bytes",
        });
    }

    Ok(())
}

pub(super) fn validate_apply_patch_fingerprint(
    field: &'static str,
    value: String,
) -> Result<String, ActionProposalError> {
    if value.trim().is_empty() {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field,
            reason: "must not be blank",
        });
    }
    if value.len() > 128 {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field,
            reason: "exceeds the byte limit",
        });
    }
    let Some((algorithm, digest)) = value.split_once(':') else {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field,
            reason: "must include an algorithm prefix",
        });
    };
    if algorithm != "fnv1a64" {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field,
            reason: "must use the fnv1a64 fingerprint prefix",
        });
    }
    if digest.len() != 16 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field,
            reason: "must include 16 hexadecimal digest characters",
        });
    }

    Ok(value)
}

pub(super) const MAX_WORKSPACE_PATCH_RELATIVE_PATH_BYTES: usize = 4096;

pub(super) fn validate_apply_patch_relative_path(
    value: String,
) -> Result<String, ActionProposalError> {
    if value.trim().is_empty() {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "must not be blank",
        });
    }
    if value.len() > MAX_WORKSPACE_PATCH_RELATIVE_PATH_BYTES {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "exceeds the byte limit",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "must not contain control characters",
        });
    }
    if value.split('/').any(str::is_empty) {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "must not contain empty path segments",
        });
    }

    let path = Path::new(&value);
    if path.is_absolute() {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "must be relative",
        });
    }

    let mut saw_component = false;
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                if value.to_str().is_none() {
                    return Err(ActionProposalError::InvalidWorkspacePatch {
                        field: "relative_path",
                        reason: "components must be UTF-8",
                    });
                }
                saw_component = true;
            }
            Component::CurDir | Component::ParentDir => {
                return Err(ActionProposalError::InvalidWorkspacePatch {
                    field: "relative_path",
                    reason: "must not contain dot segments",
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ActionProposalError::InvalidWorkspacePatch {
                    field: "relative_path",
                    reason: "must be relative",
                });
            }
        }
    }

    if !saw_component {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "must name a file",
        });
    }

    Ok(value)
}
