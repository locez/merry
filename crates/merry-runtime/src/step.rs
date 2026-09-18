//! Runtime step inputs, execution context, and provider request compilation.
//!
//! Public step types are provider-neutral. Request compilation is internal
//! glue from structured runtime state into `merry-llm` model requests; provider
//! crates render those normalized requests into wire formats.

pub use crate::user_input::StepInput;
use crate::{
    CompiledContext, FinalOutputContract, ProjectRules, PromptProfile, SkillCatalog, TaskAnchor,
    UserMessageInput, artifact::ArtifactContent, prompt::render_prompt_block,
    session::TranscriptItemSnapshot,
};
use merry_core::{PendingToolCall, ToolCallResult, ToolCallResultStatus, ToolSpec};
use merry_llm::{
    GenerationConfig, ModelContent, ModelInputItem, ModelMessage, ModelMessageRole, ModelName,
    ModelRequest, ModelToolCall, ModelToolCallId, ModelToolResult, ModelToolResultContent,
    ToolArguments,
};
use tokio_util::sync::CancellationToken;

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

pub(crate) struct StepModelRequestParts<'a> {
    pub(crate) input: &'a StepInput,
    pub(crate) model: &'a ModelName,
    pub(crate) skill_catalog: Option<&'a SkillCatalog>,
    pub(crate) project_rules: Option<&'a ProjectRules>,
    pub(crate) task_anchor: Option<&'a TaskAnchor>,
    pub(crate) plan_control: Option<&'a str>,
    pub(crate) context: &'a CompiledContext,
    pub(crate) transcript: &'a [TranscriptItemSnapshot],
    pub(crate) tool_specs: Vec<ToolSpec>,
    pub(crate) generation_config: GenerationConfig,
    pub(crate) prompt_profile: &'a PromptProfile,
    pub(crate) progress_commentary: bool,
}

/// Stable prompt material every request for one session starts with.
///
/// The agent loop and model-backed compaction share this prefix byte for byte so
/// a provider can serve the compaction request from the same cached prefix as
/// the session's normal requests. Compaction therefore appends its directive
/// and payload after the prefix instead of replacing it with a second system
/// prompt.
pub(crate) struct StablePrefixParts<'a> {
    pub(crate) prompt_profile: &'a PromptProfile,
    pub(crate) progress_commentary: bool,
    pub(crate) skill_catalog: Option<&'a SkillCatalog>,
    pub(crate) project_rules: Option<&'a ProjectRules>,
}

