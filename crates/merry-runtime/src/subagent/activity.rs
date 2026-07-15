use merry_core::{
    InteractiveRunState, RuntimeEvent, SubagentActivityPhase, SubagentActivitySnapshot, SubagentId,
    SubagentTaskId,
};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::watch;

const NON_TERMINAL_COALESCE_MS: u64 = 250;
const MAX_SUMMARY_BYTES: usize = 120;

/// Receiver for the latest sorted activity snapshot set.
pub type SubagentActivityReceiver = watch::Receiver<Vec<SubagentActivitySnapshot>>;

/// Runtime-owned latest-value activity projection for UI consumers.
#[derive(Clone)]
pub struct SubagentActivityHub {
    sender: watch::Sender<Vec<SubagentActivitySnapshot>>,
    state: Arc<std::sync::Mutex<ActivityHubState>>,
}

struct ActivityHubState {
    snapshots: BTreeMap<SubagentId, SubagentActivitySnapshot>,
    last_published_ms: BTreeMap<SubagentId, u64>,
}

impl SubagentActivityHub {
    /// Creates an empty latest-value activity hub.
    #[must_use]
    pub fn new() -> Self {
        let (sender, _receiver) = watch::channel(Vec::new());
        Self {
            sender,
            state: Arc::new(std::sync::Mutex::new(ActivityHubState {
                snapshots: BTreeMap::new(),
                last_published_ms: BTreeMap::new(),
            })),
        }
    }

    /// Subscribes to the current and future latest snapshot sets.
    #[must_use]
    pub fn subscribe(&self) -> SubagentActivityReceiver {
        self.sender.subscribe()
    }

    pub(crate) fn publish(&self, snapshot: SubagentActivitySnapshot) {
        let is_terminal = is_terminal_phase(snapshot.phase);
        let subagent_id = snapshot.subagent_id.clone();
        let updated_at_ms = snapshot.updated_at_ms;
        let values_to_update = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            if state
                .snapshots
                .get(&snapshot.subagent_id)
                .is_some_and(|previous| is_terminal_phase(previous.phase) && !is_terminal)
            {
                return;
            }

            let should_publish = is_terminal
                || state
                    .last_published_ms
                    .get(&subagent_id)
                    .is_none_or(|last_published_ms| {
                        updated_at_ms >= last_published_ms.saturating_add(NON_TERMINAL_COALESCE_MS)
                    });
            state.snapshots.insert(subagent_id.clone(), snapshot);
            let values = state.snapshots.values().cloned().collect::<Vec<_>>();

            if should_publish {
                state.last_published_ms.insert(subagent_id, updated_at_ms);
            }
            Some((values, should_publish))
        };

        if let Some((values, should_publish)) = values_to_update {
            let _ = self.sender.send_if_modified(|current| {
                *current = values;
                should_publish
            });
        }
    }

    #[allow(dead_code)]
    pub(crate) fn current(&self) -> Vec<SubagentActivitySnapshot> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshots
            .values()
            .cloned()
            .collect()
    }
}

impl Default for SubagentActivityHub {
    fn default() -> Self {
        Self::new()
    }
}

fn is_terminal_phase(phase: SubagentActivityPhase) -> bool {
    matches!(
        phase,
        SubagentActivityPhase::Completed
            | SubagentActivityPhase::Failed
            | SubagentActivityPhase::Cancelled
    )
}

#[derive(Debug, Clone)]
pub(crate) struct SubagentActivityReducer {
    subagent_id: SubagentId,
    task_id: SubagentTaskId,
    last_valid: Option<SubagentActivitySnapshot>,
}

impl SubagentActivityReducer {
    pub(crate) fn new(subagent_id: SubagentId, task_id: SubagentTaskId) -> Self {
        Self {
            subagent_id,
            task_id,
            last_valid: None,
        }
    }

