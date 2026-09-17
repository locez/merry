use super::ResolvedCitationCompactionBudget;

/// Safety room kept between a fitted request and the compaction model window.
///
/// Request sizes are byte-based estimates, so a request that exactly fills the
/// window may still be counted larger by the provider. The margin scales with the
/// room that is actually available, so a small model window can still host a
/// useful request while a large window keeps a fixed reserve.
#[must_use]
pub(crate) fn compaction_window_safety_tokens(available_tokens: u64) -> u64 {
    const PERCENT: u64 = 8;
    const MIN_TOKENS: u64 = 128;
    const MAX_TOKENS: u64 = 1_024;
    (available_tokens / PERCENT).clamp(MIN_TOKENS, MAX_TOKENS)
}

/// Reasoning allowance one compaction request reserves, as a percentage of its input.
///
/// Compaction reasoning shares the provider output ceiling with the checkpoint
/// text, and it grows with the request: the model reads every covered turn before
/// it can write the checkpoint. Sizing the reserve against the request input is
/// what gives the model room to finish.
///
/// The reserve also needs a floor, because the demand does not shrink with the
/// request. Real attempts truncated at 34,022 and 44,337 token ceilings for
/// 49,051 and 90,308 token inputs, while 59,624 and 66,956 token ceilings
/// finished for 151,458 and 180,787 token inputs. No ceiling below roughly 59,000
/// tokens finished, whatever the request size.
///
/// Reasoning has a separate, bounded window-based floor. A smaller summary
/// target must not starve reasoning and repeat the same truncated response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompactionReasoningReserve {
    percent: u64,
    floor_scale: u64,
}

impl CompactionReasoningReserve {
    /// Reserve used for a first attempt.
    pub(crate) const INITIAL: Self = Self {
        percent: 25,
        floor_scale: 1,
    };

    /// Largest reserve a retried attempt may ask for.
    const MAX_PERCENT: u64 = 100;

    /// Largest floor scaling a retried attempt may ask for.
    const MAX_FLOOR_SCALE: u64 = 4;

    /// Floor of the reasoning allowance, as a share of the compaction model window.
    ///
    /// A fifth of a 272,000-token window is 59,840 tokens, which is the smallest
    /// ceiling that finished in practice.
    const FLOOR_WINDOW_PERCENT: u64 = 22;

    /// Reasoning room is independent of the desired summary size.
    const MAX_FLOOR_TOKENS: u64 = 65_536;

    /// Returns the reserve to use after the provider truncated an attempt.
    ///
    /// A truncation proves the reserve was too small. Covering less history does
    /// not fix that on its own, because the reasoning demand shrinks with the
    /// input the model reads; the reserve ratio is what has to change. The caller
    /// still re-plans, because a larger reserve needs more window room.
    #[must_use]
    pub(crate) fn degraded(self) -> Self {
        Self {
            percent: (self.percent * 2).min(Self::MAX_PERCENT),
            // The floor covers the requests the reserve share does not reach, so a
            // retry has to raise both or it would repeat the same ceiling.
            floor_scale: (self.floor_scale * 2).min(Self::MAX_FLOOR_SCALE),
        }
    }

    /// Returns this reserve as a percentage of request input.
    #[must_use]
    pub(crate) const fn percent(self) -> u64 {
        self.percent
    }

    /// Returns the smallest reasoning allowance this reserve grants.
    #[must_use]
    fn floor(self, compactor_window_tokens: u64, _text_budget_tokens: u64) -> u64 {
        let window_share = compactor_window_tokens.saturating_mul(Self::FLOOR_WINDOW_PERCENT) / 100;
        window_share
            .min(Self::MAX_FLOOR_TOKENS)
            .saturating_mul(self.floor_scale)
    }

    /// Returns the reasoning allowance for one request input.
    #[must_use]
    fn reasoning_allowance(
        self,
        compactor_window_tokens: u64,
        text_budget_tokens: u64,
        input_tokens: u64,
    ) -> u64 {
        input_tokens
            .saturating_mul(self.percent)
            .saturating_div(100)
            .max(self.floor(compactor_window_tokens, text_budget_tokens))
    }

    /// Returns the provider `max_output_tokens` for a request with this input size.
    #[must_use]
    pub(crate) fn output_ceiling(
        self,
        resolved_budget: ResolvedCitationCompactionBudget,
        compactor_window_tokens: u64,
        input_tokens: u64,
    ) -> u64 {
        let text_budget_tokens = resolved_budget.output_token_limit();
        text_budget_tokens.saturating_add(self.reasoning_allowance(
            compactor_window_tokens,
            text_budget_tokens,
            input_tokens,
        ))
    }

    /// Returns the largest request input a compaction window can host under this reserve.
    ///
    /// A request occupies `input + text_budget + allowance(input)`, where the
    /// allowance is either the reserve share of the input or the floor. Both are
    /// monotone in the input, so the allowance is whichever term applies at the
    /// solution: the reserve share while it is at or above the floor, and the
    /// floor below it.
    #[must_use]
    pub(crate) fn allowed_input_tokens(
        self,
        compactor_window_tokens: u64,
        text_budget_tokens: u64,
    ) -> u64 {
        let usable_tokens = compactor_window_tokens.saturating_sub(text_budget_tokens);
        let floor = self.floor(compactor_window_tokens, text_budget_tokens);
        let by_percent = usable_tokens.saturating_mul(100) / (100 + self.percent);
        if by_percent.saturating_mul(self.percent) / 100 >= floor {
            by_percent
        } else {
            usable_tokens.saturating_sub(floor)
        }
    }
}

/// Safety room one refit keeps on top of the input it has to release.
///
/// Covered payload text travels into the request input almost one for one, so a
/// refit gives up the measured excess plus this much, instead of a multiple of
/// the excess that would overshoot the allowance.
const COMPACTION_REFIT_SAFETY_PERCENT: u64 = 5;

/// Share of the coverage one refit releases when the measured input already fits.
///
/// Reaching that case means the request failed on its output side, so the refit
/// has to make real progress on coverage instead of stalling on a one-token step.
const COMPACTION_REFIT_PROGRESS_STEPS: u64 = 8;

/// Returns the covered-payload budget to try after one overshoot.
///
/// Gives up the input the window cannot host plus a margin. Returns `None` when
/// the covered payload is already zero, because retaining more turns cannot
/// shrink the request any further.
#[must_use]
pub(crate) fn tightened_covered_budget(
    covered_payload_tokens: u64,
    estimated_input_tokens: u64,
    allowed_input_tokens: u64,
) -> Option<u64> {
    if covered_payload_tokens == 0 {
        return None;
    }
    let excess_input_tokens = estimated_input_tokens.saturating_sub(allowed_input_tokens);
    let step = if excess_input_tokens == 0 {
        // The measured input already fits the allowance, so this request failed on
        // its output side. Release a real share of the coverage rather than the
        // single token the excess would justify.
        covered_payload_tokens
            .div_ceil(COMPACTION_REFIT_PROGRESS_STEPS)
            .max(1)
    } else {
        let safety = excess_input_tokens.saturating_mul(COMPACTION_REFIT_SAFETY_PERCENT) / 100;
        excess_input_tokens.saturating_add(safety).max(1)
    };
    let tightened = covered_payload_tokens.saturating_sub(step);
    (tightened < covered_payload_tokens).then_some(tightened)
}
