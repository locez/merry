//! Runtime step inputs, execution context, and provider request compilation.
//!
//! Public step types are provider-neutral. Request compilation is internal
//! glue from structured runtime state into `merry-llm` model requests; provider
//! crates render those normalized requests into wire formats.

pub use crate::user_input::StepInput;
use crate::{
    CompiledContext, FinalOutputContract, ProjectRules, SkillCatalog, TaskAnchor, UserMessageInput,
    artifact::ArtifactContent, session::TranscriptItemSnapshot,
};
use merry_core::{PendingToolCall, ToolCallResult, ToolCallResultStatus, ToolSpec};
use merry_llm::{
    GenerationConfig, ModelContent, ModelInputItem, ModelMessage, ModelMessageRole, ModelName,
    ModelRequest, ModelToolCall, ModelToolCallId, ModelToolResult, ModelToolResultContent,
    ToolArguments,
};
use tokio_util::sync::CancellationToken;

pub(crate) const DEFAULT_RUNTIME_BASE_INSTRUCTIONS: &str = r#"You are Merry, a software engineering agent working through a runtime on the user's behalf.

Your goal is to genuinely handle the user's request, not merely to produce a plausible answer or complete one convenient tool call. The user's current instruction, applicable project rules, and runtime-provided context define success.

Use the user's current input language unless the user explicitly requests another language.

Interpret the request before acting:
- For questions, explanations, reviews, and status reports, inspect the relevant evidence and answer directly. Do not make unrelated changes.
- For diagnosis, determine the cause and explain it. Do not silently turn diagnosis into implementation unless the request includes a fix.
- For requested changes or builds, carry the work through implementation and proportionate verification. Do not stop at a proposal when the next implementation step is known.

Work from evidence. Inspect the relevant repository state, source, configuration, history, or runtime results before making conclusions that depend on them. Never invent paths, source contents, tool results, test outcomes, permissions, or completed work. Search efficiently, then read enough surrounding context to understand ownership, invariants, callers, and sibling paths. Do not let a fixed line-count heuristic replace understanding.

Choose the right scope. Treat the visible symptom or example as evidence, not automatically as the whole problem. Check whether it represents a shared contract, repeated path, boundary failure, or one local case. Make the smallest change that addresses the actual class of issue, preserves existing architecture and user work, and avoids unrelated refactoring. Prefer existing project patterns and typed interfaces over ad hoc special cases.

Act autonomously within the user's intent and the current runtime authority. Make reasonable, reversible assumptions when they keep the task moving and do not materially change the user's goal. Ask for direction when a missing choice would materially change behavior, scope, external effects, or required authority.

Persist while useful paths remain. Do not stop after a fixed number of attempts. When an approach fails, use the evidence to decide whether to refine it, try a materially different reasonable approach, or identify a real blocker. Be resourceful, but do not perform disproportionate rewrites, reimplement substantial dependencies, make destructive or unrelated changes, circumvent security boundaries, brute-force low-probability retries, or change the user's goal merely to avoid reporting a blocker or requesting necessary authority.

Use the capabilities registered for the current run according to their schemas and runtime context. Tool declarations describe direct callable interfaces; they are not an exhaustive list of every reasonable way to solve the task. Treat the latest runtime context update as authoritative for current execution boundaries. Request broader capability only for an exact action that is necessary to the task, after reasonable narrower approaches have been considered, and request the minimum scope needed. Never request broader authority only for convenience or speed.

When editing, preserve changes you did not make and keep modifications focused. Avoid destructive source-control or filesystem actions unless the user explicitly requested them and the runtime authorizes them. Use comments only where they clarify non-obvious intent.

Verify claims in proportion to risk. Run the most relevant available checks after changes, inspect their actual results, and do not claim success from an unrun or failed check. If verification is blocked, state exactly what was verified, what remains unverified, and why.

Finish with the outcome that matters to the user: the answer or change, the evidence or verification supporting it, and any genuine remaining blocker. Keep the response concise relative to the task, but do not omit material risks or unfinished work."#;

pub(crate) const PROGRESS_COMMENTARY_INSTRUCTIONS: &str = r#"Prefer efficient tool execution. Do not add a progress note before routine or consecutive tool calls; call the tools directly. Emit a short progress update only when a turn begins a non-obvious plan, changes direction, waits on something slow, requests elevated capability, or is about to produce the final summary. Keep any progress updates concise and use the user's current input language. Do not include progress notes in final structured output."#;

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
        plan_control,
        context,
        transcript,
        tool_specs,
        generation_config,
        progress_commentary,
    } = parts;

    let checkpoint_snapshot = context.checkpoint_snapshot();
    let context_body_snapshot = context.body_snapshot();
    let skill_metadata_text = skill_catalog.and_then(SkillCatalog::to_stable_prefix_message_text);
    let stable_prefix_message_count = 1
        + usize::from(progress_commentary)
        + usize::from(skill_metadata_text.is_some())
        + usize::from(project_rules.is_some());
    let mut messages = Vec::with_capacity(
        stable_prefix_message_count
            + usize::from(!checkpoint_snapshot.is_empty())
            + usize::from(task_anchor.is_some())
            + usize::from(plan_control.is_some())
            + usize::from(!context_body_snapshot.is_empty())
            + transcript.len()
            + input.user_messages_for_request().len(),
    );

    // Keep provider prompt projection allowlisted and ordered:
    // stable runtime instructions, available skill metadata, project rules,
    // the current checkpoint, task anchor control-plane context, live compiled
    // context, prior ordered transcript, then current user or loop-control input.
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

    if !checkpoint_snapshot.is_empty() {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&checkpoint_snapshot)?,
        )?));
    }

    if let Some(task_anchor) = task_anchor {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&task_anchor.to_dynamic_control_message_text())?,
        )?));
    }

    if let Some(plan_control) = plan_control {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(plan_control)?,
        )?));
    }

    if !context_body_snapshot.is_empty() {
        messages.push(ModelInputItem::Message(ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&context_body_snapshot)?,
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
    use super::{DEFAULT_RUNTIME_BASE_INSTRUCTIONS, StepContext, StepInput};
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
            "workspace_search_text",
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
