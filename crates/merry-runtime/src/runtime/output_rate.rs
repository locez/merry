//! Per-attempt throughput reduction, independent of event delivery and presentation.

use crate::token_estimate::estimate_utf8_tokens;
use merry_core::{ModelOutputRate, ModelUsage, OutputTimingQuality};
use merry_llm::ModelOutputProgress;

#[derive(Default)]
pub(super) struct OutputRateTracker {
    progress: Option<ModelOutputProgress>,
}

impl OutputRateTracker {
    pub(super) fn observe(
        &mut self,
        progress: Option<ModelOutputProgress>,
    ) -> Option<ModelOutputRate> {
        self.progress = progress;
        self.rate(None)
    }

    pub(super) fn rate(&self, usage: Option<ModelUsage>) -> Option<ModelOutputRate> {
        let progress = self.progress?;
        let tokens = usage.map_or_else(
            || estimate_utf8_tokens(progress.utf8_bytes()),
            |usage| usage.output_tokens(),
        );
        let quality = if progress.timing_quality() == OutputTimingQuality::ReceiveWindow
            && !progress.reasoning_observed()
            && usage.is_some_and(|usage| {
                usage
                    .reasoning_output_tokens()
                    .is_some_and(|tokens| tokens > 0)
            }) {
            OutputTimingQuality::PartialOutput
        } else {
            progress.timing_quality()
        };
        Some(
            ModelOutputRate::new(
                tokens,
                progress.elapsed(),
                if usage.is_none() {
                    merry_core::OutputTokenSource::Estimated
                } else {
                    merry_core::OutputTokenSource::ProviderUsage
                },
            )
            .with_timing_quality(quality),
        )
    }
}
