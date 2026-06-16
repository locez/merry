use super::state::{QueuePreview, TimelineItem, TuiState};
use merry_core::{RuntimeEvent, ToolCallResultStatus, ToolOutput};

#[derive(Debug, Default)]
pub(crate) struct TuiProjector {
    _private: (),
}

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
                state.push_timeline_item(TimelineItem::Muted {
                    title: format!("tool {}", call.name().as_str()),
                    detail: format!("call {}", call.id().as_str()),
                });
            }
            RuntimeEvent::ToolCallFinished { result, output, .. } => {
                let text = tool_output_text(output);
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
                } else if looks_like_patch(&text) {
                    state.push_timeline_item(TimelineItem::Expanded {
                        title: "patch".to_owned(),
                        body: text,
                    });
                } else {
                    state.push_timeline_item(TimelineItem::Muted {
                        title: "tool result".to_owned(),
                        detail: summarize(&text, 96),
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
}

fn tool_output_text(output: Option<ToolOutput>) -> String {
    match output {
        Some(ToolOutput::Text { text }) => text,
        Some(ToolOutput::Json { json }) => json,
        None => String::new(),
    }
}

fn looks_like_patch(text: &str) -> bool {
    text.contains("\n+++") || text.contains("\n---") || text.contains("*** Begin Patch")
}

fn summarize(text: &str, max_chars: usize) -> String {
    let mut compact = String::new();
    for word in text.split_whitespace() {
        if !compact.is_empty() {
            compact.push(' ');
        }
        compact.push_str(word);
    }

    if compact.chars().count() <= max_chars {
        return compact;
    }

    compact
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>()
        + "..."
}
