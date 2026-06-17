use super::state::{
    PatchChangeView, PatchLineKind, PatchLineView, QueuePreview, TimelineItem, TuiState,
};
use crate::tool_display::format_tool_call_detail;
use merry_core::{RuntimeEvent, ToolCallId, ToolCallResultStatus, ToolName, ToolOutput};
use merry_tool_workspace::WORKSPACE_PATCH_TOOL;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

const PROCESS_PREVIEW_MAX_LINES: usize = 3;
const PROCESS_PREVIEW_MAX_CHARS: usize = 120;

#[derive(Debug, Default)]
#[allow(dead_code)]
pub(crate) struct TuiProjector {
    started_tools: HashMap<ToolCallId, StartedToolView>,
    streaming_assistant_index: Option<usize>,
}

#[derive(Debug, Clone)]
struct StartedToolView {
    name: ToolName,
    timeline_index: usize,
    title: String,
    detail: String,
    patch_argument: Option<String>,
}

#[allow(dead_code)]
impl TuiProjector {
    pub(crate) fn apply(&mut self, event: RuntimeEvent, state: &mut TuiState) {
        match event {
            RuntimeEvent::AssistantMessage { text, .. } => {
                if let Some(index) = self.streaming_assistant_index.take() {
                    state.replace_timeline_item(index, TimelineItem::Assistant { text });
                } else {
                    state.push_timeline_item(TimelineItem::Assistant { text });
                }
            }
            RuntimeEvent::AssistantMessageDelta { delta, .. } => {
                if !delta.is_empty() {
                    self.streaming_assistant_index =
                        Some(state.append_assistant_delta(self.streaming_assistant_index, &delta));
                }
            }
            RuntimeEvent::InteractiveRunStateChanged { state: run_state } => {
                state.set_run_state(run_state);
            }
            RuntimeEvent::QueuedInputsChanged { inputs } => {
                state.update_queue_preview(QueuePreview {
                    next: inputs.next,
                    suspended: inputs.suspended,
                    backlog: inputs.backlog,
                });
            }
            RuntimeEvent::QueuedInputAccepted { lane, inputs } => {
                for input in inputs {
                    state.confirm_or_push_user_input(input.text, lane);
                }
            }
            RuntimeEvent::UsageUpdated { usage, .. } => {
                state.set_usage(usage);
            }
            RuntimeEvent::ToolCallStarted { call, .. } => {
                self.streaming_assistant_index = None;
                let call_id = call.id().clone();
                let tool_name = call.name().clone();
                let (title, detail) =
                    started_tool_title_and_detail(tool_name.as_str(), call.arguments().as_object());
                let timeline_index = state.timeline().len();
                state.push_timeline_item(TimelineItem::Muted {
                    title: title.clone(),
                    detail: detail.clone(),
                });
                self.started_tools.insert(
                    call_id,
                    StartedToolView {
                        name: tool_name,
                        timeline_index,
                        title,
                        detail,
                        patch_argument: call
                            .arguments()
                            .as_object()
                            .get("patch")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    },
                );
            }
            RuntimeEvent::ToolCallFinished { result, output, .. } => {
                let text = tool_output_text(output);
                let tool = self.started_tools.remove(result.call_id());
                if result.status() == ToolCallResultStatus::Failed {
                    if let Some(tool) = tool.as_ref()
                        && let Some(preview) = success_tool_preview(tool.name.as_str(), &text)
                    {
                        state.replace_timeline_item(
                            tool.timeline_index,
                            TimelineItem::Expanded {
                                title: expanded_tool_title(tool),
                                body: preview,
                            },
                        );
                        return;
                    }
                    let body = result
                        .diagnostic()
                        .map(|diagnostic| {
                            compact_failed_tool_body(diagnostic.code(), diagnostic.message(), &text)
                        })
                        .unwrap_or_else(|| compact_tool_output(&text));
                    let title = tool.as_ref().map_or_else(
                        || "tool failed".to_owned(),
                        |tool| format!("tool failed: {}", tool.name.as_str()),
                    );
                    state.push_timeline_item(TimelineItem::Diagnostic { title, body });
                } else if tool
                    .as_ref()
                    .is_some_and(|tool| tool.name.as_str() == WORKSPACE_PATCH_TOOL)
                {
                    let patch_item = parse_workspace_patch_view(
                        &text,
                        tool.as_ref()
                            .and_then(|tool| tool.patch_argument.as_deref()),
                    )
                    .unwrap_or_else(|| TimelineItem::Expanded {
                        title: "patch".to_owned(),
                        body: compact_tool_output(&text),
                    });
                    if let Some(tool) = tool.as_ref() {
                        state.replace_timeline_item(tool.timeline_index, patch_item);
                    } else {
                        state.push_timeline_item(patch_item);
                    }
                } else if let Some(tool) = tool.as_ref()
                    && let Some(preview) = success_tool_preview(tool.name.as_str(), &text)
                {
                    state.replace_timeline_item(
                        tool.timeline_index,
                        TimelineItem::Expanded {
                            title: expanded_tool_title(tool),
                            body: preview,
                        },
                    );
                }
            }
            RuntimeEvent::SubagentCompleted {
                summary,
                output_paths,
                changed_paths,
                ..
            } => {
                state.push_timeline_item(TimelineItem::Expanded {
                    title: "subagent completed".to_owned(),
                    body: format!(
                        "{summary}\nchanged: {}\noutputs: {}",
                        changed_paths.join(", "),
                        output_paths.join(", ")
                    ),
                });
            }
            RuntimeEvent::SubagentFailed { diagnostic, .. }
            | RuntimeEvent::SubagentCancelled { diagnostic, .. }
            | RuntimeEvent::RunFailed { diagnostic, .. }
            | RuntimeEvent::RunCancelled { diagnostic, .. } => {
                self.streaming_assistant_index = None;
                state.push_timeline_item(TimelineItem::Diagnostic {
                    title: diagnostic.code().to_owned(),
                    body: diagnostic.message().to_owned(),
                });
            }
            RuntimeEvent::Closed => {
                self.streaming_assistant_index = None;
                state.push_timeline_item(TimelineItem::Muted {
                    title: "closed".to_owned(),
                    detail: "runtime stream closed".to_owned(),
                });
            }
            _ => {}
        }
    }
}