/// Compiles the ordered stable prefix items for one session.
///
/// The order is part of the provider-visible cache contract: base runtime
/// instructions, optional progress commentary instructions, profile stable
/// blocks, available skill metadata, then project rules. Callers that append
/// further items must not rewrite or reorder these messages.
pub(crate) fn compile_stable_prefix_items(
    parts: StablePrefixParts<'_>,
) -> Result<Vec<ModelInputItem>, merry_llm::ModelError> {
    let StablePrefixParts {
        prompt_profile,
        progress_commentary,
        skill_catalog,
        project_rules,
    } = parts;
    let mut items = Vec::with_capacity(
        2 + prompt_profile.stable_blocks().len() + usize::from(project_rules.is_some()),
    );

    items.push(ModelInputItem::Message(ModelMessage::new(
        ModelMessageRole::System,
        ModelContent::text(prompt_profile.base_instructions())?,
    )?));

    if progress_commentary {
        items.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(prompt_profile.progress_commentary_instructions())?,
        )?));
    }

    for block in prompt_profile.stable_blocks() {
        let block_text = block.render();
        items.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&block_text)?,
        )?));
    }

    if let Some(skill_metadata_text) =
        skill_catalog.and_then(SkillCatalog::to_stable_prefix_message_text)
    {
        let skill_metadata_text = render_prompt_block("merry_skill_catalog", &skill_metadata_text);
        items.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&skill_metadata_text)?,
        )?));
    }

    if let Some(project_rules) = project_rules {
        let project_rules_text = project_rules.to_stable_prefix_message_text();
        let project_rules_text = render_prompt_block("merry_project_rules", &project_rules_text);
        items.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&project_rules_text)?,
        )?));
    }

    Ok(items)
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
        plan_control,
        context,
        transcript,
        tool_specs,
        generation_config,
        prompt_profile,
        progress_commentary,
    } = parts;

    let checkpoint_snapshot = context.checkpoint_snapshot();
    let context_body_snapshot = context.body_snapshot();
    // Keep provider prompt projection allowlisted and ordered: the shared
    // stable prefix, the current checkpoint, task anchor control-plane context,
    // live compiled context, prior ordered transcript, then current user or
    // loop-control input.
    let mut messages = compile_stable_prefix_items(StablePrefixParts {
        prompt_profile,
        progress_commentary,
        skill_catalog,
        project_rules,
    })?;
    let stable_prefix_message_count = messages.len();
    messages.reserve(
        usize::from(!checkpoint_snapshot.is_empty())
            + usize::from(task_anchor.is_some())
            + usize::from(plan_control.is_some())
            + usize::from(!context_body_snapshot.is_empty())
            + transcript.len()
            + input.user_messages_for_request().len(),
    );

    if !checkpoint_snapshot.is_empty() {
        let checkpoint_text = render_prompt_block("merry_checkpoint", &checkpoint_snapshot);
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&checkpoint_text)?,
        )?));
    }

    if let Some(task_anchor) = task_anchor {
        let task_anchor_text = task_anchor.to_dynamic_control_message_text();
        let task_anchor_text = render_prompt_block("merry_task_anchor", &task_anchor_text);
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&task_anchor_text)?,
        )?));
    }

    if let Some(plan_control) = plan_control {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(plan_control)?,
        )?));
    }

    if !context_body_snapshot.is_empty() {
        let context_text = render_prompt_block("merry_compiled_context", &context_body_snapshot);
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&context_text)?,
        )?));
    }

    for item in transcript {
        messages.push(model_input_from_transcript_snapshot(item)?);
    }

    for message in input.user_messages_for_request() {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::User,
            message.model_content()?,
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
        TranscriptItemSnapshot::UserMessage { text, images, .. } => {
            let message = UserMessageInput::new(
                text,
                images.iter().map(|image| image.input().clone()).collect(),
            )
            .map_err(|error| merry_llm::ModelError::invalid_request(error.to_string()))?;
            Ok(ModelInputItem::Message(ModelMessage::new(
                ModelMessageRole::User,
                message.model_content()?,
            )?))
        }
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
    use crate::prompt::DEFAULT_RUNTIME_BASE_INSTRUCTIONS;
    use crate::{RuntimeError, UserImageInput, UserMessageInput};
    use merry_llm::{GenerationConfig, ModelContentPart};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn default_runtime_base_instructions_define_general_agent_contract() {
        let instructions = DEFAULT_RUNTIME_BASE_INSTRUCTIONS;
        for required in [
            "You are Merry, a software engineering agent",
            "Use the user's current input language",
            "Interpret the request before acting:",
            "Work from evidence.",
            "Choose the right scope.",
            "Do not stop after a fixed number of attempts.",
            "Tool declarations describe direct callable interfaces",
            "Verify claims in proportion to risk.",
            "Finish with the outcome that matters to the user",
        ] {
            assert!(
                instructions.contains(required),
                "base prompt must contain {required:?}"
            );
        }
        for forbidden in [
            "OpenAI",
            "Anthropic",
            "GPT-",
            "roughly 120",
            "roughly 250",
            "merry_outer_sandbox:",
        ] {
            assert!(
                !instructions.contains(forbidden),
                "base prompt must not contain {forbidden:?}"
            );
        }
    }

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
    fn user_message_input_compiles_images_before_the_full_labeled_text() {
        let message = UserMessageInput::new(
            "inspect [Image #1]",
            vec![
                UserImageInput::png(
                    "[Image #1]",
                    Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]),
                    2,
                    3,
                )
                .expect("valid image"),
            ],
        )
        .expect("valid message");

        let content = message.model_content().expect("content should compile");
        assert_eq!(content.parts().len(), 4);
        assert!(matches!(
            &content.parts()[0],
            ModelContentPart::Text { text } if text == "<image name=[Image #1]>"
        ));
        assert!(matches!(
            &content.parts()[1],
            ModelContentPart::Image { image } if image.label() == "[Image #1]"
        ));
        assert!(matches!(
            &content.parts()[2],
            ModelContentPart::Text { text } if text == "</image>"
        ));
        assert!(matches!(
            &content.parts()[3],
            ModelContentPart::Text { text } if text == "inspect [Image #1]"
        ));
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
