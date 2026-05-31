//! Filesystem-backed skill metadata and stable-prefix rendering.
//!
//! Skills are discovered from `SKILL.md` files, but only frontmatter metadata
//! enters the cacheable stable prefix. Full skill bodies remain available
//! through normal workspace file reads.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

const SKILLS_INTRO: &str = "A skill is a set of local instructions stored in a `SKILL.md` file. The list below is for discovery only; skill bodies stay on disk until needed.";
const SKILLS_HOW_TO_USE: &str = r#"- If the user explicitly names a skill, use it for that turn.
- If the task clearly matches a skill description, read that skill's `SKILL.md` before relying on it.
- Use `workspace_read_file` to read the listed `SKILL.md`.
- Resolve relative paths mentioned by `SKILL.md` relative to that skill directory.
- Read only the referenced files needed for the task.
- Do not carry a skill body across unrelated turns unless it remains in raw context or is re-read."#;

/// Errors raised while validating skill metadata or configured skill roots.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SkillError {
    /// A required field was blank.
    #[error("{field} must not be blank")]
    Blank {
        /// Field name.
        field: &'static str,
    },
    /// A required field had unsupported control characters.
    #[error("{field} must not contain control characters")]
    ControlCharacters {
        /// Field name.
        field: &'static str,
    },
    /// Skill paths must be relative paths to `SKILL.md`.
    #[error("skill path must be relative and must end with SKILL.md: {path}")]
    InvalidSkillPath {
        /// Rejected path.
        path: String,
    },
    /// A catalog cannot contain duplicate normalized names.
    #[error("skill {name} is duplicated at {path}")]
    Duplicate {
        /// Duplicate skill name.
        name: String,
        /// Duplicate skill path.
        path: String,
    },
    /// Configured skill root does not exist.
    #[error("skill root does not exist: {root}")]
    RootNotFound {
        /// Configured root.
        root: String,
    },
    /// Configured skill root is not a directory.
    #[error("skill root is not a directory: {root}")]
    RootNotDirectory {
        /// Configured root.
        root: String,
    },
    /// Configured skill root could not be read.
    #[error("could not read skill root {root}: {message}")]
    RootRead {
        /// Configured root.
        root: String,
        /// IO error detail.
        message: String,
    },
}

/// Model-visible metadata for one skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMetadata {
    name: String,
    description: String,
    skill_md_path: PathBuf,
    root: PathBuf,
}

impl SkillMetadata {
    /// Creates validated skill metadata.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        skill_md_path: PathBuf,
        root: PathBuf,
    ) -> Result<Self, SkillError> {
        let name = name.into();
        validate_text("skill name", &name)?;
        let description = description.into();
        validate_text("skill description", &description)?;
        validate_skill_path(&skill_md_path)?;
        Ok(Self {
            name,
            description,
            skill_md_path,
            root,
        })
    }

    /// Skill name from `SKILL.md` frontmatter.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Skill description from `SKILL.md` frontmatter.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Workspace-readable relative path to this skill's `SKILL.md`.
    #[must_use]
    pub fn skill_md_path(&self) -> &Path {
        &self.skill_md_path
    }

    /// Configured root that owns this skill.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Deterministic catalog of available skill metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCatalog {
    skills: Vec<SkillMetadata>,
    warnings: Vec<SkillLoadWarning>,
}

