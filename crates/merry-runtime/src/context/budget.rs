use super::ContextError;
use merry_core::ContextWindowSource;

/// Share of the dynamic body budget that starts automatic compaction.
///
/// This is the same single percentage grok-build uses for `should_auto_compact`.
pub const AUTO_COMPACT_THRESHOLD_PERCENT: u8 = 85;

/// Derived context body budget and the one compaction trigger.
///
/// The budget subtracts cacheable stable-prefix tokens and output reserve from
/// an effective model context window. Automatic compaction starts when the
/// dynamic body reaches [`AUTO_COMPACT_THRESHOLD_PERCENT`] of that body budget.
/// Whether a request can be sent is still the body budget itself. The budget
/// does not perform token estimation or mutate runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    effective_window_tokens: u64,
    stable_prefix_tokens: u64,
    output_reserve_tokens: u64,
    body_budget_tokens: u64,
    compact_at_tokens: u64,
}

impl ContextBudget {
    /// Calculates the dynamic-body budget and its compaction trigger.
    pub fn from_window(
        resolved_context_window_tokens: u64,
        effective_context_window_percent: u8,
        stable_prefix_tokens: u64,
        output_reserve_tokens: u64,
    ) -> Result<Self, ContextError> {
        if !(1..=100).contains(&effective_context_window_percent) {
            return Err(ContextError::InvalidBudget {
                reason: "effective context window percent must be between 1 and 100",
            });
        }

        let effective_window_tokens = resolved_context_window_tokens
            .checked_mul(u64::from(effective_context_window_percent))
            .and_then(|value| value.checked_div(100))
            .ok_or(ContextError::InvalidBudget {
                reason: "effective context window calculation overflowed",
            })?;
        let reserved_tokens = stable_prefix_tokens
            .checked_add(output_reserve_tokens)
            .ok_or(ContextError::InvalidBudget {
                reason: "reserved context tokens overflowed",
            })?;
        let body_budget_tokens = effective_window_tokens.checked_sub(reserved_tokens).ok_or(
            ContextError::InvalidBudget {
                reason: "effective context window must exceed stable prefix and output reserve",
            },
        )?;
        if body_budget_tokens == 0 {
            return Err(ContextError::InvalidBudget {
                reason: "body budget must be greater than zero",
            });
        }

        let compact_at_tokens = compaction_threshold_tokens(body_budget_tokens)?;

        Ok(Self {
            effective_window_tokens,
            stable_prefix_tokens,
            output_reserve_tokens,
            body_budget_tokens,
            compact_at_tokens,
        })
    }

    /// Context window after applying the effective window percentage.
    #[must_use]
    pub fn effective_window_tokens(&self) -> u64 {
        self.effective_window_tokens
    }

    /// Tokens reserved for cacheable stable-prefix messages and tool profile.
    #[must_use]
    pub fn stable_prefix_tokens(&self) -> u64 {
        self.stable_prefix_tokens
    }

    /// Tokens reserved for model output.
    #[must_use]
    pub fn output_reserve_tokens(&self) -> u64 {
        self.output_reserve_tokens
    }

    /// Remaining token budget for dynamic body content.
    #[must_use]
    pub fn body_budget_tokens(&self) -> u64 {
        self.body_budget_tokens
    }

    /// Dynamic-body size that starts automatic compaction.
    #[must_use]
    pub fn compact_at_tokens(&self) -> u64 {
        self.compact_at_tokens
    }
}

fn compaction_threshold_tokens(body_budget_tokens: u64) -> Result<u64, ContextError> {
    let scaled = body_budget_tokens
        .checked_mul(u64::from(AUTO_COMPACT_THRESHOLD_PERCENT))
        .ok_or(ContextError::InvalidBudget {
            reason: "compaction threshold calculation overflowed",
        })?;
    Ok(scaled.div_ceil(100).clamp(1, body_budget_tokens))
}

/// Resolved model context window and the metadata source that supplied it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedContextWindow {
    tokens: u64,
    source: ContextWindowSource,
}

impl ResolvedContextWindow {
    /// Resolved context window size in tokens.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        self.tokens
    }

    /// Source that supplied the resolved context window.
    #[must_use]
    pub fn source(&self) -> ContextWindowSource {
        self.source
    }
}

/// Resolves context window metadata without provider probing.
pub fn resolve_context_window(
    explicit_override: Option<u64>,
    provider_capability: Option<u64>,
    bundled_catalog_value: Option<u64>,
    fallback: u64,
) -> Result<ResolvedContextWindow, ContextError> {
    let (tokens, source) = if let Some(tokens) = explicit_override {
        (tokens, ContextWindowSource::ExplicitConfig)
    } else if let Some(tokens) = provider_capability {
        (tokens, ContextWindowSource::ProviderCapabilities)
    } else if let Some(tokens) = bundled_catalog_value {
        (tokens, ContextWindowSource::BundledCatalog)
    } else {
        (fallback, ContextWindowSource::Fallback)
    };

    if tokens == 0 {
        return Err(ContextError::InvalidContextWindow {
            reason: "resolved context window must be greater than zero",
        });
    }

    Ok(ResolvedContextWindow { tokens, source })
}

/// Conservative bundled context window lookup for well-known model identifiers.
///
/// Used only when neither an explicit `context_window_tokens` override nor
/// provider-declared `ModelCapabilities::max_input_tokens` is configured.
/// Unknown or custom model names return `None` so callers fall back to
/// [`super::DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS`].
#[must_use]
pub fn bundled_model_context_window(model: &str) -> Option<u64> {
    let base = model
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    if base.starts_with("claude-3")
        || base.starts_with("claude-sonnet-4")
        || base.starts_with("claude-opus-4")
        || base.starts_with("claude-haiku-4")
    {
        return Some(200_000);
    }
    if base.starts_with("gpt-4.1") {
        return Some(1_047_576);
    }
    if base.starts_with("gpt-5") {
        return Some(400_000);
    }
    if base.starts_with("gpt-4o") || base.starts_with("gpt-4-turbo") {
        return Some(128_000);
    }
    if base == "o1"
        || base.starts_with("o1-")
        || base == "o3"
        || base.starts_with("o3-")
        || base.starts_with("o4-mini")
    {
        return Some(200_000);
    }
    if base.starts_with("gemini-2.5") || base.starts_with("gemini-2.0") {
        return Some(1_048_576);
    }
    None
}

/// Whether the dynamic body has reached the compaction threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointDecision {
    /// Dynamic body remains below the compaction threshold.
    Continue,
    /// Dynamic body reached the compaction threshold.
    Compact,
}

impl CheckpointDecision {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Compact => "compact",
        }
    }
}

/// Decides whether dynamic body growth has reached the compaction threshold.
#[must_use]
pub fn decide_checkpoint(dynamic_body_tokens: u64, budget: ContextBudget) -> CheckpointDecision {
    if dynamic_body_tokens >= budget.compact_at_tokens() {
        CheckpointDecision::Compact
    } else {
        CheckpointDecision::Continue
    }
}
