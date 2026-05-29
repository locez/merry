//! Runtime step inputs, execution context, and provider request compilation.
//!
//! Public step types are provider-neutral. Request compilation is internal
//! glue from structured runtime state into `merry-llm` model requests; provider
//! crates render those normalized requests into wire formats.

use crate::{
    CompiledContext, ProjectRules, RuntimeError, artifact::ArtifactContent,
    session::ResolvedToolContinuationSnapshot,
};
use merry_core::{PendingToolCall, ToolCallResult, ToolCallResultStatus, ToolSpec};
use merry_llm::{
    GenerationConfig, ModelContent, ModelMessage, ModelMessageRole, ModelName, ModelRequest,
    ModelToolCall, ModelToolCallId, ModelToolContinuation, ModelToolResult, ModelToolResultContent,
    ToolArguments,
};
use tokio_util::sync::CancellationToken;

pub(crate) const DEFAULT_RUNTIME_BASE_INSTRUCTIONS: &str = r#"You are Merry, a pragmatic coding agent.

Work from the runtime-provided project context and authorized filesystem view. Read files or search before editing, and do not invent paths, tool results, or verification outcomes.

Use the registered tools for workspace reads, searches, edits, and process execution. Prefer localized patches with the smallest unique context that proves the intended edit; do not rewrite whole files for small changes.

After code changes, run the most relevant available checks unless the user asks you not to or the runtime/tool policy blocks them. When a check cannot run, state exactly what remains unverified.

Respect project instructions such as AGENTS.md when present. Treat those instructions as project-specific policy layered on top of these runtime defaults."#;

/// Input snapshot for a runtime step.
///
/// The MVP step input is user text only. Runtime state such as context,
/// artifacts, tool continuations, and ledger facts is read from the owning
/// session rather than passed as raw chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepInput {
    user_text: String,
    history: StepInputHistory,
}

impl StepInput {
    /// Creates a user-text step input.
    ///
    /// Text must be non-blank and may contain newlines and tabs, but not other
    /// control characters.
    pub fn user_text(text: &str) -> Result<Self, RuntimeError> {
        validate_user_text(text)?;
        Ok(Self {
            user_text: text.to_owned(),
            history: StepInputHistory::RecordUser,
        })
    }

    pub(crate) fn loop_control_text(text: &str) -> Result<Self, RuntimeError> {
        validate_user_text(text)?;
        Ok(Self {
            user_text: text.to_owned(),
            history: StepInputHistory::ControlOnly,
        })
    }

    /// Borrows the user text for this step.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.user_text
    }

    pub(crate) fn should_record_user_history(&self) -> bool {
        self.history == StepInputHistory::RecordUser
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepInputHistory {
    RecordUser,
    ControlOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompiledSessionMessage {
    User { text: String },
    Assistant { text: String },
}

/// Context shared with runtime step producers.
///
/// The context carries cancellation and provider-neutral generation controls
/// for one step. It does not carry provider conversation state.
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
    ///
    /// Runtime producers check this token at cancellation checkpoints. Dropping
    /// the returned [`crate::RuntimeEventStream`] also cancels the step token.
    #[must_use]
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    /// Sets provider-neutral generation controls for this step.
    ///
    /// Controls are normalized Merry model settings, not provider request
    /// fields.
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

pub(crate) struct StepModelRequestParts<'a> {
    pub(crate) input: &'a StepInput,
    pub(crate) model: &'a ModelName,
    pub(crate) project_rules: Option<&'a ProjectRules>,
    pub(crate) context: &'a CompiledContext,
    pub(crate) append_only_body: &'a [CompiledSessionMessage],
    pub(crate) continuations: &'a [ResolvedToolContinuationSnapshot],
    pub(crate) tool_specs: Vec<ToolSpec>,
    pub(crate) generation_config: GenerationConfig,
}

pub(crate) fn compile_step_model_request(
    parts: StepModelRequestParts<'_>,
) -> Result<ModelRequest, merry_llm::ModelError> {
    let StepModelRequestParts {
        input,
        model,
        project_rules,
        context,
        append_only_body,
        continuations,
        tool_specs,
        generation_config,
    } = parts;

    let context_snapshot = context.to_snapshot();
    let stable_prefix_message_count = 1 + usize::from(project_rules.is_some());
    let mut messages = Vec::with_capacity(
        stable_prefix_message_count
            + if context_snapshot.is_empty() { 1 } else { 2 }
            + append_only_body.len(),
    );

    // Keep provider prompt projection allowlisted and ordered:
    // stable runtime instructions, explicit compiled context, prior
    // append-only user/assistant body, then the current user or loop-control
    // input. Tool continuations travel through provider-neutral continuation
    // fields, not ad hoc ledger or artifact text rendered into messages.
    messages.push(ModelMessage::new(
        ModelMessageRole::System,
        ModelContent::text(DEFAULT_RUNTIME_BASE_INSTRUCTIONS)?,
    )?);

    if let Some(project_rules) = project_rules {
        messages.push(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&project_rules.to_stable_prefix_message_text())?,
        )?);
    }

    if !context_snapshot.is_empty() {
        messages.push(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&context_snapshot)?,
        )?);
    }

    for message in append_only_body {
        let (role, text) = match message {
            CompiledSessionMessage::User { text } => (ModelMessageRole::User, text.as_str()),
            CompiledSessionMessage::Assistant { text } => {
                (ModelMessageRole::Assistant, text.as_str())
            }
        };
        messages.push(ModelMessage::new(role, ModelContent::text(text)?)?);
    }

    messages.push(ModelMessage::new(
        ModelMessageRole::User,
        ModelContent::text(input.text())?,
    )?);

    let continuations = continuations
        .iter()
        .map(model_tool_continuation_from_snapshot)
        .collect::<Result<Vec<_>, _>>()?;

    ModelRequest::new_with_continuations_and_stable_prefix(
        model.clone(),
        messages,
        tool_specs,
        continuations,
        generation_config,
        stable_prefix_message_count,
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