impl SkillCatalog {
    /// Loads skill metadata by scanning configured roots for `SKILL.md` files.
    pub fn load_from_roots<I>(roots: I) -> Result<Self, SkillError>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut skills = Vec::new();
        let mut warnings = Vec::new();
        for root in roots {
            scan_root(&root, &mut skills, &mut warnings)?;
        }
        Self::from_loaded_metadata(skills, warnings)
    }

    /// Creates a deterministic catalog from metadata.
    pub fn from_metadata(skills: Vec<SkillMetadata>) -> Result<Self, SkillError> {
        let mut by_key = BTreeMap::new();
        for skill in skills {
            let key = normalized_skill_name(skill.name());
            match by_key.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(skill);
                }
                Entry::Occupied(entry) => {
                    return Err(SkillError::Duplicate {
                        name: entry.get().name().to_owned(),
                        path: skill.skill_md_path.display().to_string(),
                    });
                }
            }
        }
        Ok(Self {
            skills: by_key.into_values().collect(),
            warnings: Vec::new(),
        })
    }

    fn from_loaded_metadata(
        skills: Vec<SkillMetadata>,
        mut warnings: Vec<SkillLoadWarning>,
    ) -> Result<Self, SkillError> {
        let mut by_key: BTreeMap<String, SkillMetadata> = BTreeMap::new();
        for skill in skills {
            let key = normalized_skill_name(skill.name());
            match by_key.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(skill);
                }
                Entry::Occupied(_) => warnings.push(SkillLoadWarning::new(
                    skill.skill_md_path.clone(),
                    format!("duplicate skill name `{}` was skipped", skill.name()),
                )),
            }
        }

        Ok(Self {
            skills: by_key.into_values().collect(),
            warnings,
        })
    }

    /// Returns true when the catalog has no visible skills.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Ordered skill metadata.
    #[must_use]
    pub fn skills(&self) -> &[SkillMetadata] {
        &self.skills
    }

    /// Non-fatal load warnings.
    #[must_use]
    pub fn warnings(&self) -> &[SkillLoadWarning] {
        &self.warnings
    }

    /// Renders this catalog as a cacheable stable-prefix message.
    ///
    /// The rendered text intentionally contains only metadata and usage rules.
    /// Full `SKILL.md` bodies stay out of the prefix.
    #[must_use]
    pub fn to_stable_prefix_message_text(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }

        let mut lines = vec![
            "## Skills".to_owned(),
            SKILLS_INTRO.to_owned(),
            "### Available skills".to_owned(),
        ];
        for skill in &self.skills {
            lines.push(format!(
                "- {}: {} (file: {})",
                skill.name,
                skill.description,
                skill.skill_md_path.display()
            ));
        }
        lines.push("### How to use skills".to_owned());
        lines.push(SKILLS_HOW_TO_USE.to_owned());
        Some(lines.join("\n"))
    }
}

/// Non-fatal warning raised while loading skill roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillLoadWarning {
    path: PathBuf,
    message: String,
}

impl SkillLoadWarning {
    /// Creates a skill load warning.
    #[must_use]
    pub fn new(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            path,
            message: message.into(),
        }
    }

    /// Path that produced the warning.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Human-readable warning detail.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

fn validate_text(field: &'static str, value: &str) -> Result<(), SkillError> {
    if value.trim().is_empty() {
        return Err(SkillError::Blank { field });
    }
    if value
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(SkillError::ControlCharacters { field });
    }
    Ok(())
}

fn validate_skill_path(path: &Path) -> Result<(), SkillError> {
    if path.is_absolute() || path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
        return Err(SkillError::InvalidSkillPath {
            path: path.display().to_string(),
        });
    }
    Ok(())
}

fn normalized_skill_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

const SKILLS_FILENAME: &str = "SKILL.md";
const MAX_SCAN_DEPTH: usize = 6;
const MAX_SKILL_DIRS_PER_ROOT: usize = 2_000;

fn scan_root(
    root: &Path,
    skills: &mut Vec<SkillMetadata>,
    warnings: &mut Vec<SkillLoadWarning>,
) -> Result<(), SkillError> {
    if !root.exists() {
        return Ok(());
    }
    if !root.is_dir() {
        return Err(SkillError::RootNotDirectory {
            root: root.display().to_string(),
        });
    }

    let mut scanned_dirs = 0usize;
    scan_dir(root, root, 0, &mut scanned_dirs, skills, warnings)
}

fn scan_dir(
    root: &Path,
    dir: &Path,
    depth: usize,
    scanned_dirs: &mut usize,
    skills: &mut Vec<SkillMetadata>,
    warnings: &mut Vec<SkillLoadWarning>,
) -> Result<(), SkillError> {
    if depth > MAX_SCAN_DEPTH || *scanned_dirs >= MAX_SKILL_DIRS_PER_ROOT {
        return Ok(());
    }
    *scanned_dirs += 1;

    let skill_md = dir.join(SKILLS_FILENAME);
    if skill_md.is_file() {
        match parse_skill_file(root, &skill_md) {
            Ok(metadata) => skills.push(metadata),
            Err(message) => warnings.push(SkillLoadWarning::new(skill_md, message)),
        }
    }

    let entries = fs::read_dir(dir).map_err(|source| SkillError::RootRead {
        root: root.display().to_string(),
        message: source.to_string(),
    })?;
    let mut child_dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SkillError::RootRead {
            root: root.display().to_string(),
            message: source.to_string(),
        })?;
        let file_type = entry.file_type().map_err(|source| SkillError::RootRead {
            root: root.display().to_string(),
            message: source.to_string(),
        })?;
        if file_type.is_dir() {
            child_dirs.push(entry.path());
        }
    }
    child_dirs.sort();
    for child_dir in child_dirs {
        scan_dir(
            root,
            &child_dir,
            depth.saturating_add(1),
            scanned_dirs,
            skills,
            warnings,
        )?;
    }

    Ok(())
}

