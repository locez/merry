//! Decodes tool output into bounded presentation values; never owns runtime state.

use crate::{
    apply_patch_argument::{PatchArgumentSectionKind, section_header},
    tool_display::format_tool_call_detail,
    tui::{
        process_output::process_output_preview,
        projector::StartedToolView,
        state::{
            CommandFailure, CommandView, PatchChangeView, PatchLineKind, PatchLineView,
            PatchOperationView, ProcessOutputPreview, TimelineItem,
        },
        text_wrap::truncate_chars,
        tool_error::compact_failed_tool_body,
    },
};
use merry_core::ToolOutput;
use merry_tools::{
    APPLY_PATCH_TOOL, WorkspacePatchOperationKind, WorkspacePatchSuccess,
    WorkspacePatchSuccessLineKind,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

// This bounds the timeline preview only; the workspace tool and Focus retain the full file.
pub(super) const READ_FILE_PREVIEW_MAX_LINES: usize = 120;

pub(super) const READ_FILE_PREVIEW_MAX_CHARS: usize = 180;

pub(super) fn expanded_tool_title(tool: &StartedToolView) -> String {
    if tool.detail.is_empty() {
        return tool.title.clone();
    }
    format!("{} {}", tool.title, tool.detail)
}

pub(super) fn completed_tool_title(tool: &StartedToolView, status: &str) -> String {
    format!("{} -> {status}", expanded_tool_title(tool))
}

pub(super) fn failed_tool_body(diagnostic: Option<&merry_core::ErrorInfo>, text: &str) -> String {
    diagnostic
        .map(|diagnostic| compact_failed_tool_body(diagnostic.code(), diagnostic.message(), text))
        .unwrap_or_else(|| compact_tool_output(text))
}

pub(super) fn tool_output_text(output: Option<ToolOutput>) -> String {
    match output {
        Some(ToolOutput::Text { text }) => text,
        Some(ToolOutput::Json { json }) => json,
        None => String::new(),
    }
}

pub(super) fn started_tool_title_and_detail(
    name: &str,
    arguments: &serde_json::Map<String, Value>,
) -> (String, String) {
    let detail = tui_tool_detail(name, arguments);
    let title = tui_tool_title(name);
    (title.to_owned(), detail)
}

pub(super) fn tui_tool_title(name: &str) -> &'static str {
    if parse_mcp_tool_name(name).is_some() {
        return "MCP";
    }

    match name {
        "run_process" => "Ran",
        "read_text" => "Read",
        "request_permissions" => "Permission",
        "merry_read_checkpoint_ref" => "Retrieved",
        "spawn_subagents" => "Delegated",
        "wait_subagents" => "Waited",
        "cancel_subagents" => "Cancelled",
        APPLY_PATCH_TOOL => "Patch",
        _ => "Tool",
    }
}

pub(super) fn tui_tool_detail(name: &str, arguments: &serde_json::Map<String, Value>) -> String {
    if let Some((server, tool)) = parse_mcp_tool_name(name) {
        let detail = format_tool_call_detail(name, arguments);
        return match detail {
            Some(detail) if !detail.is_empty() => format!("{server}/{tool} {detail}"),
            _ => format!("{server}/{tool}"),
        };
    }

    let detail = format_tool_call_detail(name, arguments);
    if name == "run_process" {
        return detail
            .filter(|detail| !detail.is_empty())
            .unwrap_or_else(|| name.to_owned());
    }

    match detail {
        Some(detail) if !detail.is_empty() => format!("{name} {detail}"),
        _ => name.to_owned(),
    }
}

pub(super) fn parse_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp_")?;
    let (server, tool) = rest.split_once('_')?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

pub(super) fn success_tool_bodies(name: &str, output: &str) -> Option<String> {
    match name {
        "request_permissions" => permission_output_bodies(output),
        "read_text" => read_text_output_bodies(output),
        _ => None,
    }
}

