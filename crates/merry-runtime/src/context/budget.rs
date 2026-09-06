use super::ContextError;
use merry_core::ContextWindowSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextBudgetPolicy {
    /// Earlier checkpoint planning for conservative prompt growth control.
    CostAware,
    /// Default compromise that uses most of the safe body budget.
    Balanced,
    /// Use nearly all of the safe body budget before checkpoint planning.
    Capacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WatermarkRule {
    hard_headroom_basis_points: u64,
    hard_headroom_min_tokens: u64,
    hard_headroom_max_tokens: u64,
    soft_band_basis_points: u64,
    soft_band_min_tokens: u64,
    soft_band_max_tokens: u64,
}

/// Derived context body budget and checkpoint watermarks.
///
/// The budget subtracts cacheable stable-prefix tokens and output reserve from
/// an effective model context window before calculating dynamic-body
/// watermarks. It does not perform token estimation or mutate runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    effective_window_tokens: u64,
    stable_prefix_tokens: u64,
    output_reserve_tokens: u64,
    body_budget_tokens: u64,
    soft_water_tokens: u64,
    hard_water_tokens: u64,
}

impl ContextBudget {
    /// Calculates dynamic-body budget watermarks from a resolved context window.
    pub fn from_window(
        resolved_context_window_tokens: u64,
        effective_context_window_percent: u8,
        stable_prefix_tokens: u64,
        output_reserve_tokens: u64,
        policy: ContextBudgetPolicy,
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

        let (soft_water_tokens, hard_water_tokens) =
            policy.watermarks(resolved_context_window_tokens, body_budget_tokens)?;

        if soft_water_tokens >= hard_water_tokens {
            return Err(ContextError::InvalidBudget {
                reason: "soft watermark must be below hard watermark",
            });
        }
        if hard_water_tokens > body_budget_tokens {
            return Err(ContextError::InvalidBudget {
                reason: "hard watermark must not exceed body budget",
            });
        }

        Ok(Self {
            effective_window_tokens,
            stable_prefix_tokens,
            output_reserve_tokens,
            body_budget_tokens,
            soft_water_tokens,
            hard_water_tokens,
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

    /// Dynamic-body watermark where checkpoint planning should begin.
    #[must_use]
    pub fn soft_water_tokens(&self) -> u64 {
        self.soft_water_tokens
    }

    /// Dynamic-body watermark where checkpointing should be required.
    #[must_use]
    pub fn hard_water_tokens(&self) -> u64 {
        self.hard_water_tokens
    }
}

impl ContextBudgetPolicy {
    fn watermarks(
        self,
        resolved_context_window_tokens: u64,
        body_budget_tokens: u64,
    ) -> Result<(u64, u64), ContextError> {
        match self {
            Self::CostAware => self.ratio_watermarks(body_budget_tokens, 60, 80),
            Self::Balanced | Self::Capacity => {
                let rule = self.watermark_rule();
                let nominal_hard_headroom = basis_points_of_window(
                    resolved_context_window_tokens,
                    rule.hard_headroom_basis_points,
                )?
                .clamp(rule.hard_headroom_min_tokens, rule.hard_headroom_max_tokens);
                let hard_headroom = nominal_hard_headroom.min(body_budget_tokens / 2);
                let nominal_soft_band = basis_points_of_window(
                    resolved_context_window_tokens,
                    rule.soft_band_basis_points,
                )?
                .clamp(rule.soft_band_min_tokens, rule.soft_band_max_tokens);
                let hard_water_tokens = body_budget_tokens.checked_sub(hard_headroom).ok_or(
                    ContextError::InvalidBudget {
                        reason: "body budget must exceed hard watermark headroom",
                    },
                )?;
                let soft_band = nominal_soft_band.min(hard_water_tokens / 2);
                let soft_water_tokens = hard_water_tokens.checked_sub(soft_band).ok_or(
                    ContextError::InvalidBudget {
                        reason: "hard watermark must exceed soft watermark band",
                    },
                )?;
                Ok((soft_water_tokens, hard_water_tokens))
            }
        }
    }

    fn ratio_watermarks(
        self,
        body_budget_tokens: u64,
        soft_percent: u64,
        hard_percent: u64,
    ) -> Result<(u64, u64), ContextError> {
        let soft_water_tokens = body_budget_tokens
            .checked_mul(soft_percent)
            .and_then(|value| value.checked_div(100))
            .ok_or(ContextError::InvalidBudget {
                reason: "soft watermark calculation overflowed",
            })?;
        let hard_water_tokens = body_budget_tokens
            .checked_mul(hard_percent)
            .and_then(|value| value.checked_div(100))
            .ok_or(ContextError::InvalidBudget {
                reason: "hard watermark calculation overflowed",
            })?;
        Ok((soft_water_tokens, hard_water_tokens))
    }

    fn watermark_rule(self) -> WatermarkRule {
        match self {
            Self::CostAware => unreachable!("cost-aware uses ratio watermarks"),
            Self::Balanced => WatermarkRule {
                hard_headroom_basis_points: 100,
                hard_headroom_min_tokens: 2_000,
                hard_headroom_max_tokens: 16_000,
                soft_band_basis_points: 300,
                soft_band_min_tokens: 8_000,
                soft_band_max_tokens: 48_000,
            },
            Self::Capacity => WatermarkRule {
                hard_headroom_basis_points: 50,
                hard_headroom_min_tokens: 1_000,
                hard_headroom_max_tokens: 8_000,
                soft_band_basis_points: 200,
                soft_band_min_tokens: 4_000,
                soft_band_max_tokens: 24_000,
            },
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CostAware => "cost_aware",
            Self::Balanced => "balanced",
            Self::Capacity => "capacity",
        }
    }
}

fn basis_points_of_window(window_tokens: u64, basis_points: u64) -> Result<u64, ContextError> {
    window_tokens
        .checked_mul(basis_points)
        .and_then(|value| value.checked_div(10_000))
        .ok_or(ContextError::InvalidBudget {
            reason: "window percentage calculation overflowed",
        })
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

/// Watermark-based checkpoint trigger decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointDecision {
    /// Dynamic body remains below the checkpoint planning watermark.
    Continue,
    /// Dynamic body reached the soft watermark; plan a checkpoint soon.
    PlanCheckpoint,
    /// Dynamic body reached the hard watermark; require checkpointing before more growth.
    RequireCheckpoint,
}

impl CheckpointDecision {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::PlanCheckpoint => "plan_checkpoint",
            Self::RequireCheckpoint => "require_checkpoint",
        }
    }
}

/// Decides whether dynamic body growth has reached checkpoint watermarks.
#[must_use]
pub fn decide_checkpoint(dynamic_body_tokens: u64, budget: ContextBudget) -> CheckpointDecision {
    if dynamic_body_tokens >= budget.hard_water_tokens() {
        CheckpointDecision::RequireCheckpoint
    } else if dynamic_body_tokens >= budget.soft_water_tokens() {
        CheckpointDecision::PlanCheckpoint
    } else {
        CheckpointDecision::Continue
    }
}
