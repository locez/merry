use super::{
    SessionState,
    transcript::{Transcript, TranscriptItem},
};
use crate::{RuntimeError, context::CompactedCheckpoint};
use merry_core::{ToolCallId, ToolCallResult};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ModelTurnId(u64);

impl ModelTurnId {
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
            .ok_or(RuntimeError::ModelTurnIdExhausted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelTurnStatus {
    InProgress,
    AwaitingToolResults,
    Completed,
    Aborted,
}

impl ModelTurnStatus {
    #[must_use]
    pub(crate) const fn is_open(self) -> bool {
        matches!(self, Self::InProgress | Self::AwaitingToolResults)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptHistoryProjection {
    compacted_through: Option<ModelTurnId>,
}

impl PromptHistoryProjection {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            compacted_through: None,
        }
    }

    #[must_use]
    pub(crate) const fn compacted_through(self) -> Option<ModelTurnId> {
        self.compacted_through
    }

    pub(crate) fn advanced_through(
        self,
        transcript: &Transcript,
        compacted_through: ModelTurnId,
    ) -> Result<Self, RuntimeError> {
        if self
            .compacted_through
            .is_some_and(|current| current > compacted_through)
        {
            return Err(invalid_turn_transition(
                compacted_through,
                "move prompt history projection backwards",
            ));
        }

        let current = self.compacted_through;
        let mut found_target = false;
        for turn in transcript.model_turns()? {
            if current.is_some_and(|current| turn.id() <= current) {
                continue;
            }
            if turn.id() > compacted_through {
                break;
            }
            if turn.status().is_open() {
                return Err(invalid_turn_transition(
                    turn.id(),
                    "compact an open model turn",
                ));
            }
            found_target |= turn.id() == compacted_through;
        }
        if !found_target && current != Some(compacted_through) {
            return Err(RuntimeError::UnknownModelTurn {
                model_turn_id: compacted_through.as_u64(),
            });
        }

        Ok(Self {
            compacted_through: Some(compacted_through),
        })
    }

    pub(crate) fn validate(
        self,
        transcript: &Transcript,
        compacted_checkpoint: Option<&CompactedCheckpoint>,
    ) -> Result<(), RuntimeError> {
        let Some(compacted_through) = self.compacted_through else {
            return Ok(());
        };
        if compacted_checkpoint.is_none() {
            return Err(invalid_turn_transition(
                compacted_through,
                "restore prompt history projection without checkpoint",
            ));
        }
        transcript
            .model_turns()?
            .into_iter()
            .find(|turn| turn.id() == compacted_through && !turn.status().is_open())
            .ok_or_else(|| {
                invalid_turn_transition(
                    compacted_through,
                    "restore invalid prompt history projection",
                )
            })
            .map(|_| ())
    }
}

#[derive(Debug)]
pub(crate) struct ModelTurn<'a> {
    id: ModelTurnId,
    status: ModelTurnStatus,
    items: Vec<&'a TranscriptItem>,
}

impl<'a> ModelTurn<'a> {
    #[must_use]
    pub(crate) const fn id(&self) -> ModelTurnId {
        self.id
    }

    #[must_use]
    pub(crate) const fn status(&self) -> ModelTurnStatus {
        self.status
    }

    #[must_use]
    pub(crate) fn items(&self) -> &[&'a TranscriptItem] {
        &self.items
    }
}

impl Transcript {
    pub(crate) fn begin_model_turn(&mut self) -> Result<ModelTurnId, RuntimeError> {
        let turn_id = self.next_model_turn_id;
        let next_model_turn_id = turn_id.checked_next()?;
        let previous = self
            .model_turns
            .insert(turn_id, ModelTurnStatus::InProgress);
        debug_assert!(previous.is_none());
        self.next_model_turn_id = next_model_turn_id;
        Ok(turn_id)
    }

    pub(crate) fn close_model_response(
        &mut self,
        turn_id: ModelTurnId,
        requested_tool_calls: bool,
    ) -> Result<(), RuntimeError> {
        let status = self.model_turn_status_mut(turn_id)?;
        if *status != ModelTurnStatus::InProgress {
            return Err(invalid_turn_transition(turn_id, "close model response"));
        }
        *status = if requested_tool_calls {
            ModelTurnStatus::AwaitingToolResults
        } else {
            ModelTurnStatus::Completed
        };
        Ok(())
    }