fn parse_skill_file(root: &Path, skill_md: &Path) -> Result<SkillMetadata, String> {
    let text = fs::read_to_string(skill_md).map_err(read_error)?;
    let frontmatter = frontmatter_block(&text)?;
    let fields = parse_frontmatter_fields(frontmatter)?;
    let name = fields
        .name
        .ok_or_else(|| "missing field `name`".to_owned())?;
    let description = fields
        .description
        .ok_or_else(|| "missing field `description`".to_owned())?;
    let relative_path = skill_md
        .strip_prefix(root)
        .map_err(|_| "skill path is outside configured root".to_owned())?
        .to_path_buf();
    SkillMetadata::new(name, description, relative_path, root.to_path_buf())
        .map_err(|error| error.to_string())
}

fn read_error(error: io::Error) -> String {
    format!("failed to read file: {error}")
}

fn frontmatter_block(text: &str) -> Result<&str, String> {
    let Some(rest) = text.strip_prefix("---\n") else {
        return Err("missing frontmatter delimited by ---".to_owned());
    };
    let Some((frontmatter, _body)) = rest.split_once("\n---") else {
        return Err("missing closing frontmatter delimiter".to_owned());
    };
    Ok(frontmatter)
}

#[derive(Default)]
struct FrontmatterFields {
    name: Option<String>,
    description: Option<String>,
}

fn parse_frontmatter_fields(frontmatter: &str) -> Result<FrontmatterFields, String> {
    let mut fields = FrontmatterFields::default();
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if line != trimmed {
            return Err(format!("invalid frontmatter line: {line}"));
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            return Err(format!("invalid frontmatter line: {trimmed}"));
        };
        let value = value.trim();
        match key.trim() {
            "name" => fields.name = Some(unquote_frontmatter_value(value).to_owned()),
            "description" => fields.description = Some(unquote_frontmatter_value(value).to_owned()),
            _ => {}
        }
    }
    Ok(fields)
}

fn unquote_frontmatter_value(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(name: &str, description: &str, path: &str) -> SkillMetadata {
        SkillMetadata::new(
            name,
            description,
            PathBuf::from(path),
            PathBuf::from("/workspace"),
        )
        .expect("valid skill metadata")
    }

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
        assert!(rendered.contains("workspace_read_file"));
        assert!(rendered.contains("Read only the referenced files needed"));
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
}

#[cfg(test)]
mod loader_tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    #[test]
    fn loads_skill_metadata_from_roots() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("skills");
        write(
            &root.join("frontend/SKILL.md"),
            r#"---
name: frontend-design
description: Use when building polished frontend UI.
---

# Frontend Design

full skill body sentinel
"#,
        );

        let catalog = SkillCatalog::load_from_roots([root.clone()]).expect("loads catalog");
        assert_eq!(catalog.skills().len(), 1);
        assert_eq!(catalog.skills()[0].name(), "frontend-design");
        assert_eq!(
            catalog.skills()[0].description(),
            "Use when building polished frontend UI."
        );
        assert_eq!(
            catalog.skills()[0].skill_md_path(),
            Path::new("frontend/SKILL.md")
        );
        assert!(catalog.warnings().is_empty());
    }

    #[test]
    fn skips_invalid_skill_frontmatter_with_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("skills");
        write(
            &root.join("valid/SKILL.md"),
            "---\nname: valid\n description: bad indentation\n---\n",
        );
        write(
            &root.join("missing-description/SKILL.md"),
            "---\nname: missing-description\n---\n",
        );
        write(
            &root.join("ok/SKILL.md"),
            "---\nname: ok\ndescription: Valid skill.\n---\n# OK\n",
        );

        let catalog = SkillCatalog::load_from_roots([root]).expect("load should not fail");
        assert_eq!(catalog.skills().len(), 1);
        assert_eq!(catalog.skills()[0].name(), "ok");
        assert_eq!(catalog.warnings().len(), 2);
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
