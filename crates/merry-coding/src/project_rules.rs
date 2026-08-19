use merry_runtime::{ContextError, ProjectRules};
use std::{
    fs,
    io::{self, ErrorKind, Read},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// Root project-rule file loaded by the coding composition.
pub const ROOT_PROJECT_RULES_FILE: &str = "AGENTS.md";
/// Maximum accepted root project-rule file size.
pub const MAX_ROOT_PROJECT_RULES_BYTES: usize = 1024 * 1024;

/// Loads the root `AGENTS.md` file without following a file symlink.
pub fn load_root_project_rules(root: &Path) -> Result<Option<ProjectRules>, ProjectRulesLoadError> {
    let canonical_root =
        fs::canonicalize(root).map_err(|source| ProjectRulesLoadError::ProjectRulesRead {
            path: root.to_path_buf(),
            source,
        })?;
    let path = canonical_root.join(ROOT_PROJECT_RULES_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ProjectRulesLoadError::ProjectRulesRead { path, source }),
    };
    if metadata.file_type().is_symlink() {
        return Err(ProjectRulesLoadError::ProjectRulesPathDenied {
            path,
            reason: "symbolic links are not allowed",
        });
    }
    if !metadata.is_file() {
        return Err(ProjectRulesLoadError::ProjectRulesNotRegularFile { path });
    }

    let file = match open_root_project_rules(&path) {
        Ok(file) => file,
        Err(source) if is_symlink_open_error(&source) => {
            return Err(ProjectRulesLoadError::ProjectRulesPathDenied {
                path,
                reason: "symbolic links are not allowed",
            });
        }
        Err(source) => return Err(ProjectRulesLoadError::ProjectRulesRead { path, source }),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(source) => return Err(ProjectRulesLoadError::ProjectRulesRead { path, source }),
    };
    if !metadata.is_file() {
        return Err(ProjectRulesLoadError::ProjectRulesNotRegularFile { path });
    }
    if metadata.len() > MAX_ROOT_PROJECT_RULES_BYTES as u64 {
        return Err(ProjectRulesLoadError::ProjectRulesTooLarge {
            path,
            max_bytes: MAX_ROOT_PROJECT_RULES_BYTES,
        });
    }

    let mut bytes = Vec::new();
    if let Err(source) = file
        .take(MAX_ROOT_PROJECT_RULES_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
    {
        return Err(ProjectRulesLoadError::ProjectRulesRead { path, source });
    }
    if bytes.len() > MAX_ROOT_PROJECT_RULES_BYTES {
        return Err(ProjectRulesLoadError::ProjectRulesTooLarge {
            path,
            max_bytes: MAX_ROOT_PROJECT_RULES_BYTES,
        });
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(source) => {
            return Err(ProjectRulesLoadError::ProjectRulesRead {
                path,
                source: io::Error::new(ErrorKind::InvalidData, source),
            });
        }
    };
    let text = if text.contains("\r\n") {
        text.replace("\r\n", "\n")
    } else {
        text
    };

    ProjectRules::new(ROOT_PROJECT_RULES_FILE, text)
        .map(Some)
        .map_err(|source| ProjectRulesLoadError::ProjectRulesInvalid {
            path,
            source: Box::new(source),
        })
}

/// Errors raised while loading the root project rules.
#[derive(Debug, Error)]
pub enum ProjectRulesLoadError {
    /// The root path or rules file could not be read.
    #[error("failed to read project rules at {path}: {source}")]
    ProjectRulesRead {
        /// Path involved in the failed read.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// The rules path was intentionally rejected as unsafe.
    #[error("project rules path is denied at {path}: {reason}")]
    ProjectRulesPathDenied {
        /// Path rejected by the loader.
        path: PathBuf,
        /// Stable rejection reason.
        reason: &'static str,
    },
    /// The rules path exists but is not a regular file.
    #[error("project rules path is not a regular file: {path}")]
    ProjectRulesNotRegularFile {
        /// Path that was not a regular file.
        path: PathBuf,
    },
    /// The rules file exceeds the bounded loader limit.
    #[error("project rules at {path} exceed the {max_bytes}-byte limit")]
    ProjectRulesTooLarge {
        /// Oversized rules path.
        path: PathBuf,
        /// Maximum accepted size.
        max_bytes: usize,
    },
    /// The rules content failed typed runtime validation.
    #[error("invalid project rules at {path}: {source}")]
    ProjectRulesInvalid {
        /// Invalid rules path.
        path: PathBuf,
        /// Runtime validation error.
        #[source]
        source: Box<ContextError>,
    },
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
    fn loads_crlf_root_agents_as_normalized_project_rules() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("AGENTS.md"),
            "Use CRLF rule.\r\nSecond line.\r\n",
        )
        .expect("write CRLF rules");

        let rules = load_root_project_rules(temp.path())
            .expect("CRLF rules load")
            .expect("rules exist");

        assert_eq!(rules.text(), "Use CRLF rule.\nSecond line.\n");
    }

    #[test]
    fn lone_carriage_return_remains_invalid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        std::fs::write(&path, "Use rule.\rNot a CRLF line.").expect("write lone CR rules");

        let error = load_root_project_rules(temp.path()).expect_err("lone CR rules reject");

        assert!(matches!(
            error,
            ProjectRulesLoadError::ProjectRulesInvalid { path: actual, .. } if actual == path
        ));
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
            ProjectRulesLoadError::ProjectRulesInvalid { path: actual, .. } if actual == path
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
            ProjectRulesLoadError::ProjectRulesNotRegularFile { path: actual } if actual == path
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
            ProjectRulesLoadError::ProjectRulesPathDenied { path: actual, .. } if actual == path
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
            ProjectRulesLoadError::ProjectRulesTooLarge {
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
            ProjectRulesLoadError::ProjectRulesRead {
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
