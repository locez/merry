//! Projects child-loop results and exact tool evidence into manager-owned records.

use crate::{
    AgentLoopBlockedReason, AgentLoopResult, AgentLoopStatus, ArtifactContent, Runtime,
    subagent::{
        APPLY_PATCH_TOOL_NAME, SubagentResultView, SubagentStatusLabel, child::error_info,
        manager::ManagedSubagent, spec::validate_scope_path,
    },
};
use merry_core::{RuntimeJournalEvent, RuntimeJournalPayload, ToolCallResultStatus};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Debug, Default)]
pub(in crate::subagent) struct ChildLoopProjection {
    pub(super) result: Option<SubagentResultView>,
    pub(super) changed_paths: Vec<String>,
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

        Self {
            result: explicit_result,
            changed_paths: changed_paths_from_child_events(runtime, result.events()).await,
        }
    }
}

pub(in crate::subagent) fn apply_loop_result(
    agent: &mut ManagedSubagent,
    result: &AgentLoopResult,
    projection: ChildLoopProjection,
) {
    agent.result = projection.result;
    agent.changed_paths = projection.changed_paths;

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
                        "child blocked after {max_model_turns} model turns; spawn a replacement with a larger budget"
                    );
                    agent.diagnostics = Some(error_info(
                        "subagent_max_model_turns_reached",
                        format!(
                            "child used all {max_model_turns} model turns. This is recoverable: inspect the returned status and spawn a replacement with the same plan_client_key and a larger budget within the configured maximum; use a larger value for complex tasks and continue from the shared workspace."
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
                let Ok(content) = runtime.read_artifact_content(result.artifact().id()).await
                else {
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
    content: &ArtifactContent,
    paths: &mut BTreeSet<String>,
) {
    let Some(text) = content.as_text() else {
        return;
    };
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
