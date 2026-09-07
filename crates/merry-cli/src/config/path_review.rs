use super::{ConfigError, resolve_path_access_rule_path};
use merry_process::resolve_bwrap_path;
use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource};
use std::path::Path;

pub(super) fn append_review_rules(
    rules: &mut Vec<PathAccessRule>,
    paths: &[String],
    config_dir: &Path,
    home: &Path,
) -> Result<(), ConfigError> {
    for input in paths {
        let path = resolve_path_access_rule_path(input, config_dir, home)?;
        let resolved = resolve_bwrap_path(&path);
        let ancestors = rules
            .iter()
            .filter(|rule| !rule.review_required())
            .filter(|rule| resolved.starts_with(resolve_bwrap_path(rule.path())))
            .collect::<Vec<_>>();
        if ancestors
            .iter()
            .any(|rule| rule.access() == PathAccess::Deny)
        {
            return Err(ConfigError::Invalid(format!(
                "review path `{}` is denied by path policy",
                path.display()
            )));
        }
        let access = if ancestors
            .iter()
            .any(|rule| rule.access() == PathAccess::ReadOnly)
        {
            PathAccess::ReadOnly
        } else if ancestors
            .iter()
            .any(|rule| rule.access() == PathAccess::ReadWrite)
        {
            PathAccess::ReadWrite
        } else {
            return Err(ConfigError::Invalid(format!(
                "review path `{}` must be within a declared readonly_paths, readwrite_paths, or permissions.paths grant",
                path.display()
            )));
        };
        rules.push(
            PathAccessRule::new(path, access, PathAccessRuleSource::TrustedGlobalConfig)
                .with_review_required(),
        );
    }
    Ok(())
}
