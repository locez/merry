use std::collections::BTreeSet;

use super::types::{
    WorkspacePatch, WorkspacePatchFile, WorkspacePatchHunk, WorkspacePatchLine,
    WorkspacePatchOperation,
};

#[derive(Debug)]
pub(super) struct WorkspacePatchParseError {
    pub(super) message: &'static str,
    pub(super) path: Option<String>,
}

impl WorkspacePatchParseError {
    fn new(message: &'static str, path: Option<String>) -> Self {
        Self { message, path }
    }
}

pub(super) fn parse_workspace_patch(
    raw_patch: &str,
) -> Result<WorkspacePatch, WorkspacePatchParseError> {
    const BEGIN_WORKSPACE: &str = "*** Begin Workspace Patch";
    const END_WORKSPACE: &str = "*** End Workspace Patch";
    const BEGIN_STANDARD: &str = "*** Begin Patch";
    const END_STANDARD: &str = "*** End Patch";
    const ADD_PREFIX: &str = "*** Add File: ";
    const UPDATE_PREFIX: &str = "*** Update File: ";

    let raw_patch = raw_patch.strip_prefix('\u{feff}').unwrap_or(raw_patch);
    let lines = raw_patch.lines().collect::<Vec<_>>();
    let mut index = 0;
    skip_blank_patch_lines(&lines, &mut index);

    let end = match patch_line(lines.get(index).copied()) {
        Some(BEGIN_WORKSPACE) => END_WORKSPACE,
        Some(BEGIN_STANDARD) => END_STANDARD,
        _ => {
            return Err(WorkspacePatchParseError::new(
                "workspace patch must start with *** Begin Workspace Patch",
                None,
            ));
        }
    };
    index += 1;

    let mut files = Vec::new();
    let mut seen_paths = BTreeSet::new();
    loop {
        skip_blank_patch_lines(&lines, &mut index);
        let Some(line) = patch_line(lines.get(index).copied()) else {
            return Err(WorkspacePatchParseError::new(
                "workspace patch must end with *** End Workspace Patch",
                None,
            ));
        };
        if line == end {
            index += 1;
            skip_blank_patch_lines(&lines, &mut index);
            if index != lines.len() {
                return Err(WorkspacePatchParseError::new(
                    "workspace patch must not contain text after *** End Workspace Patch",
                    None,
                ));
            }
            break;
        }

        let (is_add, path) = if let Some(path) = line.strip_prefix(ADD_PREFIX) {
            (true, path.trim())
        } else if let Some(path) = line.strip_prefix(UPDATE_PREFIX) {
            (false, path.trim())
        } else {
            return Err(WorkspacePatchParseError::new(
                "workspace patch expected *** Add File: <path> or *** Update File: <path>",
                None,
            ));
        };
        if path.is_empty() {
            return Err(WorkspacePatchParseError::new(
                "workspace patch file path must not be empty",
                None,
            ));
        }
        let path = path.to_owned();
        if !seen_paths.insert(path.clone()) {
            return Err(WorkspacePatchParseError::new(
                "workspace patch must not operate on the same file more than once",
                Some(path),
            ));
        }
        index += 1;

        let operation = if is_add {
            WorkspacePatchOperation::Add {
                lines: parse_workspace_patch_add_lines(&lines, &mut index, &path, end)?,
            }
        } else {
            WorkspacePatchOperation::Update {
                hunks: parse_workspace_patch_update_hunks(&lines, &mut index, &path, end)?,
            }
        };
        files.push(WorkspacePatchFile { path, operation });
    }

    if files.is_empty() {
        return Err(WorkspacePatchParseError::new(
            "workspace patch must contain at least one file operation",
            None,
        ));
    }

    Ok(WorkspacePatch { files })
}

