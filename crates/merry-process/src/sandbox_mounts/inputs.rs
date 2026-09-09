//! Immutable admitted inputs and source-access limits, independent of derived mounts.

use super::{SandboxMountError, absolute};
use crate::resolve_bwrap_path;
use merry_runtime::PathAccess;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(super) struct MountInput {
    pub(super) original_source: PathBuf,
    pub(super) source: PathBuf,
    pub(super) logical: PathBuf,
    pub(super) access: PathAccess,
    pub(super) optional: bool,
    pub(super) directory: bool,
    pub(super) exists: bool,
}

impl MountInput {
    pub(super) fn new(
        source: &Path,
        destination: &Path,
        access: PathAccess,
        optional: bool,
    ) -> Result<Self, SandboxMountError> {
        absolute(source)?;
        absolute(destination)?;
        let resolved = resolve_bwrap_path(source);
        Ok(Self {
            original_source: source.to_path_buf(),
            directory: resolved.is_dir(),
            exists: resolved.exists(),
            source: resolved,
            logical: destination.to_path_buf(),
            access,
            optional,
        })
    }
}

pub(super) struct AdmittedSources<'scope> {
    imports: &'scope [MountInput],
}

impl<'scope> AdmittedSources<'scope> {
    pub(super) fn new(imports: &'scope [MountInput]) -> Self {
        Self { imports }
    }

    pub(super) fn denies(&self, source: &Path) -> bool {
        self.imports
            .iter()
            .any(|input| input.access == PathAccess::Deny && source.starts_with(&input.source))
    }

    /// Derived visibility never becomes authorization or overrides an original grant.
    pub(super) fn access(&self, source: &Path) -> Option<PathAccess> {
        if self.denies(source) {
            return None;
        }
        self.imports
            .iter()
            .enumerate()
            .filter(|(_, input)| {
                input.exists
                    && input.access != PathAccess::Deny
                    && (source == input.source
                        || (input.directory && source.starts_with(&input.source)))
            })
            .max_by_key(|(index, input)| (input.source.components().count(), *index))
            .map(|(_, input)| input.access)
    }
}
