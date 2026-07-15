use super::{
    overlay::Overlay,
    plan_projector::plan_timeline_item,
    state::{PatchChangeView, PatchLineKind, PatchLineView, QueuePreview, TimelineItem, TuiState},
    tool_error::compact_failed_tool_body,
};
use crate::tool_display::format_tool_call_detail;
use merry_core::{
    PlanAttemptOutcome, PlanDirectiveStatus, PlanPhase, RuntimeEvent, ToolCallId,
    ToolCallResultStatus, ToolName, ToolOutput,
};
use merry_runtime::SessionTranscriptItem;
use merry_tool_workspace::WORKSPACE_PATCH_TOOL;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

const PROCESS_PREVIEW_MAX_LINES: usize = 3;
const PROCESS_PREVIEW_MAX_CHARS: usize = 120;
const READ_FILE_PREVIEW_MAX_LINES: usize = 120;
const READ_FILE_PREVIEW_MAX_CHARS: usize = 180;
const LIST_DIR_PREVIEW_MAX_ENTRIES: usize = 80;

#[derive(Debug, Default)]
#[allow(dead_code)]
pub(crate) struct TuiProjector {
    started_tools: HashMap<ToolCallId, StartedToolView>,
    streaming_assistant_index: Option<usize>,
    compaction_timeline_index: Option<usize>,
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
    pub(crate) fn apply_transcript_item(
        &mut self,
        item: SessionTranscriptItem,
        state: &mut TuiState,
    ) {
        match item {
            SessionTranscriptItem::UserMessage { text, .. } => {
                state.confirm_or_push_user_input(text, merry_core::QueuedInputLane::Next);
            }
            SessionTranscriptItem::AssistantText { text } => {
                self.streaming_assistant_index = None;
                state.push_timeline_item(TimelineItem::Assistant { text });
            }
            SessionTranscriptItem::ToolCall { call } => {
                self.apply(
                    RuntimeEvent::ToolCallStarted {
                        call,
                        source: transcript_source(),
                    },
                    state,
                );
            }
            SessionTranscriptItem::ToolResult { result, output, .. } => {
                self.apply(
                    RuntimeEvent::ToolCallFinished {
                        result,
                        output,
                        source: transcript_source(),
                    },
                    state,
                );
            }
        }
    }

