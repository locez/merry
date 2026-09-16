//! Parser for the `apply_patch` envelope.
//!
//! The grammar is small and deliberately strict, so most failures here are
//! patch-text mistakes. Each error therefore names the offending text and what
//! was expected instead of only reporting that the patch is invalid.

use std::collections::BTreeMap;

use crate::errors::{ERROR_PATCH_NOOP, ERROR_PATCH_SYNTAX};

use super::{
    diagnostic::single_line_preview,
    types::{
        WorkspacePatch, WorkspacePatchFile, WorkspacePatchHunk, WorkspacePatchLine,
        WorkspacePatchOperation,
    },
};

const BEGIN_WORKSPACE: &str = "*** Begin Workspace Patch";
const END_WORKSPACE: &str = "*** End Workspace Patch";
const BEGIN_STANDARD: &str = "*** Begin Patch";
const END_STANDARD: &str = "*** End Patch";
const ADD_PREFIX: &str = "*** Add File: ";
const UPDATE_PREFIX: &str = "*** Update File: ";
const DELETE_PREFIX: &str = "*** Delete File: ";

/// Longest patch line preview embedded in a parse error.
const PREVIEW_CHARS: usize = 96;

/// File section kinds accepted by the patch grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Add,
    Update,
    Delete,
}

impl SectionKind {
    fn marker(self) -> &'static str {
        match self {
            Self::Add => ADD_PREFIX,
            Self::Update => UPDATE_PREFIX,
            Self::Delete => DELETE_PREFIX,
        }
    }

    /// Splits a `*** Add File:`, `*** Update File:`, or `*** Delete File:`
    /// header into its kind and the raw path that follows it.
    ///
    /// Every place that decides whether a line starts a new file section uses
    /// this, so adding another section kind cannot leave a body parser that
    /// still swallows the new header as content.
    fn from_header(line: &str) -> Option<(Self, &str)> {
        [Self::Add, Self::Update, Self::Delete]
            .into_iter()
            .find_map(|kind| line.strip_prefix(kind.marker()).map(|path| (kind, path)))
    }
}

#[derive(Debug)]
pub(super) struct WorkspacePatchParseError {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) path: Option<String>,
}

impl WorkspacePatchParseError {
    /// Creates a failure for a patch body that does not follow the grammar.
    fn syntax(message: impl Into<String>, path: Option<String>) -> Self {
        Self {
            code: ERROR_PATCH_SYNTAX,
            message: message.into(),
            path,
        }
    }

    /// Creates a failure for a patch that would change nothing.
    fn noop(message: impl Into<String>, path: Option<String>) -> Self {
        Self {
            code: ERROR_PATCH_NOOP,
            message: message.into(),
            path,
        }
    }
}

