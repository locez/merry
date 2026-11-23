use super::SessionState;
use merry_core::{
    PendingToolCall, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallResult,
    ToolCallResultStatus,
};

const READ_FILE_TOOL_NAME: &str = "read_file";

impl SessionState {
    pub(super) fn skill_used_event_for_read(
        &mut self,
        pending: &PendingToolCall,
        result: &ToolCallResult,
    ) -> Option<RuntimeJournalEvent> {
        if result.status() != ToolCallResultStatus::Succeeded {
            return None;
        }
        if pending.name().as_str() != READ_FILE_TOOL_NAME {
            return None;
        }
        let path = pending
            .arguments()
            .as_object()
            .get("path")
            .and_then(serde_json::Value::as_str)?;
        let (skill_name, skill_md_path) = {
            let skill = self.skill_catalog.as_ref()?.find_by_skill_md_path(path)?;
            (
                skill.name().to_owned(),
                skill.skill_md_path().display().to_string(),
            )
        };

        Some(self.record_event(RuntimeJournalPayload::SkillUsed {
            skill_name,
            skill_md_path,
            tool_call_id: pending.id().clone(),
        }))
    }
}
