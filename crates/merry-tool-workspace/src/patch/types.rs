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

pub(super) fn build_patch_replacement(
    content: &str,
    hunks: &[WorkspacePatchHunk],
) -> Result<(String, usize, usize), BlockingToolError> {
    let mut replacement = content.to_owned();
    let trailing_newline = content.ends_with('\n');
    let mut preimage_bytes = 0usize;
    let mut replacement_bytes = 0usize;

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
        replacement = build_replacement(&replacement, &old_text, &new_text)?;
        preimage_bytes = preimage_bytes.saturating_add(old_text.len());
        replacement_bytes = replacement_bytes.saturating_add(new_text.len());
    }

    Ok((replacement, preimage_bytes, replacement_bytes))
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

fn build_replacement(
    content: &str,
    old_text: &str,
    new_text: &str,
) -> Result<String, BlockingToolError> {
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
    Ok(replacement)
}

pub(crate) fn stable_content_fingerprint(bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    format!("fnv1a64:{hash:016x}")
}
