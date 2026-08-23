//! Provider-neutral trajectory read-model contracts.
//!
//! The `x-merry-*` schema extensions describe normalized client types and
//! serialized-field presence for generators consuming this contract.

use crate::{
    ArtifactRef, CoreError, ErrorInfo, SessionId, ToolCallArguments, ToolCallId, ToolName,
    ToolSpec, TrajectoryRecordId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WireU64 {
    String(String),
    Number(u64),
}

fn deserialize_wire_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = WireU64::deserialize(deserializer)?;
    match value {
        WireU64::String(value) => value.parse().map_err(de::Error::custom),
        WireU64::Number(value) => Ok(value),
    }
}

mod u64_as_string {
    use super::deserialize_wire_u64;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_wire_u64(deserializer)
    }
}

mod optional_u64_as_string {
    use super::WireU64;
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&value.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<WireU64>::deserialize(deserializer)?;
        value
            .map(|value| match value {
                WireU64::String(value) => value.parse().map_err(de::Error::custom),
                WireU64::Number(value) => Ok(value),
            })
            .transpose()
    }
}

mod optional_turn_id_as_string {
    use super::{TrajectoryTurnId, WireU64};
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S>(value: &Option<TrajectoryTurnId>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&value.value().to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<TrajectoryTurnId>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<WireU64>::deserialize(deserializer)?;
        value
            .map(|value| {
                let value = match value {
                    WireU64::String(value) => value.parse().map_err(de::Error::custom)?,
                    WireU64::Number(value) => value,
                };
                TrajectoryTurnId::new(value).map_err(de::Error::custom)
            })
            .transpose()
    }
}

/// Semantic lane used to render one trajectory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryLane {
    /// User input and other session-owned input.
    Input,
    /// Model output and model lifecycle activity.
    Model,
    /// Tool calls and tool results.
    Tools,
    /// Runtime lifecycle and diagnostic activity.
    System,
}

/// Stable semantic kind for a trajectory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryRecordKind {
    /// User-provided input.
    UserInput,
    /// Assistant/model output.
    AssistantMessage,
    /// A tool call requested by the model.
    ToolCall,
    /// A legacy standalone tool result retained for persisted snapshot compatibility.
    ///
    /// New runtime projections attach tool output to the corresponding `ToolCall`
    /// record instead of emitting this kind separately.
    ToolResult,
    /// Context compaction or a compacted checkpoint.
    Compaction,
    /// A runtime lifecycle or diagnostic record.
    Lifecycle,
}

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
        let value = deserialize_wire_u64(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// Current lifecycle status of a trajectory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryRecordStatus {
    /// The runtime has accepted the record but work has not started.
    Pending,
    /// Work represented by the record is in progress.
    Running,
    /// Work completed successfully.
    Succeeded,
    /// Work completed with a failure diagnostic.
    Failed,
    /// Work stopped because cancellation was requested.
    Cancelled,
    /// A lifecycle record is complete without success/failure semantics.
    Completed,
}

/// Kind of exact textual payload retained by the trajectory inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryPayloadKind {
    /// UTF-8 tool output or message text.
    Text,
    /// JSON text returned by a tool.
    Json,
}

/// Complete payload projection used by the Web inspector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryPayload {
    kind: TrajectoryPayloadKind,
    content: String,
    truncated: bool,
}

impl TrajectoryPayload {
    /// Creates a payload projection.
    #[must_use]
    pub fn new(kind: TrajectoryPayloadKind, content: String, truncated: bool) -> Self {
        Self {
            kind,
            content,
            truncated,
        }
    }

    /// Returns the payload kind.
    #[must_use]
    pub fn kind(&self) -> TrajectoryPayloadKind {
        self.kind
    }

    /// Borrows the complete payload content.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns the legacy truncation marker.
    ///
    /// New runtime projections retain complete content and return `false`.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Tool-specific evidence attached to one trajectory record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryToolDetails {
    #[schemars(extend("x-merry-output-required" = true))]
    tool_name: Option<ToolName>,
    arguments: ToolCallArguments,
    #[serde(default)]
    #[schemars(extend("x-merry-output-required" = true))]
    arguments_json: String,
    #[schemars(extend("x-merry-output-required" = true))]
    output: Option<TrajectoryPayload>,
}

