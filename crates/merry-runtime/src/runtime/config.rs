use crate::CitationCompactionPolicy;

fn default_automatic_compaction_policy() -> CitationCompactionPolicy {
    CitationCompactionPolicy::default()
}

/// Runtime-owned policy for automatic checkpoint compaction.
///
/// This controls the pre-provider hard-watermark compaction path. Manual
/// [`crate::Runtime::compact_context_once`] calls still take an explicit
/// [`CitationCompactionPolicy`] so tests and callers can run one-off compaction
/// passes without mutating runtime construction policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomaticCompactionConfig {
    enabled: bool,
    policy: CitationCompactionPolicy,
}

impl AutomaticCompactionConfig {
    /// Enables automatic hard-watermark compaction with the provided policy.
    #[must_use]
    pub fn enabled(policy: CitationCompactionPolicy) -> Self {
        Self {
            enabled: true,
            policy,
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
        }
    }

    #[must_use]
    pub fn is_enabled(self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn policy(self) -> CitationCompactionPolicy {
        self.policy
    }
}

impl Default for AutomaticCompactionConfig {
    fn default() -> Self {
        Self::enabled(default_automatic_compaction_policy())
    }
}
