use super::SessionState;
#[cfg(test)]
use crate::summary_draft_promotion::SummaryDraftPromotionRegistrySnapshot;
use crate::{
    context::ContextEntry,
    judgment::{
        JudgmentError, JudgmentEvidence, JudgmentOutcome, JudgmentRecord, JudgmentRequest,
        SummaryDraftPromotionError, SummaryDraftPromotionInput,
        context_summary_from_accepted_summary_draft, validate_summary_draft_record_purpose,
    },
    summary_draft_promotion::{
        SummaryDraftPromotionAcceptanceResult, SummaryDraftPromotionAcceptanceStatus,
    },
};

impl SessionState {
    #[allow(dead_code)]
    pub(crate) fn preflight_judgment_request(
        &self,
        request: &JudgmentRequest,
    ) -> Result<(), JudgmentError> {
        self.validate_judgment_evidence_refs(request.evidence())
    }

    #[allow(dead_code)]
    pub(crate) fn record_judgment(
        &mut self,
        request: JudgmentRequest,
        outcome: JudgmentOutcome,
    ) -> Result<JudgmentRecord, JudgmentError> {
        self.validate_judgment_evidence(&request, &outcome)?;
        self.judgments.record_completed(request, outcome)
    }

    #[allow(dead_code)]
    pub(crate) fn record_summary_draft_judgment(
        &mut self,
        request: JudgmentRequest,
        outcome: JudgmentOutcome,
    ) -> Result<JudgmentRecord, JudgmentError> {
        validate_summary_draft_record_purpose(&request, &outcome)?;
        self.record_judgment(request, outcome)
    }

    #[allow(dead_code)]
    pub(crate) fn promote_summary_draft_to_context(
        &mut self,
        request: &JudgmentRequest,
        outcome: &JudgmentOutcome,
        input: SummaryDraftPromotionInput,
    ) -> Result<(), SummaryDraftPromotionError> {
        let summary = context_summary_from_accepted_summary_draft(request, outcome, &input)?;
        let acceptance_status = self.summary_draft_promotions.acceptance_status(&input)?;

        if self.context_entries.iter().any(|entry| {
            matches!(
                entry,
                ContextEntry::Summary(existing) if existing.id() == summary.id()
            )
        }) && acceptance_status != SummaryDraftPromotionAcceptanceStatus::AlreadyPromoted
        {
            return Err(SummaryDraftPromotionError::DuplicateSummaryId {
                summary_id: summary.id().to_owned(),
            });
        }

        let acceptance = self.summary_draft_promotions.accept(&input)?;
        let record_id = match acceptance {
            SummaryDraftPromotionAcceptanceResult::Accepted(record_id) => record_id,
            SummaryDraftPromotionAcceptanceResult::AlreadyPromoted => return Ok(()),
        };

        if let Err(error) = self.record_checked_context_entry(ContextEntry::summary(summary)) {
            self.summary_draft_promotions.mark_rejected(&record_id);
            return Err(error.into());
        }

        self.summary_draft_promotions.mark_promoted(&record_id);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn summary_draft_promotion_snapshot(&self) -> SummaryDraftPromotionRegistrySnapshot {
        self.summary_draft_promotions.snapshot()
    }

    #[allow(dead_code)]
    pub(crate) fn judgment_records(&self) -> Vec<JudgmentRecord> {
        self.judgments.snapshot().records().to_vec()
    }

    #[allow(dead_code)]
    fn validate_judgment_evidence(
        &self,
        request: &JudgmentRequest,
        outcome: &JudgmentOutcome,
    ) -> Result<(), JudgmentError> {
        self.validate_judgment_evidence_refs(request.evidence().iter().chain(outcome.evidence()))
    }

    #[allow(dead_code)]
    fn validate_judgment_evidence_refs<'a>(
        &self,
        evidence_refs: impl IntoIterator<Item = &'a JudgmentEvidence>,
    ) -> Result<(), JudgmentError> {
        for evidence in evidence_refs {
            self.artifacts
                .validate_evidence(evidence.reference())
                .map_err(|source| JudgmentError::UnreadableEvidence {
                    artifact_id: evidence.reference().artifact_id.clone(),
                    source,
                })?;
        }

        Ok(())
    }
}
