//! `SKILL.md` frontmatter extraction and YAML parsing.
//!
//! Skill frontmatter is the YAML document between the leading `---`
//! delimiters. Merry reads only `name` and `description` and intentionally
//! ignores every other top-level key, so third-party skill files that carry
//! structured `metadata`, tool policy, or license sections still load.
//!
//! Parsing the block as YAML instead of scanning it line by line is what makes
//! block scalars (`|`, `>`), quoted values, trailing comments, plain
//! multi-line scalars, CRLF line endings, and a leading byte-order mark behave
//! the way their authors expect.

use serde::Deserialize;
use thiserror::Error;

/// Frontmatter values Merry requires from one `SKILL.md` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFrontmatter {
    name: String,
    description: String,
}

impl SkillFrontmatter {
    /// Skill name as written in the frontmatter.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Skill description as written in the frontmatter.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// Errors raised while extracting and parsing `SKILL.md` frontmatter.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrontmatterError {
    /// The document does not open with a `---` line.
    #[error("missing frontmatter delimited by ---")]
    MissingDelimiter,
    /// The opening `---` line has no closing `---` line.
    #[error("missing closing frontmatter delimiter")]
    MissingClosingDelimiter,
    /// A required field is absent or null.
    #[error("missing frontmatter field `{field}`")]
    MissingField {
        /// Missing field name.
        field: &'static str,
    },
    /// The frontmatter is not valid YAML for the fields Merry reads.
    #[error("invalid frontmatter: {message}")]
    Invalid {
        /// Deserialization detail.
        message: String,
    },
}

