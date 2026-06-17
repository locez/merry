//! Runtime step inputs, execution context, and provider request compilation.
//!
//! Public step types are provider-neutral. Request compilation is internal
//! glue from structured runtime state into `merry-llm` model requests; provider
//! crates render those normalized requests into wire formats.

use crate::{
    CompiledContext, FinalOutputContract, ProjectRules, RuntimeError, SkillCatalog, TaskAnchor,
    artifact::ArtifactContent, session::TranscriptItemSnapshot,
};
use merry_core::{PendingToolCall, ToolCallResult, ToolCallResultStatus, ToolSpec};
use merry_llm::{
    GenerationConfig, ModelContent, ModelInputItem, ModelMessage, ModelMessageRole, ModelName,
    ModelRequest, ModelToolCall, ModelToolCallId, ModelToolResult, ModelToolResultContent,
    ToolArguments,
};
use tokio_util::sync::CancellationToken;

pub(crate) const DEFAULT_RUNTIME_BASE_INSTRUCTIONS: &str = r#"You are Merry, a pragmatic coding agent.

Work from the runtime-provided project context and authorized filesystem view. Do not invent paths, tool results, or verification outcomes.

Use the registered tools for workspace reads, searches, edits, and process execution. Prefer localized patches with the smallest unique context that proves the intended edit; do not rewrite whole files for small changes.

Before reading source code files, first locate relevant symbols or strings with available search tools such as workspace_search_text, rg, or grep. Avoid whole-file source reads by default.

Whole-file reads are acceptable only when the file is small, roughly 120 lines or fewer; the file is a project instruction, config, or doc where full context matters; the task requires understanding the full module structure; or targeted search did not identify a safe smaller region. For source files over roughly 120 lines, prefer targeted reads around matched lines. For source files over roughly 250 lines, do not whole-read unless explicitly justified in analysis or the user-facing summary. If a search result and nearby line references are enough to answer where something is defined or handled, use those references instead of reading entire implementation files.

After code changes, run the most relevant available checks unless the user asks you not to or the runtime/tool policy blocks them. When a check cannot run, state exactly what remains unverified.

Respect project instructions such as AGENTS.md when present. Treat those instructions as project-specific policy layered on top of these runtime defaults."#;

pub(crate) const PROGRESS_COMMENTARY_INSTRUCTIONS: &str = r#"Prefer efficient tool execution. Do not add a progress note before routine or consecutive tool calls; call the tools directly. Emit a short progress update only when a turn begins a non-obvious plan, changes direction, waits on something slow, requests elevated capability, or is about to produce the final summary. Keep any progress updates concise and use the user's current input language. Do not include progress notes in final structured output."#;

/// Input snapshot for a runtime step.
///
/// The MVP step input is user text only. Runtime state such as context,
/// artifacts, tool continuations, and ledger facts is read from the owning
/// session rather than passed as raw chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepInput {
    user_texts: Vec<String>,
    history: StepInputHistory,
}

impl StepInput {
    /// Creates a user-text step input.
    ///
    /// Text must be non-blank and may contain newlines and tabs, but not other
    /// control characters.
    pub fn user_text(text: &str) -> Result<Self, RuntimeError> {
        Self::user_texts([text])
    }

