//! Projection from internal runtime journal events to public runtime events.

use super::tool_output::public_tool_output;
use crate::{ArtifactContent, Runtime, RuntimeError};
use merry_core::{
    ArtifactId, RuntimeEvent, RuntimeEventSource, RuntimeJournalEvent, RuntimeJournalPayload,
    ToolCallId,
};
use std::collections::BTreeSet;

/// Stateful public-event projector for one journal stream.
///
/// The projector is read-only with respect to runtime state. It only reads
/// artifact contents required to make public events ergonomic and deterministic.
#[derive(Debug, Default)]
pub struct RuntimeEventProjector {
    started_tool_calls: BTreeSet<ToolCallId>,
}

impl RuntimeEventProjector {
    /// Creates an empty projector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn project(
        &mut self,
        event: RuntimeJournalEvent,
        runtime: &Runtime,
    ) -> Result<Option<RuntimeEvent>, RuntimeError> {
        let source = RuntimeEventSource::new(event.session_id, event.sequence);
        match event.payload {
            RuntimeJournalPayload::SessionStarted => {
                Ok(Some(RuntimeEvent::SessionStarted { source }))
            }
            RuntimeJournalPayload::StepStarted => Ok(Some(RuntimeEvent::StepStarted { source })),
            RuntimeJournalPayload::StepCompleted => {
                Ok(Some(RuntimeEvent::StepCompleted { source }))
            }
            RuntimeJournalPayload::CompactionStarted => {
                Ok(Some(RuntimeEvent::CompactionStarted { source }))
            }
            RuntimeJournalPayload::CompactionCompleted {
                checkpoint_id,
                covered_history_item_count,
            } => Ok(Some(RuntimeEvent::CompactionCompleted {
                checkpoint_id,
                covered_history_item_count,
                source,
            })),
            RuntimeJournalPayload::SessionUsageUpdated { usage } => {
                Ok(Some(RuntimeEvent::UsageUpdated { usage, source }))
            }
            RuntimeJournalPayload::ModelRetryAttemptStarted {
                attempt,
                max_attempts,
            } => Ok(Some(RuntimeEvent::ModelRetryAttemptStarted {
                attempt,
                max_attempts,
                source,
            })),
            RuntimeJournalPayload::ModelRetryScheduled {
                attempt,
                next_attempt,
                max_attempts,
                delay_ms,
                error_kind,
            } => Ok(Some(RuntimeEvent::ModelRetryScheduled {
                attempt,
                next_attempt,
                max_attempts,
                delay_ms,
                error_kind,
                source,
            })),
            RuntimeJournalPayload::ModelRetryExhausted {
                attempts_run,
                max_attempts,
                error_kind,
            } => Ok(Some(RuntimeEvent::ModelRetryExhausted {
                attempts_run,
                max_attempts,
                error_kind,
                source,
            })),
            RuntimeJournalPayload::ArtifactRecorded { .. } => Ok(None),
            RuntimeJournalPayload::AssistantOutputDelta { delta } => {
                Ok(Some(RuntimeEvent::AssistantMessageDelta { delta, source }))
            }
            RuntimeJournalPayload::AssistantOutputRecorded { artifact } => {
                let text = read_text_artifact(runtime, artifact.id()).await?;
                Ok(Some(RuntimeEvent::AssistantMessage {
                    text,
                    artifact,
                    source,
                }))
            }
            RuntimeJournalPayload::EvidenceReferenced { evidence } => {
                Ok(Some(RuntimeEvent::EvidenceReferenced { evidence, source }))
            }
            RuntimeJournalPayload::ToolCallPending { call }
            | RuntimeJournalPayload::BridgeToolCallRequested { call } => {
                let inserted = self.started_tool_calls.insert(call.id().clone());
                Ok(inserted.then_some(RuntimeEvent::ToolCallStarted { call, source }))
            }
            RuntimeJournalPayload::ToolCallResolved { result } => {
                let output = match runtime.read_artifact_content(result.artifact().id()).await {
                    Ok(content) => public_tool_output(&content),
                    Err(_) => None,
                };
                Ok(Some(RuntimeEvent::ToolCallFinished {
                    result,
                    output,
                    source,
                }))
            }
            RuntimeJournalPayload::FinalOutputRecorded { call_id, artifact } => {
                Ok(Some(RuntimeEvent::FinalOutputRecorded {
                    call_id,
                    artifact,
                    source,
                }))
            }
            RuntimeJournalPayload::SkillUsed {
                skill_name,
                skill_md_path,
                tool_call_id,
                artifact,
            } => Ok(Some(RuntimeEvent::SkillUsed {
                skill_name,
                skill_md_path,
                tool_call_id,
                artifact,
                source,
            })),
            RuntimeJournalPayload::SubagentSpawned {
                agent_id,
                task_id,
                task_anchor,
            } => Ok(Some(RuntimeEvent::SubagentSpawned {
                agent_id,
                task_id,
                task_anchor,
                source,
            })),
            RuntimeJournalPayload::SubagentStarted { agent_id, task_id } => {
                Ok(Some(RuntimeEvent::SubagentStarted {
                    agent_id,
                    task_id,
                    source,
                }))
            }
            RuntimeJournalPayload::SubagentStatusChanged {
                agent_id,
                task_id,
                status,
            } => Ok(Some(RuntimeEvent::SubagentStatusChanged {
                agent_id,
                task_id,
                status,
                source,
            })),
            RuntimeJournalPayload::SubagentCompleted {
                agent_id,
                task_id,
                summary,
                output_paths,
                changed_paths,
            } => Ok(Some(RuntimeEvent::SubagentCompleted {
                agent_id,
                task_id,
                summary,
                output_paths,
                changed_paths,
                source,
            })),
            RuntimeJournalPayload::SubagentFailed {
                agent_id,
                task_id,
                diagnostic,
            } => Ok(Some(RuntimeEvent::SubagentFailed {
                agent_id,
                task_id,
                diagnostic,
                source,
            })),
            RuntimeJournalPayload::SubagentCancelled {
                agent_id,
                task_id,
                diagnostic,
            } => Ok(Some(RuntimeEvent::SubagentCancelled {
                agent_id,
                task_id,
                diagnostic,
                source,
            })),
            RuntimeJournalPayload::Cancelled { diagnostic } => {
                Ok(Some(RuntimeEvent::RunCancelled { diagnostic, source }))
            }
            RuntimeJournalPayload::Failed { diagnostic } => {
                Ok(Some(RuntimeEvent::RunFailed { diagnostic, source }))
            }
            _ => Ok(None),
        }
    }
}

