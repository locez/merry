use crate::errors::{
    BlockingToolError, DomainError, ERROR_PATCH_SYNTAX, ERROR_PREIMAGE_ABSENT,
    ERROR_PREIMAGE_AMBIGUOUS,
};

use super::{
    diagnostic::{describe_preimage_ambiguity, describe_preimage_miss, line_number_at_byte},
    envelope::{WorkspacePatchSuccessLine, WorkspacePatchSuccessLineKind},
};

/// Counts the lines of UTF-8 file content.
///
/// The count matches how a reader sees the file rather than how many newline
/// bytes it holds: `str::lines()` ignores a trailing `\r`, so CRLF content
/// reports the same count as LF content, and empty content reports zero lines.
pub(super) fn count_file_lines(content: &str) -> usize {
    content.lines().count()
}

#[derive(Debug)]
pub(super) struct WorkspacePatch {
    pub(super) files: Vec<WorkspacePatchFile>,
}

#[derive(Debug)]
pub(super) struct WorkspacePatchFile {
    pub(super) path: String,
    pub(super) operation: WorkspacePatchOperation,
    /// Number of context-only hunks dropped from this file's update sections.
    pub(super) ignored_context_hunks: usize,
}

impl WorkspacePatchFile {
    /// Reports whether this file section changes any content.
    pub(super) fn has_edit(&self) -> bool {
        match &self.operation {
            WorkspacePatchOperation::Add { .. } | WorkspacePatchOperation::Delete => true,
            WorkspacePatchOperation::Update { hunks } => {
                hunks.iter().any(WorkspacePatchHunk::has_edit)
            }
        }
    }
}

#[derive(Debug)]
pub(super) enum WorkspacePatchOperation {
    Add { lines: Vec<String> },
    Update { hunks: Vec<WorkspacePatchHunk> },
    Delete,
}

#[derive(Debug)]
pub(super) struct WorkspacePatchHunk {
    pub(super) lines: Vec<WorkspacePatchLine>,
}

impl WorkspacePatchHunk {
    pub(super) fn has_edit(&self) -> bool {
        self.lines.iter().any(|line| {
            matches!(
                line,
                WorkspacePatchLine::Remove(_) | WorkspacePatchLine::Add(_)
            )
        })
    }

    pub(super) fn old_text(&self, trailing_newline: bool) -> String {
        collect_patch_hunk_text(
            self.lines.iter().filter_map(|line| match line {
                WorkspacePatchLine::Context(text) | WorkspacePatchLine::Remove(text) => Some(text),
                WorkspacePatchLine::Add(_) => None,
            }),
            trailing_newline,
        )
    }

    pub(super) fn new_text(&self, trailing_newline: bool) -> String {
        collect_patch_hunk_text(
            self.lines.iter().filter_map(|line| match line {
                WorkspacePatchLine::Context(text) | WorkspacePatchLine::Add(text) => Some(text),
                WorkspacePatchLine::Remove(_) => None,
            }),
            trailing_newline,
        )
    }
}

#[derive(Debug)]
pub(super) enum WorkspacePatchLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Debug)]
pub(super) struct WorkspacePatchReplacement {
    pub(super) text: String,
    pub(super) preimage_bytes: usize,
    pub(super) replacement_bytes: usize,
    pub(super) lines: Vec<WorkspacePatchSuccessLine>,
}

pub(super) fn build_patch_replacement(
    content: &str,
    hunks: &[WorkspacePatchHunk],
) -> Result<WorkspacePatchReplacement, BlockingToolError> {
    let mut replacement = content.to_owned();
    let trailing_newline = content.ends_with('\n');
    let mut preimage_bytes = 0usize;
    let mut replacement_bytes = 0usize;
    let mut lines = Vec::new();
    let mut applied_line_delta = 0i64;

    for hunk in hunks {
        let old_text = hunk.old_text(trailing_newline);
        let new_text = hunk.new_text(trailing_newline);
        if old_text.is_empty() {
            return Err(DomainError::new(
                ERROR_PATCH_SYNTAX,
                "workspace patch hunk has only + lines and no anchor; include at least one context line or removed line so the insert position is unambiguous",
            )
            .into());
        }
        let result =
            build_replacement(&replacement, &old_text, &new_text, hunk, applied_line_delta)?;
        replacement = result.text;
        preimage_bytes = preimage_bytes.saturating_add(old_text.len());
        replacement_bytes = replacement_bytes.saturating_add(new_text.len());
        lines.extend(result.lines);
        applied_line_delta += line_delta_for_hunk(hunk);
    }

    Ok(WorkspacePatchReplacement {
        text: replacement,
        preimage_bytes,
        replacement_bytes,
        lines,
    })
}

