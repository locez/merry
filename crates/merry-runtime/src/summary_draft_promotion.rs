//! Crate-internal summary draft promotion lifecycle registry.
//!
//! The registry is session-owned state for explicit summary-draft promotion
//! attempts. It does not authorize promotion by judgment record id, emit
//! runtime events, append ledger facts, or expose a public API.

#![cfg_attr(not(test), allow(dead_code))]

use crate::judgment::{
    JudgmentEvidence, JudgmentRecordId, SummaryDraftAcceptance, SummaryDraftAcceptanceAuthority,
    SummaryDraftPromotionError, SummaryDraftPromotionInput,
};
use merry_core::EvidenceRef;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

const SUMMARY_DRAFT_PROMOTION_RECORD_ID_PREFIX: &str = "summary-draft-promotion-";
const SUMMARY_DRAFT_PROMOTION_RECORD_ID_ORDER_DIGITS: usize = 20;

/// Validated internal identifier for one promotion lifecycle record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct SummaryDraftPromotionRecordId(String);

impl SummaryDraftPromotionRecordId {
    fn generated(order: u64) -> Self {
        Self(format!(
            "{SUMMARY_DRAFT_PROMOTION_RECORD_ID_PREFIX}{order:0SUMMARY_DRAFT_PROMOTION_RECORD_ID_ORDER_DIGITS$}"
        ))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SummaryDraftPromotionRecordId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Lifecycle state for a summary draft promotion record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SummaryDraftPromotionState {
    /// The exact promotion input has been accepted by runtime policy but not yet
    /// written into context.
    Accepted,
    /// The exact promotion input has been written into context.
    Promoted,
    /// The exact promotion input failed context validation and is terminal.
    Rejected,
}

/// Recorded internal lifecycle entry for one summary id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SummaryDraftPromotionRecord {
    id: SummaryDraftPromotionRecordId,
    key: SummaryDraftPromotionKey,
    payload: SummaryDraftPromotionPayload,
    state: SummaryDraftPromotionState,
    commit_order: u64,
}

impl SummaryDraftPromotionRecord {
    #[must_use]
    pub(crate) fn id(&self) -> &SummaryDraftPromotionRecordId {
        &self.id
    }

    #[must_use]
    pub(crate) fn summary_id(&self) -> &str {
        self.key.summary_id()
    }

    #[must_use]
    pub(crate) fn state(&self) -> SummaryDraftPromotionState {
        self.state
    }

    #[must_use]
    pub(crate) fn commit_order(&self) -> u64 {
        self.commit_order
    }

