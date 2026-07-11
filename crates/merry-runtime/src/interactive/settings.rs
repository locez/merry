use crate::{AutomaticCompactionConfig, SubagentConfig};
use merry_llm::{GenerationConfig, ModelName, ModelProvider, ModelRetryPolicy};
use std::{num::NonZeroU64, sync::Arc};

/// Provider-neutral primary model selection for subsequent interactive requests.
#[derive(Clone)]
pub struct InteractivePrimaryModel {
    pub(super) provider: Arc<dyn ModelProvider>,
    pub(super) model: ModelName,
    pub(super) retry_policy: ModelRetryPolicy,
}

/// Subagent admission and concurrency settings for subsequent delegated work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractiveSubagentSettings {
    pub(super) enabled: bool,
    pub(super) config: SubagentConfig,
}

impl InteractiveSubagentSettings {
    #[must_use]
    pub fn new(enabled: bool, config: SubagentConfig) -> Self {
        Self { enabled, config }
    }
}

impl InteractivePrimaryModel {
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        retry_policy: ModelRetryPolicy,
    ) -> Self {
        Self {
            provider,
            model,
            retry_policy,
        }
    }
}

/// Runtime settings applied by an interactive run at the next model-request boundary.
#[derive(Clone, Default)]
pub struct InteractiveSettingsUpdate {
    pub(super) generation_config: Option<GenerationConfig>,
    pub(super) primary_model: Option<InteractivePrimaryModel>,
    pub(super) subagents: Option<InteractiveSubagentSettings>,
    pub(super) automatic_compaction: Option<AutomaticCompactionConfig>,
    pub(super) context_window_tokens: Option<Option<NonZeroU64>>,
}

impl InteractiveSettingsUpdate {
    /// Replaces the generation configuration used by subsequent model requests.
    #[must_use]
    pub fn with_generation_config(mut self, generation_config: GenerationConfig) -> Self {
        self.generation_config = Some(generation_config);
        self
    }

    /// Replaces the primary provider and model used by subsequent model requests.
    #[must_use]
    pub fn with_primary_model(mut self, primary_model: InteractivePrimaryModel) -> Self {
        self.primary_model = Some(primary_model);
        self
    }

    /// Replaces subagent admission and concurrency settings.
    #[must_use]
    pub fn with_subagents(mut self, subagents: InteractiveSubagentSettings) -> Self {
        self.subagents = Some(subagents);
        self
    }

    /// Replaces automatic compaction policy for subsequent model requests.
    #[must_use]
    pub fn with_automatic_compaction(
        mut self,
        automatic_compaction: AutomaticCompactionConfig,
    ) -> Self {
        self.automatic_compaction = Some(automatic_compaction);
        self
    }

    /// Replaces or clears the explicit context-window override for subsequent requests.
    #[must_use]
    pub fn with_context_window_tokens(mut self, context_window_tokens: Option<NonZeroU64>) -> Self {
        self.context_window_tokens = Some(context_window_tokens);
        self
    }
}
