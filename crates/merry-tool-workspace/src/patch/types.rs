use serde::Serialize;

use crate::errors::{
    BlockingToolError, DomainError, ERROR_INVALID_ARGUMENTS, ERROR_PREIMAGE_ABSENT,
    ERROR_PREIMAGE_AMBIGUOUS,
};

#[derive(Debug, Serialize)]
pub(super) struct WorkspacePatchSuccess {
    pub(super) ok: bool,
    pub(super) tool: &'static str,
    pub(super) changes: Vec<WorkspacePatchSuccessChange>,
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspacePatchSuccessChange {
    pub(super) path: String,
    pub(super) hunks: usize,
    pub(super) bytes_before: usize,
    pub(super) bytes_after: usize,
    pub(super) lines: Vec<WorkspacePatchSuccessLine>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub(super) struct WorkspacePatchSuccessLine {
    pub(super) kind: WorkspacePatchSuccessLineKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) old_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) new_line: Option<usize>,
    pub(super) text: String,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum WorkspacePatchSuccessLineKind {
    Context,
    Remove,
    Add,
}

#[derive(Debug)]
pub(super) struct WorkspacePatch {
    pub(super) files: Vec<WorkspacePatchFile>,
}

#[derive(Debug)]
pub(super) struct WorkspacePatchFile {
    pub(super) path: String,
    pub(super) operation: WorkspacePatchOperation,
}

#[derive(Debug)]
pub(super) enum WorkspacePatchOperation {
    Add { lines: Vec<String> },
    Update { hunks: Vec<WorkspacePatchHunk> },
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
                ERROR_INVALID_ARGUMENTS,
                "workspace patch update hunks must include context or removed text",
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
            "workspace patch preimage was not found",
        )
        .into());
    };

    let after_start = start + old_text.len();
    if content[after_start..].contains(old_text) {
        return Err(DomainError::new(
            ERROR_PREIMAGE_AMBIGUOUS,
            "workspace patch preimage matched more than once",
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

fn line_number_at_byte(content: &str, byte_index: usize) -> usize {
    content[..byte_index]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

pub(crate) fn stable_content_fingerprint(bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    format!("fnv1a64:{hash:016x}")
}