/// Frontmatter as deserialized from YAML.
///
/// Unknown keys are ignored on purpose: real skill files carry `metadata`,
/// `license`, or tool policy sections that Merry does not model, and an
/// unsupported key must not hide the whole skill from the catalog.
#[derive(Debug, Deserialize)]
struct FrontmatterDocument {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// Extracts `name` and `description` from one `SKILL.md` document.
///
/// The complete file is accepted and only its frontmatter block is parsed.
/// Failures are reported as [`FrontmatterError`] so the caller can skip this
/// skill with a typed warning instead of rejecting the configured root.
pub fn parse(text: &str) -> Result<SkillFrontmatter, FrontmatterError> {
    let block = frontmatter_block(text)?;
    let document =
        serde_norway::from_str::<Option<FrontmatterDocument>>(block).map_err(|error| {
            FrontmatterError::Invalid {
                message: error.to_string(),
            }
        })?;
    let Some(document) = document else {
        return Err(FrontmatterError::MissingField { field: "name" });
    };
    Ok(SkillFrontmatter {
        name: document
            .name
            .ok_or(FrontmatterError::MissingField { field: "name" })?,
        description: document.description.ok_or(FrontmatterError::MissingField {
            field: "description",
        })?,
    })
}

/// Returns the YAML text between the opening and closing `---` lines.
///
/// Delimiter lines are matched textually, which stays consistent with YAML
/// document boundaries because block scalar content is always indented.
fn frontmatter_block(text: &str) -> Result<&str, FrontmatterError> {
    // Editors on Windows may save a byte-order mark before the delimiter.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut offset = 0usize;
    let mut block_start = None;
    for line in text.split_inclusive('\n') {
        if line.trim_end() == "---" {
            match block_start {
                None => block_start = Some(offset + line.len()),
                Some(start) => return Ok(&text[start..offset]),
            }
        } else if block_start.is_none() {
            return Err(FrontmatterError::MissingDelimiter);
        }
        offset += line.len();
    }
    match block_start {
        None => Err(FrontmatterError::MissingDelimiter),
        Some(_) => Err(FrontmatterError::MissingClosingDelimiter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_document(text: &str) -> SkillFrontmatter {
        parse(text).expect("frontmatter should parse")
    }

    #[test]
    fn reads_name_and_description_and_ignores_structured_metadata() {
        let skill = parse_document(
            "---\nname: sample-tool\ndescription: Use when the task needs the sample tool.\nmetadata:\n  cli_version: \">=1.2.3\"\n  category: product\n  requires:\n    bins:\n      - sample-cli\n---\n\n# Sample Tool Skill\n",
        );

        assert_eq!(skill.name(), "sample-tool");
        assert_eq!(
            skill.description(),
            "Use when the task needs the sample tool."
        );
    }

    #[test]
    fn folds_plain_multi_line_scalars() {
        let skill = parse_document(
            "---\nname: sample-tool\ndescription: Use when the task needs\n  the sample tool.\n---\n# Body\n",
        );

        assert_eq!(
            skill.description(),
            "Use when the task needs the sample tool."
        );
    }

    #[test]
    fn parses_block_scalars_and_comments() {
        let folded = parse_document(
            "---\nname: folded\ndescription: >-\n  Use when the task\n  needs the sample tool.\n---\n",
        );
        assert_eq!(
            folded.description(),
            "Use when the task needs the sample tool."
        );

        let literal = parse_document(
            "---\nname: literal\ndescription: |\n  First line.\n  Second line.\n---\n",
        );
        assert_eq!(literal.description(), "First line.\nSecond line.\n");

        let commented = parse_document(
            "---\n# generated skill\nname: commented\ndescription: Use when parsing comments. # trailing note\nlicense: MIT\n---\n",
        );
        assert_eq!(commented.name(), "commented");
        assert_eq!(commented.description(), "Use when parsing comments.");
    }

    #[test]
    fn parses_quoted_values_and_crlf_and_byte_order_mark() {
        let quoted = parse_document(
            "---\nname: \"quoted\"\ndescription: 'Use when quoted: values matter.'\n---\n",
        );
        assert_eq!(quoted.name(), "quoted");
        assert_eq!(quoted.description(), "Use when quoted: values matter.");

        let crlf = parse_document(
            "---\r\nname: windows\r\ndescription: Windows-authored skill.\r\n---\r\n# Body\r\n",
        );
        assert_eq!(crlf.description(), "Windows-authored skill.");

        let bom = parse_document(
            "\u{feff}---\nname: bom\ndescription: BOM-authored skill.\n---\n# Body\n",
        );
        assert_eq!(bom.name(), "bom");
        assert_eq!(bom.description(), "BOM-authored skill.");
    }

    #[test]
    fn reports_missing_delimiters() {
        assert_eq!(
            parse("# Body only\n"),
            Err(FrontmatterError::MissingDelimiter)
        );
        assert_eq!(
            parse("---\nname: sample-tool\n"),
            Err(FrontmatterError::MissingClosingDelimiter)
        );
    }

    #[test]
    fn reports_missing_and_null_fields() {
        assert_eq!(
            parse("---\nname: sample-tool\n---\n"),
            Err(FrontmatterError::MissingField {
                field: "description"
            })
        );
        assert_eq!(
            parse("---\ndescription: Use when needed.\n---\n"),
            Err(FrontmatterError::MissingField { field: "name" })
        );
        assert_eq!(
            parse("---\nname:\ndescription: Use when needed.\n---\n"),
            Err(FrontmatterError::MissingField { field: "name" })
        );
        assert_eq!(
            parse("---\n---\n"),
            Err(FrontmatterError::MissingField { field: "name" })
        );
    }

    #[test]
    fn reports_non_scalar_and_invalid_frontmatter() {
        let non_scalar = parse("---\nname: sample-tool\ndescription:\n  nested: value\n---\n")
            .expect_err("nested description should be rejected");
        assert!(
            matches!(non_scalar, FrontmatterError::Invalid { .. }),
            "{non_scalar:?}"
        );

        let invalid = parse("---\nname: sample-tool\ndescription: [unclosed\n---\n")
            .expect_err("invalid YAML should be rejected");
        assert!(
            matches!(invalid, FrontmatterError::Invalid { .. }),
            "{invalid:?}"
        );
    }

    #[test]
    fn stringifies_plain_scalar_field_values() {
        // YAML resolves an unquoted number to a scalar; reading it as text keeps
        // frontmatter written with `name: 123` loadable instead of rejected.
        let skill = parse_document("---\nname: 123\ndescription: Use when needed.\n---\n");

        assert_eq!(skill.name(), "123");
    }
}
