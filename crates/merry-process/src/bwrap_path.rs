use std::{
    fs,
    path::{Path, PathBuf},
};

/// Resolves the host-side source path used by a bubblewrap mount.
///
/// Bubblewrap destinations remain logical paths inside the sandbox, while
/// this helper follows symlink components in the host source path. The last
/// existing prefix is resolved so a path below a symlinked directory can be
/// mounted before its leaf has been created. If no prefix can be resolved, the
/// original path is retained so optional bubblewrap mounts keep their existing
/// missing-source behavior.
#[must_use]
pub fn resolve_bwrap_path(path: &Path) -> PathBuf {
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
    use super::resolve_bwrap_path;
    use std::{fs, os::unix::fs::symlink, path::Path};

    #[test]
    fn resolves_existing_symlink_components() {
        let temp = tempfile::tempdir().expect("temporary path");
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        fs::create_dir_all(&real).expect("real directory");
        symlink(&real, &link).expect("directory symlink");

        assert_eq!(resolve_bwrap_path(&link), fs::canonicalize(real).unwrap());
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
            resolve_bwrap_path(&missing),
            real.join("future").join("file")
        );
    }

    #[test]
    fn leaves_dangling_leaf_symlink_unresolved() {
        let temp = tempfile::tempdir().expect("temporary path");
        let link = temp.path().join("link");
        symlink(Path::new("missing"), &link).expect("dangling symlink");

        assert_eq!(resolve_bwrap_path(&link), link);
    }
}
