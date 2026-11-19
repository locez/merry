use super::SessionState;
use crate::{RuntimeError, session::transcript::ToolResultPayload, tool::ToolResultContent};
use merry_core::{ToolCallId, ToolOutput};

impl SessionState {
    /// Validates full output, optional model body, and converts to a shared payload.
    pub(super) fn prepare_tool_result_payload(
        &self,
        call_id: &ToolCallId,
        content: ToolResultContent,
    ) -> Result<ToolResultPayload, RuntimeError> {
        self.validate_tool_output(call_id, content.full())?;
        if let Some(model) = content.model() {
            self.validate_tool_output(call_id, model)?;
        }
        let (full, model, images) = content.into_parts();
        Ok(ToolResultPayload::new(full, model, images))
    }

    pub(crate) fn validate_tool_output(
        &self,
        call_id: &ToolCallId,
        output: &ToolOutput,
    ) -> Result<(), RuntimeError> {
        if output.as_str().trim().is_empty() {
            return Err(RuntimeError::UnsupportedToolResultContent {
                call_id: call_id.clone(),
                output_kind: output.kind(),
            });
        }
        Ok(())
    }
}
