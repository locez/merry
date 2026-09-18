use super::CompactionError;

/// Summary acceptance limits and preferred raw-history retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CitationCompactionPolicy {
    target_output_tokens: Option<u64>,
    max_accepted_output_bytes: Option<usize>,
    retained_model_turns: usize,
    one_shot_retained_tool_exchanges: usize,
}

/// Prompt guidance as a share of the destination window. This is not an
/// acceptance limit: the model may exceed it up to the hard ceiling.
const GUIDANCE_WINDOW_PERCENT: u64 = 5;
const MIN_GUIDANCE_TOKENS: u64 = 512;
const MAX_GUIDANCE_TOKENS: u64 = 8_192;
/// Hard acceptance is generous enough that a typical overshoot does not spend
/// another model call, without starving the compaction request on a small
/// window. Below 32k the share stays at 10%; at 32k and above it is 15% or
/// 2.5× guidance, whichever is larger, then clamped to 20480. A 128k window
/// therefore accepts an 8569-token summary; a 12k window still has room to
/// host the request; a 2M window saturates at 20480 rather than 15% of 2M.
const ACCEPTANCE_OVERSHOOT_NUMERATOR: u64 = 5;
const ACCEPTANCE_OVERSHOOT_DENOMINATOR: u64 = 2;
const SMALL_WINDOW_TOKENS: u64 = 32_000;
const SMALL_WINDOW_ACCEPTANCE_PERCENT: u64 = 10;
const ACCEPTANCE_WINDOW_PERCENT: u64 = 15;
const MIN_ACCEPTANCE_TOKENS: u64 = 1_024;
const MAX_ACCEPTANCE_TOKENS: u64 = 20_480;
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
    /// Optional override of the hard rendered-summary ceiling, not provider output.
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

    /// Resolves prompt guidance, hard acceptance, and the preferred raw tail.
    ///
    /// These three numbers stay independent: the prompt aims at the soft target,
    /// validation accepts anything up to the hard ceiling, and installation
    /// reserves that ceiling plus the retained-history target. Rejects a zero
    /// window and arithmetic overflow.
    pub fn resolve(
        self,
        primary_window_tokens: u64,
    ) -> Result<ResolvedCitationCompactionBudget, CompactionError> {
        if primary_window_tokens == 0 {
            return Err(CompactionError::InvalidPolicy {
                field: "primary_window_tokens",
            });
        }
        let (target_output_tokens, automatic_acceptance) =
            window_summary_limits(primary_window_tokens)?;
        let retained_history_token_target =
            window_share(primary_window_tokens, RETAINED_HISTORY_WINDOW_PERCENT)?
                .clamp(1, MAX_RETAINED_HISTORY_TOKENS);
        let output_token_limit = self.target_output_tokens.unwrap_or(automatic_acceptance);
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

fn window_share(window_tokens: u64, percent: u64) -> Result<u64, CompactionError> {
    window_tokens
        .checked_mul(percent)
        .and_then(|value| value.checked_div(100))
        .ok_or(CompactionError::BudgetOverflow)
}

/// Returns `(guidance, acceptance)` for one destination window.
fn window_summary_limits(window_tokens: u64) -> Result<(u64, u64), CompactionError> {
    let guidance = window_share(window_tokens, GUIDANCE_WINDOW_PERCENT)?
        .clamp(MIN_GUIDANCE_TOKENS, MAX_GUIDANCE_TOKENS);
    let scaled = guidance
        .checked_mul(ACCEPTANCE_OVERSHOOT_NUMERATOR)
        .and_then(|value| value.checked_div(ACCEPTANCE_OVERSHOOT_DENOMINATOR))
        .ok_or(CompactionError::BudgetOverflow)?;
    let share_percent = if window_tokens < SMALL_WINDOW_TOKENS {
        SMALL_WINDOW_ACCEPTANCE_PERCENT
    } else {
        ACCEPTANCE_WINDOW_PERCENT
    };
    let window_share_tokens = window_share(window_tokens, share_percent)?.max(1);
    let unclamped = if window_tokens < SMALL_WINDOW_TOKENS {
        window_share_tokens
    } else {
        scaled.max(window_share_tokens)
    };
    let mut acceptance = unclamped.clamp(MIN_ACCEPTANCE_TOKENS, MAX_ACCEPTANCE_TOKENS);
    if window_tokens < SMALL_WINDOW_TOKENS {
        acceptance = acceptance.min((window_tokens / 8).max(1));
    }
    let acceptance = acceptance.min(window_tokens.saturating_sub(1).max(1));
    Ok((guidance.min(acceptance), acceptance))
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

/// Soft guidance, hard acceptance, and the preferred raw-history budget.
///
/// The install-time body target is not stored here. The runtime builds it from
/// this hard ceiling plus the retained-history target and the fixed request body,
/// rather than from half the hard watermark.
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

    /// Prompt guidance written into the compaction instruction.
    /// Exceeding it is allowed within [`Self::output_token_limit`].
    #[must_use]
    pub fn target_output_tokens(self) -> u64 {
        self.target_output_tokens
    }

    #[must_use]
    /// Hard rendered-summary ceiling, including restored keep entries and framing.
    /// Crossing it repairs; staying under it installs without another model call.
    pub fn output_token_limit(self) -> u64 {
        self.output_token_limit
    }

    #[must_use]
    /// Maximum accepted candidate JSON size in bytes.
    pub fn max_accepted_output_bytes(self) -> usize {
        self.max_accepted_output_bytes
    }
}
