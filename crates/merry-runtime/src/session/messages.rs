use super::{SessionState, UserInputOrigin};
use crate::RuntimeError;

impl SessionState {
    pub(crate) fn record_user_message_body(&mut self, text: &str) -> Result<(), RuntimeError> {
        self.transcript
            .push_user_message(text, UserInputOrigin::ExternalUser)?;
        Ok(())
    }
}
