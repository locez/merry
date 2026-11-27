//! Wire schema for append-only `journal.jsonl` records.
//!
//! Each line is a self-contained JSON object terminated by `\n`. The first line
//! is a `Header`; subsequent lines are incremental `Commit` deltas.

use super::super::{ModelTurnId, ModelTurnStatus, transcript::UserInputOrigin};
use crate::{
    SessionStoreError, plan::PersistedPlanModeState, token_estimate::RequestTokenCalibration,
};
use merry_core::{
    ErrorInfo, PendingToolCall, SessionId, SessionUsage, TodoItem, ToolCallId,
    ToolCallResultStatus, ToolOutput,
};
use serde::{Deserialize, Serialize};

pub(crate) const CURRENT_SESSION_JOURNAL_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum JournalRecord {
    Header {
        format_version: u32,
        session_id: SessionId,
    },
    Commit {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        items: Vec<JournalItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compaction: Option<JournalCompaction>,
        #[serde(default, skip_serializing_if = "JournalStateDelta::is_empty")]
        state: Box<JournalStateDelta>,
    },
}

/// Explicit set/clear delta for optional side-state fields.
///
/// Absence (`None` on the enclosing struct) means unchanged from previous commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "value", rename_all = "snake_case")]
pub(super) enum FieldPatch<T> {
    Set(T),
    Clear,
}

