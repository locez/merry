use crate::tui::{
    overlay::Overlay,
    plan_projector::plan_timeline_item,
    process_output::process_exit_code,
    projector::tool_output::{
        compact_tool_output, completed_process_view, completed_tool_title, expanded_tool_title,
        failed_tool_body, parse_apply_patch_view, started_tool_title_and_detail,
        success_tool_bodies, tool_output_text,
    },
    state::{CommandView, QueuePreview, TimelineItem, TuiState},
};
use merry_core::{
    PlanAttemptOutcome, PlanDirectiveStatus, PlanPhase, RuntimeEvent, TOOL_CANCELLED_BY_USER_CODE,
    ToolCallId, ToolCallResultStatus, ToolName,
};
use merry_runtime::SessionTranscriptItem;
use merry_tools::APPLY_PATCH_TOOL;
use serde_json::Value;
use std::collections::HashMap;
use tokio::time::Instant;

mod tool_output;

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
                state.confirm_or_push_user_input(text, merry_core::QueuedInputLane::Next);
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
                state.apply_runtime_run_state(run_state);
            }
            RuntimeEvent::QueuedInputsChanged { inputs } => {
                state.update_queue_preview(QueuePreview {
                    next: inputs.next,
                    suspended: inputs.suspended,
                    backlog: inputs.backlog,
                });
            }
            RuntimeEvent::QueuedInputAccepted { lane, inputs } => {
                state.confirm_local_run_start();
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
                    });
                    if let Some(tool) = tool.as_ref() {
                        state.replace_timeline_item(tool.timeline_index, patch_item);
                    } else {
                        state.push_timeline_item(patch_item);
                    }
                } else if let Some(tool) = tool.as_ref()
                    && let Some(preview) = success_tool_bodies(tool.name.as_str(), &text)
                {
                    state.replace_timeline_item(
                        tool.timeline_index,
                        TimelineItem::Expanded {
                            title: expanded_tool_title(tool),
                            body: preview,
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

    /// Stops command animations when the run ends before individual results arrive.
    fn finish_pending_commands(&mut self, state: &mut TuiState, status: &str) {
        self.started_tools.retain(|_, tool| {
            if tool.name.as_str() != "run_process" {
                return true;
            }
            state.replace_timeline_item(
                tool.timeline_index,
                TimelineItem::Muted {
                    title: completed_tool_title(tool, status),
                    detail: String::new(),
                },
            );
            false
        });
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
                },
            }
        } else {
            TimelineItem::Muted {
                title: title.clone(),
                detail: detail.clone(),
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
