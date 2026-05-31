//! Runtime-owned model configuration by role.
//!
//! Role-scoped model configuration is runtime-local planning state. The current
//! provider step uses only the primary role; review roles are stored for later
//! gates without affecting provider-visible request shapes.

use merry_llm::{ModelName, ModelProvider};
use std::{collections::BTreeMap, sync::Arc};

/// Runtime model role for a configured provider/model pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeModelRole {
    /// Model used by normal [`crate::Runtime::step`] provider requests.
    Primary,
    /// Model reserved for future tool risk review.
    ToolRiskReview,
    /// Model reserved for future approval review.
    ApprovalReview,
    /// Model reserved for future summary or memory work.
    SummaryMemory,
    /// Model used to compact old dynamic context into citation-backed checkpoints.
    ContextCompaction,
}

impl RuntimeModelRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::ToolRiskReview => "tool_risk_review",
            Self::ApprovalReview => "approval_review",
            Self::SummaryMemory => "summary_memory",
            Self::ContextCompaction => "context_compaction",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ModelProviderConfig {
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
}

impl ModelProviderConfig {
    pub(crate) fn new(provider: Arc<dyn ModelProvider>, model: ModelName) -> Self {
        Self { provider, model }
    }

    pub(crate) fn provider(&self) -> Arc<dyn ModelProvider> {
        Arc::clone(&self.provider)
    }

    pub(crate) fn model(&self) -> &ModelName {
        &self.model
    }
}

/// Runtime-owned role-scoped model configuration.
#[derive(Clone, Default)]
pub(crate) struct RuntimeModelConfigs {
    configs: BTreeMap<RuntimeModelRole, ModelProviderConfig>,
}

impl RuntimeModelConfigs {
    pub(crate) fn insert(
        &mut self,
        role: RuntimeModelRole,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
    ) {
        self.configs
            .insert(role, ModelProviderConfig::new(provider, model));
    }

    pub(crate) fn get(&self, role: RuntimeModelRole) -> Option<ModelProviderConfig> {
        self.configs.get(&role).cloned()
    }

    pub(crate) fn get_with_primary_fallback(
        &self,
        role: RuntimeModelRole,
    ) -> Option<ModelProviderConfig> {
        self.get(role)
            .or_else(|| self.get(RuntimeModelRole::Primary))
    }

    pub(crate) fn contains_role(&self, role: RuntimeModelRole) -> bool {
        self.configs.contains_key(&role)
    }

    #[cfg(test)]
    pub(crate) fn model_for_role(&self, role: RuntimeModelRole) -> Option<&ModelName> {
        self.configs.get(&role).map(ModelProviderConfig::model)
    }
}
