//! Provider-neutral model and session usage snapshots.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Token usage reported by a model provider for one response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: u64,
    pub reasoning_output_tokens: Option<u64>,
    pub total_tokens: u64,
}

impl ModelUsage {
    /// Creates usage with no optional provider sub-counts.
    #[must_use]
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        let total_tokens = input_tokens
            .checked_add(output_tokens)
            .expect("model usage total token count overflowed");
        Self::with_details(input_tokens, None, output_tokens, None, total_tokens)
    }

    /// Creates usage from provider-reported counts.
    #[must_use]
    pub const fn with_details(
        input_tokens: u64,
        cached_input_tokens: Option<u64>,
        output_tokens: u64,
        reasoning_output_tokens: Option<u64>,
        total_tokens: u64,
    ) -> Self {
        Self {
            input_tokens,
            cached_input_tokens,
            output_tokens,
            reasoning_output_tokens,
            total_tokens,
        }
    }

    /// Input token count.
    #[must_use]
    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    /// Cached input token count if the provider reported it.
    #[must_use]
    pub const fn cached_input_tokens(&self) -> Option<u64> {
        self.cached_input_tokens
    }

    /// Output token count.
    #[must_use]
    pub const fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    /// Reasoning output token count if the provider reported it.
    #[must_use]
    pub const fn reasoning_output_tokens(&self) -> Option<u64> {
        self.reasoning_output_tokens
    }

    /// Provider-reported or provider-derived total token count.
    #[must_use]
    pub const fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    /// Returns the checked sum of two usage values.
    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            input_tokens: self.input_tokens.checked_add(other.input_tokens)?,
            cached_input_tokens: checked_add_optional(
                self.cached_input_tokens,
                other.cached_input_tokens,
            )?,
            output_tokens: self.output_tokens.checked_add(other.output_tokens)?,
            reasoning_output_tokens: checked_add_optional(
                self.reasoning_output_tokens,
                other.reasoning_output_tokens,
            )?,
            total_tokens: self.total_tokens.checked_add(other.total_tokens)?,
        })
    }
}

fn checked_add_optional(left: Option<u64>, right: Option<u64>) -> Option<Option<u64>> {
    match (left, right) {
        (Some(left), Some(right)) => Some(Some(left.checked_add(right)?)),
        (None, _) | (_, None) => Some(None),
    }
}

/// Runtime-owned usage snapshot for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionUsage {
    pub total: ModelUsage,
    pub last: ModelUsage,
    pub context: Option<UsageContextWindow>,
    pub compaction: Option<CompactionUsageWindow>,
}

/// Context window snapshot used by the most recent model request that produced `last`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageContextWindow {
    pub resolved_model_window_tokens: u64,
    pub effective_window_tokens: u64,
    pub source: ContextWindowSource,
}

/// Compaction budget snapshot used by the most recent model request that produced `last`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompactionUsageWindow {
    pub auto_compaction_enabled: bool,
    pub body_budget_tokens: u64,
    pub soft_water_tokens: u64,
    pub hard_water_tokens: u64,
}

/// Source used to resolve a model context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextWindowSource {
    /// Explicit runtime or caller override.
    ExplicitConfig,
    /// Provider-neutral model capabilities.
    ProviderCapabilities,
    /// Bundled model catalog metadata.
    BundledCatalog,
    /// Conservative fallback when no metadata is available.
    Fallback,
}

impl ContextWindowSource {
    /// Stable serialized label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitConfig => "explicit_config",
            Self::ProviderCapabilities => "provider_capabilities",
            Self::BundledCatalog => "bundled_catalog",
            Self::Fallback => "fallback",
        }
    }
}
