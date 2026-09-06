//! Serializable trajectory records and their typed payloads.

use crate::{
    ArtifactRef, ErrorInfo, ToolCallArguments, ToolCallId, ToolName, TrajectoryRecordId,
    trajectory::{
        TrajectoryTurnId,
        serialization::{optional_turn_id_as_string, optional_u64_as_string, u64_as_string},
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