fn expanded_tool_title(tool: &StartedToolView) -> String {
    if tool.detail.is_empty() {
        return tool.title.clone();
    }
    format!("{} {}", tool.title, tool.detail)
}

fn tool_output_text(output: Option<ToolOutput>) -> String {
    match output {
        Some(ToolOutput::Text { text }) => text,
        Some(ToolOutput::Json { json }) => json,
        None => String::new(),
    }
}

fn started_tool_title_and_detail(
    name: &str,
    arguments: &serde_json::Map<String, Value>,
) -> (String, String) {
    let detail = tui_tool_detail(name, arguments);
    let title = tui_tool_title(name);
    (title.to_owned(), detail)
}

fn tui_tool_title(name: &str) -> &'static str {
    match name {
        "run_process" => "Ran",
        "workspace_read_file" => "Read",
        "workspace_list_dir" => "Listed",
        "workspace_search_text" => "Searched",
        "request_permissions" => "Permission",
        WORKSPACE_PATCH_TOOL => "Patch",
        _ => "Tool",
    }
}

fn tui_tool_detail(name: &str, arguments: &serde_json::Map<String, Value>) -> String {
    match name {
        "workspace_read_file" | "workspace_list_dir" => arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_owned(),
        _ => format_tool_call_detail(name, arguments).unwrap_or_else(|| name.to_owned()),
    }
}

fn success_tool_preview(name: &str, output: &str) -> Option<String> {
    match name {
        "run_process" => process_output_preview(output),
        _ => None,
    }
}

