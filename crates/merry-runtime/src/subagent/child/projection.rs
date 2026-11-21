//! Projects child-loop results and exact tool evidence into manager-owned records.

use crate::{
    AgentLoopBlockedReason, AgentLoopResult, AgentLoopStatus, Runtime,
    subagent::{
        APPLY_PATCH_TOOL_NAME, SubagentResultView, SubagentStatusLabel, child::error_info,
        manager::ManagedSubagent, spec::validate_scope_path,
    },
};
use merry_core::{RuntimeJournalEvent, RuntimeJournalPayload, ToolCallResultStatus, ToolOutput};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Debug, Default)]
pub(in crate::subagent) struct ChildLoopProjection {
    pub(super) result: Option<SubagentResultView>,
    pub(super) changed_paths: Vec<String>,
    pub(super) tool_calls: u32,
}

impl ChildLoopProjection {
    pub(super) async fn from_result(runtime: &Runtime, result: &AgentLoopResult) -> Self {
        let explicit_result = match result.status() {
            AgentLoopStatus::Completed => result
                .final_output()
                .and_then(SubagentResultView::from_conclusion),
            AgentLoopStatus::Failed { .. }
            | AgentLoopStatus::Cancelled { .. }
            | AgentLoopStatus::Blocked { .. } => None,
        };
        let tool_calls = result
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    RuntimeJournalPayload::ToolCallResolved { .. }
                )
            })
            .count();

        Self {
            result: explicit_result,
            changed_paths: changed_paths_from_child_events(runtime, result.events()).await,
            tool_calls: u32::try_from(tool_calls).unwrap_or(u32::MAX),
        }
    }
}

pub(in crate::subagent) fn apply_loop_result(
    agent: &mut ManagedSubagent,
    result: &AgentLoopResult,
    projection: ChildLoopProjection,
) {
    agent.result = projection.result;
    if !projection.changed_paths.is_empty() {
        let mut merged: BTreeSet<String> = agent.changed_paths.iter().cloned().collect();
        merged.extend(projection.changed_paths);
        agent.changed_paths = merged.into_iter().collect();
    }
    agent.turns_used = agent
        .turns_used
        .saturating_add(u32::try_from(result.model_turns_run()).unwrap_or(u32::MAX));
    agent.tool_calls = agent.tool_calls.saturating_add(projection.tool_calls);
    if let Some(session_usage) = result.session_usage() {
        agent.usage = Some(session_usage.total);
    }
    if let Some(final_output) = result.final_output()
        && agent.raw_output_bytes == 0
    {
        agent.raw_output_bytes = u64::try_from(final_output.len()).unwrap_or(u64::MAX);
    }

    match result.status() {
        AgentLoopStatus::Completed => {
            agent.status = SubagentStatusLabel::Completed;
            agent.summary = "child completed".to_owned();
            agent.output_paths.clear();
        }
        AgentLoopStatus::Failed { diagnostic } => {
            agent.status = SubagentStatusLabel::Failed;
            agent.summary = format!("child failed: {}", diagnostic.message());
            agent.diagnostics = Some(diagnostic.clone());
        }
        AgentLoopStatus::Cancelled { diagnostic } => {
            agent.status = SubagentStatusLabel::Cancelled;
            agent.summary = format!("child cancelled: {}", diagnostic.message());
            agent.diagnostics = Some(diagnostic.clone());
        }
        AgentLoopStatus::Blocked { reason } => {
            agent.status = SubagentStatusLabel::Blocked;
            match reason {
                AgentLoopBlockedReason::MaxModelTurnsReached { max_model_turns } => {
                    agent.summary = format!(
                        "child blocked after {max_model_turns} model turns; resume with send_subagent_message(agent_id=\"{}\", extra_model_turns=...)",
                        agent.agent_id.as_str()
                    );
                    agent.diagnostics = Some(error_info(
                        "subagent_max_model_turns_reached",
                        format!(
                            "child used all {max_model_turns} model turns. This is recoverable: call send_subagent_message(agent_id=\"{}\", extra_model_turns=...) to wake this same subagent with its retained transcript and context.",
                            agent.agent_id.as_str()
                        ),
                    ));
                }
                _ => {
                    agent.summary = format!("child blocked: {reason:?}");
                    agent.diagnostics = Some(error_info("subagent_blocked", format!("{reason:?}")));
                }
            }
        }
    }
}

pub(super) async fn changed_paths_from_child_events(
    runtime: &Runtime,
    events: &[RuntimeJournalEvent],
) -> Vec<String> {
    let mut pending_tool_names = BTreeMap::new();
    let mut paths = BTreeSet::new();

    for event in events {
        match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => {
                pending_tool_names.insert(call.id().clone(), call.name().clone());
            }
            RuntimeJournalPayload::ToolCallResolved { result }
                if result.status() == ToolCallResultStatus::Succeeded
                    && pending_tool_names
                        .get(result.call_id())
                        .is_some_and(|tool_name| tool_name.as_str() == APPLY_PATCH_TOOL_NAME) =>
            {
                let Ok(Some(content)) = runtime.read_tool_output(result.call_id()).await else {
                    continue;
                };
                collect_apply_patch_changed_paths(&content, &mut paths);
            }
            _ => {}
        }
    }

    paths.into_iter().collect()
}

pub(super) fn collect_apply_patch_changed_paths(
    content: &ToolOutput,
    paths: &mut BTreeSet<String>,
) {
    let text = content.as_str();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
        || value.get("tool").and_then(serde_json::Value::as_str) != Some(APPLY_PATCH_TOOL_NAME)
    {
        return;
    }

    let Some(changes) = value.get("changes").and_then(serde_json::Value::as_array) else {
        return;
    };
    for change in changes {
        let Some(path) = change.get("path").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if validate_scope_path(PathBuf::from(path)).is_ok() {
            paths.insert(path.to_owned());
        }
    }
}
