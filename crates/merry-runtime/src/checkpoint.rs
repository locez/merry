use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

mod reference;

use reference::PersistedCheckpointRef;
pub use reference::{CheckpointRef, CheckpointRefManifest, CheckpointSourceKind};

const DEFAULT_MAX_REF_EXCERPT_BYTES: usize = 1200;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CheckpointError {
    #[error("{field} must not be blank")]
    BlankField { field: &'static str },

    #[error("{field} must not contain control characters other than newline or tab")]
    InvalidControlCharacter { field: &'static str },

    #[error("{field} must be a provider-safe identifier")]
    InvalidIdentifier { field: &'static str },

    #[error("checkpoint sequence range start {start} must be <= end {end}")]
    InvalidSequenceRange { start: u64, end: u64 },

    #[error("checkpoint candidate JSON is invalid")]
    InvalidCandidateJson,

    #[error("checkpoint candidate must include at least one claim")]
    EmptyClaims,

    #[error("checkpoint claim {claim_id} has no refs")]
    ClaimWithoutRefs { claim_id: String },

    #[error("checkpoint working_intent has text but no refs")]
    WorkingIntentWithoutRefs,

    #[error("checkpoint claim {claim_id} references unknown ref {ref_id}")]
    UnknownRef { claim_id: String, ref_id: String },

    #[error("checkpoint pinned ref {ref_id} does not exist in the ref manifest")]
    PinnedRefNotFound { ref_id: String },

    #[error("manual checkpoint ref {ref_id} uses the runtime-reserved history ref namespace")]
    ManualCheckpointHistoryRefReserved { ref_id: String },

    #[error("checkpoint ref {ref_id} is duplicated")]
    DuplicateRef { ref_id: String },

    #[error("legacy checkpoint excerpt ref {ref_id} has no original artifact evidence")]
    LegacyExcerptRefUnsupported { ref_id: String },

    #[error("checkpoint claim {claim_id} is duplicated")]
    DuplicateClaim { claim_id: String },

    #[error("checkpoint ref {ref_id} does not exist in checkpoint {checkpoint_id}")]
    RefNotFound {
        checkpoint_id: String,
        ref_id: String,
    },

    #[error("checkpoint output is {actual_bytes} bytes, above accepted byte cap {max_bytes}")]
    OutputTooLarge {
        actual_bytes: usize,
        max_bytes: usize,
    },

    #[error(
        "checkpoint working_intent confidence {value} is outside the inclusive 0.0..=1.0 range"
    )]
    InvalidConfidence { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckpointId(String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckpointClaimId(String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckpointRefId(String);

macro_rules! impl_checkpoint_id {
    ($type:ident, $field:literal) => {
        impl $type {
            pub fn new(value: &str) -> Result<Self, CheckpointError> {
                validate_identifier($field, value)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

impl_checkpoint_id!(CheckpointId, "checkpoint id");
impl_checkpoint_id!(CheckpointClaimId, "checkpoint claim id");
impl_checkpoint_id!(CheckpointRefId, "checkpoint ref id");

impl CheckpointRefId {
    pub(crate) fn is_runtime_history_ref(&self) -> bool {
        self.as_str().strip_prefix('h').is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointSequenceRange {
    start: u64,
    end: u64,
}

impl CheckpointSequenceRange {
    pub fn new(start: u64, end: u64) -> Result<Self, CheckpointError> {
        if start > end {
            return Err(CheckpointError::InvalidSequenceRange { start, end });
        }

        Ok(Self { start, end })
    }

    #[must_use]
    pub fn start(&self) -> u64 {
        self.start
    }

    #[must_use]
    pub fn end(&self) -> u64 {
        self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointClaimKind {
    CurrentState,
    CompletedAction,
    RejectedPath,
    CorrectedMisunderstanding,
    Constraint,
    OpenQuestion,
    NextStep,
    Verification,
}

impl CheckpointClaimKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CurrentState => "current_state",
            Self::CompletedAction => "completed_action",
            Self::RejectedPath => "rejected_path",
            Self::CorrectedMisunderstanding => "corrected_misunderstanding",
            Self::Constraint => "constraint",
            Self::OpenQuestion => "open_question",
            Self::NextStep => "next_step",
            Self::Verification => "verification",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedCitationBackedCheckpoint {
    id: String,
    claims: Vec<PersistedCheckpointClaim>,
    refs: Vec<PersistedCheckpointRef>,
    working_intent: Option<PersistedCheckpointWorkingIntent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedCheckpointClaim {
    id: String,
    kind: CheckpointClaimKind,
    text: String,
    refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedCheckpointWorkingIntent {
    text: String,
    refs: Vec<String>,
    confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointClaim {
    id: CheckpointClaimId,
    kind: CheckpointClaimKind,
    text: String,
    refs: Vec<CheckpointRefId>,
}

impl CheckpointClaim {
    #[must_use]
    pub fn id(&self) -> &CheckpointClaimId {
        &self.id
    }

    #[must_use]
    pub fn kind(&self) -> CheckpointClaimKind {
        self.kind
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn refs(&self) -> &[CheckpointRefId] {
        &self.refs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointWorkingIntent {
    text: String,
    refs: Vec<CheckpointRefId>,
    confidence_millis: u16,
}

impl CheckpointWorkingIntent {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn refs(&self) -> &[CheckpointRefId] {
        &self.refs
    }

    #[must_use]
    pub fn confidence(&self) -> f32 {
        f32::from(self.confidence_millis) / 1000.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactedCheckpointCandidate {
    claims: Vec<CheckpointClaim>,
    working_intent: Option<CheckpointWorkingIntent>,
}

impl CompactedCheckpointCandidate {
    pub fn from_json(input: &str) -> Result<Self, CheckpointError> {
        let wire = serde_json::from_str::<CompactedCheckpointCandidateWire>(input)
            .map_err(|_| CheckpointError::InvalidCandidateJson)?;
        wire.try_into()
    }

    #[must_use]
    pub fn claims(&self) -> &[CheckpointClaim] {
        &self.claims
    }

    #[must_use]
    pub fn working_intent(&self) -> Option<&CheckpointWorkingIntent> {
        self.working_intent.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationBackedCheckpoint {
    id: CheckpointId,
    claims: Vec<CheckpointClaim>,
    manifest: CheckpointRefManifest,
    working_intent: Option<CheckpointWorkingIntent>,
}

impl CitationBackedCheckpoint {
    pub fn from_candidate(
        id: CheckpointId,
        candidate: CompactedCheckpointCandidate,
        manifest: CheckpointRefManifest,
        policy: CheckpointValidationPolicy,
    ) -> Result<Self, CheckpointError> {
        Self::from_candidate_with_pinned_refs(id, candidate, manifest, policy, &BTreeSet::new())
    }

    pub(crate) fn from_candidate_with_pinned_refs(
        id: CheckpointId,
        candidate: CompactedCheckpointCandidate,
        manifest: CheckpointRefManifest,
        _policy: CheckpointValidationPolicy,
        pinned_refs: &BTreeSet<CheckpointRefId>,
    ) -> Result<Self, CheckpointError> {
        let mut seen_claims = BTreeSet::new();
        let mut used_refs = pinned_refs.clone();
        let manifest_refs = manifest
            .refs()
            .iter()
            .map(|item| item.id().clone())
            .collect::<BTreeSet<_>>();
        for ref_id in pinned_refs {
            if !manifest_refs.contains(ref_id) {
                return Err(CheckpointError::PinnedRefNotFound {
                    ref_id: ref_id.as_str().to_owned(),
                });
            }
        }

        for claim in candidate.claims() {
            if !seen_claims.insert(claim.id().clone()) {
                return Err(CheckpointError::DuplicateClaim {
                    claim_id: claim.id().as_str().to_owned(),
                });
            }
            if claim.refs().is_empty() {
                return Err(CheckpointError::ClaimWithoutRefs {
                    claim_id: claim.id().as_str().to_owned(),
                });
            }
            for ref_id in claim.refs() {
                if !manifest_refs.contains(ref_id) {
                    return Err(CheckpointError::UnknownRef {
                        claim_id: claim.id().as_str().to_owned(),
                        ref_id: ref_id.as_str().to_owned(),
                    });
                }
                used_refs.insert(ref_id.clone());
            }
        }

        if let Some(intent) = candidate.working_intent() {
            if intent.refs().is_empty() {
                return Err(CheckpointError::WorkingIntentWithoutRefs);
            }
            for ref_id in intent.refs() {
                if !manifest_refs.contains(ref_id) {
                    return Err(CheckpointError::UnknownRef {
                        claim_id: "working_intent".to_owned(),
                        ref_id: ref_id.as_str().to_owned(),
                    });
                }
                used_refs.insert(ref_id.clone());
            }
        }

        Ok(Self {
            id: id.clone(),
            claims: candidate.claims,
            manifest: manifest.filtered_for_checkpoint(id, &used_refs)?,
            working_intent: candidate.working_intent,
        })
    }

    #[must_use]
    pub fn id(&self) -> &CheckpointId {
        &self.id
    }

    #[must_use]
    pub fn claims(&self) -> &[CheckpointClaim] {
        &self.claims
    }

    #[must_use]
    pub fn manifest(&self) -> &CheckpointRefManifest {
        &self.manifest
    }

    #[must_use]
    pub fn working_intent(&self) -> Option<&CheckpointWorkingIntent> {
        self.working_intent.as_ref()
    }

    #[must_use]
    pub fn render_prompt_text(&self) -> String {
        let mut lines = Vec::with_capacity(self.claims.len() + 2);
        lines.push("claims:".to_owned());
        for claim in &self.claims {
            lines.push(format!(
                "- {} {} [{}]: {}",
                claim.id().as_str(),
                claim.kind().as_str(),
                format_ref_list(claim.refs()),
                claim.text()
            ));
        }
        if let Some(intent) = &self.working_intent {
            lines.push(format!(
                "working_intent [{}] confidence={:.3}: {}",
                format_ref_list(intent.refs()),
                intent.confidence(),
                intent.text()
            ));
        }
        lines.join("\n")
    }

    pub fn read_ref(&self, ref_id: &CheckpointRefId) -> Result<&CheckpointRef, CheckpointError> {
        let Some(item) = self.manifest.get(ref_id) else {
            return Err(CheckpointError::RefNotFound {
                checkpoint_id: self.id.as_str().to_owned(),
                ref_id: ref_id.as_str().to_owned(),
            });
        };
        Ok(item)
    }

    pub(crate) fn persisted(&self) -> PersistedCitationBackedCheckpoint {
        PersistedCitationBackedCheckpoint {
            id: self.id().as_str().to_owned(),
            claims: self
                .claims()
                .iter()
                .map(|claim| PersistedCheckpointClaim {
                    id: claim.id().as_str().to_owned(),
                    kind: claim.kind(),
                    text: claim.text().to_owned(),
                    refs: claim
                        .refs()
                        .iter()
                        .map(|ref_id| ref_id.as_str().to_owned())
                        .collect(),
                })
                .collect(),
            refs: self
                .manifest()
                .refs()
                .iter()
                .map(PersistedCheckpointRef::from)
                .collect(),
            working_intent: self
                .working_intent()
                .map(|intent| PersistedCheckpointWorkingIntent {
                    text: intent.text().to_owned(),
                    refs: intent
                        .refs()
                        .iter()
                        .map(|ref_id| ref_id.as_str().to_owned())
                        .collect(),
                    confidence: intent.confidence(),
                }),
        }
    }

    pub(crate) fn from_persisted(
        persisted: PersistedCitationBackedCheckpoint,
    ) -> Result<Self, CheckpointError> {
        let id = CheckpointId::new(&persisted.id)?;
        let refs = persisted
            .refs
            .into_iter()
            .map(CheckpointRef::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let persisted_refs = refs
            .iter()
            .map(|reference| reference.id().clone())
            .collect::<BTreeSet<_>>();
        let manifest = CheckpointRefManifest::new(id.clone(), refs)?;
        let candidate = CompactedCheckpointCandidate {
            claims: persisted
                .claims
                .into_iter()
                .map(CheckpointClaim::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            working_intent: persisted
                .working_intent
                .map(CheckpointWorkingIntent::try_from)
                .transpose()?,
        };
        Self::from_candidate_with_pinned_refs(
            id,
            candidate,
            manifest,
            CheckpointValidationPolicy::default(),
            &persisted_refs,
        )
    }
}

impl TryFrom<PersistedCheckpointClaim> for CheckpointClaim {
    type Error = CheckpointError;

    fn try_from(value: PersistedCheckpointClaim) -> Result<Self, Self::Error> {
        validate_text_field("checkpoint claim text", &value.text)?;
        let refs = value
            .refs
            .iter()
            .map(|item| CheckpointRefId::new(item))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            id: CheckpointClaimId::new(&value.id)?,
            kind: value.kind,
            text: value.text,
            refs,
        })
    }
}

impl TryFrom<PersistedCheckpointWorkingIntent> for CheckpointWorkingIntent {
    type Error = CheckpointError;

    fn try_from(value: PersistedCheckpointWorkingIntent) -> Result<Self, Self::Error> {
        validate_text_field("checkpoint working_intent text", &value.text)?;
        let refs = value
            .refs
            .iter()
            .map(|item| CheckpointRefId::new(item))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            text: value.text,
            refs,
            confidence_millis: confidence_to_millis(value.confidence)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointValidationPolicy {
    max_ref_excerpt_bytes: usize,
}

impl CheckpointValidationPolicy {
    #[must_use]
    pub fn max_ref_excerpt_bytes(&self) -> usize {
        self.max_ref_excerpt_bytes
    }

    #[must_use]
    pub fn with_max_ref_excerpt_bytes(mut self, value: usize) -> Self {
        self.max_ref_excerpt_bytes = value;
        self
    }
}

impl Default for CheckpointValidationPolicy {
    fn default() -> Self {
        Self {
            max_ref_excerpt_bytes: DEFAULT_MAX_REF_EXCERPT_BYTES,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactedCheckpointCandidateWire {
    claims: Vec<CheckpointClaimWire>,
    working_intent: Option<CheckpointWorkingIntentWire>,
}

impl TryFrom<CompactedCheckpointCandidateWire> for CompactedCheckpointCandidate {
    type Error = CheckpointError;

    fn try_from(wire: CompactedCheckpointCandidateWire) -> Result<Self, Self::Error> {
        if wire.claims.is_empty() {
            return Err(CheckpointError::EmptyClaims);
        }

        let claims = wire
            .claims
            .into_iter()
            .map(CheckpointClaim::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let working_intent = wire
            .working_intent
            .map(CheckpointWorkingIntent::try_from)
            .transpose()?;

        Ok(Self {
            claims,
            working_intent,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointClaimWire {
    id: String,
    kind: CheckpointClaimKind,
    text: String,
    refs: Vec<String>,
}

impl TryFrom<CheckpointClaimWire> for CheckpointClaim {
    type Error = CheckpointError;

    fn try_from(wire: CheckpointClaimWire) -> Result<Self, Self::Error> {
        validate_text_field("checkpoint claim text", &wire.text)?;
        let refs = wire
            .refs
            .iter()
            .map(|item| CheckpointRefId::new(item))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            id: CheckpointClaimId::new(&wire.id)?,
            kind: wire.kind,
            text: wire.text,
            refs,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointWorkingIntentWire {
    text: String,
    refs: Vec<String>,
    confidence: f32,
}

impl TryFrom<CheckpointWorkingIntentWire> for CheckpointWorkingIntent {
    type Error = CheckpointError;

    fn try_from(wire: CheckpointWorkingIntentWire) -> Result<Self, Self::Error> {
        validate_text_field("checkpoint working_intent text", &wire.text)?;
        let refs = wire
            .refs
            .iter()
            .map(|item| CheckpointRefId::new(item))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            text: wire.text,
            refs,
            confidence_millis: confidence_to_millis(wire.confidence)?,
        })
    }
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), CheckpointError> {
    if value.trim().is_empty() {
        return Err(CheckpointError::BlankField { field });
    }
    if value.trim() != value {
        return Err(CheckpointError::InvalidIdentifier { field });
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
    {
        return Ok(());
    }

    Err(CheckpointError::InvalidIdentifier { field })
}

fn validate_text_field(field: &'static str, value: &str) -> Result<(), CheckpointError> {
    if value.trim().is_empty() {
        return Err(CheckpointError::BlankField { field });
    }
    if value
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(CheckpointError::InvalidControlCharacter { field });
    }

    Ok(())
}

fn confidence_to_millis(value: f32) -> Result<u16, CheckpointError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(CheckpointError::InvalidConfidence {
            value: value.to_string(),
        });
    }

    Ok((value * 1000.0).round() as u16)
}

fn format_ref_list(refs: &[CheckpointRefId]) -> String {
    refs.iter()
        .map(CheckpointRefId::as_str)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};

    fn evidence(artifact_id: &str) -> EvidenceRef {
        EvidenceRef::new(
            ArtifactId::new(artifact_id).expect("valid artifact id"),
            EvidenceLocator::whole_artifact(),
        )
    }

    #[test]
    fn runtime_history_ref_namespace_requires_h_and_decimal_digits() {
        for reserved in ["h0", "h1", "h42", "h001"] {
            assert!(
                CheckpointRefId::new(reserved)
                    .expect("valid ref id")
                    .is_runtime_history_ref()
            );
        }
        for available in ["h", "history", "h1x", "bootstrap-ref", "r1"] {
            assert!(
                !CheckpointRefId::new(available)
                    .expect("valid ref id")
                    .is_runtime_history_ref()
            );
        }
    }

    fn ref_manifest() -> CheckpointRefManifest {
        CheckpointRefManifest::new(
            CheckpointId::new("checkpoint-1").expect("valid checkpoint id"),
            vec![
                CheckpointRef::new(
                    CheckpointRefId::new("r1").expect("valid ref id"),
                    CheckpointSourceKind::UserMessage,
                    CheckpointSequenceRange::new(1, 1).expect("valid range"),
                    evidence("user-message-1"),
                ),
                CheckpointRef::new(
                    CheckpointRefId::new("r2").expect("valid ref id"),
                    CheckpointSourceKind::AssistantMessage,
                    CheckpointSequenceRange::new(2, 2).expect("valid range"),
                    evidence("assistant-output-2"),
                ),
            ],
        )
        .expect("valid manifest")
    }

    #[test]
    fn citation_checkpoint_rejects_claim_without_ref() {
        let candidate_json = r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "rejected_path",
              "text": "Do not use an artifact graph as the context-growth solution.",
              "refs": []
            }
          ],
          "working_intent": null
        }"#;

        let candidate =
            CompactedCheckpointCandidate::from_json(candidate_json).expect("parseable candidate");
        let error = CitationBackedCheckpoint::from_candidate(
            CheckpointId::new("checkpoint-1").expect("valid checkpoint id"),
            candidate,
            ref_manifest(),
            CheckpointValidationPolicy::default(),
        )
        .expect_err("claim without refs must be rejected");

        assert!(matches!(error, CheckpointError::ClaimWithoutRefs { .. }));
    }

    #[test]
    fn citation_checkpoint_rejects_unknown_ref() {
        let candidate_json = r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "constraint",
              "text": "Runtime cannot validate open-ended semantic truth.",
              "refs": ["r-missing"]
            }
          ],
          "working_intent": null
        }"#;

        let candidate =
            CompactedCheckpointCandidate::from_json(candidate_json).expect("parseable candidate");
        let error = CitationBackedCheckpoint::from_candidate(
            CheckpointId::new("checkpoint-1").expect("valid checkpoint id"),
            candidate,
            ref_manifest(),
            CheckpointValidationPolicy::default(),
        )
        .expect_err("unknown ref must be rejected");

        assert!(matches!(error, CheckpointError::UnknownRef { .. }));
    }

    #[test]
    fn citation_checkpoint_renders_claims_with_refs_and_footer_intent() {
        let candidate_json = r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "rejected_path",
              "text": "Do not use an artifact graph as the context-growth solution.",
              "refs": ["r1", "r2"]
            }
          ],
          "working_intent": {
            "text": "Continue with citation-backed checkpoint compaction.",
            "refs": ["r1"],
            "confidence": 0.82
          }
        }"#;

        let candidate =
            CompactedCheckpointCandidate::from_json(candidate_json).expect("parseable candidate");
        let checkpoint = CitationBackedCheckpoint::from_candidate(
            CheckpointId::new("checkpoint-1").expect("valid checkpoint id"),
            candidate,
            ref_manifest(),
            CheckpointValidationPolicy::default(),
        )
        .expect("valid citation checkpoint");

        assert_eq!(
            checkpoint.render_prompt_text(),
            concat!(
                "claims:\n",
                "- c1 rejected_path [r1,r2]: Do not use an artifact graph as the context-growth solution.\n",
                "working_intent [r1] confidence=0.820: Continue with citation-backed checkpoint compaction."
            )
        );
    }

    #[test]
    fn citation_checkpoint_ref_lookup_returns_original_evidence() {
        let candidate_json = r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "constraint",
              "text": "Runtime cannot validate open-ended semantic truth.",
              "refs": ["r1"]
            }
          ],
          "working_intent": null
        }"#;

        let candidate =
            CompactedCheckpointCandidate::from_json(candidate_json).expect("parseable candidate");
        let checkpoint = CitationBackedCheckpoint::from_candidate(
            CheckpointId::new("checkpoint-1").expect("valid checkpoint id"),
            candidate,
            ref_manifest(),
            CheckpointValidationPolicy::default(),
        )
        .expect("valid checkpoint");

        let reference = checkpoint
            .read_ref(&CheckpointRefId::new("r1").expect("valid ref id"))
            .expect("ref should exist");

        assert_eq!(reference.id().as_str(), "r1");
        assert_eq!(reference.source_kind(), CheckpointSourceKind::UserMessage);
        assert_eq!(reference.evidence().artifact_id.as_str(), "user-message-1");
    }

    #[test]
    fn citation_checkpoint_filters_manifest_to_used_original_refs() {
        let candidate = CompactedCheckpointCandidate::from_json(
            r#"{
              "claims": [
                {
                  "id": "c1",
                  "kind": "constraint",
                  "text": "Only the user source is needed.",
                  "refs": ["r1"]
                }
              ],
              "working_intent": null
            }"#,
        )
        .expect("candidate parses");

        let checkpoint = CitationBackedCheckpoint::from_candidate(
            CheckpointId::new("checkpoint-filtered").expect("valid id"),
            candidate,
            ref_manifest(),
            CheckpointValidationPolicy::default(),
        )
        .expect("checkpoint builds");

        assert_eq!(checkpoint.manifest().refs().len(), 1);
        assert_eq!(checkpoint.manifest().refs()[0].id().as_str(), "r1");
    }

    #[test]
    fn citation_checkpoint_keeps_explicitly_pinned_original_refs() {
        let candidate = CompactedCheckpointCandidate::from_json(
            r#"{
              "claims": [{
                "id": "c1",
                "kind": "constraint",
                "text": "The user source remains in the checkpoint.",
                "refs": ["r1"]
              }],
              "working_intent": null
            }"#,
        )
        .expect("candidate parses");
        let pinned = [CheckpointRefId::new("r2").expect("valid ref id")]
            .into_iter()
            .collect();

        let checkpoint = CitationBackedCheckpoint::from_candidate_with_pinned_refs(
            CheckpointId::new("checkpoint-pinned").expect("valid id"),
            candidate,
            ref_manifest(),
            CheckpointValidationPolicy::default(),
            &pinned,
        )
        .expect("checkpoint builds");

        assert_eq!(
            checkpoint
                .manifest()
                .refs()
                .iter()
                .map(|reference| reference.id().as_str())
                .collect::<Vec<_>>(),
            ["r1", "r2"]
        );
    }

    #[test]
    fn citation_checkpoint_reports_missing_pinned_ref_directly() {
        let candidate = CompactedCheckpointCandidate::from_json(
            r#"{
              "claims": [{
                "id": "c1",
                "kind": "constraint",
                "text": "The user source remains in the checkpoint.",
                "refs": ["r1"]
              }],
              "working_intent": null
            }"#,
        )
        .expect("candidate parses");
        let pinned = [CheckpointRefId::new("missing").expect("valid ref id")]
            .into_iter()
            .collect();

        let error = CitationBackedCheckpoint::from_candidate_with_pinned_refs(
            CheckpointId::new("checkpoint-missing-pinned").expect("valid id"),
            candidate,
            ref_manifest(),
            CheckpointValidationPolicy::default(),
            &pinned,
        )
        .expect_err("a pinned ref must exist in the manifest");

        assert!(matches!(
            error,
            CheckpointError::PinnedRefNotFound { ref_id } if ref_id == "missing"
        ));
    }

    #[test]
    fn persisted_checkpoint_keeps_prevalidated_pinned_refs() {
        let candidate = CompactedCheckpointCandidate::from_json(
            r#"{
              "claims": [{
                "id": "c1",
                "kind": "constraint",
                "text": "The user source remains in the checkpoint.",
                "refs": ["r1"]
              }],
              "working_intent": null
            }"#,
        )
        .expect("candidate parses");
        let pinned = [CheckpointRefId::new("r2").expect("valid ref id")]
            .into_iter()
            .collect();
        let checkpoint = CitationBackedCheckpoint::from_candidate_with_pinned_refs(
            CheckpointId::new("checkpoint-pinned-round-trip").expect("valid id"),
            candidate,
            ref_manifest(),
            CheckpointValidationPolicy::default(),
            &pinned,
        )
        .expect("checkpoint builds");

        let loaded = CitationBackedCheckpoint::from_persisted(checkpoint.persisted())
            .expect("persisted checkpoint loads");

        assert_eq!(
            loaded
                .manifest()
                .refs()
                .iter()
                .map(|reference| reference.id().as_str())
                .collect::<Vec<_>>(),
            ["r1", "r2"]
        );
    }
}
