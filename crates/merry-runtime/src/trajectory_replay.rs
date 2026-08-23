//! Sequence recovery helpers for rebuilding a trajectory from session state.

use crate::{LedgerFactKind, LedgerProjection, LedgerProjectionSnapshot};
use merry_core::{
    ArtifactRef, TrajectoryRecord, TrajectoryRecordId, TrajectoryRecordKind, TrajectorySnapshot,
    TrajectoryTurnId,
};
use std::collections::{HashSet, VecDeque};

/// Matches legacy records to the ordered transcript during projection replay.
pub(crate) struct ReplayRecords {
    user_records: Vec<TrajectoryRecord>,
    assistant_records: Vec<TrajectoryRecord>,
    used_user_ids: HashSet<TrajectoryRecordId>,
    used_assistant_ids: HashSet<TrajectoryRecordId>,
}

impl ReplayRecords {
    /// Captures the user and assistant records available in a persisted snapshot.
    pub(crate) fn from_snapshot(snapshot: &TrajectorySnapshot) -> Self {
        Self {
            user_records: snapshot
                .records()
                .iter()
                .filter(|record| record.kind() == TrajectoryRecordKind::UserInput)
                .cloned()
                .collect(),
            assistant_records: snapshot
                .records()
                .iter()
                .filter(|record| record.kind() == TrajectoryRecordKind::AssistantMessage)
                .cloned()
                .collect(),
            used_user_ids: HashSet::new(),
            used_assistant_ids: HashSet::new(),
        }
    }

    /// Takes the best matching legacy user record for one transcript item.
    pub(crate) fn take_user(
        &mut self,
        artifact: &ArtifactRef,
        turn_id: TrajectoryTurnId,
    ) -> Option<TrajectoryRecord> {
        take_record(
            &self.user_records,
            &mut self.used_user_ids,
            |record| {
                record
                    .artifacts()
                    .iter()
                    .any(|current| current.id() == artifact.id())
            },
            |record| record.turn_id() == Some(turn_id),
        )
    }

    /// Takes the best matching legacy assistant record for one transcript item.
    pub(crate) fn take_assistant(&mut self, artifact: &ArtifactRef) -> Option<TrajectoryRecord> {
        take_record(
            &self.assistant_records,
            &mut self.used_assistant_ids,
            |record| {
                record
                    .artifacts()
                    .iter()
                    .any(|current| current.id() == artifact.id())
            },
            |_| false,
        )
    }
}

fn take_record(
    records: &[TrajectoryRecord],
    used_ids: &mut HashSet<TrajectoryRecordId>,
    preferred: impl Fn(&TrajectoryRecord) -> bool,
    secondary: impl Fn(&TrajectoryRecord) -> bool,
) -> Option<TrajectoryRecord> {
    let index = records
        .iter()
        .enumerate()
        .filter(|(_, record)| !used_ids.contains(record.id()))
        .find(|(_, record)| preferred(record))
        .map(|(index, _)| index)
        .or_else(|| {
            records
                .iter()
                .enumerate()
                .filter(|(_, record)| !used_ids.contains(record.id()))
                .find(|(_, record)| secondary(record))
                .map(|(index, _)| index)
        })
        .or_else(|| {
            records
                .iter()
                .enumerate()
                .find(|(_, record)| !used_ids.contains(record.id()))
                .map(|(index, _)| index)
        })?;
    let record = records[index].clone();
    used_ids.insert(record.id().clone());
    Some(record)
}

/// Durable sequence queues used when a trajectory savepoint lags the session.
pub(crate) struct ReplaySequences {
    step_sequences: Vec<u64>,
    tool_pending: VecDeque<u64>,
    last_tool_pending: Option<u64>,
    tool_resolved: VecDeque<u64>,
    pub(crate) model_retry: VecDeque<u64>,
    pub(crate) latest_sequence: u64,
}

impl ReplaySequences {
    /// Collects all source sequences needed to normalize a session projection.
    ///
    /// The transcript may contain records from before the persisted trajectory
    /// savepoint. Keep every queue, not only the savepoint tail, because legacy
    /// snapshots can contain the right records at synthetic sequence numbers.
    pub(crate) fn from_ledger(baseline: u64, ledger: &LedgerProjectionSnapshot) -> Self {
        let mut replay = Self {
            step_sequences: Vec::new(),
            tool_pending: VecDeque::new(),
            last_tool_pending: None,
            tool_resolved: VecDeque::new(),
            model_retry: VecDeque::new(),
            latest_sequence: baseline,
        };
        let mut pending_sequences = Vec::new();
        let mut resolved_sequences = Vec::new();
        for entry in ledger.entries() {
            let (sequence, kind) = match entry {
                LedgerProjection::Lifecycle { sequence, kind, .. } => (*sequence, Some(*kind)),
                LedgerProjection::Fact { sequence, .. } => (*sequence, None),
            };
            replay.latest_sequence = replay.latest_sequence.max(sequence);
            match kind {
                Some(LedgerFactKind::StepStarted) => replay.step_sequences.push(sequence),
                Some(LedgerFactKind::ToolCallPending)
                | Some(LedgerFactKind::BridgeToolCallRequested) => pending_sequences.push(sequence),
                Some(LedgerFactKind::ToolCallResolved) => {
                    resolved_sequences.push(sequence);
                    replay.tool_resolved.push_back(sequence);
                }
                Some(LedgerFactKind::ModelRetry) => replay.model_retry.push_back(sequence),
                _ => {}
            }
        }
        replay.step_sequences.sort_unstable();
        replay.last_tool_pending = pending_sequences.last().copied();
        // A batch records one pending lifecycle fact for several calls. Expand
        // that source sequence once per resolved result before the next batch.
        for (index, sequence) in pending_sequences.iter().enumerate() {
            let next_pending = pending_sequences.get(index + 1).copied();
            let batch_size = resolved_sequences
                .iter()
                .filter(|resolved| {
                    **resolved > *sequence && next_pending.is_none_or(|next| **resolved < next)
                })
                .count()
                .max(1);
            replay
                .tool_pending
                .extend(std::iter::repeat_n(*sequence, batch_size));
        }
        replay
    }

    /// Maps a transcript model turn to its durable step-start sequence.
    pub(crate) fn model_turn_sequence(&self, model_turn_id: u64) -> Option<u64> {
        let index = usize::try_from(model_turn_id.checked_sub(1)?).ok()?;
        self.step_sequences.get(index).copied()
    }

    /// Maps a lifecycle sequence to the nearest preceding model turn.
    pub(crate) fn model_turn_id_for_sequence(&self, sequence: u64) -> Option<u64> {
        let index = self
            .step_sequences
            .partition_point(|candidate| *candidate <= sequence)
            .checked_sub(1)?;
        u64::try_from(index + 1).ok()
    }

    /// Returns the next durable pending-tool sequence, or a monotonic fallback.
    pub(crate) fn next_tool_pending_sequence(&mut self) -> u64 {
        self.tool_pending
            .pop_front()
            .or(self.last_tool_pending)
            .unwrap_or_else(|| self.next_fallback())
    }

    /// Returns the next durable resolved-tool sequence when one remains.
    pub(crate) fn next_tool_resolved_sequence(&mut self) -> Option<u64> {
        self.tool_resolved.pop_front()
    }

    /// Allocates a sequence after every known ledger sequence.
    pub(crate) fn next_fallback(&mut self) -> u64 {
        self.latest_sequence = self.latest_sequence.saturating_add(1);
        self.latest_sequence
    }
}
