//! Provider-neutral stable prompt blocks and dynamic-context counters.

use crate::{
    TrajectoryRecordId,
    trajectory::serialization::{optional_u64_as_string, u64_as_string},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A stable provider-visible prompt block retained once per session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryPromptBlock {
    id: TrajectoryRecordId,
    sequence_order: u32,
    content: String,
    truncated: bool,
}

impl TrajectoryPromptBlock {
    /// Creates a prompt block with its stable identity and complete content.
    #[must_use]
    pub fn new(
        id: TrajectoryRecordId,
        sequence_order: u32,
        content: String,
        truncated: bool,
    ) -> Self {
        Self {
            id,
            sequence_order,
            content,
            truncated,
        }
    }

    /// Borrows the stable block identifier.
    #[must_use]
    pub fn id(&self) -> &TrajectoryRecordId {
        &self.id
    }

    /// Returns the provider-visible order of this block.
    #[must_use]
    pub fn sequence_order(&self) -> u32 {
        self.sequence_order
    }

    /// Borrows the complete prompt content.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns the legacy truncation marker.
    ///
    /// New runtime projections retain complete content and return `false`.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Prompt evidence kept separately from conversation records.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryPromptSnapshot {
    stable_blocks: Vec<TrajectoryPromptBlock>,
    #[serde(with = "u64_as_string")]
    #[schemars(
        with = "String",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    dynamic_context_count: u64,
    #[serde(with = "optional_u64_as_string")]
    #[schemars(
        with = "Option<String>",
        extend(
            "x-merry-wire-type" = "u64",
            "x-merry-output-required" = true
        )
    )]
    latest_dynamic_sequence: Option<u64>,
}

impl TrajectoryPromptSnapshot {
    /// Borrows stable provider-prefix blocks in request order.
    #[must_use]
    pub fn stable_blocks(&self) -> &[TrajectoryPromptBlock] {
        &self.stable_blocks
    }

    /// Returns how many dynamic context messages were observed.
    #[must_use]
    pub fn dynamic_context_count(&self) -> u64 {
        self.dynamic_context_count
    }

    /// Returns the latest request sequence that carried dynamic context.
    #[must_use]
    pub fn latest_dynamic_sequence(&self) -> Option<u64> {
        self.latest_dynamic_sequence
    }

    /// Inserts or replaces a stable block and reports whether it changed.
    pub fn upsert_stable_block(&mut self, block: TrajectoryPromptBlock) -> bool {
        if let Some(existing) = self
            .stable_blocks
            .iter_mut()
            .find(|item| item.id() == block.id())
        {
            if *existing == block {
                return false;
            }
            *existing = block;
        } else {
            self.stable_blocks.push(block);
        }
        self.stable_blocks
            .sort_by_key(|item| (item.sequence_order(), item.id().as_str().to_owned()));
        true
    }

    /// Records dynamic context messages from one compiled request.
    pub fn add_dynamic_context(&mut self, count: u64, sequence: u64) {
        self.dynamic_context_count = self.dynamic_context_count.saturating_add(count);
        if count > 0 {
            self.latest_dynamic_sequence = Some(
                self.latest_dynamic_sequence
                    .map_or(sequence, |current| current.max(sequence)),
            );
        }
    }
}