pub(super) fn parse_workspace_patch_update_hunks(
    lines: &[&str],
    index: &mut usize,
    path: &str,
    end: &str,
) -> Result<Vec<WorkspacePatchHunk>, WorkspacePatchParseError> {
    const ADD_PREFIX: &str = "*** Add File: ";
    const UPDATE_PREFIX: &str = "*** Update File: ";

    let mut hunks = Vec::new();
    let mut current = Vec::new();
    while let Some(line) = patch_line(lines.get(*index).copied()) {
        if line == end || line.starts_with(ADD_PREFIX) || line.starts_with(UPDATE_PREFIX) {
            break;
        }
        if line.trim().is_empty() && current.is_empty() {
            *index += 1;
            continue;
        }
        if line.starts_with("@@") {
            push_workspace_patch_hunk(&mut hunks, &mut current, path)?;
            *index += 1;
            continue;
        }
        let Some((prefix, text)) = line.split_at_checked(1) else {
            return Err(WorkspacePatchParseError::new(
                "workspace patch hunk line must start with space, +, or -",
                Some(path.to_owned()),
            ));
        };
        match prefix {
            " " => current.push(WorkspacePatchLine::Context(text.to_owned())),
            "-" => current.push(WorkspacePatchLine::Remove(text.to_owned())),
            "+" => current.push(WorkspacePatchLine::Add(text.to_owned())),
            _ => {
                return Err(WorkspacePatchParseError::new(
                    "workspace patch hunk line must start with space, +, or -",
                    Some(path.to_owned()),
                ));
            }
        }
        *index += 1;
    }
    push_workspace_patch_hunk(&mut hunks, &mut current, path)?;

    if hunks.is_empty() {
        return Err(WorkspacePatchParseError::new(
            "workspace patch update must contain at least one hunk",
            Some(path.to_owned()),
        ));
    }
    Ok(hunks)
}

fn parse_workspace_patch_add_lines(
    lines: &[&str],
    index: &mut usize,
    path: &str,
    end: &str,
) -> Result<Vec<String>, WorkspacePatchParseError> {
    const ADD_PREFIX: &str = "*** Add File: ";
    const UPDATE_PREFIX: &str = "*** Update File: ";

    let mut contents = Vec::new();
    while let Some(line) = patch_line(lines.get(*index).copied()) {
        if line == end || line.starts_with(ADD_PREFIX) || line.starts_with(UPDATE_PREFIX) {
            break;
        }

        // Tolerate model formatting whitespace; an intentional empty file line
        // still uses a `+` prefix and is preserved in `contents`.
        if line.trim().is_empty() {
            *index += 1;
            continue;
        }

        let Some((prefix, text)) = line.split_at_checked(1) else {
            return Err(WorkspacePatchParseError::new(
                "workspace patch add lines must start with +",
                Some(path.to_owned()),
            ));
        };
        if prefix != "+" {
            return Err(WorkspacePatchParseError::new(
                "workspace patch add lines must start with +",
                Some(path.to_owned()),
            ));
        }
        contents.push(text.to_owned());
        *index += 1;
    }

    if contents.is_empty() {
        return Err(WorkspacePatchParseError::new(
            "workspace patch add must contain at least one + line",
            Some(path.to_owned()),
        ));
    }

    Ok(contents)
}

fn push_workspace_patch_hunk(
    hunks: &mut Vec<WorkspacePatchHunk>,
    current: &mut Vec<WorkspacePatchLine>,
    path: &str,
) -> Result<(), WorkspacePatchParseError> {
    if current.is_empty() {
        return Ok(());
    }
    let hunk = WorkspacePatchHunk {
        lines: std::mem::take(current),
    };
    if !hunk.has_edit() {
        return Err(WorkspacePatchParseError::new(
            "workspace patch hunk must add or remove at least one line",
            Some(path.to_owned()),
        ));
    }
    hunks.push(hunk);
    Ok(())
}

fn patch_line(line: Option<&str>) -> Option<&str> {
    line.map(|line| line.strip_suffix('\r').unwrap_or(line))
}

fn skip_blank_patch_lines(lines: &[&str], index: &mut usize) {
    while matches!(patch_line(lines.get(*index).copied()), Some(line) if line.trim().is_empty()) {
        *index += 1;
    }
}
