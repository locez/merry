use super::{CheckpointError, CheckpointId, CheckpointRefId, CheckpointSequenceRange};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointSourceKind {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolResult,
}

impl CheckpointSourceKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserMessage => "user_message",
            Self::AssistantMessage => "assistant_message",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRef {
    id: CheckpointRefId,
    source_kind: CheckpointSourceKind,
    sequence_range: CheckpointSequenceRange,
}

impl CheckpointRef {
    #[must_use]
    pub fn new(
        id: CheckpointRefId,
        source_kind: CheckpointSourceKind,
        sequence_range: CheckpointSequenceRange,
    ) -> Self {
        Self {
            id,
            source_kind,
            sequence_range,
        }
    }

    #[must_use]
    pub fn id(&self) -> &CheckpointRefId {
        &self.id
    }

    #[must_use]
    pub fn source_kind(&self) -> CheckpointSourceKind {
        self.source_kind
    }

    #[must_use]
    pub fn sequence_range(&self) -> CheckpointSequenceRange {
        self.sequence_range
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRefManifest {
    checkpoint_id: CheckpointId,
    refs: Vec<CheckpointRef>,
}

impl CheckpointRefManifest {
    pub fn new(
        checkpoint_id: CheckpointId,
        refs: Vec<CheckpointRef>,
    ) -> Result<Self, CheckpointError> {
        let mut seen = BTreeSet::new();
        for item in &refs {
            if !seen.insert(item.id().clone()) {
                return Err(CheckpointError::DuplicateRef {
                    ref_id: item.id().as_str().to_owned(),
                });
            }
        }

        Ok(Self {
            checkpoint_id,
            refs,
        })
    }

    #[must_use]
    pub fn checkpoint_id(&self) -> &CheckpointId {
        &self.checkpoint_id
    }

    #[must_use]
    pub fn refs(&self) -> &[CheckpointRef] {
        &self.refs
    }
}
