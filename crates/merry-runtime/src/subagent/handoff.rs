//! Preloaded context file handoff and expected-output rendering for child runtimes.

use super::{ChildWorkspaceScope, SubagentError, spec::validate_scope_path};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Maximum total UTF-8 bytes preloaded across all `context_files` for one child task.
pub const MAX_CONTEXT_FILES_TOTAL_BYTES: usize = 32 * 1024;

/// Maximum number of `context_files` references accepted for one child task.
pub const MAX_CONTEXT_FILE_REFS: usize = 32;

/// Validated workspace-relative file reference with an optional 1-based inclusive line range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFileRef {
    path: PathBuf,
    line_range: Option<(usize, usize)>,
}

impl ContextFileRef {
    /// Parses a file reference of the form `path/to/file.rs` or `path/to/file.rs:1-120`.
    pub fn parse(raw: &str) -> Result<Self, SubagentError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(SubagentError::InvalidContextFileRef {
                value: raw.to_owned(),
            });
        }
        let (path_str, line_range) = match trimmed.rsplit_once(':') {
            Some((prefix, range_part)) if looks_like_line_range(range_part) => {
                let parsed = parse_line_range(range_part).ok_or_else(|| {
                    SubagentError::InvalidContextFileRef {
                        value: raw.to_owned(),
                    }
                })?;
                (prefix, Some(parsed))
            }
            _ => (trimmed, None),
        };
        if path_str == "." {
            return Err(SubagentError::InvalidContextFileRef {
                value: raw.to_owned(),
            });
        }
        let path = validate_scope_path(PathBuf::from(path_str)).map_err(|_| {
            SubagentError::InvalidContextFileRef {
                value: raw.to_owned(),
            }
        })?;
        Ok(Self { path, line_range })
    }

    /// Returns the validated workspace-relative path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the optional 1-based inclusive `(start, end)` line range.
    #[must_use]
    pub const fn line_range(&self) -> Option<(usize, usize)> {
        self.line_range
    }

    /// Formats the canonical reference label (`path` or `path:start-end`).
    #[must_use]
    pub fn display_label(&self) -> String {
        match self.line_range {
            Some((start, end)) => format!("{}:{start}-{end}", self.path.display()),
            None => self.path.display().to_string(),
        }
    }
}

/// One preloaded file slice ready for injection into `<merry_subagent_handoff>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreloadedContextFile {
    /// Display path (including line range if requested).
    pub label: String,
    /// Preloaded UTF-8 content or a compact unavailable/truncated notice.
    pub content: String,
}

/// Safely preloads `context_files` within the child's effective read boundary and byte cap.
#[must_use]
pub fn preload_context_files(
    workspace_root: &Path,
    scope: &ChildWorkspaceScope,
    refs: &[ContextFileRef],
) -> Vec<PreloadedContextFile> {
    if refs.is_empty() {
        return Vec::new();
    }
    let canonical_root = fs::canonicalize(workspace_root).ok();
    let mut remaining_bytes = MAX_CONTEXT_FILES_TOTAL_BYTES;
    let mut loaded = Vec::with_capacity(refs.len());

    for file_ref in refs {
        let label = file_ref.display_label();
        if !path_allowed_by_scope(file_ref.path(), scope) {
            loaded.push(PreloadedContextFile {
                label,
                content: "[skipped: path is outside read_scope or inside forbidden_paths]"
                    .to_owned(),
            });
            continue;
        }
        if remaining_bytes == 0 {
            loaded.push(PreloadedContextFile {
                label,
                content: "[skipped: context_files byte budget exhausted; inspect directly with read_text]".to_owned(),
            });
            continue;
        }
        let full_path = workspace_root.join(file_ref.path());
        if let Some(root) = canonical_root.as_deref() {
            match fs::canonicalize(&full_path) {
                Ok(canonical_file) if canonical_file.starts_with(root) => {}
                Ok(_) => {
                    loaded.push(PreloadedContextFile {
                        label,
                        content: "[skipped: symlink escapes workspace root]".to_owned(),
                    });
                    continue;
                }
                Err(_) => {
                    loaded.push(PreloadedContextFile {
                        label,
                        content: "[unavailable: file does not exist or cannot be read]".to_owned(),
                    });
                    continue;
                }
            }
        }
        let raw_bytes = match fs::read(&full_path) {
            Ok(bytes) => bytes,
            Err(_) => {
                loaded.push(PreloadedContextFile {
                    label,
                    content: "[unavailable: file could not be read]".to_owned(),
                });
                continue;
            }
        };
        let Ok(text) = String::from_utf8(raw_bytes) else {
            loaded.push(PreloadedContextFile {
                label,
                content: "[skipped: binary or non-UTF-8 file]".to_owned(),
            });
            continue;
        };
        let sliced = slice_lines(&text, file_ref.line_range());
        let content = truncate_utf8(&sliced, remaining_bytes);
        remaining_bytes = remaining_bytes.saturating_sub(content.len());
        loaded.push(PreloadedContextFile { label, content });
    }

    loaded
}