impl<T> FieldPatch<T> {
    pub(super) fn apply(self, slot: &mut Option<T>) {
        match self {
            Self::Set(value) => *slot = Some(value),
            Self::Clear => *slot = None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum JournalUserInputOrigin {
    ExternalUser,
    RuntimeControl,
}

impl From<UserInputOrigin> for JournalUserInputOrigin {
    fn from(value: UserInputOrigin) -> Self {
        match value {
            UserInputOrigin::ExternalUser => Self::ExternalUser,
            UserInputOrigin::RuntimeControl => Self::RuntimeControl,
        }
    }
}

impl From<JournalUserInputOrigin> for UserInputOrigin {
    fn from(value: JournalUserInputOrigin) -> Self {
        match value {
            JournalUserInputOrigin::ExternalUser => Self::ExternalUser,
            JournalUserInputOrigin::RuntimeControl => Self::RuntimeControl,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum JournalItem {
    UserMessage {
        id: u64,
        model_turn_id: ModelTurnId,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<JournalImageRef>,
        origin: JournalUserInputOrigin,
    },
    AssistantText {
        id: u64,
        model_turn_id: ModelTurnId,
        text: String,
    },
    AssistantReasoning {
        id: u64,
        model_turn_id: ModelTurnId,
        text: String,
    },
    ToolCall {
        id: u64,
        model_turn_id: ModelTurnId,
        call: PendingToolCall,
    },
    ToolResult {
        id: u64,
        model_turn_id: ModelTurnId,
        call_id: ToolCallId,
        status: ToolCallResultStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diagnostic: Option<ErrorInfo>,
        full_output: ToolOutput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_output: Option<ToolOutput>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<JournalImageRef>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        final_output: bool,
    },
}

impl JournalItem {
    pub(super) const fn id(&self) -> u64 {
        match self {
            Self::UserMessage { id, .. }
            | Self::AssistantText { id, .. }
            | Self::AssistantReasoning { id, .. }
            | Self::ToolCall { id, .. }
            | Self::ToolResult { id, .. } => *id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalImageRef {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) width: u32,
    pub(super) height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalCompaction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) compacted_through: Option<ModelTurnId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) archived_tool_call_ids: Vec<ToolCallId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalTaskAnchor {
    pub(super) objective: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum JournalContextEntry {
    Summary { id: String, text: String },
}

/// One model turn entry in a commit delta.
///
/// Map keys cannot round-trip inside an internally tagged journal line:
/// serde stores those keys as strings, then rejects them as `u64`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalModelTurn {
    pub(super) id: ModelTurnId,
    pub(super) status: ModelTurnStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) started_at_epoch_secs: Option<u64>,
}

/// Incremental delta for session side-state that cannot be derived from
/// transcript bodies alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalStateDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) next_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) session_started: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) usage: Option<FieldPatch<SessionUsage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) request_token_calibration: Option<FieldPatch<RequestTokenCalibration>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) task_anchor: Option<FieldPatch<JournalTaskAnchor>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) plan_mode: Option<FieldPatch<PersistedPlanModeState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) todos: Option<Vec<TodoItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) context_entries: Option<Vec<JournalContextEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) next_item_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) model_turns: Vec<JournalModelTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) next_model_turn_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) topic: Option<FieldPatch<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) subagents: Option<Vec<crate::subagent::PersistedSubagentRecord>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pruned_through: Option<ModelTurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pruned_external_turns: Option<u32>,
}

impl JournalStateDelta {
    pub(super) fn is_empty(&self) -> bool {
        self.next_sequence.is_none()
            && self.session_started.is_none()
            && self.usage.is_none()
            && self.request_token_calibration.is_none()
            && self.task_anchor.is_none()
            && self.plan_mode.is_none()
            && self.todos.is_none()
            && self.context_entries.is_none()
            && self.next_item_id.is_none()
            && self.model_turns.is_empty()
            && self.next_model_turn_id.is_none()
            && self.topic.is_none()
            && self.subagents.is_none()
            && self.pruned_through.is_none()
            && self.pruned_external_turns.is_none()
    }
}

pub(super) fn encode_record_line(record: &JournalRecord) -> Result<Vec<u8>, SessionStoreError> {
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    Ok(line)
}

pub(super) fn encode_header_line(session_id: &SessionId) -> Result<Vec<u8>, SessionStoreError> {
    encode_record_line(&JournalRecord::Header {
        format_version: CURRENT_SESSION_JOURNAL_FORMAT_VERSION,
        session_id: session_id.clone(),
    })
}

/// Splits `bytes` into the prefix ending at the last `\n` and, if any trailing
/// incomplete bytes follow it, the byte length to truncate back to.
pub(super) fn complete_journal_prefix(bytes: &[u8]) -> (&[u8], Option<u64>) {
    match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => {
            let prefix_len = index + 1;
            let trailing = &bytes[prefix_len..];
            let truncate_to = (!trailing.is_empty())
                .then(|| u64::try_from(prefix_len).ok())
                .flatten();
            (&bytes[..prefix_len], truncate_to)
        }
        None => (&[], None),
    }
}

pub(super) fn complete_journal_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let (complete, _) = complete_journal_prefix(bytes);
    complete.split(|byte| *byte == b'\n')
}

/// Collects all content-addressed image ids referenced by any commit line in `bytes`.
pub(super) fn collect_journal_image_ids(bytes: &[u8]) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    for line in complete_journal_lines(bytes) {
        if line.is_empty() {
            continue;
        }
        let Ok(JournalRecord::Commit { items, .. }) = serde_json::from_slice::<JournalRecord>(line)
        else {
            continue;
        };
        for item in items {
            let images = match item {
                JournalItem::UserMessage { images, .. }
                | JournalItem::ToolResult { images, .. } => images,
                JournalItem::AssistantText { .. }
                | JournalItem::AssistantReasoning { .. }
                | JournalItem::ToolCall { .. } => continue,
            };
            for image in images {
                ids.insert(image.id);
            }
        }
    }
    ids
}

/// Scans a journal byte buffer for a recorded tool result matching `call_id`.
pub(super) fn find_tool_output_in_journal_bytes(
    bytes: &[u8],
    call_id: &ToolCallId,
) -> Option<ToolOutput> {
    let mut found = None;
    for line in complete_journal_lines(bytes) {
        if line.is_empty() {
            continue;
        }
        let Ok(JournalRecord::Commit { items, .. }) = serde_json::from_slice::<JournalRecord>(line)
        else {
            continue;
        };
        for item in items {
            if let JournalItem::ToolResult {
                call_id: candidate,
                full_output,
                ..
            } = item
                && &candidate == call_id
            {
                found = Some(full_output);
            }
        }
    }
    found
}