    pub(crate) fn reduce(
        &mut self,
        event: &RuntimeEvent,
        updated_at_ms: u64,
    ) -> Option<SubagentActivitySnapshot> {
        match self.try_reduce(event, updated_at_ms) {
            Ok(Some(snapshot)) => {
                self.last_valid = Some(snapshot.clone());
                Some(snapshot)
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    subagent_id = %self.subagent_id,
                    task_id = %self.task_id,
                    error,
                    "subagent activity event projection skipped"
                );
                None
            }
        }
    }

    pub(crate) fn starting(&mut self, updated_at_ms: u64) -> SubagentActivitySnapshot {
        let snapshot = self.snapshot(SubagentActivityPhase::Starting, "starting", updated_at_ms);
        self.last_valid = Some(snapshot.clone());
        snapshot
    }

    pub(crate) fn terminal(
        &mut self,
        phase: SubagentActivityPhase,
        summary: &str,
        updated_at_ms: u64,
    ) -> SubagentActivitySnapshot {
        let snapshot = self.snapshot(phase, summary, updated_at_ms);
        self.last_valid = Some(snapshot.clone());
        snapshot
    }

    #[cfg(test)]
    fn last_valid(&self) -> Option<&SubagentActivitySnapshot> {
        self.last_valid.as_ref()
    }

    fn try_reduce(
        &self,
        event: &RuntimeEvent,
        updated_at_ms: u64,
    ) -> Result<Option<SubagentActivitySnapshot>, &'static str> {
        let (phase, summary) = match event {
            RuntimeEvent::StepStarted { .. } => {
                (SubagentActivityPhase::Running, "working".to_owned())
            }
            RuntimeEvent::ToolCallStarted { call, .. } => {
                let tool_name = call.name().as_str();
                if tool_name.is_empty() {
                    return Err("tool call name was empty");
                }
                (SubagentActivityPhase::Running, format!("tool: {tool_name}"))
            }
            RuntimeEvent::PlanUpdated { .. } => {
                (SubagentActivityPhase::Running, "subplan updated".to_owned())
            }
            RuntimeEvent::InteractiveRunStateChanged { state } => match state {
                InteractiveRunState::WaitingForInput => (
                    SubagentActivityPhase::Waiting,
                    "waiting for input".to_owned(),
                ),
                InteractiveRunState::RunningModel | InteractiveRunState::RunningTool => {
                    (SubagentActivityPhase::Running, "working".to_owned())
                }
                InteractiveRunState::Interrupting | InteractiveRunState::Closed => return Ok(None),
            },
            _ => return Ok(None),
        };

        Ok(Some(self.snapshot(phase, &summary, updated_at_ms)))
    }

    fn snapshot(
        &self,
        phase: SubagentActivityPhase,
        summary: &str,
        updated_at_ms: u64,
    ) -> SubagentActivitySnapshot {
        SubagentActivitySnapshot {
            subagent_id: self.subagent_id.clone(),
            task_id: self.task_id.clone(),
            phase,
            summary: bounded_summary(summary),
            updated_at_ms,
        }
    }
}

