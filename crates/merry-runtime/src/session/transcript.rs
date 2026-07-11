use super::{
    SessionState,
    model_turns::{ModelTurnId, ModelTurnStatus, invalid_turn_transition},
};
use crate::{
    RuntimeError,
    artifact::{ArtifactContent, ArtifactError, ArtifactRegistry},
    compaction::CompactionError,
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, PendingToolCall, ToolCallId, ToolCallResult,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolCallPromptProjection {
    Full,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolResultPromptProjection {
    Full,
    ArtifactNotice,
    Hidden,
}

pub(super) fn archived_tool_result_notice_json(
    item_id: TranscriptItemId,
    status: merry_core::ToolCallResultStatus,
    artifact_id: &ArtifactId,
) -> String {
    serde_json::json!({
        "merry_archived": true,
        "status": match status {
            merry_core::ToolCallResultStatus::Succeeded => "succeeded",
            merry_core::ToolCallResultStatus::Failed => "failed",
        },
        "artifact_id": artifact_id.as_str(),
        "ref": format!("h{}", item_id.as_u64()),
    })
    .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TranscriptItemId(u64);

impl TranscriptItemId {
    #[must_use]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub(crate) const fn as_u64(self) -> u64 {
        self.0
    }

    fn checked_next(self) -> Result<Self, RuntimeError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(RuntimeError::TranscriptItemIdExhausted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserInputOrigin {
    ExternalUser,
    #[allow(dead_code)]
    RuntimeControl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TranscriptItem {
    UserMessage {
        id: TranscriptItemId,
        model_turn_id: ModelTurnId,
        artifact_id: ArtifactId,
        origin: UserInputOrigin,
    },
    AssistantText {
        id: TranscriptItemId,
        model_turn_id: ModelTurnId,
        artifact_id: ArtifactId,
    },
    ToolCall {
        id: TranscriptItemId,
        model_turn_id: ModelTurnId,
        call: PendingToolCall,
        prompt_projection: ToolCallPromptProjection,
    },
    ToolResult {
        id: TranscriptItemId,
        model_turn_id: ModelTurnId,
        call_id: ToolCallId,
        result: ToolCallResult,
        artifact_id: ArtifactId,
        prompt_projection: ToolResultPromptProjection,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedTranscript {
    pub(crate) items: Vec<PersistedTranscriptItem>,
    pub(crate) next_id: u64,
    pub(crate) model_turns: BTreeMap<ModelTurnId, ModelTurnStatus>,
    pub(crate) next_model_turn_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PersistedTranscriptItem {
    UserMessage {
        id: u64,
        model_turn_id: ModelTurnId,
        artifact_id: ArtifactId,
        origin: PersistedUserInputOrigin,
    },
    AssistantText {
        id: u64,
        model_turn_id: ModelTurnId,
        artifact_id: ArtifactId,
    },
    ToolCall {
        id: u64,
        model_turn_id: ModelTurnId,
        call: PendingToolCall,
        prompt_projection: ToolCallPromptProjection,
    },
    ToolResult {
        id: u64,
        model_turn_id: ModelTurnId,
        call_id: ToolCallId,
        result: ToolCallResult,
        artifact_id: ArtifactId,
        prompt_projection: ToolResultPromptProjection,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedTranscriptV1 {
    pub(crate) items: Vec<PersistedTranscriptItemV1>,
    pub(crate) next_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PersistedTranscriptItemV1 {
    UserMessage {
        id: u64,
        text: String,
        origin: PersistedUserInputOrigin,
    },
    AssistantText {
        id: u64,
        artifact_id: ArtifactId,
    },
    ToolCall {
        id: u64,
        call: PendingToolCall,
    },
    ToolResult {
        id: u64,
        call_id: ToolCallId,
        result: ToolCallResult,
        artifact_id: ArtifactId,
    },
}

#[derive(Debug)]
pub(crate) enum TranscriptV1MigrationError {
    Artifact(ArtifactError),
    Transcript(RuntimeError),
}

impl From<ArtifactError> for TranscriptV1MigrationError {
    fn from(value: ArtifactError) -> Self {
        Self::Artifact(value)
    }
}

impl From<RuntimeError> for TranscriptV1MigrationError {
    fn from(value: RuntimeError) -> Self {
        Self::Transcript(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PersistedUserInputOrigin {
    ExternalUser,
    RuntimeControl,
}

impl TranscriptItem {
    #[must_use]
    pub(crate) const fn id(&self) -> TranscriptItemId {
        match self {
            Self::UserMessage { id, .. }
            | Self::AssistantText { id, .. }
            | Self::ToolCall { id, .. }
            | Self::ToolResult { id, .. } => *id,
        }
    }

    #[must_use]
    pub(crate) const fn model_turn_id(&self) -> ModelTurnId {
        match self {
            Self::UserMessage { model_turn_id, .. }
            | Self::AssistantText { model_turn_id, .. }
            | Self::ToolCall { model_turn_id, .. }
            | Self::ToolResult { model_turn_id, .. } => *model_turn_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TranscriptItemSnapshot {
    UserMessage {
        text: String,
        origin: UserInputOrigin,
    },
    AssistantText {
        text: String,
    },
    ToolCall {
        call: PendingToolCall,
    },
    ToolResult {
        call_id: ToolCallId,
        result: ToolCallResult,
        content: ArtifactContent,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Transcript {
    pub(super) items: Vec<TranscriptItem>,
    pub(super) next_id: TranscriptItemId,
    pub(super) model_turns: BTreeMap<ModelTurnId, ModelTurnStatus>,
    pub(super) next_model_turn_id: ModelTurnId,
}

impl Transcript {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            items: Vec::new(),
            next_id: TranscriptItemId::new(0),
            model_turns: BTreeMap::new(),
            next_model_turn_id: ModelTurnId::new(1),
        }
    }

    #[must_use]
    pub(crate) fn items(&self) -> &[TranscriptItem] {
        &self.items
    }

    #[must_use]
    pub(crate) const fn next_id(&self) -> TranscriptItemId {
        self.next_id
    }

    #[must_use]
    pub(crate) fn model_turn_id_for_tool_call(&self, call_id: &ToolCallId) -> Option<ModelTurnId> {
        self.items.iter().find_map(|item| match item {
            TranscriptItem::ToolCall {
                model_turn_id,
                call,
                ..
            } if call.id() == call_id => Some(*model_turn_id),
            _ => None,
        })
    }

    pub(crate) fn push_user_message(
        &mut self,
        model_turn_id: ModelTurnId,
        artifact_id: ArtifactId,
        origin: UserInputOrigin,
    ) -> Result<TranscriptItemId, RuntimeError> {
        self.ensure_model_turn_in_progress(model_turn_id)?;
        let id = self.allocate_id()?;
        self.items.push(TranscriptItem::UserMessage {
            id,
            model_turn_id,
            artifact_id,
            origin,
        });
        Ok(id)
    }

    pub(crate) fn push_assistant_text(
        &mut self,
        model_turn_id: ModelTurnId,
        artifact_id: ArtifactId,
    ) -> Result<TranscriptItemId, RuntimeError> {
        self.ensure_model_turn_in_progress(model_turn_id)?;
        let id = self.allocate_id()?;
        self.items.push(TranscriptItem::AssistantText {
            id,
            model_turn_id,
            artifact_id,
        });
        Ok(id)
    }

    pub(crate) fn push_tool_call(
        &mut self,
        model_turn_id: ModelTurnId,
        call: PendingToolCall,
    ) -> Result<TranscriptItemId, RuntimeError> {
        self.ensure_model_turn_in_progress(model_turn_id)?;
        let id = self.allocate_id()?;
        self.items.push(TranscriptItem::ToolCall {
            id,
            model_turn_id,
            call,
            prompt_projection: ToolCallPromptProjection::Full,
        });
        Ok(id)
    }

    pub(crate) fn push_tool_result(
        &mut self,
        call_id: ToolCallId,
        result: ToolCallResult,
        artifact_id: ArtifactId,
        prompt_projection: ToolResultPromptProjection,
    ) -> Result<TranscriptItemId, RuntimeError> {
        let model_turn_id = self.model_turn_id_for_tool_call(&call_id).ok_or_else(|| {
            RuntimeError::TranscriptToolCallMissing {
                call_id: call_id.clone(),
            }
        })?;
        if self.status(model_turn_id) != Some(ModelTurnStatus::AwaitingToolResults) {
            return Err(invalid_turn_transition(model_turn_id, "record tool result"));
        }
        let id = self.allocate_id()?;
        self.items.push(TranscriptItem::ToolResult {
            id,
            model_turn_id,
            call_id,
            result,
            artifact_id,
            prompt_projection,
        });
        if self.all_tool_calls_resolved(model_turn_id) {
            *self.model_turn_status_mut(model_turn_id)? = ModelTurnStatus::Completed;
        }
        Ok(id)
    }

    pub(crate) fn hide_tool_call(&mut self, call_id: &ToolCallId) -> Result<(), RuntimeError> {
        let Some(prompt_projection) = self.items.iter_mut().find_map(|item| match item {
            TranscriptItem::ToolCall {
                call,
                prompt_projection,
                ..
            } if call.id() == call_id => Some(prompt_projection),
            _ => None,
        }) else {
            return Err(RuntimeError::TranscriptToolCallMissing {
                call_id: call_id.clone(),
            });
        };
        *prompt_projection = ToolCallPromptProjection::Hidden;
        Ok(())
    }

    pub(crate) fn archive_tool_results(
        &mut self,
        call_ids: &BTreeSet<ToolCallId>,
    ) -> Result<(), RuntimeError> {
        if call_ids.is_empty() {
            return Ok(());
        }

        let mut full_calls = BTreeSet::new();
        let mut eligible_results = BTreeSet::new();
        for item in &self.items {
            match item {
                TranscriptItem::ToolCall {
                    call,
                    prompt_projection: ToolCallPromptProjection::Full,
                    ..
                } if call_ids.contains(call.id()) => {
                    full_calls.insert(call.id().clone());
                }
                TranscriptItem::ToolResult {
                    call_id,
                    prompt_projection:
                        ToolResultPromptProjection::Full | ToolResultPromptProjection::ArtifactNotice,
                    ..
                } if call_ids.contains(call_id) => {
                    eligible_results.insert(call_id.clone());
                }
                TranscriptItem::UserMessage { .. }
                | TranscriptItem::AssistantText { .. }
                | TranscriptItem::ToolCall { .. }
                | TranscriptItem::ToolResult { .. } => {}
            }
        }
        if &full_calls != call_ids || &eligible_results != call_ids {
            return Err(CompactionError::StaleWindow.into());
        }

        for item in &mut self.items {
            if let TranscriptItem::ToolResult {
                call_id,
                prompt_projection,
                ..
            } = item
                && call_ids.contains(call_id)
            {
                *prompt_projection = ToolResultPromptProjection::ArtifactNotice;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn retain_ids(&mut self, retained: std::collections::BTreeSet<TranscriptItemId>) {
        self.items.retain(|item| retained.contains(&item.id()));
    }

    fn allocate_id(&mut self) -> Result<TranscriptItemId, RuntimeError> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_next()?;
        Ok(id)
    }

    pub(crate) fn persisted(&self) -> PersistedTranscript {
        PersistedTranscript {
            items: self
                .items
                .iter()
                .map(PersistedTranscriptItem::from)
                .collect(),
            next_id: self.next_id.as_u64(),
            model_turns: self.model_turns.clone(),
            next_model_turn_id: self.next_model_turn_id.as_u64(),
        }
    }

    pub(crate) fn from_persisted(persisted: PersistedTranscript) -> Result<Self, RuntimeError> {
        let transcript = Self {
            items: persisted
                .items
                .into_iter()
                .map(TranscriptItem::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            next_id: TranscriptItemId::new(persisted.next_id),
            model_turns: persisted.model_turns,
            next_model_turn_id: ModelTurnId::new(persisted.next_model_turn_id),
        };
        transcript.validate_persisted_turns()?;
        Ok(transcript)
    }

    pub(crate) fn from_persisted_v1(
        persisted: PersistedTranscriptV1,
        artifacts: &ArtifactRegistry,
    ) -> Result<(Self, ArtifactRegistry), TranscriptV1MigrationError> {
        let legacy_turn_id = ModelTurnId::new(0);
        let mut migrated_artifacts = artifacts.clone();
        let mut items = Vec::with_capacity(persisted.items.len());
        for item in persisted.items {
            items.push(match item {
                PersistedTranscriptItemV1::UserMessage { id, text, origin } => {
                    let artifact_id = super::artifacts::user_message_id(TranscriptItemId::new(id));
                    let artifact = ArtifactRef::new(artifact_id.clone(), ArtifactKind::Text);
                    migrated_artifacts.record(artifact, ArtifactContent::text(text))?;
                    PersistedTranscriptItem::UserMessage {
                        id,
                        model_turn_id: legacy_turn_id,
                        artifact_id,
                        origin,
                    }
                }
                PersistedTranscriptItemV1::AssistantText { id, artifact_id } => {
                    PersistedTranscriptItem::AssistantText {
                        id,
                        model_turn_id: legacy_turn_id,
                        artifact_id,
                    }
                }
                PersistedTranscriptItemV1::ToolCall { id, call } => {
                    PersistedTranscriptItem::ToolCall {
                        id,
                        model_turn_id: legacy_turn_id,
                        call,
                        prompt_projection: ToolCallPromptProjection::Full,
                    }
                }
                PersistedTranscriptItemV1::ToolResult {
                    id,
                    call_id,
                    result,
                    artifact_id,
                } => PersistedTranscriptItem::ToolResult {
                    id,
                    model_turn_id: legacy_turn_id,
                    call_id,
                    result,
                    artifact_id,
                    prompt_projection: ToolResultPromptProjection::Full,
                },
            });
        }
        let transcript = Self::from_persisted(PersistedTranscript {
            items,
            next_id: persisted.next_id,
            model_turns: [(legacy_turn_id, ModelTurnStatus::Completed)]
                .into_iter()
                .collect(),
            next_model_turn_id: 1,
        })?;
        Ok((transcript, migrated_artifacts))
    }
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

impl From<UserInputOrigin> for PersistedUserInputOrigin {
    fn from(value: UserInputOrigin) -> Self {
        match value {
            UserInputOrigin::ExternalUser => Self::ExternalUser,
            UserInputOrigin::RuntimeControl => Self::RuntimeControl,
        }
    }
}

impl From<PersistedUserInputOrigin> for UserInputOrigin {
    fn from(value: PersistedUserInputOrigin) -> Self {
        match value {
            PersistedUserInputOrigin::ExternalUser => Self::ExternalUser,
            PersistedUserInputOrigin::RuntimeControl => Self::RuntimeControl,
        }
    }
}

impl From<&TranscriptItem> for PersistedTranscriptItem {
    fn from(value: &TranscriptItem) -> Self {
        match value {
            TranscriptItem::UserMessage {
                id,
                model_turn_id,
                artifact_id,
                origin,
            } => Self::UserMessage {
                id: id.as_u64(),
                model_turn_id: *model_turn_id,
                artifact_id: artifact_id.clone(),
                origin: (*origin).into(),
            },
            TranscriptItem::AssistantText {
                id,
                model_turn_id,
                artifact_id,
            } => Self::AssistantText {
                id: id.as_u64(),
                model_turn_id: *model_turn_id,
                artifact_id: artifact_id.clone(),
            },
            TranscriptItem::ToolCall {
                id,
                model_turn_id,
                call,
                prompt_projection,
            } => Self::ToolCall {
                id: id.as_u64(),
                model_turn_id: *model_turn_id,
                call: call.clone(),
                prompt_projection: *prompt_projection,
            },
            TranscriptItem::ToolResult {
                id,
                model_turn_id,
                call_id,
                result,
                artifact_id,
                prompt_projection,
            } => Self::ToolResult {
                id: id.as_u64(),
                model_turn_id: *model_turn_id,
                call_id: call_id.clone(),
                result: result.clone(),
                artifact_id: artifact_id.clone(),
                prompt_projection: *prompt_projection,
            },
        }
    }
}

impl TryFrom<PersistedTranscriptItem> for TranscriptItem {
    type Error = RuntimeError;

    fn try_from(value: PersistedTranscriptItem) -> Result<Self, Self::Error> {
        Ok(match value {
            PersistedTranscriptItem::UserMessage {
                id,
                model_turn_id,
                artifact_id,
                origin,
            } => Self::UserMessage {
                id: TranscriptItemId::new(id),
                model_turn_id,
                artifact_id,
                origin: origin.into(),
            },
            PersistedTranscriptItem::AssistantText {
                id,
                model_turn_id,
                artifact_id,
            } => Self::AssistantText {
                id: TranscriptItemId::new(id),
                model_turn_id,
                artifact_id,
            },
            PersistedTranscriptItem::ToolCall {
                id,
                model_turn_id,
                call,
                prompt_projection,
            } => Self::ToolCall {
                id: TranscriptItemId::new(id),
                model_turn_id,
                call,
                prompt_projection,
            },
            PersistedTranscriptItem::ToolResult {
                id,
                model_turn_id,
                call_id,
                result,
                artifact_id,
                prompt_projection,
            } => Self::ToolResult {
                id: TranscriptItemId::new(id),
                model_turn_id,
                call_id,
                result,
                artifact_id,
                prompt_projection,
            },
        })
    }
}

impl SessionState {
    pub(crate) fn full_transcript_snapshot(
        &self,
    ) -> Result<Vec<TranscriptItemSnapshot>, ArtifactError> {
        self.build_transcript_snapshot(false)
    }

    pub(crate) fn provider_transcript_snapshot(
        &self,
    ) -> Result<Vec<TranscriptItemSnapshot>, ArtifactError> {
        self.build_transcript_snapshot(true)
    }

    fn build_transcript_snapshot(
        &self,
        apply_prompt_projection: bool,
    ) -> Result<Vec<TranscriptItemSnapshot>, ArtifactError> {
        let transcript_items = self.transcript.items();
        let visible_items = if apply_prompt_projection {
            match self.prompt_history_projection.compacted_through() {
                Some(boundary) => {
                    let start =
                        transcript_items.partition_point(|item| item.model_turn_id() <= boundary);
                    &transcript_items[start..]
                }
                None => transcript_items,
            }
        } else {
            transcript_items
        };
        let mut snapshot = Vec::with_capacity(visible_items.len());
        for item in visible_items {
            let item =
                match item {
                    TranscriptItem::UserMessage {
                        artifact_id,
                        origin,
                        ..
                    } => {
                        let content = self.read_artifact_content(artifact_id)?;
                        let text = content.as_text().ok_or_else(|| {
                            ArtifactError::InvalidEvidenceLocator {
                                id: artifact_id.clone(),
                                reason: "user transcript artifact is not textual",
                            }
                        })?;
                        TranscriptItemSnapshot::UserMessage {
                            text: text.to_owned(),
                            origin: *origin,
                        }
                    }
                    TranscriptItem::AssistantText { artifact_id, .. } => {
                        let content = self.read_artifact_content(artifact_id)?;
                        let text = content.as_text().ok_or_else(|| {
                            ArtifactError::InvalidEvidenceLocator {
                                id: artifact_id.clone(),
                                reason: "assistant transcript artifact is not textual",
                            }
                        })?;
                        TranscriptItemSnapshot::AssistantText {
                            text: text.to_owned(),
                        }
                    }
                    TranscriptItem::ToolCall {
                        call,
                        prompt_projection,
                        ..
                    } => {
                        if apply_prompt_projection
                            && *prompt_projection == ToolCallPromptProjection::Hidden
                        {
                            continue;
                        }
                        TranscriptItemSnapshot::ToolCall { call: call.clone() }
                    }
                    TranscriptItem::ToolResult {
                        id,
                        call_id,
                        result,
                        artifact_id,
                        prompt_projection,
                        ..
                    } => {
                        if apply_prompt_projection
                            && *prompt_projection == ToolResultPromptProjection::Hidden
                        {
                            continue;
                        }
                        let content = match (apply_prompt_projection, prompt_projection) {
                            (false, _) | (true, ToolResultPromptProjection::Full) => {
                                self.read_artifact_content(artifact_id)?
                            }
                            (true, ToolResultPromptProjection::ArtifactNotice) => {
                                ArtifactContent::json(archived_tool_result_notice_json(
                                    *id,
                                    result.status(),
                                    artifact_id,
                                ))
                            }
                            (true, ToolResultPromptProjection::Hidden) => unreachable!(
                                "hidden tool results are filtered before snapshot construction"
                            ),
                        };
                        TranscriptItemSnapshot::ToolResult {
                            call_id: call_id.clone(),
                            result: result.clone(),
                            content,
                        }
                    }
                };
            snapshot.push(item);
        }
        Ok(snapshot)
    }
}
