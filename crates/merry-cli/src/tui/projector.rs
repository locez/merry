use super::state::{PatchChangeView, PatchLineView, QueuePreview, TimelineItem, TuiState};
use crate::tool_display::format_tool_call_detail;
use merry_core::{RuntimeEvent, ToolCallId, ToolCallResultStatus, ToolName, ToolOutput};
use merry_tool_workspace::WORKSPACE_PATCH_TOOL;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Default)]
#[allow(dead_code)]
pub(crate) struct TuiProjector {
    started_tools: HashMap<ToolCallId, StartedToolView>,
}

#[derive(Debug, Clone)]
struct StartedToolView {
    name: ToolName,
    timeline_index: usize,
    patch_argument: Option<String>,
}

#[allow(dead_code)]
impl TuiProjector {
    pub(crate) fn apply(&mut self, event: RuntimeEvent, state: &mut TuiState) {
        match event {
            RuntimeEvent::AssistantMessage { text, .. } => {
                state.push_timeline_item(TimelineItem::Assistant { text });
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
            RuntimeEvent::UsageUpdated { usage, .. } => {
                state.set_usage(usage);
            }
            RuntimeEvent::ToolCallStarted { call, .. } => {
                let call_id = call.id().clone();
                let tool_name = call.name().clone();
                let detail =
                    match format_tool_call_detail(tool_name.as_str(), call.arguments().as_object())
                    {
                        Some(detail) => format!("{} {detail}", tool_name.as_str()),
                        None => tool_name.as_str().to_owned(),
                    };
                let timeline_index = state.timeline().len();
                state.push_timeline_item(TimelineItem::Muted {
                    title: "tool".to_owned(),
                    detail,
                });
                self.started_tools.insert(
                    call_id,
                    StartedToolView {
                        name: tool_name,
                        timeline_index,
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
                } else {
                    // Successful non-patch outputs stay in artifacts; the started line already
                    // shows the human-readable call detail.
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
                state.push_timeline_item(TimelineItem::Diagnostic {
                    title: diagnostic.code().to_owned(),
                    body: diagnostic.message().to_owned(),
                });
            }
            RuntimeEvent::Closed => {
                state.push_timeline_item(TimelineItem::Muted {
                    title: "closed".to_owned(),
                    detail: "runtime stream closed".to_owned(),
                });
            }
            _ => {}
        }
    }
}

fn tool_output_text(output: Option<ToolOutput>) -> String {
    match output {
        Some(ToolOutput::Text { text }) => text,
        Some(ToolOutput::Json { json }) => json,
        None => String::new(),
    }
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
            let patch_lines = parsed_patch
                .as_ref()
                .and_then(|parsed| parsed.change_lines(&change.path))
                .cloned()
                .unwrap_or_default();
            let added = patch_lines
                .iter()
                .filter(|line| matches!(line, PatchLineView::Add(_)))
                .count();
            let removed = patch_lines
                .iter()
                .filter(|line| matches!(line, PatchLineView::Remove(_)))
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

    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Update File: ").map(str::trim) {
            flush_patch_change(&mut parsed, &mut current_path, &mut current_lines);
            current_path = Some(path.to_owned());
            continue;
        }
        if line.starts_with("*** ") || line.starts_with("@@") {
            continue;
        }
        let Some((prefix, text)) = line.split_at_checked(1) else {
            continue;
        };
        match prefix {
            " " => current_lines.push(PatchLineView::Context(text.to_owned())),
            "+" => current_lines.push(PatchLineView::Add(text.to_owned())),
            "-" => current_lines.push(PatchLineView::Remove(text.to_owned())),
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
