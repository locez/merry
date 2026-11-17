use crate::resolve_host_path;
use merry_runtime::PathAccessRule;
use std::path::Path;

pub(super) fn required(rules: &[PathAccessRule], path: &Path) -> bool {
    if !rules.iter().any(PathAccessRule::review_required) {
        return false;
    }
    let path = resolve_host_path(path);
    rules
        .iter()
        .filter(|rule| rule.review_required())
        .any(|rule| {
            let root = resolve_host_path(rule.path());
            path == root || path.starts_with(&root)
        })
}
