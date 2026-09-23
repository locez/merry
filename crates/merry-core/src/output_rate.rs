//! Provider-observed output throughput projected by the runtime.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Source of the token count, independent of the quality of the receive interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputTokenSource {
    /// UTF-8 byte estimate of observed output.
    Estimated,
    /// Total output tokens reported by the provider, including reported reasoning.
    ProviderUsage,
}

/// Limitations of the client-observed output window; never a server decode clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputTimingQuality {
    /// First-to-last effective output received without consumer backpressure.
    #[default]
    ReceiveWindow,
    /// Summarized, redacted, or otherwise unobserved reasoning makes the window partial.
    PartialOutput,
    /// Bounded delivery paused reading; the client receive rate includes consumer stalls.
    ConsumerLimited,
}

/// One output-rate observation with separate token and timing provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelOutputRate {
    output_tokens: u64,
    elapsed_nanos: u64,
    token_source: OutputTokenSource,
    timing_quality: OutputTimingQuality,
}

impl ModelOutputRate {
    /// Creates a sample using the provider's first-to-last output interval.
    /// Zero-duration samples are valid but cannot yield a rate.
    #[must_use]
    pub fn new(output_tokens: u64, elapsed: Duration, token_source: OutputTokenSource) -> Self {
        Self {
            output_tokens,
            elapsed_nanos: u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
            token_source,
            timing_quality: OutputTimingQuality::ReceiveWindow,
        }
    }

    /// Attaches the receive-window limitation observed by the provider adapter.
    #[must_use]
    pub const fn with_timing_quality(mut self, quality: OutputTimingQuality) -> Self {
        self.timing_quality = quality;
        self
    }

    /// Returns how the numerator was obtained.
    #[must_use]
    pub const fn token_source(self) -> OutputTokenSource {
        self.token_source
    }

    /// Returns whether the denominator covers only partial output or consumer stalls.
    #[must_use]
    pub const fn timing_quality(self) -> OutputTimingQuality {
        self.timing_quality
    }

    /// Whether token estimation or limited timing requires approximate presentation.
    #[must_use]
    pub const fn is_estimated(self) -> bool {
        matches!(self.token_source, OutputTokenSource::Estimated)
            || !matches!(self.timing_quality, OutputTimingQuality::ReceiveWindow)
    }

    /// Output token count; includes reasoning when reported in the provider's output total.
    #[must_use]
    pub const fn output_tokens(self) -> u64 {
        self.output_tokens
    }

    /// Receive-side output duration, excluding time before first output and after last output.
    #[must_use]
    pub const fn elapsed(self) -> Duration {
        Duration::from_nanos(self.elapsed_nanos)
    }

    /// Observed client receive throughput, or None for zero duration.
    /// Timing limitations retain an approximate rate; consult `is_estimated` and `timing_quality`.
    #[must_use]
    pub fn tokens_per_second(self) -> Option<f64> {
        (!self.elapsed().is_zero())
            .then(|| self.output_tokens as f64 / self.elapsed().as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_uses_fractional_seconds_and_rejects_unmeasurable_intervals() {
        assert_eq!(
            ModelOutputRate::new(
                25,
                Duration::from_millis(250),
                crate::OutputTokenSource::ProviderUsage
            )
            .tokens_per_second(),
            Some(100.0)
        );
        assert_eq!(
            ModelOutputRate::new(25, Duration::ZERO, crate::OutputTokenSource::ProviderUsage)
                .tokens_per_second(),
            None
        );
        assert_eq!(
            ModelOutputRate::new(
                0,
                Duration::from_secs(1),
                crate::OutputTokenSource::ProviderUsage
            )
            .tokens_per_second(),
            Some(0.0)
        );
    }

    #[test]
    fn timing_limitations_downgrade_precision_without_discarding_receive_rate() {
        for quality in [
            OutputTimingQuality::PartialOutput,
            OutputTimingQuality::ConsumerLimited,
        ] {
            for source in [
                OutputTokenSource::Estimated,
                OutputTokenSource::ProviderUsage,
            ] {
                let rate = ModelOutputRate::new(100, Duration::from_secs(2), source)
                    .with_timing_quality(quality);
                assert_eq!(rate.tokens_per_second(), Some(50.0));
                assert!(rate.is_estimated());
                assert_eq!(rate.token_source(), source);
                assert_eq!(rate.timing_quality(), quality);

                let untimed =
                    ModelOutputRate::new(100, Duration::ZERO, source).with_timing_quality(quality);
                assert_eq!(untimed.tokens_per_second(), None);
            }
        }
    }

    #[test]
    fn rate_serialization_has_explicit_units_and_preserves_estimate_status() {
        let rate = ModelOutputRate::new(
            50,
            Duration::from_secs(2),
            crate::OutputTokenSource::Estimated,
        );
        let value = serde_json::to_value(rate).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"output_tokens":50,"elapsed_nanos":2_000_000_000u64,"token_source":"estimated","timing_quality":"receive_window"})
        );
        assert_eq!(
            serde_json::from_value::<ModelOutputRate>(value).unwrap(),
            rate
        );
    }
}
