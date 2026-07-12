use super::CodingRuntimeError;
use merry_runtime::ProjectRules;
use std::{fs, io::ErrorKind, path::Path};

const ROOT_PROJECT_RULES_FILE: &str = "AGENTS.md";

pub(super) fn load_root_project_rules(
    root: &Path,
) -> Result<Option<ProjectRules>, CodingRuntimeError> {
    let path = root.join(ROOT_PROJECT_RULES_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CodingRuntimeError::ProjectRulesRead { path, source }),
    };

    ProjectRules::new(ROOT_PROJECT_RULES_FILE, text)
        .map(Some)
        .map_err(|source| CodingRuntimeError::ProjectRulesInvalid {
            path,
            source: Box::new(source),
        })
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
    fn unreadable_root_agents_is_a_path_aware_read_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        std::fs::create_dir(&path).expect("create directory at rules path");

        let error =
            load_root_project_rules(temp.path()).expect_err("directory cannot be read as rules");

        assert!(matches!(
            error,
            CodingRuntimeError::ProjectRulesRead { path: actual, .. } if actual == path
        ));
    }
}
