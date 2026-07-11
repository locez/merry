use super::{ModelTurnId, SessionState, UserInputOrigin, artifacts::user_message_id};
use crate::{RuntimeError, artifact::ArtifactContent};
use merry_core::{ArtifactKind, ArtifactRef};

impl SessionState {
    pub(crate) fn record_user_message_body(
        &mut self,
        turn_id: ModelTurnId,
        text: &str,
    ) -> Result<(), RuntimeError> {
        let artifact = ArtifactRef::new(
            user_message_id(self.transcript.next_id()),
            ArtifactKind::Text,
        );
        let content = ArtifactContent::text(text);
        self.artifacts.ensure_recordable(&artifact, &content)?;

        let mut transcript = self.transcript.clone();
        transcript.push_user_message(
            turn_id,
            artifact.id().clone(),
            UserInputOrigin::ExternalUser,
        )?;

        let recorded = self.artifacts.record_preflighted(artifact, content);
        self.transcript = transcript;
        debug_assert_eq!(
            recorded.id(),
            match self.transcript.items().last() {
                Some(super::transcript::TranscriptItem::UserMessage { artifact_id, .. }) =>
                    artifact_id,
                _ => unreachable!("preflighted user message should be the last transcript item"),
            }
        );
        Ok(())
    }
}
