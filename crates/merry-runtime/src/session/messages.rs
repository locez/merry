use super::{
    ModelTurnId, SessionState, UserInputOrigin,
    artifacts::{user_message_id, user_message_image_id},
};
use crate::{RuntimeError, UserMessageInput, artifact::ArtifactContent};
use merry_core::{ArtifactKind, ArtifactRef};

impl SessionState {
    #[cfg(test)]
    pub(crate) fn record_user_message_body(
        &mut self,
        turn_id: ModelTurnId,
        text: &str,
    ) -> Result<(), RuntimeError> {
        let message = UserMessageInput::text_only(text)?;
        self.record_user_message(turn_id, &message)
    }

    pub(crate) fn record_user_message(
        &mut self,
        turn_id: ModelTurnId,
        message: &UserMessageInput,
    ) -> Result<(), RuntimeError> {
        let item_id = self.transcript.next_id();
        let text_artifact = ArtifactRef::new(
            user_message_id(self.transcript.next_id()),
            ArtifactKind::Text,
        );
        let text_content = ArtifactContent::text(message.text());
        self.artifacts
            .ensure_recordable(&text_artifact, &text_content)?;

        let image_artifacts = message
            .images()
            .iter()
            .enumerate()
            .map(|(offset, image)| {
                let artifact = ArtifactRef::new(
                    user_message_image_id(item_id, offset + 1),
                    ArtifactKind::Image,
                )
                .with_label(image.label())?;
                let content = ArtifactContent::normalized_png(
                    image.shared_png_bytes(),
                    image.width(),
                    image.height(),
                );
                self.artifacts.ensure_recordable(&artifact, &content)?;
                Ok((artifact, content))
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        let image_artifact_ids = image_artifacts
            .iter()
            .map(|(artifact, _)| artifact.id().clone())
            .collect::<Vec<_>>();

        let mut transcript = self.transcript.clone();
        transcript.push_user_message_with_images(
            turn_id,
            text_artifact.id().clone(),
            image_artifact_ids,
            UserInputOrigin::ExternalUser,
        )?;

        let recorded = self
            .artifacts
            .record_preflighted(text_artifact, text_content);
        for (artifact, content) in image_artifacts {
            self.artifacts.record_preflighted(artifact, content);
        }
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