async fn read_text_artifact(
    runtime: &Runtime,
    artifact_id: &ArtifactId,
) -> Result<String, RuntimeError> {
    let content = runtime.read_artifact_content(artifact_id).await?;
    match content {
        ArtifactContent::Text { content: text } | ArtifactContent::Json { content: text } => {
            Ok(text)
        }
        ArtifactContent::Binary { .. }
        | ArtifactContent::Image { .. }
        | ArtifactContent::Other { .. } => Err(RuntimeError::InvalidStepInput {
            reason: "assistant output artifact content is not text",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArtifactContent;
    use merry_core::{
        ArtifactKind, ArtifactRef, ErrorInfo, PendingToolCall, SessionId, SubagentId,
        SubagentTaskId, ToolCallArguments, ToolCallResult, ToolName,
    };
    use serde_json::json;

    fn session_id() -> SessionId {
        SessionId::new("projector-test").expect("valid session id")
    }

    fn source_event(sequence: u64, payload: RuntimeJournalPayload) -> RuntimeJournalEvent {
        RuntimeJournalEvent::new(session_id(), sequence, payload)
    }

    fn artifact_id(value: &str) -> ArtifactId {
        ArtifactId::new(value).expect("valid artifact id")
    }

    fn tool_call_id(value: &str) -> ToolCallId {
        ToolCallId::new(value).expect("valid tool call id")
    }

    fn pending_tool_call(id: &str) -> PendingToolCall {
        PendingToolCall::new(
            tool_call_id(id),
            ToolName::new("lookup").expect("valid tool name"),
            ToolCallArguments::try_from(json!({"query": "notes"})).expect("valid tool arguments"),
        )
    }

    fn runtime() -> Runtime {
        Runtime::builder(session_id())
            .build()
            .expect("runtime should build")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generic_artifact_recording_does_not_project_to_public_event() {
        let runtime = runtime();
        let mut projector = RuntimeEventProjector::new();
        let event = source_event(
            1,
            RuntimeJournalPayload::ArtifactRecorded {
                artifact: ArtifactRef::new(artifact_id("generic-artifact"), ArtifactKind::Text),
            },
        );

        let projected = projector
            .project(event, &runtime)
            .await
            .expect("projection should not fail");

        assert_eq!(projected, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridge_tool_request_projects_to_plain_tool_started_when_seen_alone() {
        let runtime = runtime();
        let mut projector = RuntimeEventProjector::new();
        let call = pending_tool_call("call-bridge");
        let event = source_event(
            2,
            RuntimeJournalPayload::BridgeToolCallRequested { call: call.clone() },
        );

        let projected = projector
            .project(event, &runtime)
            .await
            .expect("projection should not fail")
            .expect("bridge request should project");

        assert!(matches!(
            projected,
            RuntimeEvent::ToolCallStarted { call: projected, .. } if projected == call
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compaction_journal_events_project_to_public_lifecycle_events() {
        let runtime = runtime();
        let mut projector = RuntimeEventProjector::new();
        let started = source_event(2, RuntimeJournalPayload::CompactionStarted);
        let completed = source_event(
            3,
            RuntimeJournalPayload::CompactionCompleted {
                checkpoint_id: "checkpoint-projector".to_owned(),
                covered_history_item_count: 4,
            },
        );

        let started = projector
            .project(started, &runtime)
            .await
            .expect("projection should not fail");
        let completed = projector
            .project(completed, &runtime)
            .await
            .expect("projection should not fail");

        assert!(matches!(
            started,
            Some(RuntimeEvent::CompactionStarted { .. })
        ));
        assert!(matches!(
            completed,
            Some(RuntimeEvent::CompactionCompleted {
                checkpoint_id,
                covered_history_item_count: 4,
                ..
            }) if checkpoint_id == "checkpoint-projector"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridge_tool_request_is_deduped_after_pending_started_event() {
        let runtime = runtime();
        let mut projector = RuntimeEventProjector::new();
        let call = pending_tool_call("call-bridge");
        let pending = source_event(
            2,
            RuntimeJournalPayload::ToolCallPending { call: call.clone() },
        );
        let bridge = source_event(3, RuntimeJournalPayload::BridgeToolCallRequested { call });

        let first = projector
            .project(pending, &runtime)
            .await
            .expect("projection should not fail");
        let second = projector
            .project(bridge, &runtime)
            .await
            .expect("projection should not fail");

        assert!(matches!(first, Some(RuntimeEvent::ToolCallStarted { .. })));
        assert_eq!(second, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_result_projects_with_complete_text_output() {
        let runtime = runtime();
        let mut projector = RuntimeEventProjector::new();
        let artifact = ArtifactRef::new(
            artifact_id("external-complete-artifact"),
            ArtifactKind::Text,
        );
        runtime
            .record_artifact(artifact.clone(), ArtifactContent::text("complete text"))
            .await
            .expect("artifact should record");
        let result = ToolCallResult::succeeded(tool_call_id("call-complete"), artifact);
        let event = source_event(
            4,
            RuntimeJournalPayload::ToolCallResolved {
                result: result.clone(),
            },
        );

        let projected = projector
            .project(event, &runtime)
            .await
            .expect("projection should not fail")
            .expect("tool result should project");

        assert!(matches!(
            projected,
                RuntimeEvent::ToolCallFinished {
                    result: projected,
                    output: Some(merry_core::ToolOutput::Text { text }),
                    ..
            } if projected == result && text == "complete text"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subagent_cancel_and_failed_journal_events_project_to_public_events() {
        let runtime = runtime();
        let mut projector = RuntimeEventProjector::new();
        let agent_id = SubagentId::new("agent-1").expect("valid agent id");
        let task_id = SubagentTaskId::new("task-1").expect("valid task id");
        let diagnostic =
            ErrorInfo::new("subagent_failed", "subagent failed").expect("valid diagnostic");
        let failed = source_event(
            5,
            RuntimeJournalPayload::SubagentFailed {
                agent_id: agent_id.clone(),
                task_id: task_id.clone(),
                diagnostic: diagnostic.clone(),
            },
        );
        let cancelled = source_event(
            6,
            RuntimeJournalPayload::Cancelled {
                diagnostic: diagnostic.clone(),
            },
        );

        let failed = projector
            .project(failed, &runtime)
            .await
            .expect("projection should not fail");
        let cancelled = projector
            .project(cancelled, &runtime)
            .await
            .expect("projection should not fail");

        assert!(matches!(
            failed,
            Some(RuntimeEvent::SubagentFailed {
                agent_id: projected_agent,
                task_id: projected_task,
                diagnostic: projected_diagnostic,
                ..
            }) if projected_agent == agent_id
                && projected_task == task_id
                && projected_diagnostic == diagnostic
        ));
        assert!(matches!(
            cancelled,
            Some(RuntimeEvent::RunCancelled {
                diagnostic: projected,
                ..
            }) if projected == diagnostic
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn model_retry_journal_events_project_to_public_retry_events() {
        let runtime = runtime();
        let mut projector = RuntimeEventProjector::new();
        let attempt = source_event(
            7,
            RuntimeJournalPayload::ModelRetryAttemptStarted {
                attempt: 1,
                max_attempts: 3,
            },
        );
        let scheduled = source_event(
            8,
            RuntimeJournalPayload::ModelRetryScheduled {
                attempt: 1,
                next_attempt: 2,
                max_attempts: 3,
                delay_ms: 10,
                error_kind: "unavailable".to_owned(),
            },
        );

        let attempt = projector
            .project(attempt, &runtime)
            .await
            .expect("projection should not fail");
        let scheduled = projector
            .project(scheduled, &runtime)
            .await
            .expect("projection should not fail");

        assert!(matches!(
            attempt,
            Some(RuntimeEvent::ModelRetryAttemptStarted {
                attempt: 1,
                max_attempts: 3,
                ..
            })
        ));
        assert!(matches!(
            scheduled,
            Some(RuntimeEvent::ModelRetryScheduled {
                attempt: 1,
                next_attempt: 2,
                max_attempts: 3,
                delay_ms: 10,
                ..
            })
        ));
    }
}
