#![cfg_attr(not(test), allow(dead_code))]

use super::{
    core::{JudgmentOutcome, JudgmentRequest},
    error::JudgmentError,
    payload::{render_outcome_payload, render_request_payload, validate_record_id},
};
use std::{collections::BTreeMap, fmt};

const JUDGMENT_RECORD_ID_PREFIX: &str = "judgment-record-";
const JUDGMENT_RECORD_ID_ORDER_DIGITS: usize = 20;

/// Validated internal identifier for completed judgment audit records.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct JudgmentRecordId(String);

impl JudgmentRecordId {
    pub(crate) fn new(value: &str) -> Result<Self, JudgmentError> {
        validate_record_id(value)?;
        Ok(Self(value.to_owned()))
    }

    fn generated(order: u64) -> Self {
        Self(format!(
            "{JUDGMENT_RECORD_ID_PREFIX}{order:0JUDGMENT_RECORD_ID_ORDER_DIGITS$}"
        ))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JudgmentRecordId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Internal artifact identifier for judgment audit payloads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct JudgmentInternalArtifactId(String);

impl JudgmentInternalArtifactId {
    fn for_record(record_id: &JudgmentRecordId, kind: JudgmentInternalArtifactKind) -> Self {
        Self(format!("{}-{}", record_id.as_str(), kind.as_str()))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JudgmentInternalArtifactId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JudgmentInternalArtifactKind {
    Request,
    Outcome,
}

impl JudgmentInternalArtifactKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Outcome => "outcome",
        }
    }
}

/// Internal exact payload artifact for judgment audit records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JudgmentInternalArtifact {
    id: JudgmentInternalArtifactId,
    content: String,
}

impl JudgmentInternalArtifact {
    fn new(id: JudgmentInternalArtifactId, content: String) -> Self {
        Self { id, content }
    }

    #[must_use]
    pub(crate) fn id(&self) -> &JudgmentInternalArtifactId {
        &self.id
    }

    #[must_use]
    pub(crate) fn content(&self) -> &str {
        &self.content
    }
}

/// Internal request/outcome artifact pair for a completed judgment record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JudgmentRecordArtifacts {
    request: JudgmentInternalArtifact,
    outcome: JudgmentInternalArtifact,
}

impl JudgmentRecordArtifacts {
    fn new(
        record_id: &JudgmentRecordId,
        order: u64,
        request: &JudgmentRequest,
        outcome: &JudgmentOutcome,
    ) -> Self {
        let request = JudgmentInternalArtifact::new(
            JudgmentInternalArtifactId::for_record(
                record_id,
                JudgmentInternalArtifactKind::Request,
            ),
            render_request_payload(record_id, order, request),
        );
        let outcome = JudgmentInternalArtifact::new(
            JudgmentInternalArtifactId::for_record(
                record_id,
                JudgmentInternalArtifactKind::Outcome,
            ),
            render_outcome_payload(record_id, order, outcome),
        );

        Self { request, outcome }
    }

    #[must_use]
    pub(crate) fn request(&self) -> &JudgmentInternalArtifact {
        &self.request
    }

    #[must_use]
    pub(crate) fn outcome(&self) -> &JudgmentInternalArtifact {
        &self.outcome
    }
}

/// Completed internal advisory judgment audit record.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JudgmentRecord {
    id: JudgmentRecordId,
    request: JudgmentRequest,
    outcome: JudgmentOutcome,
    artifacts: JudgmentRecordArtifacts,
    commit_order: u64,
}

impl JudgmentRecord {
    #[must_use]
    pub(crate) fn id(&self) -> &JudgmentRecordId {
        &self.id
    }

    #[must_use]
    pub(crate) fn request(&self) -> &JudgmentRequest {
        &self.request
    }

    #[must_use]
    pub(crate) fn outcome(&self) -> &JudgmentOutcome {
        &self.outcome
    }

    #[must_use]
    pub(crate) fn artifacts(&self) -> &JudgmentRecordArtifacts {
        &self.artifacts
    }

    #[must_use]
    pub(crate) fn commit_order(&self) -> u64 {
        self.commit_order
    }
}

/// Deterministic snapshot of completed judgment audit records.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JudgmentRegistrySnapshot {
    records: Vec<JudgmentRecord>,
}

impl JudgmentRegistrySnapshot {
    #[must_use]
    pub(crate) fn records(&self) -> &[JudgmentRecord] {
        &self.records
    }
}

/// Crate-internal completed judgment audit registry.
#[derive(Debug, Clone, Default)]
pub(crate) struct JudgmentRegistry {
    records: BTreeMap<JudgmentRecordId, JudgmentRecord>,
    next_order: u64,
}

impl JudgmentRegistry {
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn record_completed(
        &mut self,
        request: JudgmentRequest,
        outcome: JudgmentOutcome,
    ) -> Result<JudgmentRecord, JudgmentError> {
        let id = self.next_generated_id();
        self.record_completed_with_id(id, request, outcome)
    }

    pub(crate) fn record_completed_with_id(
        &mut self,
        id: JudgmentRecordId,
        request: JudgmentRequest,
        outcome: JudgmentOutcome,
    ) -> Result<JudgmentRecord, JudgmentError> {
        if self.records.contains_key(&id) {
            return Err(JudgmentError::DuplicateRecordId { id });
        }

        validate_record_purpose(&request, &outcome)?;

        let commit_order = self.next_order;
        let artifacts = JudgmentRecordArtifacts::new(&id, commit_order, &request, &outcome);
        let record = JudgmentRecord {
            id: id.clone(),
            request,
            outcome,
            artifacts,
            commit_order,
        };

        self.records.insert(id, record.clone());
        self.next_order += 1;
        Ok(record)
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> JudgmentRegistrySnapshot {
        let mut records = self.records.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.commit_order()
                .cmp(&right.commit_order())
                .then_with(|| left.id().cmp(right.id()))
        });

        JudgmentRegistrySnapshot { records }
    }

    fn next_generated_id(&self) -> JudgmentRecordId {
        let mut order = self.next_order;
        loop {
            let id = JudgmentRecordId::generated(order);
            if !self.records.contains_key(&id) {
                return id;
            }

            order = order
                .checked_add(1)
                .expect("judgment record id space is exhausted");
        }
    }
}

fn validate_record_purpose(
    request: &JudgmentRequest,
    outcome: &JudgmentOutcome,
) -> Result<(), JudgmentError> {
    if request.purpose() != outcome.purpose() {
        return Err(JudgmentError::RecordPurposeMismatch {
            request_purpose: request.purpose(),
            outcome_purpose: outcome.purpose(),
        });
    }

    Ok(())
}