impl TrajectoryToolDetails {
    /// Creates tool evidence with a registered tool name and arguments.
    #[must_use]
    pub fn new(tool_name: Option<ToolName>, arguments: ToolCallArguments) -> Self {
        let arguments_json = match serde_json::to_string(&arguments) {
            Ok(json) => json,
            Err(_) => "{}".to_owned(),
        };
        Self {
            tool_name,
            arguments,
            arguments_json,
            output: None,
        }
    }

    /// Borrows the registered tool name, when available.
    #[must_use]
    pub fn tool_name(&self) -> Option<&ToolName> {
        self.tool_name.as_ref()
    }

    /// Borrows the exact normalized call arguments.
    #[must_use]
    pub fn arguments(&self) -> &ToolCallArguments {
        &self.arguments
    }

    /// Borrows the exact serialized argument text for lossless inspection.
    #[must_use]
    pub fn arguments_json(&self) -> &str {
        &self.arguments_json
    }

    /// Borrows the complete result payload, when available.
    #[must_use]
    pub fn output(&self) -> Option<&TrajectoryPayload> {
        self.output.as_ref()
    }

    /// Sets or clears the result payload.
    pub fn set_output(&mut self, output: Option<TrajectoryPayload>) {
        self.output = output;
    }
}

/// Detailed evidence for a trajectory record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TrajectoryRecordDetails {
    /// No expanded content is available for this record.
    None,
    /// Complete user/assistant text.
    Message {
        /// Content retained for inspection.
        content: String,
        /// Legacy truncation marker retained for snapshot compatibility.
        truncated: bool,
    },
    /// A model tool call and its optional result.
    Tool {
        /// Tool call evidence.
        tool: TrajectoryToolDetails,
    },
}

/// One normalized record in the trajectory read model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryRecord {
    id: TrajectoryRecordId,
    lane: TrajectoryLane,
    kind: TrajectoryRecordKind,
    label: String,
    #[schemars(extend("x-merry-output-required" = true))]
    summary: Option<String>,
    status: TrajectoryRecordStatus,
    #[serde(with = "u64_as_string")]
    #[schemars(
        with = "String",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    start_sequence: u64,
    sequence_order: u32,
    #[serde(with = "optional_turn_id_as_string")]
    #[schemars(
        with = "Option<String>",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    turn_id: Option<TrajectoryTurnId>,
    #[serde(with = "optional_u64_as_string")]
    #[schemars(
        with = "Option<String>",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    end_sequence: Option<u64>,
    #[schemars(extend("x-merry-output-required" = true))]
    parent_id: Option<TrajectoryRecordId>,
    #[schemars(extend("x-merry-output-required" = true))]
    tool_call_id: Option<ToolCallId>,
    artifacts: Vec<ArtifactRef>,
    #[schemars(extend("x-merry-output-required" = true))]
    diagnostic: Option<ErrorInfo>,
    #[serde(with = "optional_u64_as_string")]
    #[schemars(
        with = "Option<String>",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    started_at_ms: Option<u64>,
    #[serde(with = "optional_u64_as_string")]
    #[schemars(
        with = "Option<String>",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    finished_at_ms: Option<u64>,
    details: TrajectoryRecordDetails,
}

impl TrajectoryRecord {
    /// Creates a trajectory record with sequence-based ordering.
    #[must_use]
    pub fn new(
        id: TrajectoryRecordId,
        lane: TrajectoryLane,
        kind: TrajectoryRecordKind,
        label: String,
        status: TrajectoryRecordStatus,
        start_sequence: u64,
    ) -> Self {
        Self {
            id,
            lane,
            kind,
            label,
            summary: None,
            status,
            start_sequence,
            sequence_order: 0,
            turn_id: None,
            end_sequence: None,
            parent_id: None,
            tool_call_id: None,
            artifacts: Vec::new(),
            diagnostic: None,
            started_at_ms: None,
            finished_at_ms: None,
            details: TrajectoryRecordDetails::None,
        }
    }

