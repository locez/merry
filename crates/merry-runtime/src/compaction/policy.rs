use super::CompactionError;

/// Summary acceptance limits and preferred raw-history retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CitationCompactionPolicy {
    target_output_tokens: Option<u64>,
    max_accepted_output_bytes: Option<usize>,
    retained_model_turns: usize,
    one_shot_retained_tool_exchanges: usize,
}

const SOFT_CHECKPOINT_WINDOW_PERCENT: u64 = 3;
const HARD_CHECKPOINT_WINDOW_PERCENT: u64 = 10;
const MIN_CHECKPOINT_TARGET_TOKENS: u64 = 512;
const MAX_CHECKPOINT_TARGET_TOKENS: u64 = 8_192;
const MIN_CHECKPOINT_OUTPUT_TOKENS: u64 = 1_024;
const MAX_CHECKPOINT_OUTPUT_TOKENS: u64 = 16_384;
const RETAINED_HISTORY_WINDOW_PERCENT: u64 = 10;
const MAX_RETAINED_HISTORY_TOKENS: u64 = 32_768;
/// Bytes per token used to convert an accepted-checkpoint byte cap into tokens.
///
/// This is a size ceiling with slack, not the runtime's estimation ratio
/// ([`crate::token_estimate`]): the cap is deliberately looser than the estimate
/// so a checkpoint that fits the token budget is never rejected on byte count.
const DEFAULT_ACCEPTED_OUTPUT_BYTES_PER_TOKEN: u64 = 8;
const DEFAULT_RETAINED_MODEL_TURNS: usize = 5;
/// Newest covered exchanges retained on the first cache-breaking attempt.
const DEFAULT_ONE_SHOT_RETAINED_TOOL_EXCHANGES: usize = 5;

impl CitationCompactionPolicy {
    /// Validates explicit summary limits and a nonzero retained-turn preference.
    pub fn new(
        target_output_tokens: Option<u64>,
        max_accepted_output_bytes: Option<usize>,
        retained_model_turns: usize,
    ) -> Result<Self, CompactionError> {
        if target_output_tokens == Some(0) {
            return Err(CompactionError::InvalidPolicy {
                field: "target_output_tokens",
            });
        }
        if max_accepted_output_bytes == Some(0) {
            return Err(CompactionError::InvalidPolicy {
                field: "max_accepted_output_bytes",
            });
        }
        if retained_model_turns == 0 {
            return Err(CompactionError::InvalidPolicy {
                field: "retained_model_turns",
            });
        }

        Ok(Self {
            target_output_tokens,
            max_accepted_output_bytes,
            retained_model_turns,
            one_shot_retained_tool_exchanges: DEFAULT_ONE_SHOT_RETAINED_TOOL_EXCHANGES,
        })
    }

    #[must_use]
    /// Optional override of the rendered summary ceiling, not provider output.
    pub fn target_output_tokens(self) -> Option<u64> {
        self.target_output_tokens
    }

    #[must_use]
    /// Optional upper bound on candidate JSON bytes accepted from the model.
    pub fn max_accepted_output_bytes(self) -> Option<usize> {
        self.max_accepted_output_bytes
    }

    #[must_use]
    /// Preferred completed turns to retain; fitting may choose a shorter tail.
    pub fn retained_model_turns(self) -> usize {
        self.retained_model_turns
    }

    #[must_use]
    /// Recent covered exchanges initially kept after prefix reuse cannot fit.
    pub fn one_shot_retained_tool_exchanges(self) -> usize {
        self.one_shot_retained_tool_exchanges
    }

    /// Sets how many recent covered tool exchanges a rebuilt request first keeps.
    #[must_use]
    pub fn with_one_shot_retained_tool_exchanges(self, retained_tool_exchanges: usize) -> Self {
        Self {
            one_shot_retained_tool_exchanges: retained_tool_exchanges,
            ..self
        }
    }