/// Renders `<merry_subagent_handoff>` when either preloaded files or `expected_output` exist.
#[must_use]
pub fn render_subagent_handoff(
    expected_output: Option<&str>,
    files: &[PreloadedContextFile],
) -> Option<String> {
    let expected_output = expected_output
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if expected_output.is_none() && files.is_empty() {
        return None;
    }

    let mut sections = Vec::new();
    if let Some(expected) = expected_output {
        sections.push(format!("<expected_output>\n{expected}\n</expected_output>"));
    }
    for file in files {
        let mut block = format!("<context_file path=\"{}\">\n", file.label);
        block.push_str(&file.content);
        if !file.content.ends_with('\n') {
            block.push('\n');
        }
        block.push_str("</context_file>");
        sections.push(block);
    }

    Some(crate::prompt::render_prompt_block(
        "merry_subagent_handoff",
        &sections.join("\n\n"),
    ))
}

fn path_allowed_by_scope(path: &Path, scope: &ChildWorkspaceScope) -> bool {
    let in_read_scope = scope
        .read_scope()
        .iter()
        .any(|root| root == Path::new(".") || path == root || path.starts_with(root));
    if !in_read_scope {
        return false;
    }
    let in_forbidden = scope
        .forbidden_paths()
        .iter()
        .any(|denied| denied == Path::new(".") || path == denied || path.starts_with(denied));
    !in_forbidden
}

fn looks_like_line_range(part: &str) -> bool {
    !part.is_empty()
        && part
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
}

fn parse_line_range(part: &str) -> Option<(usize, usize)> {
    let (start_str, end_str) = match part.split_once('-') {
        Some((start, end)) => (start, end),
        None => (part, part),
    };
    let start: usize = start_str.parse().ok()?;
    let end: usize = end_str.parse().ok()?;
    if start == 0 || end < start {
        return None;
    }
    Some((start, end))
}

fn slice_lines(text: &str, line_range: Option<(usize, usize)>) -> String {
    let Some((start, end)) = line_range else {
        return text.to_owned();
    };
    let mut out = String::new();
    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        if line_no < start {
            continue;
        }
        if line_no > end {
            break;
        }
        out.push_str(&format!("{line_no}: {line}\n"));
    }
    out
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let suffix = "\n...[truncated; inspect remaining lines with read_text]";
    let limit = max_bytes.saturating_sub(suffix.len());
    let mut end = limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = text[..end].to_owned();
    truncated.push_str(suffix);
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_line_ranged_context_file_refs() {
        let plain = ContextFileRef::parse("crates/merry-runtime/src/lib.rs").expect("valid ref");
        assert_eq!(plain.path(), Path::new("crates/merry-runtime/src/lib.rs"));
        assert_eq!(plain.line_range(), None);
        assert_eq!(plain.display_label(), "crates/merry-runtime/src/lib.rs");

        let ranged =
            ContextFileRef::parse("crates/merry-runtime/src/lib.rs:10-40").expect("valid range");
        assert_eq!(ranged.path(), Path::new("crates/merry-runtime/src/lib.rs"));
        assert_eq!(ranged.line_range(), Some((10, 40)));
        assert_eq!(
            ranged.display_label(),
            "crates/merry-runtime/src/lib.rs:10-40"
        );

        let single = ContextFileRef::parse("src/main.rs:5").expect("valid single line");
        assert_eq!(single.line_range(), Some((5, 5)));

        for invalid in [
            "",
            ".",
            "../secret.rs",
            "/etc/passwd",
            "src/lib.rs:0-5",
            "src/lib.rs:10-5",
        ] {
            assert!(
                ContextFileRef::parse(invalid).is_err(),
                "{invalid:?} must be rejected"
            );
        }
    }

    #[test]
    fn preloads_line_ranges_enforces_scope_and_renders_handoff() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("src")).expect("src dir");
        fs::create_dir_all(dir.path().join("secret")).expect("secret dir");
        fs::write(dir.path().join("src/a.rs"), "line1\nline2\nline3\nline4\n").expect("write a");
        fs::write(dir.path().join("secret/key.txt"), "top-secret\n").expect("write secret");

        let scope = ChildWorkspaceScope {
            read_scope: vec![PathBuf::from("src")],
            write_scope: Vec::new(),
            forbidden_paths: vec![PathBuf::from("secret")],
        };
        let refs = vec![
            ContextFileRef::parse("src/a.rs:2-3").expect("valid"),
            ContextFileRef::parse("secret/key.txt").expect("valid"),
        ];
        let files = preload_context_files(dir.path(), &scope, &refs);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].content, "2: line2\n3: line3\n");
        assert!(files[1].content.contains("skipped"));

        let handoff = render_subagent_handoff(Some("Return JSON findings."), &files)
            .expect("handoff should render");
        assert!(handoff.starts_with("<merry_subagent_handoff>\n"));
        assert!(handoff.contains("<expected_output>\nReturn JSON findings.\n</expected_output>"));
        assert!(handoff.contains("<context_file path=\"src/a.rs:2-3\">"));
        assert!(handoff.contains("2: line2"));
    }
}
