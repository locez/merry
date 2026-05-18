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
    generation_config: GenerationConfig,
}

impl StepContext {
    /// Creates a step context with the provided cancellation token.
    #[must_use]
    pub fn new(cancellation_token: CancellationToken) -> Self {
        Self {
            cancellation_token,
            generation_config: GenerationConfig::default(),
        }
    }

    /// Returns the cancellation token for this step.
    #[must_use]
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    /// Sets provider-neutral generation controls for this step.
    #[must_use]
    pub fn with_generation_config(mut self, generation_config: GenerationConfig) -> Self {
        self.generation_config = generation_config;
        self
    }

    /// Returns provider-neutral generation controls for this step.
    #[must_use]
    pub fn generation_config(&self) -> &GenerationConfig {
        &self.generation_config
    }

    pub(crate) fn into_parts(self) -> (CancellationToken, GenerationConfig) {
        (self.cancellation_token, self.generation_config)
    }
}

impl Default for StepContext {
    fn default() -> Self {
        Self {
            cancellation_token: CancellationToken::new(),
            generation_config: GenerationConfig::default(),
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
    generation_config: GenerationConfig,
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

    ModelRequest::new(model.clone(), messages, Vec::new(), generation_config)
}

#[cfg(test)]
mod tests {
    use super::{StepContext, StepInput};
    use crate::RuntimeError;
    use merry_llm::GenerationConfig;
    use tokio_util::sync::CancellationToken;

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

    #[test]
    fn step_context_uses_default_generation_config() {
        let context = StepContext::new(CancellationToken::new());

        assert_eq!(context.generation_config(), &GenerationConfig::default());
    }

    #[test]
    fn step_context_allows_step_scoped_generation_config_override() {
        let generation_config =
            GenerationConfig::new(Some(16), false).expect("valid generation config");
        let context =
            StepContext::new(CancellationToken::new()).with_generation_config(generation_config);

        assert_eq!(context.generation_config().max_output_tokens(), Some(16));
        assert!(!context.generation_config().allow_parallel_tool_calls());
    }
}
