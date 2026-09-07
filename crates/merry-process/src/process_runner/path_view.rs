use super::mount_aliases::MountAliases;
use crate::resolve_bwrap_path;
use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource, ProcessRunnerError};
use std::path::{Path, PathBuf};

/// One preparation's mount aliases and admitted logical-to-source mappings.
pub(super) struct ActionPathView {
    pub(super) aliases: MountAliases,
    denied: Vec<PathBuf>,
    reviewed: Vec<PathBuf>,
    granted: Vec<PathBuf>,
    bindings: Vec<(PathBuf, PathBuf)>,
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
            (PathBuf::from("/"), PathBuf::from("/")),
            (PathBuf::from("/tmp"), resolve_bwrap_path(tmp_source)),
            (workspace.clone(), workspace),
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
                bindings.push((path.clone(), path));
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
                bindings.extend(aliases.action_paths(rule.path(), tmp_source));
            }
        }
        for rule in rules
            .iter()
            .filter(|rule| rule.source() == PathAccessRuleSource::PermissionReview)
        {
            bindings.extend(aliases.grant_paths(rule.path(), tmp_source));
        }
        Ok(Self {
            aliases,
            denied,
            reviewed,
            granted,
            bindings,
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

    pub(super) fn source(&self, path: &Path) -> Result<Option<PathBuf>, ProcessRunnerError> {
        if !self.visible(path) {
            return Ok(None);
        }
        let destination = crate::resolve_sandbox_path(path, |prefix| {
            self.visible(prefix)
                .then(|| self.imported_source(prefix))
                .flatten()
        })
        .map_err(|error| ProcessRunnerError::infrastructure(error.to_string()))?;
        let Some(source) = self.imported_source(&destination) else {
            return Ok(None);
        };
        Ok((self.visible(&destination) && self.visible(&source)).then_some(source))
    }

    fn imported_source(&self, path: &Path) -> Option<PathBuf> {
        let (_, (destination, source)) = self
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, (destination, _))| path.starts_with(destination))
            .max_by_key(|(index, (destination, _))| (destination.components().count(), *index))?;
        let relative = path.strip_prefix(destination).ok()?;
        Some(if relative.as_os_str().is_empty() {
            source.clone()
        } else {
            source.join(relative)
        })
    }
}

#[cfg(test)]
mod tests;
