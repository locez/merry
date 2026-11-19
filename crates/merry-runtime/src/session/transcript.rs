use super::{
    SessionState,
    model_turns::{ModelTurnId, ModelTurnStatus, invalid_turn_transition},
};
use crate::{
    RuntimeError, UserImageInput,
    compaction::CompactionError,
    session_projection::{SessionTrajectory, SessionTrajectoryItem},
};
use merry_core::{PendingToolCall, ToolCallId, ToolCallResult, ToolOutput};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolCallPromptProjection {
    Full,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolResultPromptProjection {
    /// Complete tool-authored model body, or the full output when absent.
    Full,
    /// Compact omission notice for compacted history; full output stays on the item.
    Collapsed,
    /// Hidden tool exchange (for internal final-output tool calls).
    Hidden,
}

pub(super) fn collapsed_tool_result_notice_json(
    status: merry_core::ToolCallResultStatus,
    call_id: &ToolCallId,
) -> String {
    serde_json::json!({
        "merry_omitted": true,
        "status": match status {
            merry_core::ToolCallResultStatus::Succeeded => "succeeded",
            merry_core::ToolCallResultStatus::Failed => "failed",
        },
        "call_id": call_id.as_str(),
    })
    .to_string()
}

/// Shared dual-view tool output and optional normalized images owned by one transcript item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolResultPayload {
    full: Arc<ToolOutput>,
    model: Option<Arc<ToolOutput>>,
    images: Vec<UserImageInput>,
}

impl ToolResultPayload {
    pub(crate) fn new(
        full: ToolOutput,
        model: Option<ToolOutput>,
        images: Vec<UserImageInput>,
    ) -> Self {
        let model = model.filter(|candidate| candidate != &full).map(Arc::new);
        Self {
            full: Arc::new(full),
            model,
            images,
        }
    }

    #[must_use]
    pub(crate) fn full(&self) -> &ToolOutput {
        &self.full
    }

    #[must_use]
    pub(crate) fn model(&self) -> Option<&ToolOutput> {
        self.model.as_deref()
    }

    #[must_use]
    pub(crate) fn model_or_full(&self) -> &ToolOutput {
        self.model.as_deref().unwrap_or(&self.full)
    }

