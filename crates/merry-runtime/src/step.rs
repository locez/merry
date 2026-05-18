//! Runtime step inputs, execution context, and provider request compilation.

use crate::{CompiledContext, RuntimeError};
use merry_llm::{
    GenerationConfig, ModelContent, ModelMessage, ModelMessageRole, ModelName, ModelRequest,
};
use tokio_util::sync::CancellationToken;

/// Input snapshot for a runtime step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepInput {
    user_text: String,
}

impl StepInput {
    /// Creates a user-text step input.
    pub fn user_text(text: &str) -> Result<Self, RuntimeError> {
        validate_user_text(text)?;
        Ok(Self {
            user_text: text.to_owned(),
        })
    }

    /// Borrows the user text for this step.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.user_text
    }
}

/// Context shared with runtime step producers.
#[derive(Debug, Clone)]
pub struct StepContext {
    cancellation_token: CancellationToken,
}

impl StepContext {
    /// Creates a step context with the provided cancellation token.
    #[must_use]
    pub fn new(cancellation_token: CancellationToken) -> Self {
        Self { cancellation_token }
    }

    /// Returns the cancellation token for this step.
    #[must_use]
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    pub(crate) fn into_cancellation_token(self) -> CancellationToken {
        self.cancellation_token
    }
}

impl Default for StepContext {
    fn default() -> Self {
        Self {
            cancellation_token: CancellationToken::new(),
        }
    }
}

fn validate_user_text(text: &str) -> Result<(), RuntimeError> {
    if text.trim().is_empty() {
        return Err(RuntimeError::InvalidStepInput {
            reason: "user text must not be blank",
        });
    }

    if text
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(RuntimeError::InvalidStepInput {
            reason: "user text must not contain control characters other than newline or tab",
        });
    }

    Ok(())
}

pub(crate) fn compile_step_model_request(
    input: &StepInput,
    model: &ModelName,
    context: &CompiledContext,
) -> Result<ModelRequest, merry_llm::ModelError> {
    let context_snapshot = context.to_snapshot();
    let mut messages = Vec::with_capacity(if context_snapshot.is_empty() { 1 } else { 2 });

    if !context_snapshot.is_empty() {
        messages.push(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&context_snapshot)?,
        )?);
    }

    messages.push(ModelMessage::new(
        ModelMessageRole::User,
        ModelContent::text(input.text())?,
    )?);

    ModelRequest::new(
        model.clone(),
        messages,
        Vec::new(),
        GenerationConfig::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::StepInput;
    use crate::RuntimeError;

    #[test]
    fn user_text_rejects_blank_text() {
        let err = StepInput::user_text(" \n\t ").expect_err("blank text should be rejected");

        assert!(matches!(err, RuntimeError::InvalidStepInput { .. }));
    }

    #[test]
    fn user_text_allows_newline_and_tab() {
        let input =
            StepInput::user_text("line one\n\tline two").expect("newline and tab are allowed");

        assert_eq!(input.text(), "line one\n\tline two");
    }

    #[test]
    fn user_text_rejects_other_control_characters() {
        let err = StepInput::user_text("hello\u{7}").expect_err("bell should be rejected");

        assert!(matches!(err, RuntimeError::InvalidStepInput { .. }));
    }
}
