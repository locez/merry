//! Session-local feedback from complete request estimates and matching provider usage.

use super::estimate_request_input_tokens;
use merry_core::ProviderName;
use merry_llm::{ModelInputItem, ModelName, ModelRequest, RequestContentHash};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

const SCALE_PRECISION: u64 = 1_000_000;

/// Fixed-point input multiplier; estimates round upward and saturate on overflow.
/// An overflowing ratio is stored as the maximum value and fails closed on estimation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct TokenEstimateScale(NonZeroU64);

impl Default for TokenEstimateScale {
    fn default() -> Self {
        Self(NonZeroU64::MIN.saturating_add(SCALE_PRECISION - 1))
    }
}

impl TokenEstimateScale {
    /// Converts a base estimate to the calibrated input-token domain.
    pub(crate) fn estimate(self, base_tokens: u64) -> u64 {
        if base_tokens != 0 && self.0 == NonZeroU64::MAX {
            return u64::MAX;
        }
        let tokens = (u128::from(base_tokens) * u128::from(self.0.get()))
            .div_ceil(u128::from(SCALE_PRECISION));
        u64::try_from(tokens).unwrap_or(u64::MAX)
    }

    fn from_usage(base_tokens: u64, actual_tokens: u64) -> Option<Self> {
        if base_tokens == 0 || actual_tokens == 0 {
            return None;
        }
        let ratio = (u128::from(actual_tokens) * u128::from(SCALE_PRECISION))
            .div_ceil(u128::from(base_tokens));
        NonZeroU64::new(u64::try_from(ratio).unwrap_or(u64::MAX)).map(Self)
    }

    /// Corrects underestimation immediately; releases excess headroom gradually.
    fn updated(self, sample: Self) -> Self {
        if sample.0 >= self.0 {
            return sample;
        }
        let smoothed = self.0.get() - (self.0.get() - sample.0.get()) / 4;
        NonZeroU64::new(smoothed).map_or(sample, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestTokenProfile {
    provider: ProviderName,
    model: ModelName,
    stable_prefix: RequestContentHash,
    has_images: bool,
}

impl RequestTokenProfile {
    fn new(provider: &ProviderName, request: &ModelRequest) -> Self {
        Self {
            provider: provider.clone(),
            model: request.model().clone(),
            stable_prefix: request.stable_prefix_hash().clone(),
            has_images: request_has_images(request),
        }
    }

    fn matches(&self, provider: &ProviderName, request: &ModelRequest) -> bool {
        self.provider == *provider
            && self.model == *request.model()
            && self.stable_prefix == *request.stable_prefix_hash()
            && self.has_images == request_has_images(request)
    }
}

fn request_has_images(request: &ModelRequest) -> bool {
    request.input().iter().any(|item| {
        matches!(item, ModelInputItem::Message(message) if message.content().images().next().is_some())
    })
}

/// Captured after compaction and before sending, so usage cannot match a stale estimate.
pub(crate) struct RequestTokenObservation {
    profile: RequestTokenProfile,
    base_input_tokens: u64,
}

impl RequestTokenObservation {
    pub(crate) fn new(provider: &ProviderName, request: &ModelRequest) -> Self {
        Self {
            profile: RequestTokenProfile::new(provider, request),
            base_input_tokens: estimate_request_input_tokens(request),
        }
    }
}

/// Bounded to one active request profile; unrelated models and contracts start uncalibrated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestTokenCalibration {
    profile: RequestTokenProfile,
    scale: TokenEstimateScale,
}

impl RequestTokenCalibration {
    pub(crate) fn scale_for(
        &self,
        provider: &ProviderName,
        request: &ModelRequest,
    ) -> TokenEstimateScale {
        if self.profile.matches(provider, request) {
            self.scale
        } else {
            TokenEstimateScale::default()
        }
    }

    /// Returns no update for zero measurements; actual input includes cached tokens.
    pub(crate) fn observe(
        previous: Option<&Self>,
        observation: RequestTokenObservation,
        actual_input_tokens: u64,
    ) -> Option<Self> {
        let sample =
            TokenEstimateScale::from_usage(observation.base_input_tokens, actual_input_tokens)?;
        let previous_scale = previous
            .filter(|previous| previous.profile == observation.profile)
            .map_or_else(TokenEstimateScale::default, |previous| previous.scale);
        let scale = previous_scale.updated(sample);
        tracing::debug!(
            event = "runtime.context.estimate_calibrated",
            provider = observation.profile.provider.as_str(),
            model = observation.profile.model.as_str(),
            base_input_tokens = observation.base_input_tokens,
            actual_input_tokens,
            scale_parts_per_million = scale.0.get(),
            "updated primary request token estimate calibration"
        );
        Some(Self {
            profile: observation.profile,
            scale,
        })
    }
}

#[cfg(test)]
mod tests;
