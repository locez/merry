use super::{ModelTurnId, SessionState};
use crate::RuntimeError;
use merry_core::{
    RuntimeJournalEvent, RuntimeJournalPayload, ToolCallId, ToolOutput, ToolOutputPreview,
};

impl SessionState {
    pub(crate) fn record_assistant_text_output(
        &mut self,
        turn_id: ModelTurnId,
        text: String,
    ) -> Result<RuntimeJournalEvent, RuntimeError> {
        let mut transcript = self.transcript.clone();
        transcript.push_assistant_text(turn_id, text.as_str().into())?;
        self.transcript = transcript;
        Ok(self.record_event(RuntimeJournalPayload::AssistantOutputRecorded { text }))
    }

    pub(crate) fn read_tool_output(&self, call_id: &ToolCallId) -> Option<ToolOutput> {
        self.transcript
            .tool_result_payload(call_id)
            .map(|payload| payload.full().clone())
    }

    pub(crate) fn read_tool_output_preview(
        &self,
        call_id: &ToolCallId,
        max_bytes: usize,
    ) -> Option<ToolOutputPreview> {
        self.transcript
            .tool_result_payload(call_id)
            .map(|payload| payload.full().bounded_preview(max_bytes))
    }

    /// Model-facing preview for the trajectory. The full body stays in the transcript.
    pub(crate) fn read_model_tool_preview(
        &self,
        call_id: &ToolCallId,
        max_bytes: usize,
    ) -> Option<ToolOutputPreview> {
        self.transcript
            .tool_result_payload(call_id)
            .map(|payload| payload.model_or_full().bounded_preview(max_bytes))
    }
}