pub(super) fn parse_apply_patch(
    raw_patch: &str,
) -> Result<WorkspacePatch, WorkspacePatchParseError> {
    let raw_patch = raw_patch.strip_prefix('\u{feff}').unwrap_or(raw_patch);
    let lines = raw_patch.lines().collect::<Vec<_>>();
    let mut index = 0;
    skip_blank_patch_lines(&lines, &mut index);

    let end = match patch_line(lines.get(index).copied()) {
        Some(BEGIN_WORKSPACE) => END_WORKSPACE,
        Some(BEGIN_STANDARD) => END_STANDARD,
        Some(first) => {
            return Err(WorkspacePatchParseError::syntax(
                format!(
                    "workspace patch must start with `*** Begin Patch` (or `*** Begin Workspace Patch`); the first non-blank line is `{}`",
                    single_line_preview(first, PREVIEW_CHARS),
                ),
                None,
            ));
        }
        None => {
            return Err(WorkspacePatchParseError::syntax(
                "workspace patch must not be empty; send one `*** Begin Patch` ... `*** End Patch` envelope with the file sections inside it",
                None,
            ));
        }
    };
    index += 1;

    let mut files: Vec<WorkspacePatchFile> = Vec::new();
    let mut file_index_by_path: BTreeMap<String, usize> = BTreeMap::new();
    loop {
        skip_blank_patch_lines(&lines, &mut index);
        let Some(line) = patch_line(lines.get(index).copied()) else {
            return Err(WorkspacePatchParseError::syntax(
                format!("workspace patch must end with `{end}`"),
                None,
            ));
        };
        if line == end {
            index += 1;
            skip_blank_patch_lines(&lines, &mut index);
            if index != lines.len() {
                let trailing = lines.get(index).copied().unwrap_or_default();
                return Err(WorkspacePatchParseError::syntax(
                    format!(
                        "workspace patch must not contain text after `{end}`; found `{}`",
                        single_line_preview(trailing, PREVIEW_CHARS),
                    ),
                    None,
                ));
            }
            break;
        }
        if line == BEGIN_WORKSPACE || line == BEGIN_STANDARD {
            return Err(WorkspacePatchParseError::syntax(
                "workspace patch contains a duplicate begin marker; provide exactly one patch envelope",
                None,
            ));
        }

        let Some((kind, path)) = SectionKind::from_header(line) else {
            return Err(WorkspacePatchParseError::syntax(
                format!(
                    "workspace patch expected `*** Add File: <path>`, `*** Update File: <path>`, or `*** Delete File: <path>`; found `{}`",
                    single_line_preview(line, PREVIEW_CHARS),
                ),
                None,
            ));
        };
        let path = path.trim();
        if path.is_empty() {
            return Err(WorkspacePatchParseError::syntax(
                format!(
                    "workspace patch {} section must name a workspace-relative path",
                    kind.marker().trim(),
                ),
                None,
            ));
        }
        let path = path.to_owned();
        index += 1;

        let operation = match kind {
            SectionKind::Add => WorkspacePatchOperation::Add {
                lines: parse_apply_patch_add_lines(&lines, &mut index, &path, end)?,
            },
            SectionKind::Update => WorkspacePatchOperation::Update {
                hunks: parse_apply_patch_update_hunks(&lines, &mut index, &path, end)?,
            },
            SectionKind::Delete => {
                parse_apply_patch_delete_section(&lines, &mut index, &path, end)?;
                WorkspacePatchOperation::Delete
            }
        };

        match file_index_by_path.get(&path).copied() {
            None => {
                file_index_by_path.insert(path.clone(), files.len());
                files.push(WorkspacePatchFile {
                    path,
                    operation,
                    ignored_context_hunks: 0,
                });
            }
            // Repeated update sections for one file are a common shape when a
            // caller edits distant regions, so they merge into a single file
            // plan that still fails as a whole when any hunk misses.
            Some(existing) => match (&mut files[existing].operation, operation) {
                (
                    WorkspacePatchOperation::Update { hunks },
                    WorkspacePatchOperation::Update { hunks: additional },
                ) => hunks.extend(additional),
                (WorkspacePatchOperation::Add { .. }, WorkspacePatchOperation::Add { .. }) => {
                    return Err(WorkspacePatchParseError::syntax(
                        format!(
                            "workspace patch adds `{path}` more than once; use one {ADD_PREFIX}section per new file"
                        ),
                        Some(path),
                    ));
                }
                _ => {
                    return Err(WorkspacePatchParseError::syntax(
                        format!(
                            "workspace patch mixes Add File, Update File, or Delete File sections for `{path}`; use one section per file and merge its hunks into that section"
                        ),
                        Some(path),
                    ));
                }
            },
        }
    }

    if files.is_empty() {
        return Err(WorkspacePatchParseError::syntax(
            "workspace patch must contain at least one file section (`*** Add File:`, `*** Update File:`, or `*** Delete File:`)",
            None,
        ));
    }

    if !files.iter().any(WorkspacePatchFile::has_edit) {
        return Err(WorkspacePatchParseError::noop(
            "workspace patch contains no `+` or `-` lines, so nothing would change. Send the added or removed lines as `+`/`-` hunk lines to edit the file, or use `read_text` when you only need to inspect the current content",
            match files.as_slice() {
                [file] => Some(file.path.clone()),
                _ => None,
            },
        ));
    }

    // Context-only hunks describe the caller's belief about unchanged lines,
    // not edits. Once the envelope is known to contain a real edit they are
    // anchored by the edited hunks anyway, so they are dropped and counted
    // instead of silently disappearing.
    for file in &mut files {
        if let WorkspacePatchOperation::Update { hunks } = &mut file.operation {
            let before = hunks.len();
            hunks.retain(WorkspacePatchHunk::has_edit);
            file.ignored_context_hunks = before - hunks.len();
        }
    }
    files.retain(WorkspacePatchFile::has_edit);

    Ok(WorkspacePatch { files })
}

