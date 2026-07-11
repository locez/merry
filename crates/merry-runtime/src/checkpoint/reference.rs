use super::{CheckpointError, CheckpointId, CheckpointRefId, CheckpointSequenceRange};
use merry_core::EvidenceRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointSourceKind {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolResult,
    ArtifactRange,
}

impl CheckpointSourceKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserMessage => "user_message",
            Self::AssistantMessage => "assistant_message",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::ArtifactRange => "artifact_range",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum PersistedCheckpointRef {
    Evidence(PersistedEvidenceCheckpointRef),
    LegacyExcerpt(PersistedLegacyExcerptCheckpointRef),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedEvidenceCheckpointRef {
    id: String,
    source_kind: CheckpointSourceKind,
    sequence_start: u64,
    sequence_end: u64,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedLegacyExcerptCheckpointRef {
    id: String,
    source_kind: String,
    source_id: String,
    sequence_start: u64,
    sequence_end: u64,
    locator: String,
    excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRef {
    id: CheckpointRefId,
    source_kind: CheckpointSourceKind,
    sequence_range: CheckpointSequenceRange,
    evidence: EvidenceRef,
}

impl CheckpointRef {
    #[must_use]
    pub fn new(
        id: CheckpointRefId,
        source_kind: CheckpointSourceKind,
        sequence_range: CheckpointSequenceRange,
        evidence: EvidenceRef,
    ) -> Self {
        Self {
            id,
            source_kind,
            sequence_range,
            evidence,
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

    #[must_use]
    pub fn evidence(&self) -> &EvidenceRef {
        &self.evidence
    }
}

impl From<&CheckpointRef> for PersistedCheckpointRef {
    fn from(value: &CheckpointRef) -> Self {
        Self::Evidence(PersistedEvidenceCheckpointRef {
            id: value.id().as_str().to_owned(),
            source_kind: value.source_kind(),
            sequence_start: value.sequence_range().start(),
            sequence_end: value.sequence_range().end(),
            evidence: value.evidence().clone(),
        })
    }
}

impl TryFrom<PersistedCheckpointRef> for CheckpointRef {
    type Error = CheckpointError;

    fn try_from(value: PersistedCheckpointRef) -> Result<Self, Self::Error> {
        match value {
            PersistedCheckpointRef::Evidence(value) => Ok(Self::new(
                CheckpointRefId::new(&value.id)?,
                value.source_kind,
                CheckpointSequenceRange::new(value.sequence_start, value.sequence_end)?,
                value.evidence,
            )),
            PersistedCheckpointRef::LegacyExcerpt(value) => {
                let PersistedLegacyExcerptCheckpointRef {
                    id,
                    source_kind,
                    source_id,
                    sequence_start,
                    sequence_end,
                    locator,
                    excerpt,
                } = value;
                drop((
                    source_kind,
                    source_id,
                    sequence_start,
                    sequence_end,
                    locator,
                    excerpt,
                ));
                Err(CheckpointError::LegacyExcerptRefUnsupported { ref_id: id })
            }
        }
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

    pub(super) fn get(&self, id: &CheckpointRefId) -> Option<&CheckpointRef> {
        self.refs.iter().find(|item| item.id() == id)
    }

    pub(super) fn filtered_for_checkpoint(
        &self,
        checkpoint_id: CheckpointId,
        used_refs: &BTreeSet<CheckpointRefId>,
    ) -> Result<Self, CheckpointError> {
        Self::new(
            checkpoint_id,
            self.refs
                .iter()
                .filter(|item| used_refs.contains(item.id()))
                .cloned()
                .collect(),
        )
    }
}