    /// Borrows the stable record identifier.
    #[must_use]
    pub fn id(&self) -> &TrajectoryRecordId {
        &self.id
    }

    /// Returns the semantic lane.
    #[must_use]
    pub fn lane(&self) -> TrajectoryLane {
        self.lane
    }

    /// Returns the semantic record kind.
    #[must_use]
    pub fn kind(&self) -> TrajectoryRecordKind {
        self.kind
    }

    /// Borrows the short display label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Borrows the optional display summary.
    #[must_use]
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// Returns the current lifecycle status.
    #[must_use]
    pub fn status(&self) -> TrajectoryRecordStatus {
        self.status
    }

    /// Returns the first journal sequence represented by this record.
    #[must_use]
    pub fn start_sequence(&self) -> u64 {
        self.start_sequence
    }

    /// Returns the deterministic order among records sharing a sequence.
    #[must_use]
    pub fn sequence_order(&self) -> u32 {
        self.sequence_order
    }

    /// Returns the logical conversation turn, when the record belongs to one.
    #[must_use]
    pub fn turn_id(&self) -> Option<TrajectoryTurnId> {
        self.turn_id
    }

    /// Returns the final journal sequence when the record is complete.
    #[must_use]
    pub fn end_sequence(&self) -> Option<u64> {
        self.end_sequence
    }

    /// Borrows the optional parent record identifier.
    #[must_use]
    pub fn parent_id(&self) -> Option<&TrajectoryRecordId> {
        self.parent_id.as_ref()
    }

    /// Borrows the optional provider-originated tool call identifier.
    #[must_use]
    pub fn tool_call_id(&self) -> Option<&ToolCallId> {
        self.tool_call_id.as_ref()
    }

    /// Borrows exact artifact references attached to this record.
    #[must_use]
    pub fn artifacts(&self) -> &[ArtifactRef] {
        &self.artifacts
    }

    /// Borrows the optional failure diagnostic.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&ErrorInfo> {
        self.diagnostic.as_ref()
    }

    /// Returns the optional real start timestamp. Missing means timing is unknown.
    #[must_use]
    pub fn started_at_ms(&self) -> Option<u64> {
        self.started_at_ms
    }

    /// Returns the optional real finish timestamp. Missing means timing is unknown.
    #[must_use]
    pub fn finished_at_ms(&self) -> Option<u64> {
        self.finished_at_ms
    }

    /// Borrows expanded evidence for this record.
    #[must_use]
    pub fn details(&self) -> &TrajectoryRecordDetails {
        &self.details
    }

    /// Sets a bounded display summary.
    pub fn set_summary(&mut self, summary: Option<String>) {
        self.summary = summary;
    }

    /// Replaces the short display label.
    pub fn set_label(&mut self, label: String) {
        self.label = label;
    }

    /// Stores complete message content for inspection.
    pub fn set_message_details(&mut self, content: String, truncated: bool) {
        self.details = TrajectoryRecordDetails::Message { content, truncated };
    }

    /// Stores a normalized tool call and its registered tool name.
    pub fn set_tool_details(&mut self, tool_name: Option<ToolName>, arguments: ToolCallArguments) {
        self.details = TrajectoryRecordDetails::Tool {
            tool: TrajectoryToolDetails::new(tool_name, arguments),
        };
    }

    /// Sets the deterministic order among records sharing a source sequence.
    pub fn set_sequence_order(&mut self, sequence_order: u32) {
        self.sequence_order = sequence_order;
    }

    /// Reassigns the record to its durable source sequence during replay.
    pub fn set_start_sequence(&mut self, sequence: u64) {
        self.start_sequence = sequence;
    }

    /// Associates the record with a logical conversation turn.
    pub fn set_turn_id(&mut self, turn_id: Option<TrajectoryTurnId>) {
        self.turn_id = turn_id;
    }

    /// Updates the output inside existing tool details.
    pub fn set_tool_output(&mut self, output: Option<TrajectoryPayload>) {
        if let TrajectoryRecordDetails::Tool { tool } = &mut self.details {
            tool.set_output(output);
        }
    }

    /// Marks the record with a new status and ending sequence.
    pub fn finish(&mut self, status: TrajectoryRecordStatus, end_sequence: u64) {
        self.status = status;
        self.end_sequence = Some(end_sequence);
    }

    /// Associates the record with a parent and/or tool call.
    pub fn set_relationship(
        &mut self,
        parent_id: Option<TrajectoryRecordId>,
        tool_call_id: Option<ToolCallId>,
    ) {
        self.parent_id = parent_id;
        self.tool_call_id = tool_call_id;
    }

    /// Adds an exact artifact reference if it is not already present.
    pub fn add_artifact(&mut self, artifact: ArtifactRef) {
        if !self
            .artifacts
            .iter()
            .any(|current| current.id() == artifact.id())
        {
            self.artifacts.push(artifact);
        }
    }

    /// Stores a diagnostic and marks the record as failed.
    pub fn fail(&mut self, diagnostic: ErrorInfo, end_sequence: u64) {
        self.diagnostic = Some(diagnostic);
        self.finish(TrajectoryRecordStatus::Failed, end_sequence);
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

/// A stable provider-visible prompt block retained once per session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryPromptBlock {
    id: TrajectoryRecordId,
    sequence_order: u32,
    content: String,
    truncated: bool,
}