    /// Creates a step input with multiple consecutive user messages.
    ///
    /// Each text item must be non-blank and may contain newlines and tabs, but
    /// not other control characters.
    pub fn user_texts<I, S>(texts: I) -> Result<Self, RuntimeError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let user_texts = texts
            .into_iter()
            .map(|text| {
                let text = text.as_ref();
                validate_user_text(text)?;
                Ok(text.to_owned())
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;

        if user_texts.is_empty() {
            return Err(RuntimeError::InvalidStepInput {
                reason: "user text burst must contain at least one message",
            });
        }

        Ok(Self {
            user_texts,
            history: StepInputHistory::RecordUser,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn loop_control_text(text: &str) -> Result<Self, RuntimeError> {
        validate_user_text(text)?;
        Ok(Self {
            user_texts: vec![text.to_owned()],
            history: StepInputHistory::ControlOnly,
        })
    }

    pub(crate) fn no_new_user_input() -> Self {
        Self {
            user_texts: Vec::new(),
            history: StepInputHistory::ControlOnly,
        }
    }

    /// Borrows the user text for this step.
    #[must_use]
    pub fn text(&self) -> &str {
        self.user_texts
            .first()
            .map(String::as_str)
            .expect("StepInput::text is available only for user text inputs")
    }

    /// Borrows the user texts for this step.
    #[must_use]
    pub fn texts(&self) -> &[String] {
        &self.user_texts
    }

    pub(crate) fn user_texts_for_request(&self) -> &[String] {
        &self.user_texts
    }

    pub(crate) fn user_texts_for_history(&self) -> &[String] {
        if self.history == StepInputHistory::RecordUser {
            &self.user_texts
        } else {
            &[]
        }
    }

    pub(crate) fn memory_activation_query(&self) -> Option<String> {
        (!self.user_texts.is_empty()).then(|| self.user_texts.join("\n\n"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepInputHistory {
    RecordUser,
    ControlOnly,
}

/// Context shared with runtime step producers.
///
/// The context carries cancellation and provider-neutral generation controls
/// for one step. It does not carry provider conversation state.
#[derive(Debug, Clone)]
pub struct StepContext {
    cancellation_token: CancellationToken,
    generation_config: GenerationConfig,
    final_output_contract: Option<FinalOutputContract>,
}

impl StepContext {
    /// Creates a step context with the provided cancellation token.
    #[must_use]
    pub fn new(cancellation_token: CancellationToken) -> Self {
        Self {
            cancellation_token,
            generation_config: GenerationConfig::default(),
            final_output_contract: None,
        }
    }

    /// Returns the cancellation token for this step.
    ///
    /// Runtime producers check this token at cancellation checkpoints. Dropping
    /// the returned [`crate::RuntimeJournalEventStream`] also cancels the step token.
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

    /// Adds a runtime-owned final-output contract to this step.
    #[must_use]
    pub fn with_final_output_contract(mut self, contract: FinalOutputContract) -> Self {
        self.final_output_contract = Some(contract);
        self
    }

    /// Returns provider-neutral generation controls for this step.
    #[must_use]
    pub fn generation_config(&self) -> &GenerationConfig {
        &self.generation_config
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        CancellationToken,
        GenerationConfig,
        Option<FinalOutputContract>,
    ) {
        (
            self.cancellation_token,
            self.generation_config,
            self.final_output_contract,
        )
    }
}

impl Default for StepContext {
    fn default() -> Self {
        Self {
            cancellation_token: CancellationToken::new(),
            generation_config: GenerationConfig::default(),
            final_output_contract: None,
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
    pub(crate) skill_catalog: Option<&'a SkillCatalog>,
    pub(crate) project_rules: Option<&'a ProjectRules>,
    pub(crate) task_anchor: Option<&'a TaskAnchor>,
    pub(crate) context: &'a CompiledContext,
    pub(crate) transcript: &'a [TranscriptItemSnapshot],
    pub(crate) tool_specs: Vec<ToolSpec>,
    pub(crate) generation_config: GenerationConfig,
    pub(crate) progress_commentary: bool,
}

pub(crate) fn compile_step_model_request(
    parts: StepModelRequestParts<'_>,
) -> Result<ModelRequest, merry_llm::ModelError> {
    let StepModelRequestParts {
        input,
        model,
        skill_catalog,
        project_rules,
        task_anchor,
        context,
        transcript,
        tool_specs,
        generation_config,
        progress_commentary,
    } = parts;

    let context_snapshot = context.to_snapshot();
    let skill_metadata_text = skill_catalog.and_then(SkillCatalog::to_stable_prefix_message_text);
    let stable_prefix_message_count = 1
        + usize::from(progress_commentary)
        + usize::from(skill_metadata_text.is_some())
        + usize::from(project_rules.is_some());
    let mut messages = Vec::with_capacity(
        stable_prefix_message_count
            + usize::from(task_anchor.is_some())
            + if context_snapshot.is_empty() { 1 } else { 2 }
            + transcript.len()
            + input.user_texts_for_request().len(),
    );

    // Keep provider prompt projection allowlisted and ordered:
    // stable runtime instructions, available skill metadata, project rules,
    // task anchor control-plane context, explicit compiled context, prior
    // ordered transcript, then the current user or loop-control input.
    messages.push(ModelInputItem::Message(ModelMessage::new(
        ModelMessageRole::System,
        ModelContent::text(DEFAULT_RUNTIME_BASE_INSTRUCTIONS)?,
    )?));

    if progress_commentary {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(PROGRESS_COMMENTARY_INSTRUCTIONS)?,
        )?));
    }

    if let Some(skill_metadata_text) = skill_metadata_text {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&skill_metadata_text)?,
        )?));
    }

    if let Some(project_rules) = project_rules {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&project_rules.to_stable_prefix_message_text())?,
        )?));
    }

    if let Some(task_anchor) = task_anchor {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&task_anchor.to_dynamic_control_message_text())?,
        )?));
    }

    if !context_snapshot.is_empty() {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&context_snapshot)?,
        )?));
    }

    for item in transcript {
        messages.push(model_input_from_transcript_snapshot(item)?);
    }

    for text in input.user_texts_for_request() {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::User,
            ModelContent::text(text)?,
        )?));
    }

    ModelRequest::new_with_input_and_stable_prefix(
        model.clone(),
        messages,
        tool_specs,
        generation_config,
        stable_prefix_message_count,
    )
}

