use super::{SessionState, history::SessionMessage};
use crate::{artifact::ArtifactError, step::CompiledSessionMessage};

impl SessionState {
    pub(crate) fn record_user_message_body(&mut self, text: &str) {
        let history_id = self.next_history_id();
        self.append_only_body
            .push(SessionMessage::user(history_id, text.to_owned()));
    }

    pub(crate) fn append_only_body_snapshot(
        &self,
    ) -> Result<Vec<CompiledSessionMessage>, ArtifactError> {
        self.append_only_body
            .iter()
            .map(|message| match message {
                SessionMessage::User { text, .. } => {
                    Ok(CompiledSessionMessage::User { text: text.clone() })
                }
                SessionMessage::Assistant { artifact_id, .. } => {
                    let content = self.read_artifact_content(artifact_id)?;
                    let text =
                        content
                            .as_text()
                            .ok_or_else(|| ArtifactError::InvalidEvidenceLocator {
                                id: artifact_id.clone(),
                                reason: "assistant history artifact is not textual",
                            })?;
                    Ok(CompiledSessionMessage::Assistant {
                        text: text.to_owned(),
                    })
                }
            })
            .collect()
    }

    pub(super) fn next_history_id(&mut self) -> u64 {
        let id = self.next_history_id;
        self.next_history_id = self.next_history_id.wrapping_add(1);
        id
    }

    #[cfg(test)]
    pub(super) fn history_item_ids(&self) -> Vec<u64> {
        let mut ids = self
            .append_only_body
            .iter()
            .map(|message| match message {
                SessionMessage::User { history_id, .. }
                | SessionMessage::Assistant { history_id, .. } => *history_id,
            })
            .chain(
                self.uncheckpointed_tool_continuations
                    .iter()
                    .map(|continuation| continuation.history_id),
            )
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    #[cfg(test)]
    pub(crate) fn append_only_body_text_for_tests(&self) -> Vec<String> {
        self.append_only_body
            .iter()
            .map(|message| match message {
                SessionMessage::User { text, .. } => text.clone(),
                SessionMessage::Assistant { artifact_id, .. } => self
                    .read_artifact_content(artifact_id)
                    .expect("assistant artifact should be readable")
                    .as_text()
                    .expect("assistant artifact should be text")
                    .to_owned(),
            })
            .collect()
    }
}
