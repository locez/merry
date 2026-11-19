use super::{ModelTurnId, SessionState, UserInputOrigin};
use crate::{RuntimeError, UserMessageInput};

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
        let mut transcript = self.transcript.clone();
        transcript.push_user_message_with_images(
            turn_id,
            message.text().into(),
            message.images().to_vec(),
            UserInputOrigin::ExternalUser,
        )?;
        self.transcript = transcript;
        Ok(())
    }
}