fn bounded_summary(summary: &str) -> String {
    let sanitized = summary
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let mut end = sanitized.len().min(MAX_SUMMARY_BYTES);
    while !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    sanitized[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{
        PendingToolCall, RuntimeEventSource, ToolCallArguments, ToolCallId, ToolName,
    };
    use serde_json::Map;

    fn agent_id(value: &str) -> SubagentId {
        SubagentId::new(value).expect("valid subagent id")
    }

    fn task_id(value: &str) -> SubagentTaskId {
        SubagentTaskId::new(value).expect("valid task id")
    }

    fn source() -> RuntimeEventSource {
        RuntimeEventSource::new(
            merry_core::SessionId::new("child-session").expect("valid session id"),
            1,
        )
    }

    fn snapshot(
        id: &str,
        phase: SubagentActivityPhase,
        updated_at_ms: u64,
    ) -> SubagentActivitySnapshot {
        SubagentActivitySnapshot {
            subagent_id: agent_id(id),
            task_id: task_id("task-1"),
            phase,
            summary: "working".to_owned(),
            updated_at_ms,
        }
    }

    fn tool_call(name: &str) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("call-1").expect("valid call id"),
            ToolName::new(name).expect("valid tool name"),
            ToolCallArguments::new(Map::new()),
        )
    }

    #[test]
    fn reducer_maps_structured_events_and_ignores_assistant_text() {
        let mut reducer = SubagentActivityReducer::new(agent_id("agent-1"), task_id("task-1"));

        let started = reducer
            .reduce(&RuntimeEvent::StepStarted { source: source() }, 10)
            .expect("step start projects activity");
        assert_eq!(started.phase, SubagentActivityPhase::Running);
        assert_eq!(started.summary, "working");

        let tool = reducer
            .reduce(
                &RuntimeEvent::ToolCallStarted {
                    call: tool_call("workspace_patch"),
                    source: source(),
                },
                20,
            )
            .expect("tool start projects activity");
        assert_eq!(tool.phase, SubagentActivityPhase::Running);
        assert_eq!(tool.summary, "tool: workspace_patch");
        assert_eq!(reducer.last_valid(), Some(&tool));

        assert!(
            reducer
                .reduce(
                    &RuntimeEvent::AssistantMessageDelta {
                        delta: "unstructured assistant prose".to_owned(),
                        source: source(),
                    },
                    30,
                )
                .is_none()
        );
        assert_eq!(reducer.last_valid(), Some(&tool));

        let plan_updated = reducer
            .reduce(
                &RuntimeEvent::PlanUpdated {
                    snapshot: merry_core::PlanSnapshot {
                        plan_id: merry_core::PlanId::new("plan-1").expect("valid plan id"),
                        revision: 1,
                        phase: merry_core::PlanPhase::Executing,
                        activation_source: merry_core::PlanActivationSource::User,
                        root_node_id: None,
                        coordinator_node_id: None,
                        execution_contract_fingerprint: None,
                        execution_authorization_refs: Vec::new(),
                        authorized_capability_envelope: None,
                        approval_requirements: Vec::new(),
                        nodes: Vec::new(),
                        attempts: Vec::new(),
                        leases: Vec::new(),
                        attempt_progress: Vec::new(),
                        directives: Vec::new(),
                        resource_policy_snapshot: merry_core::PlanResourcePolicySnapshot::default(),
                        max_concurrency_hint: None,
                        scheduler_status: merry_core::PlanSchedulerStatus::Active,
                        revision_summaries: Vec::new(),
                    },
                    summary: merry_core::PlanRevisionSummary::new(1, "subplan changed")
                        .expect("valid plan revision summary"),
                    source: source(),
                },
                35,
            )
            .expect("plan update projects activity");
        assert_eq!(plan_updated.phase, SubagentActivityPhase::Running);
        assert_eq!(plan_updated.summary, "subplan updated");

        let waiting = reducer
            .reduce(
                &RuntimeEvent::InteractiveRunStateChanged {
                    state: InteractiveRunState::WaitingForInput,
                },
                40,
            )
            .expect("waiting state projects activity");
        assert_eq!(waiting.phase, SubagentActivityPhase::Waiting);
    }

    #[test]
    fn summaries_sanitize_controls_and_cap_without_splitting_utf8() {
        let sanitized = bounded_summary("before\n\tafter");
        assert_eq!(sanitized, "before  after");

        let capped = bounded_summary(&"界".repeat(50));
        assert_eq!(capped.len(), 120);
        assert_eq!(capped.chars().count(), 40);
    }

    #[test]
    fn hub_publishes_sorted_latest_values_and_coalesces_nonterminal_updates() {
        let hub = SubagentActivityHub::new();
        let mut receiver = hub.subscribe();

        hub.publish(snapshot("agent-b", SubagentActivityPhase::Starting, 100));
        assert!(receiver.has_changed().expect("receiver remains open"));
        let _ = receiver.borrow_and_update();

        hub.publish(snapshot("agent-b", SubagentActivityPhase::Running, 200));
        assert!(!receiver.has_changed().expect("receiver remains open"));
        assert_eq!(receiver.borrow()[0].updated_at_ms, 200);

        hub.publish(snapshot("agent-a", SubagentActivityPhase::Starting, 350));
        assert!(receiver.has_changed().expect("receiver remains open"));
        let values = receiver.borrow_and_update().clone();
        assert_eq!(values[0].subagent_id, agent_id("agent-a"));
        assert_eq!(values[1].subagent_id, agent_id("agent-b"));
        assert_eq!(values[1].updated_at_ms, 200);

        hub.publish(snapshot("agent-b", SubagentActivityPhase::Completed, 201));
        assert!(
            receiver
                .has_changed()
                .expect("terminal update is immediate")
        );
        assert_eq!(hub.current()[1].phase, SubagentActivityPhase::Completed);
    }

    #[test]
    fn dropping_a_receiver_does_not_cancel_activity_production() {
        let hub = SubagentActivityHub::new();
        let receiver = hub.subscribe();
        drop(receiver);

        hub.publish(snapshot("agent-1", SubagentActivityPhase::Running, 1));
        assert_eq!(hub.current().len(), 1);
    }
}
