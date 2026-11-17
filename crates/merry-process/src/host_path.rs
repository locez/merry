use std::{
    fs,
    path::{Path, PathBuf},
};

/// Resolves a host path through the last existing prefix.
///
/// Symlink components that already exist are followed. A missing leaf keeps
/// the unresolved suffix so a grant can name a path that has not been created
/// yet. If nothing on the path exists, the original path is returned.
#[must_use]
pub fn resolve_host_path(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    let mut unresolved = Vec::new();

    loop {
        if let Ok(resolved) = fs::canonicalize(&current) {
            let mut result = resolved;
            for component in unresolved.iter().rev() {
                result.push(component);
            }
            return result;
        }

        let Some(component) = current.file_name() else {
            return path.to_path_buf();
        };
        unresolved.push(component.to_owned());
        if !current.pop() {
            return path.to_path_buf();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::resolve_host_path;
    use std::{fs, os::unix::fs::symlink, path::Path};

    #[test]
    fn resolves_existing_symlink_components() {
        let temp = tempfile::tempdir().expect("temporary path");
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        fs::create_dir_all(&real).expect("real directory");
        symlink(&real, &link).expect("directory symlink");

        assert_eq!(resolve_host_path(&link), fs::canonicalize(real).unwrap());
    }

    #[test]
    fn resolves_existing_prefix_for_missing_leaf() {
        let temp = tempfile::tempdir().expect("temporary path");
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        fs::create_dir_all(&real).expect("real directory");
        symlink(&real, &link).expect("directory symlink");
        let missing = link.join("future").join("file");

        assert_eq!(
            resolve_host_path(&missing),
            real.join("future").join("file")
        );
    }

    #[test]
    fn leaves_dangling_leaf_symlink_unresolved() {
        let temp = tempfile::tempdir().expect("temporary path");
        let link = temp.path().join("link");
        symlink(Path::new("missing"), &link).expect("dangling symlink");

        assert_eq!(resolve_host_path(&link), link);
    }
}
