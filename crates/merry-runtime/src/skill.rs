//! Filesystem-backed skill metadata and stable-prefix rendering.
//!
//! Skills are discovered from `SKILL.md` files, but only frontmatter metadata
//! enters the cacheable stable prefix. Full skill bodies remain available
//! through normal workspace file reads. Frontmatter is parsed as YAML by the
//! `frontmatter` submodule, which reads only `name` and `description`.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    fmt, fs,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::text;

#[path = "skill/frontmatter.rs"]
mod frontmatter;

pub use frontmatter::FrontmatterError;

const SKILLS_INTRO: &str = "A skill is a set of local instructions stored in a `SKILL.md` file. The list below is metadata for discovery only; skill bodies stay on disk until needed.";
const SKILLS_HOW_TO_USE: &str = r#"- If the user explicitly names a skill, including with a `$skill-name` token, use it for that turn.
- If the task clearly matches a skill description, read that skill's `SKILL.md` before relying on it.
- Use `read_text` with a bounded line range to read the listed `SKILL.md`; request further ranges only when the task needs them.
- Resolve relative paths mentioned by `SKILL.md` relative to that skill directory.
- Read only the referenced files and ranges needed for the task.
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
    /// A required single-line field contained control characters or line breaks.
    #[error("{field} must be single-line text without control characters")]
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
    ///
    /// Normalization happens here so every consumer sees the same single-line
    /// values: `name` is trimmed and `description` has its whitespace
    /// collapsed. Fields that stay blank or contain control characters are
    /// rejected.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        skill_md_path: PathBuf,
        root: PathBuf,
    ) -> Result<Self, SkillError> {
        let name = name.into();
        let name = name.trim();
        validate_single_line("skill name", name)?;
        let description = text::collapse_whitespace(&description.into());
        validate_single_line("skill description", &description)?;
        validate_skill_path(&skill_md_path)?;
        Ok(Self {
            name: name.to_owned(),
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
                    SkillLoadWarningReason::DuplicateName {
                        name: skill.name().to_owned(),
                    },
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

    pub(crate) fn find_by_skill_md_path(&self, path: &str) -> Option<&SkillMetadata> {
        self.skills
            .iter()
            .find(|skill| skill.skill_md_path.to_string_lossy() == path)
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
    reason: SkillLoadWarningReason,
}

impl SkillLoadWarning {
    /// Creates a skill load warning.
    #[must_use]
    pub fn new(path: PathBuf, reason: SkillLoadWarningReason) -> Self {
        Self { path, reason }
    }

    /// Path that produced the warning.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Typed reason the skill was left out of the catalog.
    #[must_use]
    pub fn reason(&self) -> &SkillLoadWarningReason {
        &self.reason
    }
}

impl fmt::Display for SkillLoadWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path.display(), self.reason)
    }
}

/// Reason a discovered `SKILL.md` file was left out of the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SkillLoadWarningReason {
    /// The file could not be read from disk.
    #[error("failed to read file: {message}")]
    Read {
        /// IO error detail.
        message: String,
    },
    /// The frontmatter was missing or invalid.
    #[error(transparent)]
    Frontmatter(#[from] FrontmatterError),
    /// The frontmatter parsed, but the resulting metadata was rejected.
    #[error(transparent)]
    InvalidMetadata(#[from] SkillError),
    /// Another skill in the same catalog already used this name.
    #[error("duplicate skill name `{name}` was skipped")]
    DuplicateName {
        /// Duplicate skill name.
        name: String,
    },
}

fn validate_single_line(field: &'static str, value: &str) -> Result<(), SkillError> {
    if value.trim().is_empty() {
        return Err(SkillError::Blank { field });
    }
    if value.chars().any(char::is_control) {
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
    scan_dir(
        root,
        root,
        Path::new(""),
        0,
        &mut scanned_dirs,
        skills,
        warnings,
    )
}

fn scan_dir(
    root: &Path,
    dir: &Path,
    relative_dir: &Path,
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
        let relative_skill_md = relative_dir.join(SKILLS_FILENAME);
        match parse_skill_file(&skill_md, &relative_skill_md, root) {
            Ok(metadata) => skills.push(metadata),
            Err(reason) => warnings.push(SkillLoadWarning::new(skill_md, reason)),
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
            child_dirs.push((entry.path(), relative_dir.join(entry.file_name())));
        }
    }
    child_dirs.sort();
    for (child_dir, child_relative_dir) in child_dirs {
        scan_dir(
            root,
            &child_dir,
            &child_relative_dir,
            depth.saturating_add(1),
            scanned_dirs,
            skills,
            warnings,
        )?;
    }

    Ok(())
}

/// Reads one skill file and turns its frontmatter into validated metadata.
///
/// `relative_skill_md` is built by the directory walk, so metadata never has to
/// re-derive a root-relative path from the absolute scan path.
fn parse_skill_file(
    skill_md: &Path,
    relative_skill_md: &Path,
    root: &Path,
) -> Result<SkillMetadata, SkillLoadWarningReason> {
    let text = fs::read_to_string(skill_md).map_err(|source| SkillLoadWarningReason::Read {
        message: source.to_string(),
    })?;
    let fields = frontmatter::parse(&text)?;
    SkillMetadata::new(
        fields.name(),
        fields.description(),
        relative_skill_md.to_path_buf(),
        root.to_path_buf(),
    )
    .map_err(SkillLoadWarningReason::InvalidMetadata)
}

#[cfg(test)]
#[path = "skill/tests.rs"]
mod tests;