pub(super) fn build_new_file_replacement(lines: &[String]) -> WorkspacePatchReplacement {
    let mut text = String::new();
    let mut success_lines = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        text.push_str(line);
        text.push('\n');
        success_lines.push(WorkspacePatchSuccessLine {
            kind: WorkspacePatchSuccessLineKind::Add,
            old_line: None,
            new_line: Some(index + 1),
            text: line.clone(),
        });
    }

    WorkspacePatchReplacement {
        preimage_bytes: 0,
        replacement_bytes: text.len(),
        text,
        lines: success_lines,
    }
}

fn collect_patch_hunk_text<'a>(
    lines: impl Iterator<Item = &'a String>,
    trailing_newline: bool,
) -> String {
    let mut output = String::new();
    let mut count = 0usize;
    for line in lines {
        output.push_str(line);
        output.push('\n');
        count += 1;
    }
    if count > 0 && !trailing_newline {
        output.pop();
    }
    output
}

struct SingleHunkReplacement {
    text: String,
    lines: Vec<WorkspacePatchSuccessLine>,
}

fn build_replacement(
    content: &str,
    old_text: &str,
    new_text: &str,
    hunk: &WorkspacePatchHunk,
    applied_line_delta: i64,
) -> Result<SingleHunkReplacement, BlockingToolError> {
    let Some(start) = content.find(old_text) else {
        return Err(DomainError::new(
            ERROR_PREIMAGE_ABSENT,
            format!(
                "workspace patch preimage was not found{}",
                describe_preimage_miss(content, old_text)
            ),
        )
        .into());
    };

    let after_start = start + old_text.len();
    if content[after_start..].contains(old_text) {
        return Err(DomainError::new(
            ERROR_PREIMAGE_AMBIGUOUS,
            format!(
                "workspace patch preimage matched more than once{}",
                describe_preimage_ambiguity(content, old_text, start)
            ),
        )
        .into());
    }

    let mut replacement = String::with_capacity(
        content
            .len()
            .saturating_sub(old_text.len())
            .saturating_add(new_text.len()),
    );
    replacement.push_str(&content[..start]);
    replacement.push_str(new_text);
    replacement.push_str(&content[after_start..]);
    let new_start_line = line_number_at_byte(content, start);
    let old_start_line = old_line_number_before_applied_delta(new_start_line, applied_line_delta);
    Ok(SingleHunkReplacement {
        text: replacement,
        lines: success_lines_for_hunk(old_start_line, new_start_line, hunk),
    })
}

fn success_lines_for_hunk(
    old_start_line: usize,
    new_start_line: usize,
    hunk: &WorkspacePatchHunk,
) -> Vec<WorkspacePatchSuccessLine> {
    let mut old_line = old_start_line;
    let mut new_line = new_start_line;
    let mut lines = Vec::with_capacity(hunk.lines.len());

    for line in &hunk.lines {
        match line {
            WorkspacePatchLine::Context(text) => {
                lines.push(WorkspacePatchSuccessLine {
                    kind: WorkspacePatchSuccessLineKind::Context,
                    old_line: Some(old_line),
                    new_line: Some(new_line),
                    text: text.clone(),
                });
                old_line += 1;
                new_line += 1;
            }
            WorkspacePatchLine::Remove(text) => {
                lines.push(WorkspacePatchSuccessLine {
                    kind: WorkspacePatchSuccessLineKind::Remove,
                    old_line: Some(old_line),
                    new_line: None,
                    text: text.clone(),
                });
                old_line += 1;
            }
            WorkspacePatchLine::Add(text) => {
                lines.push(WorkspacePatchSuccessLine {
                    kind: WorkspacePatchSuccessLineKind::Add,
                    old_line: None,
                    new_line: Some(new_line),
                    text: text.clone(),
                });
                new_line += 1;
            }
        }
    }

    lines
}

fn old_line_number_before_applied_delta(new_line: usize, applied_line_delta: i64) -> usize {
    let old_line = (new_line as i64).saturating_sub(applied_line_delta);
    old_line.max(1) as usize
}

fn line_delta_for_hunk(hunk: &WorkspacePatchHunk) -> i64 {
    hunk.lines.iter().fold(0, |delta, line| match line {
        WorkspacePatchLine::Context(_) => delta,
        WorkspacePatchLine::Remove(_) => delta - 1,
        WorkspacePatchLine::Add(_) => delta + 1,
    })
}

pub(crate) fn stable_content_fingerprint(bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    format!("fnv1a64:{hash:016x}")
}
