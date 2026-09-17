use crate::CitationCompactionPolicy;
use merry_llm::ReasoningEffort;

fn default_automatic_compaction_policy() -> CitationCompactionPolicy {
    CitationCompactionPolicy::default()
}

/// Runtime-owned policy for checkpoint compaction.
///
/// `enabled` and `policy` drive the pre-provider hard-watermark path. Manual
/// [`crate::Runtime::compact_context_once`] calls still take an explicit
/// [`CitationCompactionPolicy`] so tests and callers can run one-off compaction
/// passes without mutating runtime construction policy.
///
/// `reasoning_effort` is not path-specific: it applies to every compaction
/// request, automatic or manual, which is why this type is named for compaction
/// rather than for one of its callers.
///
/// Compaction is a summarization turn over the whole covered window, so it never
/// inherits the primary model's reasoning effort: a primary tuned for hard
/// coding turns can spend its entire output budget reasoning about history and
/// never write the checkpoint. `None` leaves the provider default in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionConfig {
    enabled: bool,
    policy: CitationCompactionPolicy,
    reasoning_effort: Option<ReasoningEffort>,
}

impl CompactionConfig {
    /// Enables automatic hard-watermark compaction with the provided policy.
    #[must_use]
    pub fn enabled(policy: CitationCompactionPolicy) -> Self {
        Self {
            enabled: true,
            policy,
            reasoning_effort: None,
        }
    }

    /// Disables automatic hard-watermark compaction.
    ///
    /// The policy remains populated with defaults so disabled configs can be
    /// inspected or re-enabled by callers without constructing a dummy policy.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            policy: default_automatic_compaction_policy(),
            reasoning_effort: None,
        }
    }

    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn policy(&self) -> CitationCompactionPolicy {
        self.policy
    }

    /// Returns a copy configured with a reasoning-effort level for compaction.
    #[must_use]
    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<ReasoningEffort>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    /// Optional reasoning-effort level for compaction model requests.
    #[must_use]
    pub fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.reasoning_effort.as_ref()
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self::enabled(default_automatic_compaction_policy())
    }
}
