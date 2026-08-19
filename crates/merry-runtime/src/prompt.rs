//! Provider-neutral prompt composition primitives.
//!
//! The runtime owns the placement contract for stable and dynamic request
//! material. Higher-level compositions supply typed stable blocks without
//! importing provider wire types or reaching into request compilation.

use thiserror::Error;

pub(crate) const DEFAULT_RUNTIME_BASE_INSTRUCTIONS: &str = r#"<merry_runtime_instructions>
You are Merry, a software engineering agent. The user's current instruction, applicable project rules, and runtime-provided context define success.

Use the user's current input language unless the user explicitly requests another language.

Interpret the request before acting:
- For questions, explanations, reviews, and status reports, inspect the relevant evidence and answer directly. Do not make unrelated changes.
- For diagnosis, determine the cause and explain it. Do not silently turn diagnosis into implementation unless the request includes a fix.
- For requested changes or builds, carry the work through implementation and proportionate verification. Do not stop at a proposal when the next implementation step is known.

Work from evidence. Inspect the relevant repository state, source, configuration, history, or runtime results before making conclusions that depend on them. Never invent paths, source contents, tool results, test outcomes, permissions, or completed work. Search efficiently, then read enough surrounding context to understand ownership, invariants, callers, and sibling paths. Do not let a fixed line-count heuristic replace understanding.

Choose the right scope. Treat the visible symptom or example as evidence, not automatically as the whole problem. Check whether it represents a shared contract, repeated path, boundary failure, or one local case. Make the smallest change that addresses the actual class of issue, preserves existing architecture and user work, and avoids unrelated refactoring. Prefer existing project patterns and typed interfaces over ad hoc special cases.

Act autonomously within the user's intent and the current runtime authority. Make reasonable, reversible assumptions when they keep the task moving and do not materially change the user's goal. Ask for direction when a missing choice would materially change behavior, scope, external effects, or required authority.

Persist while useful paths remain. Do not stop after a fixed number of attempts. When an approach fails, use the evidence to decide whether to refine it, try a materially different reasonable approach, or identify a real blocker. Be resourceful, but do not perform disproportionate rewrites, reimplement substantial dependencies, make destructive or unrelated changes, circumvent security boundaries, brute-force low-probability retries, or change the user's goal merely to avoid reporting a blocker or requesting necessary authority.

Use the capabilities registered for the current run according to their schemas and runtime context. Tool declarations describe direct callable interfaces; they are not an exhaustive list of every reasonable way to solve a task. Treat the latest runtime context update as authoritative for current execution boundaries. Request broader capability only for an exact action that is necessary to the task, after reasonable narrower approaches have been considered, and request the minimum scope needed. Never request broader authority only for convenience or speed. Never bypass authentication, TLS, validation, sandbox, permission, or cancellation boundaries.

When editing, preserve changes you did not make and keep modifications focused. Avoid destructive source-control or filesystem actions unless the user explicitly requested them and the runtime authorizes them. Use comments only where they clarify non-obvious intent.

Verify claims in proportion to risk. Run the most relevant available checks after changes, inspect their actual results, and do not claim success from an unrun or failed check. If verification is blocked, state exactly what was verified, what remains unverified, and why.

Finish with the outcome that matters to the user: the answer or change, the evidence or verification supporting it, and any genuine remaining blocker. Keep the response concise relative to the task, but do not omit material risks or unfinished work.
</merry_runtime_instructions>"#;

pub(crate) const DEFAULT_PROGRESS_COMMENTARY_INSTRUCTIONS: &str = r#"<merry_progress_commentary>
Prefer efficient tool execution. Do not add a progress note before routine or consecutive tool calls; call the tools directly. Emit a short progress update only when a turn begins a non-obvious plan, changes direction, waits on something slow, requests elevated capability, or is about to produce the final summary. Keep any progress updates concise and use the user's current input language. Do not include progress notes in final structured output.
</merry_progress_commentary>"#;

/// One stable provider-neutral prompt block supplied by a higher-level policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptBlock {
    tag: String,
    text: String,
}

impl PromptBlock {
    /// Creates a named stable prompt block.
    pub fn new(tag: impl Into<String>, text: impl Into<String>) -> Result<Self, PromptError> {
        let tag = tag.into();
        let text = text.into();
        validate_tag(&tag)?;
        validate_text("prompt block", &text)?;
        Ok(Self { tag, text })
    }

    /// Returns the stable block tag.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Returns the exact stable block body.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn render(&self) -> String {
        let mut rendered = String::with_capacity(self.tag.len() * 2 + self.text.len() + 7);
        rendered.push('<');
        rendered.push_str(&self.tag);
        rendered.push_str(">\n");
        rendered.push_str(&self.text);
        if !self.text.ends_with('\n') {
            rendered.push('\n');
        }
        rendered.push_str("</");
        rendered.push_str(&self.tag);
        rendered.push('>');
        rendered
    }
}

