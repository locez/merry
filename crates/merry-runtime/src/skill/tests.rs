//! Tests for skill metadata rendering and filesystem-based loading.
//!
//! Rendering tests build metadata directly, so they pin the stable-prefix text
//! and the single-line invariants without touching the filesystem. Loader tests
//! write real `SKILL.md` files, so they pin what a skill root accepts, what it
//! warns about, and what it skips.

use super::*;

/// Builds valid metadata for one skill without touching the filesystem.
fn metadata(name: &str, description: &str, path: &str) -> SkillMetadata {
    SkillMetadata::new(
        name,
        description,
        PathBuf::from(path),
        PathBuf::from("/workspace"),
    )
    .expect("valid skill metadata")
}

/// Stable-prefix rendering of catalog metadata.
mod rendering {
    use super::*;

    #[test]
    fn renders_available_skills_without_bodies() {
        let catalog = SkillCatalog::from_metadata(vec![
            metadata(
                "frontend-design",
                "Use for polished frontend implementation.",
                "skills/frontend-design/SKILL.md",
            ),
            metadata(
                "debugging",
                "Use for systematic debugging.",
                "skills/debugging/SKILL.md",
            ),
        ])
        .expect("valid catalog");

        let rendered = catalog
            .to_stable_prefix_message_text()
            .expect("catalog should render");

        assert!(rendered.contains("## Skills"));
        assert!(rendered.contains("frontend-design"));
        assert!(rendered.contains("Use for polished frontend implementation."));
        assert!(rendered.contains("skills/frontend-design/SKILL.md"));
        assert!(rendered.contains("read_text"));
        assert!(rendered.contains("Read only the referenced files and ranges needed"));
        assert!(!rendered.contains("# Frontend Design"));
        assert!(!rendered.contains("full skill body sentinel"));
    }

    #[test]
    fn metadata_order_is_deterministic() {
        let first = SkillCatalog::from_metadata(vec![
            metadata("zeta", "Last alphabetically.", "skills/zeta/SKILL.md"),
            metadata("alpha", "First alphabetically.", "skills/alpha/SKILL.md"),
        ])
        .expect("valid catalog");
        let second = SkillCatalog::from_metadata(vec![
            metadata("alpha", "First alphabetically.", "skills/alpha/SKILL.md"),
            metadata("zeta", "Last alphabetically.", "skills/zeta/SKILL.md"),
        ])
        .expect("valid catalog");

        assert_eq!(
            first.to_stable_prefix_message_text().expect("renders"),
            second.to_stable_prefix_message_text().expect("renders")
        );
    }