fn process_output_preview(output: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    let stdout = value
        .pointer("/stdout/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = value
        .pointer("/stderr/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let status = value
        .get("status")
        .and_then(Value::as_i64)
        .or_else(|| value.pointer("/status/code").and_then(Value::as_i64));

    let mut lines = Vec::new();
    if let Some(status) = status
        && status != 0
    {
        lines.push(format!("  exit {status}"));
    }
    append_stream_preview(&mut lines, "stdout", stdout);
    append_stream_preview(&mut lines, "stderr", stderr);

    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn append_stream_preview(lines: &mut Vec<String>, label: &str, text: &str) {
    let mut stream_lines = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(PROCESS_PREVIEW_MAX_LINES);
    let Some(first) = stream_lines.next() else {
        return;
    };
    lines.push(format!(
        "  {label}: {}",
        truncate_chars(first, PROCESS_PREVIEW_MAX_CHARS)
    ));
    lines.extend(
        stream_lines.map(|line| format!("    {}", truncate_chars(line, PROCESS_PREVIEW_MAX_CHARS))),
    );
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars().take(max_chars - 3).collect::<String>() + "..."
}

fn compact_failed_tool_body(code: &str, message: &str, output: &str) -> String {
    let mut lines = vec![format!("{code}: {message}")];
    if let Some(guidance) = parse_guidance_message(output) {
        lines.push(guidance);
    }
    lines.join("\n")
}

fn parse_guidance_message(output: &str) -> Option<String> {
    serde_json::from_str::<Value>(output)
        .ok()
        .and_then(|value| {
            value
                .get("guidance")?
                .get("message")?
                .as_str()
                .map(str::to_owned)
        })
}

fn compact_tool_output(output: &str) -> String {
    let output = output.trim();
    if output.is_empty() {
        return String::new();
    }
    let mut compact = output
        .chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .take(600)
        .collect::<String>();
    if output.chars().count() > 600 {
        compact.push_str("...");
    }
    compact
}

fn parse_workspace_patch_view(output: &str, patch_argument: Option<&str>) -> Option<TimelineItem> {
    let output = serde_json::from_str::<WorkspacePatchOutput>(output).ok()?;
    if !output.ok || output.tool.as_deref() != Some(WORKSPACE_PATCH_TOOL) {
        return None;
    }
    let parsed_patch = patch_argument.map(parse_workspace_patch_argument);
    let changes = output
        .changes
        .into_iter()
        .map(|change| {
            let patch_lines = change
                .lines
                .as_ref()
                .map(|lines| {
                    lines
                        .iter()
                        .filter_map(WorkspacePatchOutputLine::to_patch_line_view)
                        .collect::<Vec<_>>()
                })
                .filter(|lines| !lines.is_empty())
                .or_else(|| {
                    parsed_patch
                        .as_ref()
                        .and_then(|parsed| parsed.change_lines(&change.path))
                        .cloned()
                })
                .unwrap_or_default();
            let added = patch_lines
                .iter()
                .filter(|line| line.kind == PatchLineKind::Add)
                .count();
            let removed = patch_lines
                .iter()
                .filter(|line| line.kind == PatchLineKind::Remove)
                .count();
            PatchChangeView {
                path: change.path,
                added,
                removed,
                hunks: change.hunks,
                bytes_before: Some(change.bytes_before),
                bytes_after: Some(change.bytes_after),
                lines: patch_lines,
            }
        })
        .collect::<Vec<_>>();
    Some(TimelineItem::Patch { changes })
}

#[derive(Debug, Deserialize)]
struct WorkspacePatchOutput {
    ok: bool,
    tool: Option<String>,
    changes: Vec<WorkspacePatchOutputChange>,
}

#[derive(Debug, Deserialize)]
struct WorkspacePatchOutputChange {
    path: String,
    hunks: usize,
    bytes_before: usize,
    bytes_after: usize,
    #[serde(default)]
    lines: Option<Vec<WorkspacePatchOutputLine>>,
}

#[derive(Debug, Deserialize)]
struct WorkspacePatchOutputLine {
    kind: String,
    old_line: Option<usize>,
    new_line: Option<usize>,
    text: String,
}

impl WorkspacePatchOutputLine {
    fn to_patch_line_view(&self) -> Option<PatchLineView> {
        let kind = match self.kind.as_str() {
            "context" => PatchLineKind::Context,
            "remove" => PatchLineKind::Remove,
            "add" => PatchLineKind::Add,
            _ => return None,
        };
        Some(PatchLineView {
            kind,
            old_line: self.old_line,
            new_line: self.new_line,
            text: self.text.clone(),
        })
    }
}

#[derive(Debug, Default, Clone)]
struct ParsedPatchArgument {
    changes: HashMap<String, Vec<PatchLineView>>,
}

impl ParsedPatchArgument {
    fn change_lines(&self, path: &str) -> Option<&Vec<PatchLineView>> {
        self.changes.get(path)
    }
}

fn parse_workspace_patch_argument(patch: &str) -> ParsedPatchArgument {
    let mut parsed = ParsedPatchArgument::default();
    let mut current_path: Option<String> = None;
    let mut current_lines = Vec::new();
    let mut line_numbers = PatchLineNumbers::default();

    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Update File: ").map(str::trim) {
            flush_patch_change(&mut parsed, &mut current_path, &mut current_lines);
            current_path = Some(path.to_owned());
            line_numbers = PatchLineNumbers::default();
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

fn flush_patch_change(
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
struct PatchLineNumbers {
    old_next: Option<usize>,
    new_next: Option<usize>,
}

impl PatchLineNumbers {
    fn context(&mut self, text: String) -> PatchLineView {
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

    fn add(&mut self, text: String) -> PatchLineView {
        let new_line = self.new_next;
        self.advance_new();
        PatchLineView::add(text, new_line)
    }

    fn remove(&mut self, text: String) -> PatchLineView {
        let old_line = self.old_next;
        self.advance_old();
        PatchLineView::remove(text, old_line)
    }

    fn advance_old(&mut self) {
        if let Some(line) = self.old_next.as_mut() {
            *line += 1;
        }
    }

    fn advance_new(&mut self) {
        if let Some(line) = self.new_next.as_mut() {
            *line += 1;
        }
    }
}

fn parse_unified_hunk_header(line: &str) -> Option<PatchLineNumbers> {
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

fn parse_hunk_start(value: &str, prefix: char) -> Option<usize> {
    let value = value.strip_prefix(prefix)?;
    let start = value.split_once(',').map_or(value, |(start, _)| start);
    start.parse().ok()
}
