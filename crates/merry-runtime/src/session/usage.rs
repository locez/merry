use super::SessionState;
use crate::token_estimate::{RequestTokenCalibration, RequestTokenObservation, TokenEstimateScale};
use merry_core::{
    CompactionUsageWindow, ErrorInfo, ModelUsage, RuntimeJournalEvent, RuntimeJournalPayload,
    SessionUsage, UsageContextWindow,
};

impl SessionState {
    pub(crate) fn token_estimate_scale(
        &self,
        provider: &merry_core::ProviderName,
        request: &merry_llm::ModelRequest,
    ) -> TokenEstimateScale {
        self.request_token_calibration
            .as_ref()
            .map_or_else(TokenEstimateScale::default, |calibration| {
                calibration.scale_for(provider, request)
            })
    }

    pub(crate) fn calibrate_request_tokens(
        &mut self,
        observation: RequestTokenObservation,
        usage: ModelUsage,
    ) {
        if let Some(calibration) = RequestTokenCalibration::observe(
            self.request_token_calibration.as_ref(),
            observation,
            usage.input_tokens(),
        ) {
            self.request_token_calibration = Some(calibration);
        }
    }

    pub(crate) fn usage(&self) -> Option<SessionUsage> {
        self.usage.clone()
    }

    pub(crate) fn record_model_usage(
        &mut self,
        model_usage: ModelUsage,
        context: Option<UsageContextWindow>,
        compaction: Option<CompactionUsageWindow>,
    ) -> Result<RuntimeJournalEvent, ErrorInfo> {
        let total = match self.usage.as_ref() {
            Some(current) => current.total.checked_add(model_usage).ok_or_else(|| {
                ErrorInfo::new(
                    "usage_overflow",
                    "session usage token totals overflowed during accumulation",
                )
                .expect("static usage overflow diagnostic is valid")
            })?,
            None => model_usage,
        };

        let subagent_total = self
            .usage
            .as_ref()
            .and_then(|current| current.subagent_total);

        let snapshot = SessionUsage {
            total,
            last: model_usage,
            context,
            compaction,
            subagent_total,
        };
        self.usage = Some(snapshot.clone());

        Ok(self.record_event(RuntimeJournalPayload::SessionUsageUpdated { usage: snapshot }))
    }

    /// Accumulates incremental token usage reported by a managed subagent into
    /// this parent session's total usage without altering the parent's own
    /// `last`, `context`, or `compaction` window snapshots.
    pub(crate) fn record_subagent_usage(
        &mut self,
        delta: ModelUsage,
    ) -> Result<RuntimeJournalEvent, ErrorInfo> {
        let overflow = || {
            ErrorInfo::new(
                "usage_overflow",
                "session usage token totals overflowed during subagent rollup",
            )
            .expect("static usage overflow diagnostic is valid")
        };
        let (total, last, context, compaction, subagent_total) = match self.usage.as_ref() {
            Some(current) => {
                let total = current.total.checked_add(delta).ok_or_else(overflow)?;
                let subagent_total = match current.subagent_total {
                    Some(prev) => prev.checked_add(delta).ok_or_else(overflow)?,
                    None => delta,
                };
                (
                    total,
                    current.last,
                    current.context,
                    current.compaction,
                    Some(subagent_total),
                )
            }
            None => (delta, ModelUsage::new(0, 0), None, None, Some(delta)),
        };

        let snapshot = SessionUsage {
            total,
            last,
            context,
            compaction,
            subagent_total,
        };
        self.usage = Some(snapshot.clone());

        Ok(self.record_event(RuntimeJournalPayload::SessionUsageUpdated { usage: snapshot }))
    }
}