fn model_input_from_transcript_snapshot(
    snapshot: &TranscriptItemSnapshot,
) -> Result<ModelInputItem, merry_llm::ModelError> {
    match snapshot {
        TranscriptItemSnapshot::UserMessage { text, .. } => Ok(ModelInputItem::Message(
            ModelMessage::new(ModelMessageRole::User, ModelContent::text(text)?)?,
        )),
        TranscriptItemSnapshot::AssistantText { text } => Ok(ModelInputItem::Message(
            ModelMessage::new(ModelMessageRole::Assistant, ModelContent::text(text)?)?,
        )),
        TranscriptItemSnapshot::ToolCall { call } => Ok(ModelInputItem::ToolCall(
            model_tool_call_from_pending(call)?,
        )),
        TranscriptItemSnapshot::ToolResult {
            call_id,
            result,
            content,
        } => {
            debug_assert_eq!(call_id, result.call_id());
            Ok(ModelInputItem::ToolResult(model_tool_result_from_result(
                result, content,
            )?))
        }
    }
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
        ArtifactContent::Text { content: text } => ModelToolResultContent::text(text),
        ArtifactContent::Json { content: json } => ModelToolResultContent::json(json),
        ArtifactContent::Binary { .. }
        | ArtifactContent::Image { .. }
        | ArtifactContent::Other { .. } => Err(merry_llm::ModelError::invalid_request(
            "tool result continuation content must be text or json",
        )),
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
    fn user_texts_preserve_multiple_user_messages() {
        let input = StepInput::user_texts(["first", "second"]).expect("valid burst");

        assert_eq!(input.texts(), ["first", "second"]);
        assert_eq!(input.text(), "first");
    }

    #[test]
    fn user_texts_reject_empty_burst() {
        let err = StepInput::user_texts(std::iter::empty::<&str>())
            .expect_err("empty burst should be rejected");

        assert!(matches!(err, RuntimeError::InvalidStepInput { .. }));
    }

    #[test]
    fn user_texts_reject_blank_item() {
        let err = StepInput::user_texts(["first", " \n\t "])
            .expect_err("blank burst item should be rejected");

        assert!(matches!(err, RuntimeError::InvalidStepInput { .. }));
    }

    #[test]
    fn no_new_user_input_has_no_request_texts() {
        let input = StepInput::no_new_user_input();

        assert!(input.texts().is_empty());
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
