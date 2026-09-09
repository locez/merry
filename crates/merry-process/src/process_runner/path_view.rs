use super::mount_aliases::MountAliases;
use crate::{PreparedSandboxMountPlan, SandboxMountPlan, SandboxPathSource, resolve_bwrap_path};
use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource, ProcessRunnerError};
use std::path::{Path, PathBuf};

/// One preparation's mount aliases and admitted logical-to-source mappings.
pub(super) struct ActionPathView {
    pub(super) aliases: MountAliases,
    denied: Vec<PathBuf>,
    reviewed: Vec<PathBuf>,
    granted: Vec<PathBuf>,
    namespace: PreparedSandboxMountPlan,
}

impl ActionPathView {
    pub(super) fn prepare(
        rules: &[PathAccessRule],
        tmp_source: &Path,
        workspace: &Path,
    ) -> Result<Self, ProcessRunnerError> {
        let aliases = MountAliases::current()?;
        let mut denied = Vec::new();
        let mut reviewed = Vec::new();
        let mut granted = Vec::new();
        let workspace = resolve_bwrap_path(workspace);
        let mut bindings = vec![
            (PathBuf::from("/"), PathBuf::from("/"), PathAccess::ReadOnly),
            (
                PathBuf::from("/tmp"),
                resolve_bwrap_path(tmp_source),
                PathAccess::ReadWrite,
            ),
            (workspace.clone(), workspace, PathAccess::ReadWrite),
        ];
        for rule in rules {
            if rule.access() == PathAccess::Deny {
                denied.extend(aliases.action_paths(rule.path(), tmp_source).into_keys());
            } else if rule.review_required() {
                reviewed.extend(aliases.action_paths(rule.path(), tmp_source).into_keys());
            } else if rule.source() == PathAccessRuleSource::PermissionReview {
                granted.extend(aliases.grant_paths(rule.path(), tmp_source).into_keys());
            }
            if !rule.review_required() && rule.access() != PathAccess::Deny && rule.path().exists()
            {
                let path = resolve_bwrap_path(rule.path());
                bindings.push((path.clone(), path, rule.access()));
            }
        }
        for rule in rules.iter().filter(|rule| {
            rule.access() == PathAccess::ReadOnly
                && !rule.review_required()
                && matches!(
                    rule.source(),
                    PathAccessRuleSource::TrustedGlobalConfig
                        | PathAccessRuleSource::TrustedGlobalConfigWritableCeiling
                )
        }) {
            if rule.path().exists() {
                bindings.extend(
                    aliases
                        .action_paths(rule.path(), tmp_source)
                        .into_iter()
                        .map(|(destination, source)| (destination, source, rule.access())),
                );
            }
        }
        for rule in rules
            .iter()
            .filter(|rule| rule.source() == PathAccessRuleSource::PermissionReview)
        {
            bindings.extend(
                aliases
                    .grant_paths(rule.path(), tmp_source)
                    .into_iter()
                    .map(|(destination, source)| (destination, source, rule.access())),
            );
        }
        let mut namespace = SandboxMountPlan::new();
        for (destination, source, access) in bindings {
            namespace
                .bind(&source, &destination, access, false)
                .map_err(|error| ProcessRunnerError::infrastructure(error.to_string()))?;
        }
        for path in ["/proc", "/dev"] {
            namespace
                .opaque(Path::new(path))
                .map_err(|error| ProcessRunnerError::infrastructure(error.to_string()))?;
        }
        let namespace = namespace
            .complete(&[])
            .map_err(|error| ProcessRunnerError::infrastructure(error.to_string()))?;
        Ok(Self {
            aliases,
            denied,
            reviewed,
            granted,
            namespace,
        })
    }

    pub(super) fn visible(&self, path: &Path) -> bool {
        !self.denied.iter().any(|root| path.starts_with(root))
            && self.reviewed.iter().all(|root| {
                !path.starts_with(root)
                    || self
                        .granted
                        .iter()
                        .any(|grant| grant.starts_with(root) && path.starts_with(grant))
            })
    }

    pub(super) fn resolve(
        &self,
        path: &Path,
    ) -> Result<Option<SandboxPathSource>, ProcessRunnerError> {
        self.namespace
            .resolve_checked(path, |prefix| self.visible(prefix))
            .map_err(|error| ProcessRunnerError::infrastructure(error.to_string()))
    }

    pub(super) fn destination(&self, path: &Path) -> Result<PathBuf, ProcessRunnerError> {
        self.namespace
            .destination(path)
            .map_err(|error| ProcessRunnerError::infrastructure(error.to_string()))
    }
}

#[cfg(test)]
mod tests;
