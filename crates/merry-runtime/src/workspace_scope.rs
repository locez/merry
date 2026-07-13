use std::path::{Component, Path};

pub(crate) const WORKSPACE_ROOT_SCOPE: &str = ".";

pub(crate) fn is_valid_workspace_scope(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    if text == WORKSPACE_ROOT_SCOPE {
        return true;
    }
    if text.is_empty()
        || path.is_absolute()
        || text.contains('\\')
        || text.chars().any(char::is_control)
        || text
            .split('/')
            .any(|segment| matches!(segment, "" | "." | ".."))
    {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

pub(crate) fn workspace_scope_contains(parent: &Path, child: &Path) -> bool {
    parent == Path::new(WORKSPACE_ROOT_SCOPE)
        || parent == child
        || (child != Path::new(WORKSPACE_ROOT_SCOPE) && child.starts_with(parent))
}

pub(crate) fn workspace_scopes_overlap(left: &Path, right: &Path) -> bool {
    left == Path::new(WORKSPACE_ROOT_SCOPE)
        || right == Path::new(WORKSPACE_ROOT_SCOPE)
        || left == right
        || left.starts_with(right)
        || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_root_is_valid_and_contains_every_concrete_scope() {
        let root = Path::new(".");
        let crate_path = Path::new("crates/merry-runtime");

        assert!(is_valid_workspace_scope(root));
        assert!(workspace_scope_contains(root, crate_path));
        assert!(workspace_scopes_overlap(root, crate_path));
    }

    #[test]
    fn traversal_and_embedded_current_segments_remain_invalid() {
        for path in ["..", "../outside", "crates/./runtime", "/tmp", "a//b"] {
            assert!(!is_valid_workspace_scope(Path::new(path)), "{path}");
        }
    }
}
