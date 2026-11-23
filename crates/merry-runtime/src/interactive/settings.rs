use crate::{
    CompactionConfig, PermissionAdmissionSource, PermissionReviewMode, RuntimeModelRole,
    SubagentConfig,
};
use merry_llm::{GenerationConfig, ModelName, ModelProvider, ModelRetryPolicy, ReasoningEffort};
use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::Arc,
};

/// Provider-neutral model selection for subsequent interactive requests.
#[derive(Clone)]
pub struct InteractivePrimaryModel {
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
    pub(crate) retry_policy: ModelRetryPolicy,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
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
            reasoning_effort: None,
        }
    }

    /// Attaches an optional role-specific reasoning effort override.
    #[must_use]
    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<ReasoningEffort>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }
}

/// Runtime settings applied by an interactive run at the next model-request boundary.
#[derive(Clone, Default)]
pub struct InteractiveSettingsUpdate {
    pub(super) generation_config: Option<GenerationConfig>,
    pub(super) primary_model: Option<InteractivePrimaryModel>,
    pub(super) role_models: Vec<(RuntimeModelRole, Option<InteractivePrimaryModel>)>,
    pub(super) subagents: Option<InteractiveSubagentSettings>,
    pub(super) automatic_compaction: Option<CompactionConfig>,
    pub(super) context_window_tokens: Option<Option<NonZeroU64>>,
    pub(super) max_parallel_tool_calls: Option<NonZeroUsize>,
    pub(super) permission_review_mode: Option<PermissionReviewMode>,
    pub(super) permission_admission_source: Option<Arc<dyn PermissionAdmissionSource>>,
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

    /// Replaces or clears a role-specific provider/model/effort override (such as
    /// `ContextCompaction` or `ApprovalReview`). Passing `None` falls back to primary.
    #[must_use]
    pub fn with_role_model(
        mut self,
        role: RuntimeModelRole,
        model: Option<InteractivePrimaryModel>,
    ) -> Self {
        self.role_models.push((role, model));
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
    pub fn with_automatic_compaction(mut self, automatic_compaction: CompactionConfig) -> Self {
        self.automatic_compaction = Some(automatic_compaction);
        self
    }

    /// Replaces or clears the explicit context-window override for subsequent requests.
    #[must_use]
    pub fn with_context_window_tokens(mut self, context_window_tokens: Option<NonZeroU64>) -> Self {
        self.context_window_tokens = Some(context_window_tokens);
        self
    }

    /// Replaces the maximum concurrent parallel tool calls per tool batch.
    #[must_use]
    pub fn with_max_parallel_tool_calls(mut self, limit: NonZeroUsize) -> Self {
        self.max_parallel_tool_calls = Some(limit);
        self
    }

    /// Replaces the live permission review mode for subsequent tool executions.
    #[must_use]
    pub fn with_permission_review_mode(mut self, mode: PermissionReviewMode) -> Self {
        self.permission_review_mode = Some(mode);
        self
    }

    /// Supplies or replaces the host permission admission source used by host-reviewed modes.
    #[must_use]
    pub fn with_permission_admission_source(
        mut self,
        source: Arc<dyn PermissionAdmissionSource>,
    ) -> Self {
        self.permission_admission_source = Some(source);
        self
    }
}