    pub(crate) fn abort_model_turn(&mut self, turn_id: ModelTurnId) -> Result<(), RuntimeError> {
        let status = self.model_turn_status_mut(turn_id)?;
        if *status != ModelTurnStatus::InProgress {
            return Err(invalid_turn_transition(turn_id, "abort model turn"));
        }
        *status = ModelTurnStatus::Aborted;
        Ok(())
    }

    #[must_use]
    pub(crate) fn status(&self, turn_id: ModelTurnId) -> Option<ModelTurnStatus> {
        self.model_turns.get(&turn_id).copied()
    }

    pub(crate) fn model_turns(&self) -> Result<Vec<ModelTurn<'_>>, RuntimeError> {
        let mut turns = self
            .model_turns
            .iter()
            .map(|(&id, &status)| ModelTurn {
                id,
                status,
                items: Vec::new(),
            })
            .collect::<Vec<_>>();
        let turn_indexes = turns
            .iter()
            .enumerate()
            .map(|(index, turn)| (turn.id, index))
            .collect::<BTreeMap<_, _>>();
        let mut last_item_turn = None;
        let mut call_turns = BTreeMap::<ToolCallId, ModelTurnId>::new();
        let mut result_call_ids = BTreeSet::<ToolCallId>::new();

        for item in &self.items {
            let turn_id = item.model_turn_id();
            let Some(&turn_index) = turn_indexes.get(&turn_id) else {
                return Err(RuntimeError::UnknownModelTurn {
                    model_turn_id: turn_id.as_u64(),
                });
            };
            if last_item_turn.is_some_and(|last| turn_id < last) {
                return Err(invalid_turn_transition(
                    turn_id,
                    "restore interleaved model turns",
                ));
            }
            last_item_turn = Some(turn_id);

            match item {
                TranscriptItem::ToolCall { call, .. } => {
                    if call_turns.insert(call.id().clone(), turn_id).is_some() {
                        return Err(invalid_turn_transition(
                            turn_id,
                            "restore duplicate tool call",
                        ));
                    }
                }
                TranscriptItem::ToolResult {
                    call_id, result, ..
                } => {
                    validate_tool_result_order(
                        turn_id,
                        call_id,
                        result,
                        &call_turns,
                        &mut result_call_ids,
                    )?;
                }
                TranscriptItem::UserMessage { .. } | TranscriptItem::AssistantText { .. } => {}
            }
            turns[turn_index].items.push(item);
        }

        for turn in &turns {
            if turn.status == ModelTurnStatus::Completed {
                let unresolved = turn.items.iter().any(|item| match item {
                    TranscriptItem::ToolCall { call, .. } => !result_call_ids.contains(call.id()),
                    _ => false,
                });
                if unresolved {
                    return Err(invalid_turn_transition(
                        turn.id,
                        "restore completed turn with unresolved tool call",
                    ));
                }
            }
        }

        Ok(turns)
    }

    pub(super) fn ensure_model_turn_in_progress(
        &self,
        turn_id: ModelTurnId,
    ) -> Result<(), RuntimeError> {
        match self.status(turn_id) {
            Some(ModelTurnStatus::InProgress) => Ok(()),
            Some(_) => Err(invalid_turn_transition(turn_id, "record model output")),
            None => Err(RuntimeError::UnknownModelTurn {
                model_turn_id: turn_id.as_u64(),
            }),
        }
    }

    pub(super) fn model_turn_status_mut(
        &mut self,
        turn_id: ModelTurnId,
    ) -> Result<&mut ModelTurnStatus, RuntimeError> {
        self.model_turns
            .get_mut(&turn_id)
            .ok_or(RuntimeError::UnknownModelTurn {
                model_turn_id: turn_id.as_u64(),
            })
    }

    pub(super) fn all_tool_calls_resolved(&self, turn_id: ModelTurnId) -> bool {
        let result_call_ids = self
            .items
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::ToolResult {
                    model_turn_id,
                    call_id,
                    ..
                } if *model_turn_id == turn_id => Some(call_id),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut has_tool_call = false;
        let all_resolved = self.items.iter().all(|item| match item {
            TranscriptItem::ToolCall {
                model_turn_id,
                call,
                ..
            } if *model_turn_id == turn_id => {
                has_tool_call = true;
                result_call_ids.contains(call.id())
            }
            _ => true,
        });
        has_tool_call && all_resolved
    }