/// Stable prompt material and its runtime placement contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptProfile {
    base_instructions: String,
    progress_commentary_instructions: String,
    stable_blocks: Vec<PromptBlock>,
}

impl PromptProfile {
    /// Creates a prompt profile with runtime instructions and commentary text.
    pub fn new(
        base_instructions: impl Into<String>,
        progress_commentary_instructions: impl Into<String>,
    ) -> Result<Self, PromptError> {
        let base_instructions = base_instructions.into();
        let progress_commentary_instructions = progress_commentary_instructions.into();
        validate_text("base instructions", &base_instructions)?;
        validate_text(
            "progress commentary instructions",
            &progress_commentary_instructions,
        )?;
        Ok(Self {
            base_instructions,
            progress_commentary_instructions,
            stable_blocks: Vec::new(),
        })
    }

    /// Adds one stable block after runtime instructions and before dynamic context.
    pub fn with_stable_block(mut self, block: PromptBlock) -> Result<Self, PromptError> {
        if self
            .stable_blocks
            .iter()
            .any(|item| item.tag() == block.tag())
        {
            return Err(PromptError::DuplicateBlockTag {
                tag: block.tag().to_owned(),
            });
        }
        self.stable_blocks.push(block);
        Ok(self)
    }

    /// Returns the exact runtime base instructions.
    #[must_use]
    pub fn base_instructions(&self) -> &str {
        &self.base_instructions
    }

    /// Returns the exact progress-commentary instructions.
    #[must_use]
    pub fn progress_commentary_instructions(&self) -> &str {
        &self.progress_commentary_instructions
    }

    /// Returns stable higher-level prompt blocks in provider-visible order.
    #[must_use]
    pub fn stable_blocks(&self) -> &[PromptBlock] {
        &self.stable_blocks
    }
}

impl Default for PromptProfile {
    fn default() -> Self {
        Self {
            base_instructions: DEFAULT_RUNTIME_BASE_INSTRUCTIONS.to_owned(),
            progress_commentary_instructions: DEFAULT_PROGRESS_COMMENTARY_INSTRUCTIONS.to_owned(),
            stable_blocks: Vec::new(),
        }
    }
}

/// Invalid typed prompt composition.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PromptError {
    /// A required prompt value was empty or whitespace-only.
    #[error("{field} must not be blank")]
    Blank { field: &'static str },
    /// A prompt value contains an unsupported control character.
    #[error("{field} contains an unsupported control character")]
    ControlCharacter { field: &'static str },
    /// A prompt block tag is not a valid stable identifier.
    #[error("prompt block tag must contain only ASCII letters, digits, '_' or '-': {tag:?}")]
    InvalidTag { tag: String },
    /// A prompt profile tried to reuse one stable block tag.
    #[error("prompt block tag is registered more than once: {tag}")]
    DuplicateBlockTag { tag: String },
}

fn validate_text(field: &'static str, value: &str) -> Result<(), PromptError> {
    if value.trim().is_empty() {
        return Err(PromptError::Blank { field });
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(PromptError::ControlCharacter { field });
    }
    Ok(())
}

fn validate_tag(tag: &str) -> Result<(), PromptError> {
    if tag.is_empty()
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(PromptError::InvalidTag {
            tag: tag.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PromptBlock, PromptError, PromptProfile};

    #[test]
    fn stable_blocks_render_in_registration_order() {
        let profile = PromptProfile::default()
            .with_stable_block(
                PromptBlock::new("coding_policy", "inspect before editing").expect("valid block"),
            )
            .expect("profile should accept block")
            .with_stable_block(
                PromptBlock::new("project_policy", "preserve user changes").expect("valid block"),
            )
            .expect("profile should accept block");

        assert_eq!(profile.stable_blocks()[0].tag(), "coding_policy");
        assert_eq!(profile.stable_blocks()[1].tag(), "project_policy");
        assert_eq!(
            profile.stable_blocks()[0].render(),
            "<coding_policy>\ninspect before editing\n</coding_policy>"
        );
    }

    #[test]
    fn duplicate_stable_block_tags_are_rejected() {
        let block = PromptBlock::new("coding_policy", "one").expect("valid block");
        let error = PromptProfile::default()
            .with_stable_block(block.clone())
            .expect("first block should be accepted")
            .with_stable_block(block)
            .expect_err("duplicate block should be rejected");

        assert!(matches!(error, PromptError::DuplicateBlockTag { .. }));
    }
}
