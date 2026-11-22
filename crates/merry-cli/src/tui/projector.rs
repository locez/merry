use crate::tui::{
    process_output::process_exit_code,
    projector::tool_output::{
        compact_tool_output, completed_plan_or_todo_view, completed_process_view,
        completed_tool_title, expanded_tool_title, failed_tool_body, parse_apply_patch_view,
        permission_output_bodies, read_text_output_preview, started_tool_title_and_detail,
        tool_output_text,
    },
    state::{CommandView, TimelineItem, ToolOutputPreview, TuiState},
};
use merry_core::{
    PlanModeState, PlanReviewDecision, RuntimeEvent, TOOL_CANCELLED_BY_USER_CODE, ToolCallId,
    ToolCallResultStatus, ToolName,
};
use merry_runtime::SessionTranscriptItem;
use merry_tools::{APPLY_PATCH_TOOL, READ_TEXT_TOOL};
use serde_json::Value;
use std::collections::HashMap;
use tokio::time::Instant;

mod tool_output;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TuiProjector {
    started_tools: HashMap<ToolCallId, StartedToolView>,
    streaming_assistant_index: Option<usize>,
    compaction_timeline_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartedToolView {
    name: ToolName,
    timeline_index: usize,
    title: String,
    detail: String,
    patch_argument: Option<String>,
    command: String,
    cwd: String,
    started_at: Option<Instant>,
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
                state.confirm_or_push_user_input(text);
            }
            SessionTranscriptItem::AssistantText { text } => {
                self.streaming_assistant_index = None;
                state.push_timeline_item(TimelineItem::Assistant { text });
            }
            SessionTranscriptItem::ToolCall { call } => {
                let call_id = call.id().clone();
                self.apply(
                    RuntimeEvent::ToolCallStarted {
                        call,
                        source: transcript_source(),
                    },
                    state,
                );
                if let Some(tool) = self.started_tools.get_mut(&call_id) {
                    tool.started_at = None;
                }
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
        state.observe_output_rate(&event);
        match event {
            RuntimeEvent::ReasoningDelta { delta, .. } if !delta.is_empty() => {
                state.append_reasoning_delta(&delta);
            }
            RuntimeEvent::ReasoningDelta { .. } => {}
            RuntimeEvent::AssistantMessage { text, .. } => {
                state.finish_reasoning();
                if let Some(index) = self.streaming_assistant_index.take() {
                    state.replace_timeline_item(index, TimelineItem::Assistant { text });
                } else {
                    state.push_timeline_item(TimelineItem::Assistant { text });
                }
            }
            RuntimeEvent::AssistantMessageDelta { delta, .. } if !delta.is_empty() => {
                state.finish_reasoning();
                self.streaming_assistant_index =
                    Some(state.append_assistant_delta(self.streaming_assistant_index, &delta));
            }
            RuntimeEvent::AssistantMessageDelta { .. } => {}
            RuntimeEvent::ModelRetryScheduled { .. } => {
                state.finish_reasoning();
                if let Some(index) = self.streaming_assistant_index.take() {
                    state.discard_streaming_assistant(index);
                }
            }
            RuntimeEvent::InteractiveRunStateChanged { state: run_state } => {
                state.apply_runtime_run_state(run_state);
            }
            RuntimeEvent::QueuedInputsChanged { inputs } => {
                state.update_queue_preview(inputs);
            }
            RuntimeEvent::QueuedInputAccepted { inputs } => {
                state.confirm_local_run_start();
                for input in inputs {
                    state.confirm_or_push_user_input(input.text);
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
            RuntimeEvent::ToolCallStarted { call, .. } => {
                state.finish_reasoning();
                self.start_tool(call, state);
            }
            RuntimeEvent::ToolCallBatchStarted { batch, .. } => {
                state.finish_reasoning();
                for call in batch.calls() {
                    self.start_tool(call.clone(), state);
                }
            }
            RuntimeEvent::ToolCallOutputDelta { call_id, delta, .. } if !delta.is_empty() => {
                if let Some(tool) = self.started_tools.get(&call_id)
                    && tool.name.as_str() == "run_process"
                {
                    state.append_running_command_output(tool.timeline_index, &delta);
                }
            }
            RuntimeEvent::ToolCallFinished { result, output, .. } => {
                let text = tool_output_text(output);
                let tool = self.started_tools.remove(result.call_id());
                let cancelled_by_user = result
                    .diagnostic()
                    .is_some_and(|diagnostic| diagnostic.code() == TOOL_CANCELLED_BY_USER_CODE);
                let failed = result.status() == ToolCallResultStatus::Failed;
                let process_exit_code = tool
                    .as_ref()
                    .filter(|tool| tool.name.as_str() == "run_process")
                    .and_then(|_| process_exit_code(&text));
                if let Some(tool) = tool.as_ref()
                    && tool.name.as_str() == "run_process"
                {
                    state.replace_timeline_item(
                        tool.timeline_index,
                        completed_process_view(tool, &text, process_exit_code, &result),
                    );
                } else if cancelled_by_user {
                    let item = TimelineItem::Muted {
                        title: tool.as_ref().map_or_else(
                            || "Tool -> cancelled".to_owned(),
                            |tool| completed_tool_title(tool, "cancelled"),
                        ),
                        detail: "cancelled by user".to_owned(),
                    };
                    if let Some(tool) = tool.as_ref() {
                        state.replace_timeline_item(tool.timeline_index, item);
                    } else {
                        state.push_timeline_item(item);
                    }
                } else if failed {
                    let body = failed_tool_body(result.diagnostic(), &text);
                    let item = TimelineItem::Diagnostic {
                        title: tool.as_ref().map_or_else(
                            || "tool failed".to_owned(),
                            |tool| completed_tool_title(tool, "failed"),
                        ),
                        body,
                    };
                    if let Some(tool) = tool.as_ref() {
                        state.replace_timeline_item(tool.timeline_index, item);
                    } else {
                        state.push_timeline_item(item);
                    }
                } else if tool
                    .as_ref()
                    .is_some_and(|tool| tool.name.as_str() == APPLY_PATCH_TOOL)
                {
                    let patch_item = parse_apply_patch_view(
                        &text,
                        tool.as_ref()
                            .and_then(|tool| tool.patch_argument.as_deref()),
                    )
                    .unwrap_or_else(|| TimelineItem::Expanded {
                        title: "patch".to_owned(),
                        body: compact_tool_output(&text),
                        expanded: false,
                    });
                    if let Some(tool) = tool.as_ref() {
                        state.replace_timeline_item(tool.timeline_index, patch_item);
                    } else {
                        state.push_timeline_item(patch_item);
                    }
                } else if let Some(tool) = tool.as_ref()
                    && tool.name.as_str() == READ_TEXT_TOOL
                {
                    state.replace_timeline_item(
                        tool.timeline_index,
                        TimelineItem::Read {
                            title: expanded_tool_title(tool),
                            preview: read_text_output_preview(&text)
                                .unwrap_or_else(|| ToolOutputPreview::new(text.lines(), false)),
                            expanded: false,
                        },
                    );
                } else if let Some(tool) = tool.as_ref()
                    && tool.name.as_str() == "request_permissions"
                    && let Some(preview) = permission_output_bodies(&text)
                {
                    state.replace_timeline_item(
                        tool.timeline_index,
                        TimelineItem::Expanded {
                            title: expanded_tool_title(tool),
                            body: preview,
                            expanded: false,
                        },
                    );
                } else if let Some(tool) = tool.as_ref()
                    && let Some(item) = completed_plan_or_todo_view(tool, &text)
                {
                    state.replace_timeline_item(tool.timeline_index, item);
                } else if let Some(tool) = tool.as_ref() {
                    state.replace_timeline_item(
                        tool.timeline_index,
                        TimelineItem::Expanded {
                            title: completed_tool_title(tool, "succeeded"),
                            body: compact_tool_output(&text),
                            expanded: false,
                        },
                    );
                }
            }
            RuntimeEvent::SubagentSpawned {
                agent_id,
                task_id,
                display_name,
                ..
            } => {
                state.upsert_subagent(crate::tui::state::SubagentUpdate {
                    agent_id: agent_id.as_str().to_owned(),
                    task_id: task_id.as_str().to_owned(),
                    display_name,
                    phase: crate::tui::subagent_view::SubagentPhase::Starting,
                    summary: "spawned".to_owned(),
                    changed_paths: None,
                    output_paths: None,
                    diagnostic: None,
                });
            }
            RuntimeEvent::SubagentStarted {
                agent_id, task_id, ..
            } => {
                state.upsert_subagent(crate::tui::state::SubagentUpdate {
                    agent_id: agent_id.as_str().to_owned(),
                    task_id: task_id.as_str().to_owned(),
                    display_name: None,
                    phase: crate::tui::subagent_view::SubagentPhase::Running,
                    summary: "running".to_owned(),
                    changed_paths: None,
                    output_paths: None,
                    diagnostic: None,
                });
            }
            RuntimeEvent::SubagentStatusChanged {
                agent_id,
                task_id,
                status,
                ..
            } => {
                let phase: crate::tui::subagent_view::SubagentPhase = status.into();
                state.upsert_subagent(crate::tui::state::SubagentUpdate {
                    agent_id: agent_id.as_str().to_owned(),
                    task_id: task_id.as_str().to_owned(),
                    display_name: None,
                    phase,
                    summary: phase.label().to_ascii_lowercase(),
                    changed_paths: None,
                    output_paths: None,
                    diagnostic: None,
                });
            }
            RuntimeEvent::SubagentCompleted {
                agent_id,
                task_id,
                summary,
                output_paths,
                changed_paths,
                ..
            } => {
                state.upsert_subagent(crate::tui::state::SubagentUpdate {
                    agent_id: agent_id.as_str().to_owned(),
                    task_id: task_id.as_str().to_owned(),
                    display_name: None,
                    phase: crate::tui::subagent_view::SubagentPhase::Completed,
                    summary,
                    changed_paths: Some(changed_paths),
                    output_paths: Some(output_paths),
                    diagnostic: None,
                });
            }
            RuntimeEvent::SubagentFailed {
                agent_id,
                task_id,
                diagnostic,
                ..
            } => {
                self.streaming_assistant_index = None;
                let detail = format!("{}: {}", diagnostic.code(), diagnostic.message());
                state.upsert_subagent(crate::tui::state::SubagentUpdate {
                    agent_id: agent_id.as_str().to_owned(),
                    task_id: task_id.as_str().to_owned(),
                    display_name: None,
                    phase: crate::tui::subagent_view::SubagentPhase::Failed,
                    summary: detail.clone(),
                    changed_paths: None,
                    output_paths: None,
                    diagnostic: Some(detail),
                });
            }
            RuntimeEvent::SubagentCancelled {
                agent_id,
                task_id,
                diagnostic,
                ..
            } => {
                self.streaming_assistant_index = None;
                let detail = format!("{}: {}", diagnostic.code(), diagnostic.message());
                state.upsert_subagent(crate::tui::state::SubagentUpdate {
                    agent_id: agent_id.as_str().to_owned(),
                    task_id: task_id.as_str().to_owned(),
                    display_name: None,
                    phase: crate::tui::subagent_view::SubagentPhase::Cancelled,
                    summary: detail.clone(),
                    changed_paths: None,
                    output_paths: None,
                    diagnostic: Some(detail),
                });
            }
            RuntimeEvent::PlanModeChanged { mode, .. } => {
                let previous = state.plan().mode();
                state.plan_mut().set_mode(mode);
                if previous != mode {
                    let detail = match mode {
                        PlanModeState::Pending => {
                            "Plan Mode armed (read-only exploration on next prompt)"
                        }
                        PlanModeState::Active => "Plan Mode active (workspace writes blocked)",
                        PlanModeState::ExitPending => {
                            "Plan Mode will exit after the current turn completes"
                        }
                        PlanModeState::Inactive => "Plan Mode off (workspace writes enabled)",
                    };
                    state.push_timeline_item(TimelineItem::Muted {
                        title: "Plan Mode".to_owned(),
                        detail: detail.to_owned(),
                    });
                }
            }
            RuntimeEvent::PlanSaved { plan, .. } => {
                let detail = format!(
                    "{} ({}, v{})",
                    plan.summary.title, plan.summary.file_name, plan.summary.revision
                );
                state.plan_mut().upsert_plan(plan.clone());
                state.refresh_open_plan_view(&plan);
                if !self.has_started_tool("write_plan") {
                    state.push_timeline_item(TimelineItem::Muted {
                        title: "Plan saved".to_owned(),
                        detail,
                    });
                }
            }
            RuntimeEvent::PlanApprovalRequested { plan, .. } => {
                let plan_id = plan.summary.id.clone();
                let detail = format!(
                    "{} ({}, v{}) — ready for review",
                    plan.summary.title, plan.summary.file_name, plan.summary.revision
                );
                state.plan_mut().mark_approval_requested(plan);
                if !self.has_started_tool("exit_plan_mode") {
                    state.push_timeline_item(TimelineItem::Muted {
                        title: "Plan review".to_owned(),
                        detail,
                    });
                }
                state.open_plan_view(Some(plan_id));
            }
            RuntimeEvent::PlanReviewed {
                plan_id, decision, ..
            } => {
                state.plan_mut().apply_review(&plan_id, &decision);
                let title = match &decision {
                    PlanReviewDecision::Approved { .. } => "Plan approved",
                    PlanReviewDecision::ChangesRequested { .. } => "Plan changes requested",
                    PlanReviewDecision::Abandoned => "Plan abandoned",
                };
                let detail = state
                    .plan()
                    .plan_by_id(&plan_id)
                    .map(|doc| doc.summary.title.clone())
                    .unwrap_or_else(|| plan_id.as_str().to_owned());
                state.push_timeline_item(TimelineItem::Muted {
                    title: title.to_owned(),
                    detail,
                });
            }
            RuntimeEvent::TodoListUpdated { items, .. } => {
                state.todo_dock_mut().set_items(items);
            }
            RuntimeEvent::StepCompleted { .. } => {
                state.finish_reasoning();
            }
            RuntimeEvent::RunFailed { diagnostic, .. } => {
                state.finish_reasoning();
                self.finish_pending_commands(state, "failed");
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
                state.finish_reasoning();
                self.finish_pending_commands(state, "cancelled");
                self.streaming_assistant_index = None;
                let had_compaction = if let Some(index) = self.compaction_timeline_index.take() {
                    state.replace_timeline_item(
                        index,
                        TimelineItem::Muted {
                            title: "Compaction cancelled".to_owned(),
                            detail: diagnostic.message().to_owned(),
                        },
                    );
                    true
                } else {
                    false
                };
                let completed_stop = state.complete_stop_feedback();
                if !had_compaction && !completed_stop {
                    state.push_timeline_item(TimelineItem::Diagnostic {
                        title: diagnostic.code().to_owned(),
                        body: diagnostic.message().to_owned(),
                    });
                }
            }
            RuntimeEvent::Closed => {
                state.finish_reasoning();
                self.finish_pending_commands(state, "interrupted");
                self.streaming_assistant_index = None;
                state.push_timeline_item(TimelineItem::Muted {
                    title: "closed".to_owned(),
                    detail: "runtime stream closed".to_owned(),
                });
            }
            _ => {}
        }
    }

    fn has_started_tool(&self, name: &str) -> bool {
        self.started_tools
            .values()
            .any(|tool| tool.name.as_str() == name)
    }

    /// Stops running-tool animations when the run ends before individual results arrive.
    fn finish_pending_commands(&mut self, state: &mut TuiState, status: &str) {
        for (_, tool) in self.started_tools.drain() {
            state.replace_timeline_item(
                tool.timeline_index,
                TimelineItem::Muted {
                    title: completed_tool_title(&tool, status),
                    detail: String::new(),
                },
            );
        }
    }

    fn start_tool(&mut self, call: merry_core::PendingToolCall, state: &mut TuiState) {
        self.streaming_assistant_index = None;
        let call_id = call.id().clone();
        let tool_name = call.name().clone();
        let (title, detail) =
            started_tool_title_and_detail(tool_name.as_str(), call.arguments().as_object());
        let timeline_index = state.timeline().len();
        let started_at = Instant::now();
        let item = if tool_name.as_str() == "run_process" {
            TimelineItem::Command {
                view: CommandView::Running {
                    detail: detail.clone(),
                    started_at,
                    live_preview: crate::tui::state::LiveCommandOutput::default(),
                    expanded: false,
                },
            }
        } else {
            TimelineItem::ToolRunning {
                title: title.clone(),
                detail: detail.clone(),
                started_at,
                expanded: false,
            }
        };
        state.push_timeline_item(item);
        self.started_tools.insert(
            call_id,
            StartedToolView {
                name: tool_name,
                timeline_index,
                title,
                detail,
                started_at: Some(started_at),
                command: call
                    .arguments()
                    .as_object()
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                cwd: call
                    .arguments()
                    .as_object()
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_owned(),
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
