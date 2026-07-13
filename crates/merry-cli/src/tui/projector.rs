use super::state::{
    PatchChangeView, PatchLineKind, PatchLineView, QueuePreview, TimelineItem, TuiState,
};
use crate::tool_display::format_tool_call_detail;
use merry_core::{
    PlanAttemptOutcome, PlanDirectiveStatus, PlanNodeStatus, PlanPhase, PlanSnapshot, RuntimeEvent,
    ToolCallId, ToolCallResultStatus, ToolName, ToolOutput,
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
                let timeline_item =
                    plan_timeline_item(state.plan().snapshot(), &snapshot, summary.summary());
                state.plan_mut().update_snapshot(snapshot);
                if let Some(item) = timeline_item {
                    state.push_timeline_item(item);
                }
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

fn plan_timeline_item(
    previous: Option<&PlanSnapshot>,
    snapshot: &PlanSnapshot,
    summary: &str,
) -> Option<TimelineItem> {
    let Some(previous) = previous else {
        return Some(TimelineItem::Muted {
            title: "Plan mode".to_owned(),
            detail: format!(
                "{} · revision {} · {} nodes",
                plan_phase_label(snapshot.phase),
                snapshot.revision,
                snapshot.nodes.len()
            ),
        });
    };

    if previous.phase != snapshot.phase {
        let title = match snapshot.phase {
            PlanPhase::Planning => "Plan revision requested",
            PlanPhase::AwaitingApproval => "Plan awaiting approval",
            PlanPhase::Executing => "Plan execution started",
            PlanPhase::Completed => "Plan completed",
            PlanPhase::Blocked => "Plan blocked",
            PlanPhase::Cancelled => "Plan cancelled",
        };
        return Some(TimelineItem::Muted {
            title: title.to_owned(),
            detail: summary.to_owned(),
        });
    }

    if previous.scheduler_status != snapshot.scheduler_status {
        return Some(TimelineItem::Muted {
            title: format!("Plan scheduling {:?}", snapshot.scheduler_status).to_ascii_lowercase(),
            detail: summary.to_owned(),
        });
    }

    if previous.approval_requirements != snapshot.approval_requirements {
        return Some(TimelineItem::Muted {
            title: "Plan approval updated".to_owned(),
            detail: summary.to_owned(),
        });
    }

    if snapshot.nodes.len() > previous.nodes.len() {
        return Some(TimelineItem::Muted {
            title: "Plan expanded".to_owned(),
            detail: format!("{} · {} nodes", summary, snapshot.nodes.len()),
        });
    }

    if plan_definition_changed(previous, snapshot) {
        return Some(TimelineItem::Muted {
            title: "Plan revised".to_owned(),
            detail: summary.to_owned(),
        });
    }

    let newly_unhealthy = snapshot.nodes.iter().find(|node| {
        matches!(
            node.status,
            PlanNodeStatus::Blocked | PlanNodeStatus::Failed
        ) && previous
            .nodes
            .iter()
            .find(|candidate| candidate.id == node.id)
            .is_none_or(|candidate| candidate.status != node.status)
    });
    newly_unhealthy.map(|node| TimelineItem::Diagnostic {
        title: format!("Plan node {}", plan_node_status_label(node.status)),
        body: node.objective.clone(),
    })
}

fn plan_definition_changed(previous: &PlanSnapshot, snapshot: &PlanSnapshot) -> bool {
    previous.root_node_id != snapshot.root_node_id
        || previous.coordinator_node_id != snapshot.coordinator_node_id
        || previous.max_concurrency_hint != snapshot.max_concurrency_hint
        || snapshot.nodes.iter().any(|node| {
            previous
                .nodes
                .iter()
                .find(|candidate| candidate.id == node.id)
                .is_none_or(|candidate| {
                    candidate.parent_id != node.parent_id
                        || candidate.sibling_order != node.sibling_order
                        || candidate.objective != node.objective
                        || candidate.acceptance != node.acceptance
                        || candidate.executor_policy != node.executor_policy
                        || candidate.harness != node.harness
                        || candidate.recovery_policy != node.recovery_policy
                        || candidate.depends_on != node.depends_on
                })
        })
}

fn plan_phase_label(phase: PlanPhase) -> &'static str {
    match phase {
        PlanPhase::Planning => "planning",
        PlanPhase::AwaitingApproval => "awaiting approval",
        PlanPhase::Executing => "executing",
        PlanPhase::Completed => "completed",
        PlanPhase::Blocked => "blocked",
        PlanPhase::Cancelled => "cancelled",
    }
}

fn plan_node_status_label(status: PlanNodeStatus) -> &'static str {
    match status {
        PlanNodeStatus::Blocked => "blocked",
        PlanNodeStatus::Failed => "failed",
        _ => "updated",
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

    match name {
        "workspace_read_file" | "workspace_list_dir" => arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_owned(),
        "merry_read_checkpoint_ref" => named_string_argument(arguments, "ref"),
        "spawn_subagents" => format_spawn_subagents_detail(arguments),
        "wait_subagents" => format_wait_subagents_detail(arguments),
        "cancel_subagents" => format_agent_count_detail(arguments),
        "run_process" | "workspace_search_text" | "request_permissions" | WORKSPACE_PATCH_TOOL => {
            format_tool_call_detail(name, arguments).unwrap_or_else(|| name.to_owned())
        }
        _ => format_tool_call_detail(name, arguments)
            .map_or_else(|| name.to_owned(), |detail| format!("{name} {detail}")),
    }
}

fn named_string_argument(arguments: &serde_json::Map<String, Value>, name: &str) -> String {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map_or_else(|| name.to_owned(), |value| format!("{name}={value}"))
}

fn format_spawn_subagents_detail(arguments: &serde_json::Map<String, Value>) -> String {
    let task_count = arguments
        .get("tasks")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    match arguments.get("max_concurrency").and_then(Value::as_u64) {
        Some(max_concurrency) => format!("tasks={task_count} max={max_concurrency}"),
        None => format!("tasks={task_count}"),
    }
}

fn format_wait_subagents_detail(arguments: &serde_json::Map<String, Value>) -> String {
    let mut detail = format_agent_count_detail(arguments);
    if let Some(mode) = arguments.get("mode").and_then(Value::as_str) {
        detail.push_str(" mode=");
        detail.push_str(mode);
    }
    if let Some(timeout_ms) = arguments.get("timeout_ms").and_then(Value::as_u64) {
        detail.push_str(" timeout=");
        if timeout_ms.is_multiple_of(1_000) {
            detail.push_str(&(timeout_ms / 1_000).to_string());
            detail.push('s');
        } else {
            detail.push_str(&timeout_ms.to_string());
            detail.push_str("ms");
        }
    }
    detail
}

fn format_agent_count_detail(arguments: &serde_json::Map<String, Value>) -> String {
    let agent_count = arguments
        .get("agent_ids")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    format!("agents={agent_count}")
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
        "workspace_read_file" => read_file_output_bodies(output),
        "workspace_list_dir" => list_dir_output_bodies(output),
        _ => None,
    }
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
