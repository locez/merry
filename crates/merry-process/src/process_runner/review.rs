use super::mount_aliases::MountAliases;
use crate::resolve_bwrap_path;
use merry_runtime::PathAccessRule;
use std::path::Path;

pub(super) fn required(rules: &[PathAccessRule], path: &Path, aliases: &MountAliases) -> bool {
    if !rules.iter().any(PathAccessRule::review_required) {
        return false;
    }
    let path = resolve_bwrap_path(path);
    rules
        .iter()
        .filter(|rule| rule.review_required())
        .any(|rule| {
            aliases
                .paths(rule.path())
                .iter()
                .any(|root| path.starts_with(root))
        })
}