pub(super) fn parse_apply_patch_update_hunks(
    lines: &[&str],
    index: &mut usize,
    path: &str,
    end: &str,
) -> Result<Vec<WorkspacePatchHunk>, WorkspacePatchParseError> {
    let mut hunks = Vec::new();
    let mut current = Vec::new();
    while let Some(line) = patch_line(lines.get(*index).copied()) {
        if line == end || SectionKind::from_header(line).is_some() {
            break;
        }
        if line.trim().is_empty() && current.is_empty() {
            *index += 1;
            continue;
        }
        if line.starts_with("@@") {
            push_apply_patch_hunk(&mut hunks, &mut current);
            *index += 1;
            continue;
        }
        let Some((prefix, text)) = line.split_at_checked(1) else {
            return Err(WorkspacePatchParseError::syntax(
                "workspace patch hunk line must start with a space, `+`, or `-`; found a blank line where a hunk line was expected (prefix context lines with one space)",
                Some(path.to_owned()),
            ));
        };
        match prefix {
            " " => current.push(WorkspacePatchLine::Context(text.to_owned())),
            "-" => current.push(WorkspacePatchLine::Remove(text.to_owned())),
            "+" => current.push(WorkspacePatchLine::Add(text.to_owned())),
            _ => {
                return Err(WorkspacePatchParseError::syntax(
                    format!(
                        "workspace patch hunk line must start with a space, `+`, or `-`; found `{}`",
                        single_line_preview(line, PREVIEW_CHARS),
                    ),
                    Some(path.to_owned()),
                ));
            }
        }
        *index += 1;
    }
    push_apply_patch_hunk(&mut hunks, &mut current);

    if hunks.is_empty() {
        return Err(WorkspacePatchParseError::syntax(
            format!(
                "workspace patch update section for `{path}` contains no hunk lines; send `@@` followed by context, `+`, or `-` lines"
            ),
            Some(path.to_owned()),
        ));
    }
    Ok(hunks)
}

fn parse_apply_patch_add_lines(
    lines: &[&str],
    index: &mut usize,
    path: &str,
    end: &str,
) -> Result<Vec<String>, WorkspacePatchParseError> {
    let mut contents = Vec::new();
    while let Some(line) = patch_line(lines.get(*index).copied()) {
        if line == end || SectionKind::from_header(line).is_some() {
            break;
        }

        // Tolerate model formatting whitespace; an intentional empty file line
        // still uses a `+` prefix and is preserved in `contents`.
        if line.trim().is_empty() {
            *index += 1;
            continue;
        }

        let Some((prefix, text)) = line.split_at_checked(1) else {
            return Err(WorkspacePatchParseError::syntax(
                "workspace patch add lines must start with `+`; found a blank line",
                Some(path.to_owned()),
            ));
        };
        if prefix != "+" {
            return Err(WorkspacePatchParseError::syntax(
                format!(
                    "workspace patch add lines must start with `+`; found `{}`",
                    single_line_preview(line, PREVIEW_CHARS),
                ),
                Some(path.to_owned()),
            ));
        }
        contents.push(text.to_owned());
        *index += 1;
    }

    if contents.is_empty() {
        return Err(WorkspacePatchParseError::syntax(
            format!(
                "workspace patch add section for `{path}` contains no `+` lines; every line of the new file needs a `+` prefix"
            ),
            Some(path.to_owned()),
        ));
    }

    Ok(contents)
}

/// Consumes a `*** Delete File:` section, which has no hunk body.
fn parse_apply_patch_delete_section(
    lines: &[&str],
    index: &mut usize,
    path: &str,
    end: &str,
) -> Result<(), WorkspacePatchParseError> {
    while let Some(line) = patch_line(lines.get(*index).copied()) {
        if line == end || SectionKind::from_header(line).is_some() {
            return Ok(());
        }
        if line.trim().is_empty() {
            *index += 1;
            continue;
        }

        return Err(WorkspacePatchParseError::syntax(
            format!(
                "workspace patch delete section for `{path}` must not contain content lines; found `{}`",
                single_line_preview(line, PREVIEW_CHARS),
            ),
            Some(path.to_owned()),
        ));
    }
    Ok(())
}

/// Moves the accumulated hunk lines into the section's hunk list.
///
/// Context-only hunks are kept here on purpose: the caller decides whether to
/// drop them next to real edits or to report a patch that changes nothing.
fn push_apply_patch_hunk(
    hunks: &mut Vec<WorkspacePatchHunk>,
    current: &mut Vec<WorkspacePatchLine>,
) {
    if current.is_empty() {
        return;
    }
    hunks.push(WorkspacePatchHunk {
        lines: std::mem::take(current),
    });
}

fn patch_line(line: Option<&str>) -> Option<&str> {
    line.map(|line| line.strip_suffix('\r').unwrap_or(line))
}

fn skip_blank_patch_lines(lines: &[&str], index: &mut usize) {
    while matches!(patch_line(lines.get(*index).copied()), Some(line) if line.trim().is_empty()) {
        *index += 1;
    }
}