    pub(super) fn validate_persisted_turns(&self) -> Result<(), RuntimeError> {
        if self.next_model_turn_id.as_u64() == 0 {
            return Err(RuntimeError::UnknownModelTurn { model_turn_id: 0 });
        }
        let mut item_ids = BTreeSet::new();
        for item in &self.items {
            if !item_ids.insert(item.id().as_u64()) || item.id().as_u64() >= self.next_id.as_u64() {
                return Err(RuntimeError::InvalidModelTurnTransition {
                    model_turn_id: item.model_turn_id().as_u64(),
                    attempted: "restore invalid transcript item ids",
                });
            }
        }
        let next_model_turn_id = self.next_model_turn_id.as_u64();
        let mut expected_turn_id = 1_u64;
        for turn_id in self
            .model_turns
            .keys()
            .copied()
            .filter(|turn_id| turn_id.as_u64() != 0)
        {
            if turn_id.as_u64() != expected_turn_id || turn_id.as_u64() >= next_model_turn_id {
                return Err(RuntimeError::InvalidModelTurnTransition {
                    model_turn_id: turn_id.as_u64(),
                    attempted: "restore unreachable model turn sequence",
                });
            }
            expected_turn_id = expected_turn_id
                .checked_add(1)
                .ok_or(RuntimeError::ModelTurnIdExhausted)?;
        }
        if expected_turn_id != next_model_turn_id {
            return Err(RuntimeError::UnknownModelTurn {
                model_turn_id: next_model_turn_id,
            });
        }
        self.model_turns().map(|_| ())
    }
}

fn validate_tool_result_order(
    turn_id: ModelTurnId,
    call_id: &ToolCallId,
    result: &ToolCallResult,
    call_turns: &BTreeMap<ToolCallId, ModelTurnId>,
    result_call_ids: &mut BTreeSet<ToolCallId>,
) -> Result<(), RuntimeError> {
    if result.call_id() != call_id {
        return Err(invalid_turn_transition(
            turn_id,
            "restore mismatched tool result identity",
        ));
    }
    let Some(call_turn_id) = call_turns.get(call_id) else {
        return Err(invalid_turn_transition(
            turn_id,
            "restore tool result before call",
        ));
    };
    if *call_turn_id != turn_id {
        return Err(invalid_turn_transition(
            turn_id,
            "restore cross-turn tool result",
        ));
    }
    if !result_call_ids.insert(call_id.clone()) {
        return Err(invalid_turn_transition(
            turn_id,
            "restore duplicate tool result",
        ));
    }
    Ok(())
}

pub(super) fn invalid_turn_transition(
    turn_id: ModelTurnId,
    attempted: &'static str,
) -> RuntimeError {
    RuntimeError::InvalidModelTurnTransition {
        model_turn_id: turn_id.as_u64(),
        attempted,
    }
}

impl SessionState {
    pub(crate) fn begin_model_turn(&mut self) -> Result<ModelTurnId, RuntimeError> {
        self.transcript.begin_model_turn()
    }

    pub(crate) fn close_model_response(
        &mut self,
        turn_id: ModelTurnId,
        requested_tool_calls: bool,
    ) -> Result<(), RuntimeError> {
        self.transcript
            .close_model_response(turn_id, requested_tool_calls)
    }

    pub(crate) fn abort_model_turn(&mut self, turn_id: ModelTurnId) -> Result<(), RuntimeError> {
        self.transcript.abort_model_turn(turn_id)
    }

    #[must_use]
    pub(crate) fn model_turn_status(&self, turn_id: ModelTurnId) -> Option<ModelTurnStatus> {
        self.transcript.status(turn_id)
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) const fn prompt_history_projection(&self) -> PromptHistoryProjection {
        self.prompt_history_projection
    }

    #[cfg(test)]
    pub(crate) fn advance_prompt_history_projection(
        &mut self,
        compacted_through: ModelTurnId,
    ) -> Result<(), RuntimeError> {
        self.prompt_history_projection = self
            .prompt_history_projection
            .advanced_through(&self.transcript, compacted_through)?;
        Ok(())
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn validate_prompt_history_projection(&self) -> Result<(), RuntimeError> {
        self.prompt_history_projection
            .validate(&self.transcript, self.compacted_checkpoint.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_turn_id_exhaustion_does_not_mutate_transcript() {
        let mut transcript = Transcript {
            items: Vec::new(),
            next_id: super::super::transcript::TranscriptItemId::new(0),
            model_turns: BTreeMap::new(),
            next_model_turn_id: ModelTurnId::new(u64::MAX),
        };
        let before = transcript.persisted();

        let error = transcript
            .begin_model_turn()
            .expect_err("exhausted model turn id should reject allocation");

        assert!(matches!(error, RuntimeError::ModelTurnIdExhausted));
        assert_eq!(transcript.persisted(), before);
    }
}