pub(super) fn permission_output_bodies(output: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("ok").and_then(Value::as_bool) != Some(true)
        || value.get("kind").and_then(Value::as_str) != Some("process_action")
    {
        return None;
    }
    let rationale = value
        .pointer("/permission_review/rationale")
        .and_then(Value::as_str)
        .unwrap_or("permission request was admitted");
    let profile = value
        .get("permission_profile_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let body = format!("allowed: {rationale}\nprofile: {profile}");
    Some(body)
}

pub(super) fn read_text_output_bodies(output: &str) -> Option<String> {
    let output = serde_json::from_str::<WorkspaceReadTextOutput>(output).ok()?;
    if !output.ok || output.tool.as_deref() != Some("read_text") {
        return None;
    }

    let mut lines = output
        .content
        .lines()
        .take(READ_FILE_PREVIEW_MAX_LINES)
        .map(|line| truncate_chars(line, READ_FILE_PREVIEW_MAX_CHARS))
        .collect::<Vec<_>>();
    if output.truncated {
        lines.push("... truncated".to_owned());
    }
    if lines.is_empty() {
        lines.push(format!("{} is empty", output.path));
    }
    Some(lines.join("\n"))
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkspaceReadTextOutput {
    pub(super) ok: bool,
    pub(super) tool: Option<String>,
    pub(super) path: String,
    pub(super) content: String,
    #[serde(default)]
    pub(super) truncated: bool,
}

pub(super) fn completed_process_view(
    tool: &StartedToolView,
    output: &str,
    exit_code: Option<i64>,
    result: &merry_core::ToolCallResult,
) -> TimelineItem {
    let failure = if result
        .diagnostic()
        .is_some_and(|diagnostic| diagnostic.code() == merry_core::TOOL_CANCELLED_BY_USER_CODE)
    {
        Some(CommandFailure::Cancelled)
    } else if result.status() == merry_core::ToolCallResultStatus::Failed
        && !exit_code.is_some_and(|code| code != 0)
    {
        Some(CommandFailure::Failed)
    } else {
        None
    };
    let mut preview = process_output_preview(output)
        .unwrap_or_else(|| ProcessOutputPreview::new(&compact_tool_output(output), "", false));
    if failure == Some(CommandFailure::Failed) {
        preview = ProcessOutputPreview::new(
            &failed_tool_body(result.diagnostic(), output),
            "",
            preview.truncated,
        );
    }
    TimelineItem::Command {
        view: CommandView::Finished {
            detail: tool.detail.clone(),
            exit_code,
            failure,
            preview,
            command: tool.command.clone(),
            cwd: tool.cwd.clone(),
            artifact: result.artifact().clone(),
            elapsed: tool.started_at.map(|started_at| started_at.elapsed()),
        },
    }
}

pub(super) fn compact_tool_output(output: &str) -> String {
    let output = output.trim();
    if output.is_empty() {
        return String::new();
    }
    let mut compact = crate::text::without_control_chars_keeping_newlines(output)
        .chars()
        .take(600)
        .collect::<String>();
    if output.chars().count() > 600 {
        compact.push_str("...");
    }
    compact
}

pub(super) fn parse_apply_patch_view(
    output: &str,
    patch_argument: Option<&str>,
) -> Option<TimelineItem> {
    // The envelope type is owned by the tool crate, so a field cannot drift
    // between the writer and this reader.
    let output = serde_json::from_str::<WorkspacePatchSuccess>(output).ok()?;
    if !output.ok || output.tool != APPLY_PATCH_TOOL {
        return None;
    }
    let parsed_patch = patch_argument.map(parse_apply_patch_argument);
    let changes = output
        .changes
        .into_iter()
        .map(|change| {
            let operation = match change.operation() {
                WorkspacePatchOperationKind::Add => PatchOperationView::Add,
                WorkspacePatchOperationKind::Update => PatchOperationView::Update,
                WorkspacePatchOperationKind::Delete => PatchOperationView::Delete,
                // An operation recorded by a newer build still describes a
                // change to a file, so it renders as the historical default
                // instead of falling back to the raw result.
                WorkspacePatchOperationKind::Unknown => PatchOperationView::Update,
            };
            let patch_lines = change
                .lines
                .iter()
                .filter_map(envelope_line_view)
                .collect::<Vec<_>>();
            // Envelopes recorded before the tool echoed hunk lines fall back to
            // the lines of the pending call's own patch argument.
            let patch_lines = if patch_lines.is_empty() {
                parsed_patch
                    .as_ref()
                    .and_then(|parsed| parsed.change_lines(&change.path))
                    .cloned()
                    .unwrap_or_default()
            } else {
                patch_lines
            };
            let hunk_added = patch_lines
                .iter()
                .filter(|line| line.kind == PatchLineKind::Add)
                .count();
            let hunk_removed = patch_lines
                .iter()
                .filter(|line| line.kind == PatchLineKind::Remove)
                .count();
            // A delete reports no hunk lines because the whole file leaves at
            // once, so the file's own line count is the honest removal count.
            let (added, removed) = if operation == PatchOperationView::Delete {
                (0, change.lines_before.unwrap_or(0))
            } else {
                (hunk_added, hunk_removed)
            };
            PatchChangeView {
                path: change.path,
                operation,
                added,
                removed,
                hunks: change.hunks,
                lines_before: change.lines_before,
                lines_after: change.lines_after,
                bytes_before: change.bytes_before,
                bytes_after: change.bytes_after,
                lines: patch_lines,
            }
        })
        .collect::<Vec<_>>();
    Some(TimelineItem::Patch { changes })
}

/// Maps one envelope line onto the timeline's line view.
fn envelope_line_view(line: &merry_tools::WorkspacePatchSuccessLine) -> Option<PatchLineView> {
    let kind = match line.kind {
        WorkspacePatchSuccessLineKind::Context => PatchLineKind::Context,
        WorkspacePatchSuccessLineKind::Remove => PatchLineKind::Remove,
        WorkspacePatchSuccessLineKind::Add => PatchLineKind::Add,
        // A line kind this build does not know is left out of the preview
        // instead of hiding the rest of the change.
        WorkspacePatchSuccessLineKind::Unknown => return None,
    };
    Some(PatchLineView {
        kind,
        old_line: line.old_line,
        new_line: line.new_line,
        text: line.text.clone(),
    })
}

#[derive(Debug, Default, Clone)]
pub(super) struct ParsedPatchArgument {
    pub(super) changes: HashMap<String, Vec<PatchLineView>>,
}

impl ParsedPatchArgument {
    pub(super) fn change_lines(&self, path: &str) -> Option<&Vec<PatchLineView>> {
        self.changes.get(path)
    }
}

pub(super) fn parse_apply_patch_argument(patch: &str) -> ParsedPatchArgument {
    let mut parsed = ParsedPatchArgument::default();
    let mut current_path: Option<String> = None;
    let mut current_lines = Vec::new();
    let mut line_numbers = PatchLineNumbers::default();

    for line in patch.lines() {
        if let Some((kind, path)) = section_header(line) {
            flush_patch_change(&mut parsed, &mut current_path, &mut current_lines);
            current_path = Some(path.to_owned());
            line_numbers = match kind {
                // A created file is numbered from its first line, while an
                // updated or deleted file starts from the hunk headers.
                PatchArgumentSectionKind::Add => PatchLineNumbers {
                    old_next: None,
                    new_next: Some(1),
                },
                PatchArgumentSectionKind::Update | PatchArgumentSectionKind::Delete => {
                    PatchLineNumbers::default()
                }
            };
            continue;
        }
        if line.starts_with("*** ") {
            continue;
        }
        if line.starts_with("@@") {
            if let Some(parsed_numbers) = parse_unified_hunk_header(line) {
                line_numbers = parsed_numbers;
            }
            continue;
        }
        let Some((prefix, text)) = line.split_at_checked(1) else {
            continue;
        };
        match prefix {
            " " => current_lines.push(line_numbers.context(text.to_owned())),
            "+" => current_lines.push(line_numbers.add(text.to_owned())),
            "-" => current_lines.push(line_numbers.remove(text.to_owned())),
            _ => {}
        }
    }

    flush_patch_change(&mut parsed, &mut current_path, &mut current_lines);
    parsed
}

pub(super) fn flush_patch_change(
    parsed: &mut ParsedPatchArgument,
    current_path: &mut Option<String>,
    current_lines: &mut Vec<PatchLineView>,
) {
    let Some(path) = current_path.take() else {
        current_lines.clear();
        return;
    };
    let lines = std::mem::take(current_lines);
    parsed.changes.insert(path, lines);
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct PatchLineNumbers {
    pub(super) old_next: Option<usize>,
    pub(super) new_next: Option<usize>,
}

impl PatchLineNumbers {
    pub(super) fn context(&mut self, text: String) -> PatchLineView {
        let old_line = self.old_next;
        let new_line = self.new_next;
        self.advance_old();
        self.advance_new();
        PatchLineView {
            kind: PatchLineKind::Context,
            old_line,
            new_line,
            text,
        }
    }

    pub(super) fn add(&mut self, text: String) -> PatchLineView {
        let new_line = self.new_next;
        self.advance_new();
        PatchLineView::add(text, new_line)
    }

    pub(super) fn remove(&mut self, text: String) -> PatchLineView {
        let old_line = self.old_next;
        self.advance_old();
        PatchLineView::remove(text, old_line)
    }

    pub(super) fn advance_old(&mut self) {
        if let Some(line) = self.old_next.as_mut() {
            *line += 1;
        }
    }

    pub(super) fn advance_new(&mut self) {
        if let Some(line) = self.new_next.as_mut() {
            *line += 1;
        }
    }
}

pub(super) fn parse_unified_hunk_header(line: &str) -> Option<PatchLineNumbers> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "@@" {
        return None;
    }
    let old_next = parse_hunk_start(parts.next()?, '-')?;
    let new_next = parse_hunk_start(parts.next()?, '+')?;
    Some(PatchLineNumbers {
        old_next: Some(old_next),
        new_next: Some(new_next),
    })
}

pub(super) fn parse_hunk_start(value: &str, prefix: char) -> Option<usize> {
    let value = value.strip_prefix(prefix)?;
    let start = value.split_once(',').map_or(value, |(start, _)| start);
    start.parse().ok()
}
