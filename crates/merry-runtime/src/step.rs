//! Runtime step inputs, execution context, and provider request compilation.

use crate::{
    CompiledContext, RuntimeError, artifact::ArtifactContent,
    session::ResolvedToolContinuationSnapshot,
};
use merry_core::{PendingToolCall, ToolCallResult, ToolCallResultStatus, ToolSpec};
use merry_llm::{
    GenerationConfig, ModelContent, ModelMessage, ModelMessageRole, ModelName, ModelRequest,
    ModelToolCall, ModelToolCallId, ModelToolContinuation, ModelToolResult, ModelToolResultContent,
    ToolArguments,
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
    continuations: &[ResolvedToolContinuationSnapshot],
    tool_specs: Vec<ToolSpec>,
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

    let continuations = continuations
        .iter()
        .map(model_tool_continuation_from_snapshot)
        .collect::<Result<Vec<_>, _>>()?;

    ModelRequest::new_with_continuations(
        model.clone(),
        messages,
        tool_specs,
        continuations,
        generation_config,
    )
}

fn model_tool_continuation_from_snapshot(
    snapshot: &ResolvedToolContinuationSnapshot,
) -> Result<ModelToolContinuation, merry_llm::ModelError> {
    let call = model_tool_call_from_pending(snapshot.call())?;
    let result = model_tool_result_from_result(snapshot.result(), snapshot.content())?;
    ModelToolContinuation::new(call, result)
}

fn model_tool_call_from_pending(
    call: &PendingToolCall,
) -> Result<ModelToolCall, merry_llm::ModelError> {
    Ok(ModelToolCall::new(
        ModelToolCallId::new(call.id().as_str())?,
        call.name().clone(),
        ToolArguments::new(call.arguments().as_object().clone()),
    ))
}

fn model_tool_result_from_result(
    result: &ToolCallResult,
    content: &ArtifactContent,
) -> Result<ModelToolResult, merry_llm::ModelError> {
    let call_id = ModelToolCallId::new(result.call_id().as_str())?;
    let content = model_tool_result_content(content)?;

    match result.status() {
        ToolCallResultStatus::Succeeded => ModelToolResult::new(
            call_id,
            ToolCallResultStatus::Succeeded,
            content,
            result.diagnostic().cloned(),
        ),
        ToolCallResultStatus::Failed => ModelToolResult::new(
            call_id,
            ToolCallResultStatus::Failed,
            content,
            result.diagnostic().cloned(),
        ),
    }
}

fn model_tool_result_content(
    content: &ArtifactContent,
) -> Result<ModelToolResultContent, merry_llm::ModelError> {
    match content {
        ArtifactContent::Text(text) => ModelToolResultContent::text(text),
        ArtifactContent::Json(json) => ModelToolResultContent::json(json),
        ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
            Err(merry_llm::ModelError::invalid_request(
                "tool result continuation content must be text or json",
            ))
        }
    }
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
