//! Provider-neutral trajectory read-model contracts.
//!
//! The `x-merry-*` schema extensions describe normalized client types and
//! serialized-field presence for generators consuming this contract.

use crate::{
    CoreError, SessionId, ToolSpec,
    trajectory::serialization::{optional_u64_as_string, u64_as_string},
};
pub use prompt::{TrajectoryPromptBlock, TrajectoryPromptSnapshot};
pub use record::{
    TrajectoryLane, TrajectoryPayload, TrajectoryPayloadKind, TrajectoryRecord,
    TrajectoryRecordDetails, TrajectoryRecordKind, TrajectoryRecordStatus, TrajectoryToolDetails,
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

mod prompt;

mod record;

/// Stable logical conversation turn identifier.
///
/// A turn is created for accepted user input and is shared by the assistant,
/// tool, and lifecycle records produced while that input is being processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, JsonSchema)]
#[serde(transparent)]
#[schemars(
    with = "String",
    extend(
        "x-merry-wire-type" = "u64",
        "x-merry-output-required" = true
    )
)]
pub struct TrajectoryTurnId(u64);

impl TrajectoryTurnId {
    /// Creates a non-zero turn identifier.
    pub fn new(value: u64) -> Result<Self, CoreError> {
        if value == 0 {
            return Err(CoreError::InvalidIdentifier {
                kind: "TrajectoryTurnId",
                value: value.to_string(),
                reason: "must be greater than zero",
            });
        }
        Ok(Self(value))
    }

    /// Returns the numeric turn identifier.
    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

impl Serialize for TrajectoryTurnId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TrajectoryTurnId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64_as_string::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// Current normalized trajectory state for one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrajectorySnapshot {
    session_id: SessionId,
    #[serde(with = "u64_as_string")]
    #[schemars(
        with = "String",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    revision: u64,
    #[serde(with = "u64_as_string")]
    #[schemars(
        with = "String",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    latest_sequence: u64,
    #[serde(default)]
    #[schemars(extend("x-merry-output-required" = true))]
    closed: bool,
    #[serde(with = "optional_u64_as_string")]
    #[schemars(
        with = "Option<String>",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    history_truncated_before: Option<u64>,
    prompt: TrajectoryPromptSnapshot,
    tool_specs: Vec<ToolSpec>,
    records: Vec<TrajectoryRecord>,
}

impl TrajectorySnapshot {
    /// Creates an empty snapshot for a session.
    #[must_use]
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            revision: 0,
            latest_sequence: 0,
            closed: false,
            history_truncated_before: None,
            prompt: TrajectoryPromptSnapshot::default(),
            tool_specs: Vec::new(),
            records: Vec::new(),
        }
    }

    /// Borrows the owning session identifier.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Returns the projection revision.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the highest observed journal sequence.
    #[must_use]
    pub fn latest_sequence(&self) -> u64 {
        self.latest_sequence
    }

    /// Returns whether the owning runtime session has emitted its terminal event.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Marks the snapshot as terminal after the session close event is durable.
    pub fn mark_closed(&mut self) {
        self.closed = true;
    }

    /// Reopens a persisted snapshot before a runtime resumes appending events.
    ///
    /// A snapshot can be terminal for the runtime instance that wrote it and
    /// still be the starting point for a later resumable runtime. Reopening is
    /// intentionally explicit so ordinary read-only consumers cannot mutate
    /// the lifecycle state accidentally.
    pub fn reopen(&mut self) {
        self.closed = false;
    }

    /// Returns the legacy sequence marker for snapshots that evicted history.
    ///
    /// New runtime projections retain every record and return `None`.
    #[must_use]
    pub fn history_truncated_before(&self) -> Option<u64> {
        self.history_truncated_before
    }

    /// Borrows records in stable sequence order.
    #[must_use]
    pub fn records(&self) -> &[TrajectoryRecord] {
        &self.records
    }

    /// Borrows prompt evidence retained for this session.
    #[must_use]
    pub fn prompt(&self) -> &TrajectoryPromptSnapshot {
        &self.prompt
    }

    /// Borrows the session-level tool catalog.
    #[must_use]
    pub fn tool_specs(&self) -> &[ToolSpec] {
        &self.tool_specs
    }

    /// Replaces the session-level tool catalog in deterministic order.
    pub fn set_tool_specs(&mut self, mut tool_specs: Vec<ToolSpec>) {
        tool_specs.sort_by(|left, right| left.name().as_str().cmp(right.name().as_str()));
        self.tool_specs = tool_specs;
    }

    /// Inserts or replaces one stable prompt block.
    pub fn upsert_prompt_block(&mut self, block: TrajectoryPromptBlock) -> bool {
        self.prompt.upsert_stable_block(block)
    }

    /// Adds dynamic prompt context evidence.
    pub fn add_dynamic_context(&mut self, count: u64, sequence: u64) {
        self.prompt.add_dynamic_context(count, sequence);
    }

    /// Inserts or replaces a record while preserving sequence order.
    pub fn upsert_record(&mut self, record: TrajectoryRecord) -> bool {
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|existing| existing.id() == record.id())
        {
            if *existing == record {
                return false;
            }
            *existing = record;
            self.sort_records();
            return true;
        }
        self.records.push(record);
        self.sort_records();
        true
    }

    fn sort_records(&mut self) {
        self.records.sort_by_key(|record| {
            (
                record.start_sequence(),
                record.sequence_order(),
                record.id().as_str().to_owned(),
            )
        });
    }

    /// Advances the highest observed source sequence.
    pub fn advance_latest_sequence(&mut self, sequence: u64) {
        self.latest_sequence = self.latest_sequence.max(sequence);
    }

    /// Advances the projection revision.
    pub fn advance_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

/// Incremental change published to Web and SDK observers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TrajectoryEvent {
    /// A complete snapshot, normally sent when a subscriber connects.
    Snapshot { snapshot: TrajectorySnapshot },
    /// A record was inserted or updated.
    RecordUpsert {
        #[serde(with = "u64_as_string")]
        #[schemars(
            with = "String",
            extend(
                "x-merry-wire-type" = "u64",
                "x-merry-output-required" = true
            )
        )]
        revision: u64,
        #[serde(with = "u64_as_string")]
        #[schemars(
            with = "String",
            extend(
                "x-merry-wire-type" = "u64",
                "x-merry-output-required" = true
            )
        )]
        latest_sequence: u64,
        record: Box<TrajectoryRecord>,
    },
    /// Prompt evidence was inserted or its dynamic context count advanced.
    PromptUpdated {
        #[serde(with = "u64_as_string")]
        #[schemars(
            with = "String",
            extend(
                "x-merry-wire-type" = "u64",
                "x-merry-output-required" = true
            )
        )]
        revision: u64,
        #[serde(with = "u64_as_string")]
        #[schemars(
            with = "String",
            extend(
                "x-merry-wire-type" = "u64",
                "x-merry-output-required" = true
            )
        )]
        latest_sequence: u64,
        prompt: TrajectoryPromptSnapshot,
    },
    /// The runtime session has closed.
    SessionClosed {
        #[serde(with = "u64_as_string")]
        #[schemars(
            with = "String",
            extend(
                "x-merry-wire-type" = "u64",
                "x-merry-output-required" = true
            )
        )]
        revision: u64,
        #[serde(with = "u64_as_string")]
        #[schemars(
            with = "String",
            extend(
                "x-merry-wire-type" = "u64",
                "x-merry-output-required" = true
            )
        )]
        latest_sequence: u64,
    },
}

mod serialization;

#[cfg(test)]
#[path = "trajectory_tests.rs"]
mod tests;