    #[test]
    fn rejects_blank_or_control_metadata() {
        let blank = SkillMetadata::new(
            " ",
            "Valid description.",
            PathBuf::from("skills/blank/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect_err("blank name should be rejected");
        assert!(blank.to_string().contains("skill name"));

        let control = SkillMetadata::new(
            "bad\u{7}name",
            "Valid description.",
            PathBuf::from("skills/bad/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect_err("control characters should be rejected");
        assert!(control.to_string().contains("skill name"));
    }

    #[test]
    fn normalizes_description_to_one_line() {
        let skill = metadata(
            "sample-tool",
            "Use when the description\n  spans several lines.",
            "skills/sample-tool/SKILL.md",
        );
        assert_eq!(
            skill.description(),
            "Use when the description spans several lines."
        );

        let catalog = SkillCatalog::from_metadata(vec![skill]).expect("valid catalog");
        let rendered = catalog
            .to_stable_prefix_message_text()
            .expect("catalog should render");
        let entry = rendered
            .lines()
            .find(|line| line.contains("sample-tool"))
            .expect("catalog should list the skill");
        assert_eq!(
            entry,
            "- sample-tool: Use when the description spans several lines. (file: skills/sample-tool/SKILL.md)"
        );
    }

    #[test]
    fn rejects_multi_line_names() {
        let error = SkillMetadata::new(
            "sample\ntool",
            "Valid description.",
            PathBuf::from("skills/sample/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect_err("line breaks in a name should be rejected");

        assert!(error.to_string().contains("skill name"), "{error}");
    }
}

/// Loading skill roots from the filesystem.
mod loader {
    use super::*;

    /// Writes one test file, creating the parent directories it needs.
    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    #[test]
    fn loads_skill_frontmatter_from_roots() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("skills");
        write(
            &root.join("frontend/SKILL.md"),
            "---\nname: frontend-design\ndescription: Use when building polished frontend UI.\n---\n\n# Frontend Design\n\nfull skill body sentinel\n",
        );
        write(
            &root.join("sample-tool/SKILL.md"),
            "---\nname: sample-tool\ndescription: Use when the task needs\n  the sample tool.\nmetadata:\n  cli_version: \">=1.2.3\"\n  requires:\n    bins:\n      - sample-cli\n---\n\n# Sample Tool Skill\n",
        );
        write(
            &root.join("windows/SKILL.md"),
            "\u{feff}---\r\nname: windows\r\ndescription: Windows-authored skill.\r\n---\r\n# Windows\r\n",
        );

        let catalog = SkillCatalog::load_from_roots([root]).expect("loads catalog");
        assert!(catalog.warnings().is_empty(), "{:?}", catalog.warnings());
        let skills = catalog
            .skills()
            .iter()
            .map(|skill| {
                (
                    skill.name(),
                    skill.description(),
                    skill.skill_md_path().to_path_buf(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            skills,
            vec![
                (
                    "frontend-design",
                    "Use when building polished frontend UI.",
                    PathBuf::from("frontend/SKILL.md"),
                ),
                (
                    "sample-tool",
                    "Use when the task needs the sample tool.",
                    PathBuf::from("sample-tool/SKILL.md"),
                ),
                (
                    "windows",
                    "Windows-authored skill.",
                    PathBuf::from("windows/SKILL.md"),
                ),
            ]
        );

        let rendered = catalog
            .to_stable_prefix_message_text()
            .expect("catalog should render");
        assert!(rendered.contains("frontend/SKILL.md"));
        assert!(!rendered.contains("# Frontend Design"));
        assert!(!rendered.contains("full skill body sentinel"));
    }

    #[test]
    fn skips_invalid_skill_files_with_typed_warnings() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("skills");
        write(
            &root.join("bad-name/SKILL.md"),
            "---\nname: |\n  bad\n  name\ndescription: Use when invalid.\n---\n",
        );
        write(
            &root.join("missing-description/SKILL.md"),
            "---\nname: missing-description\n---\n",
        );
        write(
            &root.join("nested-description/SKILL.md"),
            "---\nname: nested-description\ndescription:\n  nested: value\n---\n",
        );
        write(&root.join("no-frontmatter/SKILL.md"), "# Body only\n");
        write(
            &root.join("ok/SKILL.md"),
            "---\nname: ok\ndescription: Valid skill.\n---\n# OK\n",
        );

        let catalog = SkillCatalog::load_from_roots([root]).expect("load should not fail");
        assert_eq!(catalog.skills().len(), 1);
        assert_eq!(catalog.skills()[0].name(), "ok");

        let warnings = catalog.warnings();
        assert_eq!(warnings.len(), 4);
        assert!(warnings[0].path().ends_with("bad-name/SKILL.md"));
        assert!(matches!(
            warnings[0].reason(),
            SkillLoadWarningReason::InvalidMetadata(SkillError::ControlCharacters {
                field: "skill name"
            })
        ));
        assert!(matches!(
            warnings[1].reason(),
            SkillLoadWarningReason::Frontmatter(FrontmatterError::MissingField {
                field: "description"
            })
        ));
        assert!(matches!(
            warnings[2].reason(),
            SkillLoadWarningReason::Frontmatter(FrontmatterError::Invalid { .. })
        ));
        assert!(matches!(
            warnings[3].reason(),
            SkillLoadWarningReason::Frontmatter(FrontmatterError::MissingDelimiter)
        ));
    }

    #[test]
    fn warns_about_duplicate_skill_names() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        write(
            &first_root.join("sample/SKILL.md"),
            "---\nname: sample-tool\ndescription: First copy.\n---\n",
        );
        write(
            &second_root.join("sample/SKILL.md"),
            "---\nname: Sample-Tool\ndescription: Second copy.\n---\n",
        );

        let catalog =
            SkillCatalog::load_from_roots([first_root, second_root]).expect("loads catalog");

        assert_eq!(catalog.skills().len(), 1);
        assert_eq!(catalog.skills()[0].description(), "First copy.");
        assert_eq!(catalog.warnings().len(), 1);
        assert!(matches!(
            catalog.warnings()[0].reason(),
            SkillLoadWarningReason::DuplicateName { name } if name == "Sample-Tool"
        ));
    }

    #[test]
    fn missing_skill_root_loads_empty_catalog() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("missing-skills");

        let catalog = SkillCatalog::load_from_roots([root]).expect("missing root is empty");

        assert!(catalog.is_empty());
        assert!(catalog.warnings().is_empty());
    }
}