impl TrajectoryPromptBlock {
    /// Creates a prompt block with its stable identity and complete content.
    #[must_use]
    pub fn new(
        id: TrajectoryRecordId,
        sequence_order: u32,
        content: String,
        truncated: bool,
    ) -> Self {
        Self {
            id,
            sequence_order,
            content,
            truncated,
        }
    }

    /// Borrows the stable block identifier.
    #[must_use]
    pub fn id(&self) -> &TrajectoryRecordId {
        &self.id
    }

    /// Returns the provider-visible order of this block.
    #[must_use]
    pub fn sequence_order(&self) -> u32 {
        self.sequence_order
    }

    /// Borrows the complete prompt content.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns the legacy truncation marker.
    ///
    /// New runtime projections retain complete content and return `false`.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Prompt evidence kept separately from conversation records.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryPromptSnapshot {
    stable_blocks: Vec<TrajectoryPromptBlock>,
    #[serde(with = "u64_as_string")]
    #[schemars(
        with = "String",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    dynamic_context_count: u64,
    #[serde(with = "optional_u64_as_string")]
    #[schemars(
        with = "Option<String>",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    latest_dynamic_sequence: Option<u64>,
}

impl TrajectoryPromptSnapshot {
    /// Borrows stable provider-prefix blocks in request order.
    #[must_use]
    pub fn stable_blocks(&self) -> &[TrajectoryPromptBlock] {
        &self.stable_blocks
    }

    /// Returns how many dynamic context messages were observed.
    #[must_use]
    pub fn dynamic_context_count(&self) -> u64 {
        self.dynamic_context_count
    }

    /// Returns the latest request sequence that carried dynamic context.
    #[must_use]
    pub fn latest_dynamic_sequence(&self) -> Option<u64> {
        self.latest_dynamic_sequence
    }

    /// Inserts or replaces a stable block and reports whether it changed.
    pub fn upsert_stable_block(&mut self, block: TrajectoryPromptBlock) -> bool {
        if let Some(existing) = self
            .stable_blocks
            .iter_mut()
            .find(|item| item.id() == block.id())
        {
            if *existing == block {
                return false;
            }
            *existing = block;
        } else {
            self.stable_blocks.push(block);
        }
        self.stable_blocks
            .sort_by_key(|item| (item.sequence_order(), item.id().as_str().to_owned()));
        true
    }

    /// Records dynamic context messages from one compiled request.
    pub fn add_dynamic_context(&mut self, count: u64, sequence: u64) {
        self.dynamic_context_count = self.dynamic_context_count.saturating_add(count);
        if count > 0 {
            self.latest_dynamic_sequence = Some(
                self.latest_dynamic_sequence
                    .map_or(sequence, |current| current.max(sequence)),
            );
        }
    }
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

#[cfg(test)]
#[path = "trajectory_tests.rs"]
mod tests;
