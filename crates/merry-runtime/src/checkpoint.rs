use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

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

    #[error("checkpoint ref {ref_id} is duplicated")]
    DuplicateRef { ref_id: String },

    #[error("checkpoint claim {claim_id} is duplicated")]
    DuplicateClaim { claim_id: String },

    #[error("checkpoint ref {ref_id} does not exist in checkpoint {checkpoint_id}")]
    RefNotFound {
        checkpoint_id: String,
        ref_id: String,
    },

    #[error(
        "checkpoint id {checkpoint_id} does not match requested checkpoint {requested_checkpoint_id}"
    )]
    CheckpointIdMismatch {
        checkpoint_id: String,
        requested_checkpoint_id: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointSourceKind {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolResult,
    ArtifactRange,
    PriorCheckpointClaim,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedCheckpointRef {
    id: String,
    source_kind: CheckpointSourceKind,
    source_id: String,
    sequence_start: u64,
    sequence_end: u64,
    locator: String,
    excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedCheckpointWorkingIntent {
    text: String,
    refs: Vec<String>,
    confidence: f32,
}

impl CheckpointSourceKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserMessage => "user_message",
            Self::AssistantMessage => "assistant_message",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::ArtifactRange => "artifact_range",
            Self::PriorCheckpointClaim => "prior_checkpoint_claim",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRef {
    id: CheckpointRefId,
    source_kind: CheckpointSourceKind,
    source_id: String,
    sequence_range: CheckpointSequenceRange,
    locator: String,
    excerpt: String,
}

impl CheckpointRef {
    pub fn new(
        id: CheckpointRefId,
        source_kind: CheckpointSourceKind,
        source_id: impl Into<String>,
        sequence_range: CheckpointSequenceRange,
        locator: impl Into<String>,
        excerpt: impl Into<String>,
    ) -> Result<Self, CheckpointError> {
        let source_id = source_id.into();
        validate_text_field("checkpoint ref source id", &source_id)?;
        let locator = locator.into();
        validate_text_field("checkpoint ref locator", &locator)?;
        let excerpt = excerpt.into();
        validate_text_field("checkpoint ref excerpt", &excerpt)?;

        Ok(Self {
            id,
            source_kind,
            source_id,
            sequence_range,
            locator,
            excerpt,
        })
    }

    #[must_use]
    pub fn id(&self) -> &CheckpointRefId {
        &self.id
    }

    #[must_use]
    pub fn source_kind(&self) -> CheckpointSourceKind {
        self.source_kind
    }

    #[must_use]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    #[must_use]
    pub fn sequence_range(&self) -> CheckpointSequenceRange {
        self.sequence_range
    }

    #[must_use]
    pub fn locator(&self) -> &str {
        &self.locator
    }

    #[must_use]
    pub fn excerpt(&self) -> &str {
        &self.excerpt
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRefManifest {
    checkpoint_id: CheckpointId,
    refs: Vec<CheckpointRef>,
}

impl CheckpointRefManifest {
    pub fn new(
        checkpoint_id: CheckpointId,
        refs: Vec<CheckpointRef>,
    ) -> Result<Self, CheckpointError> {
        let mut seen = BTreeSet::new();
        for item in &refs {
            if !seen.insert(item.id().clone()) {
                return Err(CheckpointError::DuplicateRef {
                    ref_id: item.id().as_str().to_owned(),
                });
            }
        }

        Ok(Self {
            checkpoint_id,
            refs,
        })
    }

    pub fn from_prior_checkpoint_claims(
        checkpoint_id: CheckpointId,
        prior: &CitationBackedCheckpoint,
        max_claim_refs: usize,
    ) -> Result<Self, CheckpointError> {
        let refs = prior
            .claims()
            .iter()
            .take(max_claim_refs)
            .map(|claim| {
                CheckpointRef::new(
                    CheckpointRefId::new(&format!("prior-{}", claim.id().as_str()))?,
                    CheckpointSourceKind::PriorCheckpointClaim,
                    format!(
                        "checkpoint:{}:claim:{}",
                        prior.id().as_str(),
                        claim.id().as_str()
                    ),
                    CheckpointSequenceRange::new(0, 0)?,
                    format!("checkpoint_claim:{}", claim.id().as_str()),
                    claim.text(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        Self::new(checkpoint_id, refs)
    }

    #[must_use]
    pub fn checkpoint_id(&self) -> &CheckpointId {
        &self.checkpoint_id
    }

    #[must_use]
    pub fn refs(&self) -> &[CheckpointRef] {
        &self.refs
    }

    fn get(&self, id: &CheckpointRefId) -> Option<&CheckpointRef> {
        self.refs.iter().find(|item| item.id() == id)
    }
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
        let mut seen_claims = BTreeSet::new();
        let manifest_refs = manifest
            .refs()
            .iter()
            .map(|item| item.id().clone())
            .collect::<BTreeSet<_>>();

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
            }
        }

        let bounded_refs = manifest
            .refs()
            .iter()
            .map(|item| {
                CheckpointRef::new(
                    item.id().clone(),
                    item.source_kind(),
                    item.source_id(),
                    item.sequence_range(),
                    item.locator(),
                    bounded_excerpt(item.excerpt(), policy.max_ref_excerpt_bytes()),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            id: id.clone(),
            claims: candidate.claims,
            manifest: CheckpointRefManifest::new(id, bounded_refs)?,
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

    pub fn read_ref(
        &self,
        ref_id: &CheckpointRefId,
    ) -> Result<CheckpointRefExcerpt, CheckpointError> {
        let Some(item) = self.manifest.get(ref_id) else {
            return Err(CheckpointError::RefNotFound {
                checkpoint_id: self.id.as_str().to_owned(),
                ref_id: ref_id.as_str().to_owned(),
            });
        };

        Ok(CheckpointRefExcerpt {
            checkpoint_id: self.id.clone(),
            ref_id: item.id().clone(),
            source_kind: item.source_kind(),
            source_id: item.source_id().to_owned(),
            sequence_range: item.sequence_range(),
            locator: item.locator().to_owned(),
            excerpt: item.excerpt().to_owned(),
        })
    }

    pub fn read_ref_for_checkpoint(
        &self,
        checkpoint_id: &CheckpointId,
        ref_id: &CheckpointRefId,
    ) -> Result<CheckpointRefExcerpt, CheckpointError> {
        if checkpoint_id != &self.id {
            return Err(CheckpointError::CheckpointIdMismatch {
                checkpoint_id: self.id.as_str().to_owned(),
                requested_checkpoint_id: checkpoint_id.as_str().to_owned(),
            });
        }

        self.read_ref(ref_id)
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
                .map(|item| PersistedCheckpointRef {
                    id: item.id().as_str().to_owned(),
                    source_kind: item.source_kind(),
                    source_id: item.source_id().to_owned(),
                    sequence_start: item.sequence_range().start(),
                    sequence_end: item.sequence_range().end(),
                    locator: item.locator().to_owned(),
                    excerpt: item.excerpt().to_owned(),
                })
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
        Self::from_candidate(
            id,
            candidate,
            manifest,
            CheckpointValidationPolicy::default(),
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

impl TryFrom<PersistedCheckpointRef> for CheckpointRef {
    type Error = CheckpointError;

    fn try_from(value: PersistedCheckpointRef) -> Result<Self, Self::Error> {
        Self::new(
            CheckpointRefId::new(&value.id)?,
            value.source_kind,
            value.source_id,
            CheckpointSequenceRange::new(value.sequence_start, value.sequence_end)?,
            value.locator,
            value.excerpt,
        )
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRefExcerpt {
    checkpoint_id: CheckpointId,
    ref_id: CheckpointRefId,
    source_kind: CheckpointSourceKind,
    source_id: String,
    sequence_range: CheckpointSequenceRange,
    locator: String,
    excerpt: String,
}

impl CheckpointRefExcerpt {
    #[must_use]
    pub fn checkpoint_id(&self) -> &CheckpointId {
        &self.checkpoint_id
    }

    #[must_use]
    pub fn ref_id(&self) -> &CheckpointRefId {
        &self.ref_id
    }

    #[must_use]
    pub fn source_kind(&self) -> CheckpointSourceKind {
        self.source_kind
    }

    #[must_use]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    #[must_use]
    pub fn sequence_range(&self) -> CheckpointSequenceRange {
        self.sequence_range
    }

    #[must_use]
    pub fn locator(&self) -> &str {
        &self.locator
    }

    #[must_use]
    pub fn excerpt(&self) -> &str {
        &self.excerpt
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

fn bounded_excerpt(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }

    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated]", &text[..end])
}

fn format_ref_list(refs: &[CheckpointRefId]) -> String {
    refs.iter()
        .map(CheckpointRefId::as_str)
        .collect::<Vec<_>>()
        .join(",")
}

#[allow(dead_code)]
fn refs_by_id(manifest: &CheckpointRefManifest) -> BTreeMap<&str, &CheckpointRef> {
    manifest
        .refs()
        .iter()
        .map(|item| (item.id().as_str(), item))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_manifest() -> CheckpointRefManifest {
        CheckpointRefManifest::new(
            CheckpointId::new("checkpoint-1").expect("valid checkpoint id"),
            vec![
                CheckpointRef::new(
                    CheckpointRefId::new("r1").expect("valid ref id"),
                    CheckpointSourceKind::UserMessage,
                    "history:1",
                    CheckpointSequenceRange::new(1, 1).expect("valid range"),
                    "body[0]",
                    "user corrected the direction",
                )
                .expect("valid ref"),
                CheckpointRef::new(
                    CheckpointRefId::new("r2").expect("valid ref id"),
                    CheckpointSourceKind::AssistantMessage,
                    "history:2",
                    CheckpointSequenceRange::new(2, 2).expect("valid range"),
                    "body[1]",
                    "assistant proposed the rejected artifact graph path",
                )
                .expect("valid ref"),
            ],
        )
        .expect("valid manifest")
    }

    fn citation_checkpoint_with_one_claim_for_tests(
        checkpoint_id: &str,
        claim_id: &str,
        kind: &str,
        text: &str,
        ref_id: &str,
        excerpt: &str,
    ) -> CitationBackedCheckpoint {
        let manifest = CheckpointRefManifest::new(
            CheckpointId::new(checkpoint_id).expect("valid checkpoint id"),
            vec![
                CheckpointRef::new(
                    CheckpointRefId::new(ref_id).expect("valid ref id"),
                    CheckpointSourceKind::UserMessage,
                    "history:1",
                    CheckpointSequenceRange::new(1, 1).expect("valid range"),
                    "body[0]",
                    excerpt,
                )
                .expect("valid ref"),
            ],
        )
        .expect("valid manifest");
        let candidate = CompactedCheckpointCandidate::from_json(&format!(
            r#"{{
              "claims": [
                {{
                  "id": {claim_id_json},
                  "kind": {kind_json},
                  "text": {text_json},
                  "refs": [{ref_id_json}]
                }}
              ],
              "working_intent": null
            }}"#,
            claim_id_json = serde_json::to_string(claim_id).expect("claim id serializes"),
            kind_json = serde_json::to_string(kind).expect("kind serializes"),
            text_json = serde_json::to_string(text).expect("text serializes"),
            ref_id_json = serde_json::to_string(ref_id).expect("ref id serializes"),
        ))
        .expect("candidate parses");

        CitationBackedCheckpoint::from_candidate(
            CheckpointId::new(checkpoint_id).expect("valid checkpoint id"),
            candidate,
            manifest,
            CheckpointValidationPolicy::default(),
        )
        .expect("citation checkpoint builds")
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
    fn citation_checkpoint_ref_lookup_returns_bounded_excerpt() {
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

        let excerpt = checkpoint
            .read_ref(&CheckpointRefId::new("r1").expect("valid ref id"))
            .expect("ref should exist");

        assert_eq!(excerpt.ref_id().as_str(), "r1");
        assert_eq!(excerpt.source_kind(), CheckpointSourceKind::UserMessage);
        assert_eq!(excerpt.excerpt(), "user corrected the direction");
    }

    #[test]
    fn rolling_checkpoint_can_cite_prior_checkpoint_claim() {
        let first = citation_checkpoint_with_one_claim_for_tests(
            "checkpoint-first",
            "c1",
            "constraint",
            "Runtime cannot validate open semantic truth.",
            "r1",
            "user correction source excerpt",
        );

        let manifest = CheckpointRefManifest::from_prior_checkpoint_claims(
            CheckpointId::new("checkpoint-second").expect("valid id"),
            &first,
            16,
        )
        .expect("prior claims become refs");

        let candidate = CompactedCheckpointCandidate::from_json(
            r#"{
              "claims": [
                {
                  "id": "c2",
                  "kind": "constraint",
                  "text": "Carry forward that runtime cannot validate open semantic truth.",
                  "refs": ["prior-c1"]
                }
              ],
              "working_intent": null
            }"#,
        )
        .expect("candidate parses");

        let second = CitationBackedCheckpoint::from_candidate(
            CheckpointId::new("checkpoint-second").expect("valid id"),
            candidate,
            manifest,
            CheckpointValidationPolicy::default(),
        )
        .expect("rolling checkpoint builds");

        let excerpt = second
            .read_ref(&CheckpointRefId::new("prior-c1").expect("valid ref id"))
            .expect("prior claim ref resolves");
        assert_eq!(
            excerpt.source_kind(),
            CheckpointSourceKind::PriorCheckpointClaim
        );
        assert!(
            excerpt
                .excerpt()
                .contains("Runtime cannot validate open semantic truth")
        );
    }
}
