use super::state::{QueuePreview, TimelineItem, TuiState};
use merry_core::{RuntimeEvent, ToolCallId, ToolCallResultStatus, ToolName, ToolOutput};
use merry_tool_workspace::WORKSPACE_PATCH_TOOL;
use std::collections::HashMap;

#[derive(Debug, Default)]
#[allow(dead_code)]
pub(crate) struct TuiProjector {
    started_tools: HashMap<ToolCallId, ToolName>,
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
                state.push_timeline_item(TimelineItem::Muted {
                    title: tool_name.as_str().to_owned(),
                    detail: "started".to_owned(),
                });
                self.started_tools.insert(call_id, tool_name);
            }
            RuntimeEvent::ToolCallFinished { result, output, .. } => {
                let text = tool_output_text(output);
                let tool_name = self.started_tools.get(result.call_id()).cloned();
                if result.status() == ToolCallResultStatus::Failed {
                    let body = result
                        .diagnostic()
                        .map(|diagnostic| {
                            format!("{}: {}\n{}", diagnostic.code(), diagnostic.message(), text)
                        })
                        .unwrap_or(text);
                    state.push_timeline_item(TimelineItem::Diagnostic {
                        title: "tool failed".to_owned(),
                        body,
                    });
                } else if self.is_workspace_patch_result(result.call_id()) {
                    state.push_timeline_item(TimelineItem::Expanded {
                        title: "patch".to_owned(),
                        body: text,
                    });
                } else {
                    state.push_timeline_item(TimelineItem::Muted {
                        title: tool_name
                            .as_ref()
                            .map(|name| name.as_str().to_owned())
                            .unwrap_or_else(|| "tool result".to_owned()),
                        detail: "completed".to_owned(),
                    });
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

    fn is_workspace_patch_result(&self, call_id: &ToolCallId) -> bool {
        self.started_tools
            .get(call_id)
            .is_some_and(|name| name.as_str() == WORKSPACE_PATCH_TOOL)
    }
}

fn tool_output_text(output: Option<ToolOutput>) -> String {
    match output {
        Some(ToolOutput::Text { text }) => text,
        Some(ToolOutput::Json { json }) => json,
        None => String::new(),
    }
}
