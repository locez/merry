//! Runtime-owned trajectory projection and subscriptions.

use merry_core::{
    QueuedInputView, ToolCallId, ToolSpec, TrajectoryEvent, TrajectoryLane, TrajectoryRecord,
    TrajectoryRecordId, TrajectoryRecordKind, TrajectoryRecordStatus, TrajectorySnapshot,
    TrajectoryTurnId,
};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::broadcast;

mod events;

mod prompt;

mod records;

mod replay;

const TRAJECTORY_EVENT_BUFFER: usize = 256;

/// Runtime-owned read model for one session's trajectory.
pub(crate) struct RuntimeObservability {
    state: Mutex<ProjectionState>,
    updates: broadcast::Sender<TrajectoryEvent>,
}

struct ProjectionState {
    snapshot: TrajectorySnapshot,
    pending_inputs: Vec<QueuedInputView>,
    next_turn_id: u64,
    active_turn_id: Option<TrajectoryTurnId>,
    active_parent_id: Option<TrajectoryRecordId>,
}

impl RuntimeObservability {
    pub(crate) fn new(session_id: merry_core::SessionId, tool_specs: Vec<ToolSpec>) -> Arc<Self> {
        let (updates, _) = broadcast::channel(TRAJECTORY_EVENT_BUFFER);
        let mut snapshot = TrajectorySnapshot::new(session_id);
        snapshot.set_tool_specs(tool_specs);
        Arc::new(Self {
            state: Mutex::new(ProjectionState {
                snapshot,
                pending_inputs: Vec::new(),
                next_turn_id: 1,
                active_turn_id: None,
                active_parent_id: None,
            }),
            updates,
        })
    }

    pub(crate) fn snapshot(&self) -> TrajectorySnapshot {
        self.lock_state().snapshot.clone()
    }

    pub(crate) fn subscribe_with_snapshot(
        &self,
    ) -> (TrajectorySnapshot, broadcast::Receiver<TrajectoryEvent>) {
        let state = self.lock_state();
        let receiver = self.updates.subscribe();
        (state.snapshot.clone(), receiver)
    }

    pub(crate) fn record_queued_input_accepted(&self, inputs: &[QueuedInputView]) {
        let mut state = self.lock_state();
        state.pending_inputs.extend(inputs.iter().cloned());
    }

    pub(crate) fn close(&self) {
        self.publish_session_closed();
    }

    fn publish(&self, record: Option<TrajectoryRecord>, sequence: u64) {
        let Some(mut record) = record else {
            return;
        };
        let event = {
            let mut state = self.lock_state();
            if state.snapshot.is_closed() {
                return;
            }
            if record.turn_id().is_none() {
                record.set_turn_id(state.active_turn_id);
            }
            if record.parent_id().is_none() && record.lane() != TrajectoryLane::Input {
                let tool_call_id = record.tool_call_id().cloned();
                record.set_relationship(state.active_parent_id.clone(), tool_call_id);
            }
            state.snapshot.advance_latest_sequence(sequence);
            if !state.snapshot.upsert_record(record.clone()) {
                return;
            }
            state.snapshot.advance_revision();
            TrajectoryEvent::RecordUpsert {
                revision: state.snapshot.revision(),
                latest_sequence: state.snapshot.latest_sequence(),
                record: Box::new(record),
            }
        };
        let _ = self.updates.send(event);
    }

    fn active_compaction_record(&self) -> Option<TrajectoryRecord> {
        let state = self.lock_state();
        state
            .snapshot
            .records()
            .iter()
            .rev()
            .find(|record| {
                record.kind() == TrajectoryRecordKind::Compaction
                    && record.status() == TrajectoryRecordStatus::Running
            })
            .cloned()
    }

    fn publish_session_closed(&self) {
        let event = {
            let mut state = self.lock_state();
            if state.snapshot.is_closed() {
                return;
            }
            state.snapshot.mark_closed();
            state.snapshot.advance_revision();
            TrajectoryEvent::SessionClosed {
                revision: state.snapshot.revision(),
                latest_sequence: state.snapshot.latest_sequence(),
            }
        };
        let _ = self.updates.send(event);
    }

    fn find_record_by_tool_call(&self, call_id: &ToolCallId) -> Option<TrajectoryRecord> {
        self.lock_state()
            .snapshot
            .records()
            .iter()
            .find(|record| record.tool_call_id() == Some(call_id))
            .cloned()
    }

    fn set_active_parent(&self, parent_id: TrajectoryRecordId) {
        self.lock_state().active_parent_id = Some(parent_id);
    }

    fn lock_state(&self) -> MutexGuard<'_, ProjectionState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
#[path = "trajectory_tests.rs"]
mod tests;
