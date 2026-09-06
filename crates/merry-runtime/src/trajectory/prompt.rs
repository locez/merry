//! Stable prompt and dynamic-context projection into trajectory records.

use crate::{
    ProjectRules, PromptProfile, SkillCatalog,
    trajectory::{RuntimeObservability, records::record_id},
};
use merry_core::{TrajectoryEvent, TrajectoryPromptBlock};
use merry_llm::{ModelInputItem, ModelMessageRole, ModelRequest};

impl RuntimeObservability {
    /// Seeds stable prompt material for a new or legacy-resumed session.
    ///
    /// New-format resumed sessions restore the persisted projection instead;
    /// this fallback only supplies initial navigation data when no projection
    /// was stored by an older session format.
    pub(crate) fn seed_stable_prompt(&self, blocks: &[String]) {
        let mut state = self.lock_state();
        if state.snapshot.is_closed() || !state.snapshot.prompt().stable_blocks().is_empty() {
            return;
        }
        for (index, content) in blocks.iter().enumerate() {
            let Some(block) = prompt_block(content, index as u32) else {
                continue;
            };
            state.snapshot.upsert_prompt_block(block);
        }
    }

    pub(crate) fn seed_prompt_profile(
        &self,
        profile: &PromptProfile,
        progress_commentary: bool,
        skill_catalog: Option<&SkillCatalog>,
        project_rules: Option<&ProjectRules>,
    ) {
        let mut blocks = vec![profile.base_instructions().to_owned()];
        if progress_commentary {
            blocks.push(profile.progress_commentary_instructions().to_owned());
        }
        blocks.extend(profile.stable_blocks().iter().map(|block| block.render()));
        if let Some(skill_catalog) = skill_catalog
            && let Some(text) = skill_catalog.to_stable_prefix_message_text()
        {
            blocks.push(format_prompt_block("merry_skill_catalog", &text));
        }
        if let Some(project_rules) = project_rules {
            blocks.push(format_prompt_block(
                "merry_project_rules",
                &project_rules.to_stable_prefix_message_text(),
            ));
        }
        self.seed_stable_prompt(&blocks);
    }

    /// Projects provider-visible prompt evidence into the session-level prompt
    /// snapshot. Prompt blocks are deduplicated by content identity and
    /// dynamic context is represented by an aggregate count rather than one
    /// repeated row per model request.
    ///
    /// The request is the runtime-owned normalized boundary, so this keeps the
    /// trajectory aligned with what the provider actually receives while
    /// leaving provider wire formats outside the projection.
    pub(crate) fn observe_model_request(&self, request: &ModelRequest, sequence: u64) {
        let mut stable_blocks = Vec::new();
        let mut dynamic_context_count = 0_u64;
        for (index, item) in request.input().iter().enumerate() {
            let ModelInputItem::Message(message) = item else {
                continue;
            };
            if message.role() != ModelMessageRole::System {
                continue;
            }

            let content = message.content().as_text();
            if index < request.stable_prefix_item_count() {
                if let Some(block) = prompt_block(content, index as u32) {
                    stable_blocks.push(block);
                }
            } else {
                dynamic_context_count = dynamic_context_count.saturating_add(1);
            }
        }

        if stable_blocks.is_empty() && dynamic_context_count == 0 {
            return;
        }
        let event = {
            let mut state = self.lock_state();
            if state.snapshot.is_closed() {
                return;
            }
            let mut changed = false;
            for block in stable_blocks {
                changed |= state.snapshot.upsert_prompt_block(block);
            }
            if dynamic_context_count > 0 {
                state
                    .snapshot
                    .add_dynamic_context(dynamic_context_count, sequence);
                changed = true;
            }
            state.snapshot.advance_latest_sequence(sequence);
            if !changed {
                return;
            }
            state.snapshot.advance_revision();
            TrajectoryEvent::PromptUpdated {
                revision: state.snapshot.revision(),
                latest_sequence: state.snapshot.latest_sequence(),
                prompt: state.snapshot.prompt().clone(),
            }
        };
        let _ = self.updates.send(event);
    }
}

pub(super) fn prompt_block(content: &str, sequence_order: u32) -> Option<TrajectoryPromptBlock> {
    let identity = format!("{sequence_order}:{content}");
    let id = record_id("prompt", &identity)?;
    Some(TrajectoryPromptBlock::new(
        id,
        sequence_order,
        content.to_owned(),
        false,
    ))
}

pub(super) fn format_prompt_block(tag: &str, content: &str) -> String {
    format!("<{tag}>{content}</{tag}>")
}