    pub(crate) fn apply(&mut self, event: RuntimeEvent, state: &mut TuiState) {
        match event {
            RuntimeEvent::AssistantMessage { text, .. } => {
                if let Some(index) = self.streaming_assistant_index.take() {
                    state.replace_timeline_item(index, TimelineItem::Assistant { text });
                } else {
                    state.push_timeline_item(TimelineItem::Assistant { text });
                }
            }
            RuntimeEvent::AssistantMessageDelta { delta, .. } if !delta.is_empty() => {
                self.streaming_assistant_index =
                    Some(state.append_assistant_delta(self.streaming_assistant_index, &delta));
            }
            RuntimeEvent::AssistantMessageDelta { .. } => {}
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
            RuntimeEvent::CompactionStarted { .. } => {
                self.streaming_assistant_index = None;
                let timeline_index = state.timeline().len();
                state.push_timeline_item(TimelineItem::Muted {
                    title: "Compacting".to_owned(),
                    detail: "preparing checkpoint".to_owned(),
                });
                self.compaction_timeline_index = Some(timeline_index);
            }
            RuntimeEvent::CompactionCompleted {
                checkpoint_id,
                covered_history_item_count,
                ..
            } => {
                let item = TimelineItem::Muted {
                    title: "Compacted".to_owned(),
                    detail: format!("{covered_history_item_count} history items · {checkpoint_id}"),
                };
                if let Some(index) = self.compaction_timeline_index.take() {
                    state.replace_timeline_item(index, item);
                } else {
                    state.push_timeline_item(item);
                }
            }
            RuntimeEvent::UsageUpdated { usage, .. } => {
                state.set_usage(usage);
            }
            RuntimeEvent::ToolCallStarted { call, .. } => self.start_tool(call, state),
            RuntimeEvent::ToolCallBatchStarted { batch, .. } => {
                for call in batch.calls() {
                    self.start_tool(call.clone(), state);
                }
            }
            RuntimeEvent::ToolCallFinished { result, output, .. } => {
                let text = tool_output_text(output);
                let tool = self.started_tools.remove(result.call_id());
                if result.status() == ToolCallResultStatus::Failed {
                    if let Some(tool) = tool.as_ref()
                        && let Some((preview, focus_body)) =
                            success_tool_bodies(tool.name.as_str(), &text)
                    {
                        state.replace_timeline_item(
                            tool.timeline_index,
                            TimelineItem::ExpandedDetail {
                                title: expanded_tool_title(tool),
                                body: preview,
                                focus_body,
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
                    && let Some((preview, focus_body)) =
                        success_tool_bodies(tool.name.as_str(), &text)
                {
                    state.replace_timeline_item(
                        tool.timeline_index,
                        TimelineItem::ExpandedDetail {
                            title: expanded_tool_title(tool),
                            body: preview,
                            focus_body,
                        },
                    );
                } else if let Some(tool) = tool.as_ref() {
                    state.replace_timeline_item(
                        tool.timeline_index,
                        TimelineItem::Expanded {
                            title: completed_tool_title(tool, "succeeded"),
                            body: compact_tool_output(&text),
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
            | RuntimeEvent::SubagentCancelled { diagnostic, .. } => {
                self.streaming_assistant_index = None;
                state.push_timeline_item(TimelineItem::Diagnostic {
                    title: diagnostic.code().to_owned(),
                    body: diagnostic.message().to_owned(),
                });
            }
            RuntimeEvent::PlanUpdated {
                snapshot, summary, ..
            } => {
                let refresh_open_approval =
                    matches!(state.overlay(), Some(Overlay::PlanApproval(_)))
                        && state.plan().snapshot().is_some_and(|current| {
                            current.plan_id != snapshot.plan_id
                                || current.revision != snapshot.revision
                        });
                let entered_planning_review = snapshot.phase == PlanPhase::Planning
                    && snapshot.root_node_id.is_some()
                    && !state.plan().snapshot().is_some_and(|current| {
                        current.phase == PlanPhase::Planning && current.root_node_id.is_some()
                    });
                let entered_awaiting_approval = snapshot.phase == PlanPhase::AwaitingApproval
                    && !state
                        .plan()
                        .snapshot()
                        .is_some_and(|current| current.phase == PlanPhase::AwaitingApproval);
                let timeline_item =
                    plan_timeline_item(state.plan().snapshot(), &snapshot, summary.summary());
                state.plan_mut().update_snapshot(snapshot);
                if let Some(item) = timeline_item {
                    state.push_timeline_item(item);
                }
                if entered_planning_review || entered_awaiting_approval || refresh_open_approval {
                    state.open_plan_approval();
                }
            }
            RuntimeEvent::PlanLeaseStarted { lease, .. } => {
                state.plan_mut().update_lease(lease);
            }
            RuntimeEvent::PlanProgressUpdated { progress, .. }
            | RuntimeEvent::PlanAttemptProgressReported { progress, .. } => {
                state.plan_mut().update_progress(progress);
            }
            RuntimeEvent::PlanProgressReviewRequested { reason, .. } => {
                state.push_timeline_item(TimelineItem::Muted {
                    title: "Plan review requested".to_owned(),
                    detail: reason,
                });
            }
            RuntimeEvent::PlanDirectiveUpdated { directive, .. }
                if matches!(
                    directive.status,
                    PlanDirectiveStatus::Queued | PlanDirectiveStatus::Applied
                ) =>
            {
                state.push_timeline_item(TimelineItem::Muted {
                    title: match directive.status {
                        PlanDirectiveStatus::Queued => "Plan steering queued",
                        PlanDirectiveStatus::Applied => "Plan steering applied",
                        _ => unreachable!("match guard limits directive status"),
                    }
                    .to_owned(),
                    detail: format!("{:?}: {}", directive.kind, directive.reason)
                        .to_ascii_lowercase(),
                });
            }
            RuntimeEvent::PlanAttemptFinished { attempt, .. }
                if matches!(
                    attempt.outcome,
                    Some(
                        PlanAttemptOutcome::Blocked
                            | PlanAttemptOutcome::SemanticFailure
                            | PlanAttemptOutcome::Interrupted
                            | PlanAttemptOutcome::Cancelled
                    )
                ) =>
            {
                state.plan_mut().update_attempt(attempt.clone());
                let outcome = attempt
                    .outcome
                    .map(|outcome| format!("{outcome:?}").to_ascii_lowercase())
                    .unwrap_or_else(|| "finished".to_owned());
                state.push_timeline_item(TimelineItem::Diagnostic {
                    title: format!("Plan node {outcome}"),
                    body: attempt
                        .diagnostic
                        .as_ref()
                        .map(|diagnostic| diagnostic.message().to_owned())
                        .unwrap_or_else(|| attempt.node_id.as_str().to_owned()),
                });
            }
            RuntimeEvent::RunFailed { diagnostic, .. } => {
                self.streaming_assistant_index = None;
                let item = TimelineItem::Diagnostic {
                    title: if self.compaction_timeline_index.is_some() {
                        "compaction failed".to_owned()
                    } else {
                        diagnostic.code().to_owned()
                    },
                    body: diagnostic.message().to_owned(),
                };
                if let Some(index) = self.compaction_timeline_index.take() {
                    state.replace_timeline_item(index, item);
                } else {
                    state.push_timeline_item(item);
                }
            }
            RuntimeEvent::RunCancelled { diagnostic, .. } => {
                self.streaming_assistant_index = None;
                let item = if self.compaction_timeline_index.is_some() {
                    TimelineItem::Muted {
                        title: "Compaction cancelled".to_owned(),
                        detail: diagnostic.message().to_owned(),
                    }
                } else {
                    TimelineItem::Diagnostic {
                        title: diagnostic.code().to_owned(),
                        body: diagnostic.message().to_owned(),
                    }
                };
                if let Some(index) = self.compaction_timeline_index.take() {
                    state.replace_timeline_item(index, item);
                } else {
                    state.push_timeline_item(item);
                }
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

    fn start_tool(&mut self, call: merry_core::PendingToolCall, state: &mut TuiState) {
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
}

fn transcript_source() -> merry_core::RuntimeEventSource {
    merry_core::RuntimeEventSource::new(
        merry_core::SessionId::new("resume-transcript")
            .expect("static resume transcript session id is valid"),
        0,
    )
}

fn expanded_tool_title(tool: &StartedToolView) -> String {
    if tool.detail.is_empty() {
        return tool.title.clone();
    }
    format!("{} {}", tool.title, tool.detail)
}

fn completed_tool_title(tool: &StartedToolView, status: &str) -> String {
    format!("{} -> {status}", expanded_tool_title(tool))
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
    if parse_mcp_tool_name(name).is_some() {
        return "MCP";
    }

    match name {
        "run_process" => "Ran",
        "workspace_read_file" => "Read",
        "workspace_list_dir" => "Listed",
        "workspace_search_text" => "Searched",
        "request_permissions" => "Permission",
        "merry_read_checkpoint_ref" => "Retrieved",
        "spawn_subagents" => "Delegated",
        "wait_subagents" => "Waited",
        "cancel_subagents" => "Cancelled",
        WORKSPACE_PATCH_TOOL => "Patch",
        _ => "Tool",
    }
}

fn tui_tool_detail(name: &str, arguments: &serde_json::Map<String, Value>) -> String {
    if let Some((server, tool)) = parse_mcp_tool_name(name) {
        let detail = format_tool_call_detail(name, arguments);
        return match detail {
            Some(detail) if !detail.is_empty() => format!("{server}/{tool} {detail}"),
            _ => format!("{server}/{tool}"),
        };
    }

    match format_tool_call_detail(name, arguments) {
        Some(detail) if !detail.is_empty() => format!("{name} {detail}"),
        _ => name.to_owned(),
    }
}

fn parse_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp_")?;
    let (server, tool) = rest.split_once('_')?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

fn success_tool_bodies(name: &str, output: &str) -> Option<(String, String)> {
    match name {
        "run_process" => process_output_bodies(output),
        "request_permissions" => permission_output_bodies(output),
        "workspace_read_file" => read_file_output_bodies(output),
        "workspace_list_dir" => list_dir_output_bodies(output),
        _ => None,
    }
}

fn permission_output_bodies(output: &str) -> Option<(String, String)> {
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
    Some((body.clone(), body))
}

fn read_file_output_bodies(output: &str) -> Option<(String, String)> {
    let output = serde_json::from_str::<WorkspaceReadFileOutput>(output).ok()?;
    if !output.ok || output.tool.as_deref() != Some("workspace_read_file") {
        return None;
    }

    let focus_body = if output.content.is_empty() {
        format!("{} is empty", output.path)
    } else {
        output.content.clone()
    };
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
    Some((lines.join("\n"), focus_body))
}

#[derive(Debug, Deserialize)]
struct WorkspaceReadFileOutput {
    ok: bool,
    tool: Option<String>,
    path: String,
    content: String,
    #[serde(default)]
    truncated: bool,
}

fn list_dir_output_bodies(output: &str) -> Option<(String, String)> {
    let output = serde_json::from_str::<WorkspaceListDirOutput>(output).ok()?;
    if !output.ok || output.tool.as_deref() != Some("workspace_list_dir") {
        return None;
    }

    let all_lines = output
        .entries
        .iter()
        .map(directory_entry_label)
        .collect::<Vec<_>>();
    let mut preview_lines = output
        .entries
        .iter()
        .take(LIST_DIR_PREVIEW_MAX_ENTRIES)
        .map(directory_entry_label)
        .collect::<Vec<_>>();
    if output.truncated {
        preview_lines.push("... truncated".to_owned());
    }
    if preview_lines.is_empty() {
        preview_lines.push(format!("{} is empty", output.path));
    }
    let focus_body = if all_lines.is_empty() {
        format!("{} is empty", output.path)
    } else {
        all_lines.join("\n")
    };
    Some((preview_lines.join("\n"), focus_body))
}

fn directory_entry_label(entry: &WorkspaceListDirEntry) -> String {
    let suffix = if entry.kind == "directory" { "/" } else { "" };
    format!("{}{}", entry.path, suffix)
}

#[derive(Debug, Deserialize)]
struct WorkspaceListDirOutput {
    ok: bool,
    tool: Option<String>,
    path: String,
    entries: Vec<WorkspaceListDirEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct WorkspaceListDirEntry {
    path: String,
    kind: String,
}

fn process_output_bodies(output: &str) -> Option<(String, String)> {
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

    let mut focus_lines = Vec::new();
    if let Some(status) = status
        && status != 0
    {
        focus_lines.push(format!("  exit {status}"));
    }
    append_stream_full(&mut focus_lines, "stdout", stdout);
    append_stream_full(&mut focus_lines, "stderr", stderr);

    (!lines.is_empty()).then(|| (lines.join("\n"), focus_lines.join("\n")))
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

fn append_stream_full(lines: &mut Vec<String>, label: &str, text: &str) {
    let mut stream_lines = text.lines().filter(|line| !line.trim().is_empty());
    let Some(first) = stream_lines.next() else {
        return;
    };
    lines.push(format!("  {label}: {first}"));
    lines.extend(stream_lines.map(|line| format!("    {line}")));
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