    /// Changes retention without resetting other limits; rejects zero.
    pub fn with_retained_model_turns(
        self,
        retained_model_turns: usize,
    ) -> Result<Self, CompactionError> {
        if retained_model_turns == 0 {
            return Err(CompactionError::InvalidPolicy {
                field: "retained_model_turns",
            });
        }
        Ok(Self {
            retained_model_turns,
            ..self
        })
    }

    /// Resolves bounded summary limits for the destination window; rejects zero and overflow.
    pub fn resolve(
        self,
        primary_window_tokens: u64,
    ) -> Result<ResolvedCitationCompactionBudget, CompactionError> {
        if primary_window_tokens == 0 {
            return Err(CompactionError::InvalidPolicy {
                field: "primary_window_tokens",
            });
        }
        let target_output_tokens = primary_window_tokens
            .checked_mul(SOFT_CHECKPOINT_WINDOW_PERCENT)
            .and_then(|value| value.checked_div(100))
            .ok_or(CompactionError::BudgetOverflow)?
            .clamp(MIN_CHECKPOINT_TARGET_TOKENS, MAX_CHECKPOINT_TARGET_TOKENS);
        let automatic = primary_window_tokens
            .checked_mul(HARD_CHECKPOINT_WINDOW_PERCENT)
            .and_then(|value| value.checked_div(100))
            .ok_or(CompactionError::BudgetOverflow)?
            .clamp(MIN_CHECKPOINT_OUTPUT_TOKENS, MAX_CHECKPOINT_OUTPUT_TOKENS)
            .min((primary_window_tokens / 8).max(1));
        let retained_history_token_target = primary_window_tokens
            .checked_mul(RETAINED_HISTORY_WINDOW_PERCENT)
            .and_then(|value| value.checked_div(100))
            .ok_or(CompactionError::BudgetOverflow)?
            .clamp(1, MAX_RETAINED_HISTORY_TOKENS);
        let output_token_limit = self.target_output_tokens.unwrap_or(automatic);
        let derived_bytes = output_token_limit
            .checked_mul(DEFAULT_ACCEPTED_OUTPUT_BYTES_PER_TOKEN)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(CompactionError::BudgetOverflow)?;

        Ok(ResolvedCitationCompactionBudget {
            target_output_tokens: target_output_tokens.min(output_token_limit),
            retained_history_token_target,
            output_token_limit,
            max_accepted_output_bytes: self.max_accepted_output_bytes.unwrap_or(derived_bytes),
        })
    }
}

impl Default for CitationCompactionPolicy {
    fn default() -> Self {
        Self {
            target_output_tokens: None,
            max_accepted_output_bytes: None,
            retained_model_turns: DEFAULT_RETAINED_MODEL_TURNS,
            one_shot_retained_tool_exchanges: DEFAULT_ONE_SHOT_RETAINED_TOOL_EXCHANGES,
        }
    }
}

/// Summary ceilings and preferred raw-history budget for a destination window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCitationCompactionBudget {
    target_output_tokens: u64,
    retained_history_token_target: u64,
    output_token_limit: u64,
    max_accepted_output_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CitationCompactionInputPolicy {
    pub(super) resolved_budget: ResolvedCitationCompactionBudget,
}

impl CitationCompactionInputPolicy {
    pub(crate) const fn new(
        _policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
    ) -> Self {
        Self { resolved_budget }
    }
}

impl ResolvedCitationCompactionBudget {
    /// Preferred retained-history size, independent of summary and reasoning limits.
    pub(crate) const fn retained_history_token_target(self) -> u64 {
        self.retained_history_token_target
    }

    /// Window-derived soft target; exceeding it is allowed within the hard ceiling.
    #[must_use]
    pub fn target_output_tokens(self) -> u64 {
        self.target_output_tokens
    }

    #[must_use]
    /// Maximum accepted rendered summary size, including checkpoint framing.
    pub fn output_token_limit(self) -> u64 {
        self.output_token_limit
    }

    #[must_use]
    /// Maximum accepted candidate JSON size in bytes.
    pub fn max_accepted_output_bytes(self) -> usize {
        self.max_accepted_output_bytes
    }
}