    #[must_use]
    pub(crate) fn images(&self) -> &[UserImageInput] {
        &self.images
    }
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
        text: Arc<str>,
        images: Vec<UserImageInput>,
        origin: UserInputOrigin,
    },
    AssistantText {
        id: TranscriptItemId,
        model_turn_id: ModelTurnId,
        text: Arc<str>,
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
        payload: ToolResultPayload,
        prompt_projection: ToolResultPromptProjection,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedUserImage {
    pub(crate) label: String,
    pub(crate) png_bytes: Arc<[u8]>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl From<&UserImageInput> for PersistedUserImage {
    fn from(image: &UserImageInput) -> Self {
        Self {
            label: image.label().to_owned(),
            png_bytes: image.shared_png_bytes(),
            width: image.width(),
            height: image.height(),
        }
    }
}

impl TryFrom<PersistedUserImage> for UserImageInput {
    type Error = RuntimeError;

    fn try_from(image: PersistedUserImage) -> Result<Self, Self::Error> {
        Self::png(&image.label, image.png_bytes, image.width, image.height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedTranscript {
    pub(crate) items: Vec<PersistedTranscriptItem>,
    pub(crate) next_id: u64,
    pub(crate) model_turns: BTreeMap<ModelTurnId, ModelTurnStatus>,
    #[serde(default)]
    pub(crate) model_turn_sequences: BTreeMap<ModelTurnId, u64>,
    pub(crate) next_model_turn_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PersistedTranscriptItem {
    UserMessage {
        id: u64,
        model_turn_id: ModelTurnId,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<PersistedUserImage>,
        origin: PersistedUserInputOrigin,
    },
    AssistantText {
        id: u64,
        model_turn_id: ModelTurnId,
        text: String,
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
        full_output: ToolOutput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_output: Option<ToolOutput>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<PersistedUserImage>,
        prompt_projection: ToolResultPromptProjection,
    },
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
        images: Vec<UserImageInput>,
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
        output: ToolOutput,
        images: Vec<UserImageInput>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Transcript {
    pub(super) items: Vec<TranscriptItem>,
    pub(super) tool_result_index: BTreeMap<ToolCallId, usize>,
    pub(super) next_id: TranscriptItemId,
    pub(super) model_turns: BTreeMap<ModelTurnId, ModelTurnStatus>,
    pub(super) model_turn_sequences: BTreeMap<ModelTurnId, u64>,
    pub(super) next_model_turn_id: ModelTurnId,
}

impl Transcript {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            items: Vec::new(),
            tool_result_index: BTreeMap::new(),
            next_id: TranscriptItemId::new(0),
            model_turns: BTreeMap::new(),
            model_turn_sequences: BTreeMap::new(),
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
    pub(crate) fn tool_result_payload(&self, call_id: &ToolCallId) -> Option<&ToolResultPayload> {
        let &index = self.tool_result_index.get(call_id)?;
        match self.items.get(index) {
            Some(TranscriptItem::ToolResult { payload, .. }) => Some(payload),
            _ => None,
        }
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

    #[cfg(test)]
    pub(crate) fn push_user_message(
        &mut self,
        model_turn_id: ModelTurnId,
        text: impl Into<Arc<str>>,
        origin: UserInputOrigin,
    ) -> Result<TranscriptItemId, RuntimeError> {
        self.push_user_message_with_images(model_turn_id, text.into(), Vec::new(), origin)
    }

    pub(crate) fn push_user_message_with_images(
        &mut self,
        model_turn_id: ModelTurnId,
        text: Arc<str>,
        images: Vec<UserImageInput>,
        origin: UserInputOrigin,
    ) -> Result<TranscriptItemId, RuntimeError> {
        self.ensure_model_turn_in_progress(model_turn_id)?;
        let id = self.allocate_id()?;
        self.items.push(TranscriptItem::UserMessage {
            id,
            model_turn_id,
            text,
            images,
            origin,
        });
        Ok(id)
    }

    pub(crate) fn push_assistant_text(
        &mut self,
        model_turn_id: ModelTurnId,
        text: Arc<str>,
    ) -> Result<TranscriptItemId, RuntimeError> {
        self.ensure_model_turn_in_progress(model_turn_id)?;
        let id = self.allocate_id()?;
        self.items.push(TranscriptItem::AssistantText {
            id,
            model_turn_id,
            text,
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
        payload: ToolResultPayload,
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
        let index = self.items.len();
        self.items.push(TranscriptItem::ToolResult {
            id,
            model_turn_id,
            call_id: call_id.clone(),
            result,
            payload,
            prompt_projection,
        });
        self.tool_result_index.insert(call_id, index);
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
                        ToolResultPromptProjection::Full | ToolResultPromptProjection::Collapsed,
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
                *prompt_projection = ToolResultPromptProjection::Collapsed;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn retain_ids(&mut self, retained: BTreeSet<TranscriptItemId>) {
        self.items.retain(|item| retained.contains(&item.id()));
        self.rebuild_tool_result_index();
    }

    #[cfg(test)]
    fn rebuild_tool_result_index(&mut self) {
        self.tool_result_index.clear();
        for (index, item) in self.items.iter().enumerate() {
            if let TranscriptItem::ToolResult { call_id, .. } = item {
                self.tool_result_index.insert(call_id.clone(), index);
            }
        }
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
            model_turn_sequences: self.model_turn_sequences.clone(),
            next_model_turn_id: self.next_model_turn_id.as_u64(),
        }
    }

    pub(crate) fn from_persisted(persisted: PersistedTranscript) -> Result<Self, RuntimeError> {
        let items = persisted
            .items
            .into_iter()
            .map(TranscriptItem::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let mut tool_result_index = BTreeMap::new();
        for (index, item) in items.iter().enumerate() {
            if let TranscriptItem::ToolResult { call_id, .. } = item
                && tool_result_index.insert(call_id.clone(), index).is_some()
            {
                return Err(RuntimeError::InvalidStepInput {
                    reason: "persisted transcript contains duplicate tool result call_id",
                });
            }
        }
        let transcript = Self {
            items,
            tool_result_index,
            next_id: TranscriptItemId::new(persisted.next_id),
            model_turns: persisted.model_turns,
            model_turn_sequences: persisted.model_turn_sequences,
            next_model_turn_id: ModelTurnId::new(persisted.next_model_turn_id),
        };
        transcript.validate_persisted_turns()?;
        Ok(transcript)
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
                text,
                images,
                origin,
            } => Self::UserMessage {
                id: id.as_u64(),
                model_turn_id: *model_turn_id,
                text: text.to_string(),
                images: images.iter().map(PersistedUserImage::from).collect(),
                origin: (*origin).into(),
            },
            TranscriptItem::AssistantText {
                id,
                model_turn_id,
                text,
            } => Self::AssistantText {
                id: id.as_u64(),
                model_turn_id: *model_turn_id,
                text: text.to_string(),
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
                payload,
                prompt_projection,
            } => Self::ToolResult {
                id: id.as_u64(),
                model_turn_id: *model_turn_id,
                call_id: call_id.clone(),
                result: result.clone(),
                full_output: payload.full().clone(),
                model_output: payload.model().cloned(),
                images: payload
                    .images()
                    .iter()
                    .map(PersistedUserImage::from)
                    .collect(),
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
                text,
                images,
                origin,
            } => Self::UserMessage {
                id: TranscriptItemId::new(id),
                model_turn_id,
                text: text.into(),
                images: images
                    .into_iter()
                    .map(UserImageInput::try_from)
                    .collect::<Result<Vec<_>, _>>()?,
                origin: origin.into(),
            },
            PersistedTranscriptItem::AssistantText {
                id,
                model_turn_id,
                text,
            } => Self::AssistantText {
                id: TranscriptItemId::new(id),
                model_turn_id,
                text: text.into(),
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
                full_output,
                model_output,
                images,
                prompt_projection,
            } => Self::ToolResult {
                id: TranscriptItemId::new(id),
                model_turn_id,
                call_id,
                result,
                payload: ToolResultPayload::new(
                    full_output,
                    model_output,
                    images
                        .into_iter()
                        .map(UserImageInput::try_from)
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                prompt_projection,
            },
        })
    }
}

impl SessionState {
    pub(crate) fn trajectory_items(&self) -> SessionTrajectory {
        let items = self
            .transcript
            .items()
            .iter()
            .map(|item| match item {
                TranscriptItem::UserMessage {
                    id,
                    model_turn_id,
                    text,
                    ..
                } => SessionTrajectoryItem::UserMessage {
                    item_id: id.as_u64(),
                    model_turn_id: model_turn_id.as_u64(),
                    text: text.to_string(),
                },
                TranscriptItem::AssistantText {
                    id,
                    model_turn_id,
                    text,
                } => SessionTrajectoryItem::AssistantText {
                    item_id: id.as_u64(),
                    model_turn_id: model_turn_id.as_u64(),
                    text: text.to_string(),
                },
                TranscriptItem::ToolCall {
                    id,
                    model_turn_id,
                    call,
                    ..
                } => SessionTrajectoryItem::ToolCall {
                    item_id: id.as_u64(),
                    model_turn_id: model_turn_id.as_u64(),
                    call: call.clone(),
                },
                TranscriptItem::ToolResult {
                    id,
                    model_turn_id,
                    call_id,
                    result,
                    payload,
                    ..
                } => SessionTrajectoryItem::ToolResult {
                    item_id: id.as_u64(),
                    model_turn_id: model_turn_id.as_u64(),
                    call_id: call_id.clone(),
                    result: result.clone(),
                    output: Some(payload.model_or_full().clone()),
                },
            })
            .collect();
        let model_turn_sequences = self
            .transcript
            .model_turn_sequences()
            .iter()
            .map(|(turn_id, sequence)| (turn_id.as_u64(), *sequence))
            .collect();
        SessionTrajectory {
            items,
            model_turn_sequences,
        }
    }

    pub(crate) fn full_transcript_snapshot(&self) -> Vec<TranscriptItemSnapshot> {
        self.build_transcript_snapshot(false)
    }

    pub(crate) fn provider_transcript_snapshot(&self) -> Vec<TranscriptItemSnapshot> {
        self.build_transcript_snapshot(true)
    }

    pub(crate) fn provider_transcript_history_ids(&self) -> Vec<u64> {
        self.transcript_items_for_projection(true)
            .map(|item| item.id().as_u64())
            .collect()
    }

    fn transcript_items_for_projection(
        &self,
        apply_prompt_projection: bool,
    ) -> impl Iterator<Item = &TranscriptItem> {
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
        visible_items.iter().filter(move |item| {
            !apply_prompt_projection
                || !matches!(
                    item,
                    TranscriptItem::ToolCall {
                        prompt_projection: ToolCallPromptProjection::Hidden,
                        ..
                    } | TranscriptItem::ToolResult {
                        prompt_projection: ToolResultPromptProjection::Hidden,
                        ..
                    }
                )
        })
    }

    fn build_transcript_snapshot(
        &self,
        apply_prompt_projection: bool,
    ) -> Vec<TranscriptItemSnapshot> {
        let mut snapshot = Vec::new();
        for item in self.transcript_items_for_projection(apply_prompt_projection) {
            let item = match item {
                TranscriptItem::UserMessage {
                    text,
                    images,
                    origin,
                    ..
                } => TranscriptItemSnapshot::UserMessage {
                    text: text.to_string(),
                    images: images.clone(),
                    origin: *origin,
                },
                TranscriptItem::AssistantText { text, .. } => {
                    TranscriptItemSnapshot::AssistantText {
                        text: text.to_string(),
                    }
                }
                TranscriptItem::ToolCall { call, .. } => {
                    TranscriptItemSnapshot::ToolCall { call: call.clone() }
                }
                TranscriptItem::ToolResult {
                    call_id,
                    result,
                    payload,
                    prompt_projection,
                    ..
                } => {
                    let output = match (apply_prompt_projection, prompt_projection) {
                        (false, _) => payload.full().clone(),
                        (true, ToolResultPromptProjection::Full) => payload.model_or_full().clone(),
                        (true, ToolResultPromptProjection::Collapsed) => ToolOutput::json(
                            collapsed_tool_result_notice_json(result.status(), call_id),
                        ),
                        (true, ToolResultPromptProjection::Hidden) => unreachable!(
                            "hidden tool results are filtered before snapshot construction"
                        ),
                    };
                    let include_images = !apply_prompt_projection
                        || *prompt_projection == ToolResultPromptProjection::Full;
                    let images = if include_images {
                        payload.images().to_vec()
                    } else {
                        Vec::new()
                    };
                    TranscriptItemSnapshot::ToolResult {
                        call_id: call_id.clone(),
                        result: result.clone(),
                        output,
                        images,
                    }
                }
            };
            snapshot.push(item);
        }
        snapshot
    }
}
