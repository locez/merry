use super::CodingRuntimeError;
use merry_runtime::ProjectRules;
use std::{
    fs,
    io::{self, ErrorKind, Read},
    path::Path,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const ROOT_PROJECT_RULES_FILE: &str = "AGENTS.md";
const MAX_ROOT_PROJECT_RULES_BYTES: usize = 1024 * 1024;

pub(super) fn load_root_project_rules(
    root: &Path,
) -> Result<Option<ProjectRules>, CodingRuntimeError> {
    let canonical_root =
        fs::canonicalize(root).map_err(|source| CodingRuntimeError::ProjectRulesRead {
            path: root.to_path_buf(),
            source,
        })?;
    let path = canonical_root.join(ROOT_PROJECT_RULES_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CodingRuntimeError::ProjectRulesRead { path, source }),
    };
    if metadata.file_type().is_symlink() {
        return Err(CodingRuntimeError::ProjectRulesPathDenied {
            path,
            reason: "symbolic links are not allowed",
        });
    }
    if !metadata.is_file() {
        return Err(CodingRuntimeError::ProjectRulesNotRegularFile { path });
    }

    let file = match open_root_project_rules(&path) {
        Ok(file) => file,
        Err(source) if is_symlink_open_error(&source) => {
            return Err(CodingRuntimeError::ProjectRulesPathDenied {
                path,
                reason: "symbolic links are not allowed",
            });
        }
        Err(source) => return Err(CodingRuntimeError::ProjectRulesRead { path, source }),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(source) => return Err(CodingRuntimeError::ProjectRulesRead { path, source }),
    };
    if !metadata.is_file() {
        return Err(CodingRuntimeError::ProjectRulesNotRegularFile { path });
    }
    if metadata.len() > MAX_ROOT_PROJECT_RULES_BYTES as u64 {
        return Err(CodingRuntimeError::ProjectRulesTooLarge {
            path,
            max_bytes: MAX_ROOT_PROJECT_RULES_BYTES,
        });
    }

    let mut bytes = Vec::new();
    if let Err(source) = file
        .take(MAX_ROOT_PROJECT_RULES_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
    {
        return Err(CodingRuntimeError::ProjectRulesRead { path, source });
    }
    if bytes.len() > MAX_ROOT_PROJECT_RULES_BYTES {
        return Err(CodingRuntimeError::ProjectRulesTooLarge {
            path,
            max_bytes: MAX_ROOT_PROJECT_RULES_BYTES,
        });
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(source) => {
            return Err(CodingRuntimeError::ProjectRulesRead {
                path,
                source: io::Error::new(ErrorKind::InvalidData, source),
            });
        }
    };

    ProjectRules::new(ROOT_PROJECT_RULES_FILE, text)
        .map(Some)
        .map_err(|source| CodingRuntimeError::ProjectRulesInvalid {
            path,
            source: Box::new(source),
        })
}

#[cfg(unix)]
fn open_root_project_rules(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options.open(path)
}

#[cfg(not(unix))]
fn open_root_project_rules(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().read(true).open(path)
}

#[cfg(unix)]
fn is_symlink_open_error(source: &io::Error) -> bool {
    source.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_open_error(_: &io::Error) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_root_agents_as_project_rules() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("AGENTS.md"), "Use fixture rule.\n").expect("write rules");

        let rules = load_root_project_rules(temp.path())
            .expect("rules load")
            .expect("rules exist");

        assert_eq!(rules.source_path(), "AGENTS.md");
        assert_eq!(rules.text(), "Use fixture rule.\n");
    }

    #[test]
    fn missing_root_agents_adds_no_project_rules() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            load_root_project_rules(temp.path()).expect("missing rules are allowed"),
            None
        );
    }

    #[test]
    fn blank_root_agents_is_a_path_aware_validation_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        std::fs::write(&path, " \n\t").expect("write blank rules");

        let error = load_root_project_rules(temp.path()).expect_err("blank rules reject");

        assert!(matches!(
            error,
            CodingRuntimeError::ProjectRulesInvalid { path: actual, .. } if actual == path
        ));
    }

    #[test]
    fn directory_root_agents_is_not_a_regular_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        std::fs::create_dir(&path).expect("create directory at rules path");

        let error =
            load_root_project_rules(temp.path()).expect_err("directory cannot be read as rules");

        assert!(matches!(
            error,
            CodingRuntimeError::ProjectRulesNotRegularFile { path: actual } if actual == path
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_agents_is_path_denied() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("create workspace");
        let secret_path = temp.path().join("secret.txt");
        std::fs::write(&secret_path, "outside workspace secret\n").expect("write secret");
        let path = workspace.join("AGENTS.md");
        std::os::unix::fs::symlink(&secret_path, &path).expect("symlink rules to secret");

        let error = load_root_project_rules(&workspace).expect_err("symlink must be denied");

        assert!(matches!(
            error,
            CodingRuntimeError::ProjectRulesPathDenied { path: actual, .. } if actual == path
        ));
    }

    #[test]
    fn oversized_root_agents_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        std::fs::write(&path, vec![b'a'; MAX_ROOT_PROJECT_RULES_BYTES + 1])
            .expect("write oversized rules");

        let error = load_root_project_rules(temp.path()).expect_err("oversized rules reject");

        assert!(matches!(
            error,
            CodingRuntimeError::ProjectRulesTooLarge {
                path: actual,
                max_bytes: MAX_ROOT_PROJECT_RULES_BYTES,
            } if actual == path
        ));
    }

    #[test]
    fn exact_max_size_root_agents_loads() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        let expected = "a".repeat(MAX_ROOT_PROJECT_RULES_BYTES);
        std::fs::write(&path, &expected).expect("write max-sized rules");

        let rules = load_root_project_rules(temp.path())
            .expect("max-sized rules load")
            .expect("rules exist");

        assert_eq!(rules.text(), expected);
    }

    #[test]
    fn invalid_utf8_root_agents_preserves_invalid_data_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        std::fs::write(&path, [0xff]).expect("write invalid UTF-8 rules");

        let error = load_root_project_rules(temp.path()).expect_err("invalid UTF-8 rejects");

        match error {
            CodingRuntimeError::ProjectRulesRead {
                path: actual,
                source,
            } => {
                assert_eq!(actual, path);
                assert_eq!(source.kind(), ErrorKind::InvalidData);
            }
            other => panic!("expected path-aware read error, got {other:?}"),
        }
    }
}