    #[must_use]
    pub(crate) fn source_record_id(&self) -> Option<&JudgmentRecordId> {
        self.payload.source_record_id()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SummaryDraftPromotionKey {
    summary_id: String,
}

impl SummaryDraftPromotionKey {
    fn from_input(input: &SummaryDraftPromotionInput) -> Self {
        Self {
            summary_id: input.summary_id().to_owned(),
        }
    }

    fn summary_id(&self) -> &str {
        &self.summary_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SummaryDraftPromotionPayload {
    draft_text: String,
    selected_evidence: Vec<SummaryDraftPromotionEvidence>,
    acceptance: SummaryDraftPromotionAcceptance,
    source_record_id: Option<JudgmentRecordId>,
}

impl SummaryDraftPromotionPayload {
    fn from_input(input: &SummaryDraftPromotionInput) -> Self {
        Self {
            draft_text: input.draft_text().to_owned(),
            selected_evidence: input
                .selected_evidence()
                .iter()
                .map(SummaryDraftPromotionEvidence::from_judgment_evidence)
                .collect(),
            acceptance: SummaryDraftPromotionAcceptance::from_acceptance(input.acceptance()),
            source_record_id: input.source_record_id().cloned(),
        }
    }

    fn source_record_id(&self) -> Option<&JudgmentRecordId> {
        self.source_record_id.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SummaryDraftPromotionEvidence {
    label: String,
    reference: EvidenceRef,
}

impl SummaryDraftPromotionEvidence {
    fn from_judgment_evidence(evidence: &JudgmentEvidence) -> Self {
        Self {
            label: evidence.label().to_owned(),
            reference: evidence.reference().clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SummaryDraftPromotionAcceptance {
    authority: SummaryDraftAcceptanceAuthority,
    source_label: String,
    rationale: String,
}

impl SummaryDraftPromotionAcceptance {
    fn from_acceptance(acceptance: &SummaryDraftAcceptance) -> Self {
        Self {
            authority: acceptance.authority(),
            source_label: acceptance.source_label().to_owned(),
            rationale: acceptance.rationale().to_owned(),
        }
    }
}

/// Deterministic snapshot of internal summary draft promotion lifecycle records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SummaryDraftPromotionRegistrySnapshot {
    records: Vec<SummaryDraftPromotionRecord>,
}

impl SummaryDraftPromotionRegistrySnapshot {
    #[must_use]
    pub(crate) fn records(&self) -> &[SummaryDraftPromotionRecord] {
        &self.records
    }
}

/// Result of attempting to accept a promotion input into the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SummaryDraftPromotionAcceptanceResult {
    /// New accepted record was written and caller should attempt context mutation.
    Accepted(SummaryDraftPromotionRecordId),
    /// Exact input was already promoted; caller should treat promotion as idempotent.
    AlreadyPromoted,
}

/// Read-only status for an attempted promotion input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SummaryDraftPromotionAcceptanceStatus {
    /// No lifecycle record exists for this summary id.
    New,
    /// Exact input is already accepted but not promoted.
    Accepted,
    /// Exact input was already promoted.
    AlreadyPromoted,
}

/// Crate-internal session-owned summary draft promotion lifecycle registry.
#[derive(Debug, Clone, Default)]
pub(crate) struct SummaryDraftPromotionRegistry {
    records: BTreeMap<SummaryDraftPromotionKey, SummaryDraftPromotionRecord>,
    next_order: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedSummaryDraftPromotionRegistry {
    records: Vec<SummaryDraftPromotionRecord>,
}

impl SummaryDraftPromotionRegistry {
    pub(crate) fn acceptance_status(
        &self,
        input: &SummaryDraftPromotionInput,
    ) -> Result<SummaryDraftPromotionAcceptanceStatus, SummaryDraftPromotionError> {
        let key = SummaryDraftPromotionKey::from_input(input);
        let payload = SummaryDraftPromotionPayload::from_input(input);

        let Some(existing) = self.records.get(&key) else {
            return Ok(SummaryDraftPromotionAcceptanceStatus::New);
        };

        if existing.payload != payload {
            return Err(SummaryDraftPromotionError::PromotionPayloadConflict {
                summary_id: key.summary_id,
            });
        }

        match existing.state {
            SummaryDraftPromotionState::Accepted => {
                Ok(SummaryDraftPromotionAcceptanceStatus::Accepted)
            }
            SummaryDraftPromotionState::Promoted => {
                Ok(SummaryDraftPromotionAcceptanceStatus::AlreadyPromoted)
            }
            SummaryDraftPromotionState::Rejected => {
                Err(SummaryDraftPromotionError::PromotionAlreadyRejected {
                    summary_id: key.summary_id,
                })
            }
        }
    }

    pub(crate) fn accept(
        &mut self,
        input: &SummaryDraftPromotionInput,
    ) -> Result<SummaryDraftPromotionAcceptanceResult, SummaryDraftPromotionError> {
        let key = SummaryDraftPromotionKey::from_input(input);
        let payload = SummaryDraftPromotionPayload::from_input(input);

        if let Some(existing) = self.records.get(&key) {
            if existing.payload != payload {
                return Err(SummaryDraftPromotionError::PromotionPayloadConflict {
                    summary_id: key.summary_id,
                });
            }

            return match existing.state {
                SummaryDraftPromotionState::Accepted => Ok(
                    SummaryDraftPromotionAcceptanceResult::Accepted(existing.id.clone()),
                ),
                SummaryDraftPromotionState::Promoted => {
                    Ok(SummaryDraftPromotionAcceptanceResult::AlreadyPromoted)
                }
                SummaryDraftPromotionState::Rejected => {
                    Err(SummaryDraftPromotionError::PromotionAlreadyRejected {
                        summary_id: key.summary_id,
                    })
                }
            };
        }

        let id = self.next_generated_id();
        let commit_order = self.next_order;
        let record = SummaryDraftPromotionRecord {
            id: id.clone(),
            key: key.clone(),
            payload,
            state: SummaryDraftPromotionState::Accepted,
            commit_order,
        };

        self.records.insert(key, record);
        self.next_order += 1;
        Ok(SummaryDraftPromotionAcceptanceResult::Accepted(id))
    }

    pub(crate) fn mark_promoted(&mut self, id: &SummaryDraftPromotionRecordId) {
        let record = self
            .records
            .values_mut()
            .find(|record| record.id() == id)
            .expect("summary draft promotion record id accepted by this registry");
        debug_assert_eq!(record.state, SummaryDraftPromotionState::Accepted);
        record.state = SummaryDraftPromotionState::Promoted;
    }

    pub(crate) fn mark_rejected(&mut self, id: &SummaryDraftPromotionRecordId) {
        let record = self
            .records
            .values_mut()
            .find(|record| record.id() == id)
            .expect("summary draft promotion record id accepted by this registry");
        debug_assert_eq!(record.state, SummaryDraftPromotionState::Accepted);
        record.state = SummaryDraftPromotionState::Rejected;
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> SummaryDraftPromotionRegistrySnapshot {
        let mut records = self.records.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.commit_order()
                .cmp(&right.commit_order())
                .then_with(|| left.id().cmp(right.id()))
        });

        SummaryDraftPromotionRegistrySnapshot { records }
    }

    #[must_use]
    pub(crate) fn persisted(&self) -> PersistedSummaryDraftPromotionRegistry {
        PersistedSummaryDraftPromotionRegistry {
            records: self.snapshot().records,
        }
    }

    pub(crate) fn from_persisted(
        persisted: PersistedSummaryDraftPromotionRegistry,
    ) -> Result<Self, SummaryDraftPromotionError> {
        let mut records = BTreeMap::new();
        let mut next_order = 0;
        for record in persisted.records {
            if records.contains_key(&record.key) {
                return Err(SummaryDraftPromotionError::PromotionPayloadConflict {
                    summary_id: record.key.summary_id().to_owned(),
                });
            }
            next_order = next_order.max(record.commit_order().saturating_add(1));
            records.insert(record.key.clone(), record);
        }
        Ok(Self {
            records,
            next_order,
        })
    }

    fn next_generated_id(&self) -> SummaryDraftPromotionRecordId {
        let mut order = self.next_order;
        loop {
            let id = SummaryDraftPromotionRecordId::generated(order);
            if self.records.values().all(|record| record.id() != &id) {
                return id;
            }

            order = order
                .checked_add(1)
                .expect("summary draft promotion record id space is exhausted");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SummaryDraftPromotionAcceptanceResult, SummaryDraftPromotionRegistry,
        SummaryDraftPromotionState,
    };
    use crate::judgment::{
        JudgmentEvidence, SummaryDraftAcceptance, SummaryDraftAcceptanceAuthority,
        SummaryDraftPromotionError, SummaryDraftPromotionInput,
    };
    use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};

    #[test]
    fn registry_snapshot_orders_lifecycle_records_by_commit_order() {
        let mut registry = SummaryDraftPromotionRegistry::default();
        let first = match registry
            .accept(&promotion_input("summary-a", "first draft"))
            .expect("first acceptance records")
        {
            SummaryDraftPromotionAcceptanceResult::Accepted(id) => id,
            SummaryDraftPromotionAcceptanceResult::AlreadyPromoted => {
                panic!("new input cannot be already promoted")
            }
        };
        let second = match registry
            .accept(&promotion_input("summary-b", "second draft"))
            .expect("second acceptance records")
        {
            SummaryDraftPromotionAcceptanceResult::Accepted(id) => id,
            SummaryDraftPromotionAcceptanceResult::AlreadyPromoted => {
                panic!("new input cannot be already promoted")
            }
        };

        registry.mark_promoted(&second);
        registry.mark_rejected(&first);

        let snapshot = registry.snapshot();
        assert_eq!(snapshot.records().len(), 2);
        assert_eq!(snapshot.records()[0].summary_id(), "summary-a");
        assert_eq!(
            snapshot.records()[0].state(),
            SummaryDraftPromotionState::Rejected
        );
        assert_eq!(
            snapshot.records()[0].id().as_str(),
            "summary-draft-promotion-00000000000000000000"
        );
        assert_eq!(snapshot.records()[1].summary_id(), "summary-b");
        assert_eq!(
            snapshot.records()[1].state(),
            SummaryDraftPromotionState::Promoted
        );
        assert_eq!(
            snapshot.records()[1].id().as_str(),
            "summary-draft-promotion-00000000000000000001"
        );
    }

    #[test]
    fn exact_promoted_duplicate_is_idempotent_but_conflict_rejects() {
        let mut registry = SummaryDraftPromotionRegistry::default();
        let input = promotion_input("summary-a", "first draft");
        let id = match registry.accept(&input).expect("accepts new input") {
            SummaryDraftPromotionAcceptanceResult::Accepted(id) => id,
            SummaryDraftPromotionAcceptanceResult::AlreadyPromoted => {
                panic!("new input cannot be already promoted")
            }
        };
        registry.mark_promoted(&id);

        assert_eq!(
            registry
                .accept(&input)
                .expect("exact promoted replay succeeds"),
            SummaryDraftPromotionAcceptanceResult::AlreadyPromoted
        );
        assert_eq!(
            registry
                .accept(&promotion_input("summary-a", "different draft"))
                .expect_err("same summary id different payload conflicts"),
            SummaryDraftPromotionError::PromotionPayloadConflict {
                summary_id: "summary-a".to_owned(),
            }
        );
    }

    fn promotion_input(summary_id: &str, draft_text: &str) -> SummaryDraftPromotionInput {
        SummaryDraftPromotionInput::new(
            summary_id,
            draft_text,
            vec![evidence("source", "summary-source")],
            SummaryDraftAcceptance::new(
                SummaryDraftAcceptanceAuthority::HardPolicy,
                "hard policy",
                "Hard policy accepted the draft for context promotion.",
            )
            .expect("valid acceptance"),
            None,
        )
        .expect("valid promotion input")
    }

    fn evidence(label: &str, id: &str) -> JudgmentEvidence {
        JudgmentEvidence::new(
            label,
            EvidenceRef::new(
                ArtifactId::new(id).expect("valid artifact id"),
                EvidenceLocator::whole_artifact(),
            ),
        )
        .expect("valid judgment evidence")
    }
}
