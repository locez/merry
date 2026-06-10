use super::SessionState;
use crate::ledger::LedgerFactKind;
use merry_core::{
    CompactionUsageWindow, ErrorInfo, ModelUsage, RuntimeJournalEvent, RuntimeJournalPayload,
    SessionUsage, UsageContextWindow,
};

impl SessionState {
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

        let snapshot = SessionUsage {
            total,
            last: model_usage,
            context,
            compaction,
        };
        self.usage = Some(snapshot.clone());

        Ok(self.record_event(
            RuntimeJournalPayload::SessionUsageUpdated { usage: snapshot },
            LedgerFactKind::SessionUsageUpdated,
        ))
    }
}
