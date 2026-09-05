use super::{
    CheckpointError, CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest,
    domain::CitationBackedCheckpoint, reference::PersistedCheckpointRef, validate_identifier,
    validate_text_field,
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckpointEntryId(String);

impl CheckpointEntryId {
    pub fn new(value: &str) -> Result<Self, CheckpointError> {
        validate_identifier("checkpoint entry id", value)?;
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckpointSection {
    ConfirmedDecision,
    RejectedApproach,
    ConstraintPreferenceBoundary,
    CorrectedMisunderstanding,
    DurableConclusion,
    OpenQuestion,
    CurrentProgressAndNextStep,
    ExactDetail,
}

impl CheckpointSection {
    pub const ALL: [Self; 8] = [
        Self::ConfirmedDecision,
        Self::RejectedApproach,
        Self::ConstraintPreferenceBoundary,
        Self::CorrectedMisunderstanding,
        Self::DurableConclusion,
        Self::OpenQuestion,
        Self::CurrentProgressAndNextStep,
        Self::ExactDetail,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConfirmedDecision => "confirmed_decisions",
            Self::RejectedApproach => "rejected_approaches",
            Self::ConstraintPreferenceBoundary => "constraints_preferences_boundaries",
            Self::CorrectedMisunderstanding => "corrected_misunderstandings",
            Self::DurableConclusion => "durable_conclusions",
            Self::OpenQuestion => "open_questions",
            Self::CurrentProgressAndNextStep => "current_progress_and_next_steps",
            Self::ExactDetail => "exact_details",
        }
    }

    fn requires_rationale(self) -> bool {
        matches!(self, Self::ConfirmedDecision | Self::RejectedApproach)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointEntry {
    id: CheckpointEntryId,
    text: String,
    rationale: Option<String>,
    refs: Vec<CheckpointRefId>,
}

impl CheckpointEntry {
    #[must_use]
    pub fn id(&self) -> &CheckpointEntryId {
        &self.id
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn rationale(&self) -> Option<&str> {
        self.rationale.as_deref()
    }

    #[must_use]
    pub fn refs(&self) -> &[CheckpointRefId] {
        &self.refs
    }

    fn try_from_wire(
        section: CheckpointSection,
        wire: CheckpointEntryWire,
    ) -> Result<Self, CheckpointError> {
        let id = CheckpointEntryId::new(&wire.id)?;
        validate_text_field("checkpoint entry text", &wire.text)?;
        let rationale = match wire.rationale {
            Some(value) if section.requires_rationale() && value.trim().is_empty() => {
                return Err(CheckpointError::EntryRationaleRequired {
                    entry_id: id.as_str().to_owned(),
                });
            }
            Some(value) => {
                validate_text_field("checkpoint entry rationale", &value)?;
                Some(value)
            }
            None if section.requires_rationale() => {
                return Err(CheckpointError::EntryRationaleRequired {
                    entry_id: id.as_str().to_owned(),
                });
            }
            None => None,
        };
        if wire.refs.is_empty() {
            return Err(CheckpointError::EntryWithoutRefs {
                entry_id: id.as_str().to_owned(),
            });
        }
        let refs = wire
            .refs
            .iter()
            .map(|item| CheckpointRefId::new(item))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            id,
            text: wire.text,
            rationale,
            refs,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckpointSections {
    confirmed_decisions: Vec<CheckpointEntry>,
    rejected_approaches: Vec<CheckpointEntry>,
    constraints_preferences_boundaries: Vec<CheckpointEntry>,
    corrected_misunderstandings: Vec<CheckpointEntry>,
    durable_conclusions: Vec<CheckpointEntry>,
    open_questions: Vec<CheckpointEntry>,
    current_progress_and_next_steps: Vec<CheckpointEntry>,
    exact_details: Vec<CheckpointEntry>,
}

impl CheckpointSections {
    #[must_use]
    pub fn entries(&self, section: CheckpointSection) -> &[CheckpointEntry] {
        match section {
            CheckpointSection::ConfirmedDecision => &self.confirmed_decisions,
            CheckpointSection::RejectedApproach => &self.rejected_approaches,
            CheckpointSection::ConstraintPreferenceBoundary => {
                &self.constraints_preferences_boundaries
            }
            CheckpointSection::CorrectedMisunderstanding => &self.corrected_misunderstandings,
            CheckpointSection::DurableConclusion => &self.durable_conclusions,
            CheckpointSection::OpenQuestion => &self.open_questions,
            CheckpointSection::CurrentProgressAndNextStep => &self.current_progress_and_next_steps,
            CheckpointSection::ExactDetail => &self.exact_details,
        }
    }

    #[must_use]
    pub fn entry_count(&self) -> usize {
        CheckpointSection::ALL
            .into_iter()
            .map(|section| self.entries(section).len())
            .sum()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (CheckpointSection, &CheckpointEntry)> {
        CheckpointSection::ALL.into_iter().flat_map(move |section| {
            self.entries(section)
                .iter()
                .map(move |entry| (section, entry))
        })
    }

    fn restore_entry_at(
        &mut self,
        section: CheckpointSection,
        index: usize,
        entry: CheckpointEntry,
    ) {
        let entry_id = entry.id().clone();
        for candidate_section in CheckpointSection::ALL {
            self.entries_mut(candidate_section)
                .retain(|candidate| candidate.id() != &entry_id);
        }
        let insertion_index = index.min(self.entries(section).len());
        self.entries_mut(section).insert(insertion_index, entry);
    }

    fn entries_mut(&mut self, section: CheckpointSection) -> &mut Vec<CheckpointEntry> {
        match section {
            CheckpointSection::ConfirmedDecision => &mut self.confirmed_decisions,
            CheckpointSection::RejectedApproach => &mut self.rejected_approaches,
            CheckpointSection::ConstraintPreferenceBoundary => {
                &mut self.constraints_preferences_boundaries
            }
            CheckpointSection::CorrectedMisunderstanding => &mut self.corrected_misunderstandings,
            CheckpointSection::DurableConclusion => &mut self.durable_conclusions,
            CheckpointSection::OpenQuestion => &mut self.open_questions,
            CheckpointSection::CurrentProgressAndNextStep => {
                &mut self.current_progress_and_next_steps
            }
            CheckpointSection::ExactDetail => &mut self.exact_details,
        }
    }

    fn from_wire(wire: CompactedCheckpointCandidateWire) -> Result<Self, CheckpointError> {
        let sections = Self {
            confirmed_decisions: parse_entries(
                CheckpointSection::ConfirmedDecision,
                wire.confirmed_decisions,
            )?,
            rejected_approaches: parse_entries(
                CheckpointSection::RejectedApproach,
                wire.rejected_approaches,
            )?,
            constraints_preferences_boundaries: parse_entries(
                CheckpointSection::ConstraintPreferenceBoundary,
                wire.constraints_preferences_boundaries,
            )?,
            corrected_misunderstandings: parse_entries(
                CheckpointSection::CorrectedMisunderstanding,
                wire.corrected_misunderstandings,
            )?,
            durable_conclusions: parse_entries(
                CheckpointSection::DurableConclusion,
                wire.durable_conclusions,
            )?,
            open_questions: parse_entries(CheckpointSection::OpenQuestion, wire.open_questions)?,
            current_progress_and_next_steps: parse_entries(
                CheckpointSection::CurrentProgressAndNextStep,
                wire.current_progress_and_next_steps,
            )?,
            exact_details: parse_entries(CheckpointSection::ExactDetail, wire.exact_details)?,
        };
        let mut seen = BTreeSet::new();
        for (_, entry) in sections.iter() {
            if !seen.insert(entry.id().clone()) {
                return Err(CheckpointError::DuplicateEntry {
                    entry_id: entry.id().as_str().to_owned(),
                });
            }
        }
        Ok(sections)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointHandoffAction {
    Keep,
    Replace,
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointHandoff {
    Keep {
        old_id: CheckpointEntryId,
    },
    Replace {
        old_id: CheckpointEntryId,
        new_ids: Vec<CheckpointEntryId>,
        reason: String,
    },
    Drop {
        old_id: CheckpointEntryId,
        reason: String,
    },
}

impl CheckpointHandoff {
    #[must_use]
    pub fn action(&self) -> CheckpointHandoffAction {
        match self {
            Self::Keep { .. } => CheckpointHandoffAction::Keep,
            Self::Replace { .. } => CheckpointHandoffAction::Replace,
            Self::Drop { .. } => CheckpointHandoffAction::Drop,
        }
    }

    #[must_use]
    pub fn old_id(&self) -> &CheckpointEntryId {
        match self {
            Self::Keep { old_id } | Self::Replace { old_id, .. } | Self::Drop { old_id, .. } => {
                old_id
            }
        }
    }

    #[must_use]
    pub fn new_ids(&self) -> &[CheckpointEntryId] {
        match self {
            Self::Replace { new_ids, .. } => new_ids,
            Self::Keep { .. } | Self::Drop { .. } => &[],
        }
    }

    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Replace { reason, .. } | Self::Drop { reason, .. } => Some(reason),
            Self::Keep { .. } => None,
        }
    }

    fn try_from_wire(wire: CheckpointHandoffWire) -> Result<Self, CheckpointError> {
        match wire {
            CheckpointHandoffWire::Keep { old_id } => Ok(Self::Keep {
                old_id: CheckpointEntryId::new(&old_id)?,
            }),
            CheckpointHandoffWire::Replace {
                old_id,
                new_ids,
                reason,
            } => {
                let old_id = CheckpointEntryId::new(&old_id)?;
                let new_ids = new_ids
                    .iter()
                    .map(|value| CheckpointEntryId::new(value))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self::Replace {
                    old_id,
                    new_ids,
                    reason,
                })
            }
            CheckpointHandoffWire::Drop { old_id, reason } => {
                let old_id = CheckpointEntryId::new(&old_id)?;
                Ok(Self::Drop { old_id, reason })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactedCheckpointCandidate {
    sections: CheckpointSections,
    handoffs: Vec<CheckpointHandoff>,
}

impl CompactedCheckpointCandidate {
    pub fn from_json(input: &str) -> Result<Self, CheckpointError> {
        let wire =
            serde_json::from_str::<CompactedCheckpointCandidateWire>(input).map_err(|error| {
                CheckpointError::InvalidCandidateJson {
                    message: error.to_string(),
                }
            })?;
        wire.try_into()
    }

    #[must_use]
    pub fn sections(&self) -> &CheckpointSections {
        &self.sections
    }

    #[must_use]
    pub fn handoffs(&self) -> &[CheckpointHandoff] {
        &self.handoffs
    }

    /// Restores entries explicitly referenced by rolling `keep` handoffs.
    ///
    /// The candidate section arrays are the complete next checkpoint. A prior entry that is
    /// absent from both those arrays and the handoffs is intentionally removed; it must not be
    /// converted into an implicit `keep`.
    pub(crate) fn materialize_kept_entries(&mut self, previous: &CitationBackedCheckpoint) {
        let kept_ids = self
            .handoffs
            .iter()
            .filter_map(|handoff| match handoff {
                CheckpointHandoff::Keep { old_id } => Some(old_id.clone()),
                CheckpointHandoff::Replace { .. } | CheckpointHandoff::Drop { .. } => None,
            })
            .collect::<BTreeSet<_>>();

        for section in CheckpointSection::ALL {
            for (index, entry) in previous.sections().entries(section).iter().enumerate() {
                if kept_ids.contains(entry.id()) {
                    self.sections
                        .restore_entry_at(section, index, entry.clone());
                }
            }
        }
    }

    pub(super) fn into_parts(self) -> (CheckpointSections, Vec<CheckpointHandoff>) {
        (self.sections, self.handoffs)
    }
}

impl TryFrom<CompactedCheckpointCandidateWire> for CompactedCheckpointCandidate {
    type Error = CheckpointError;

    fn try_from(mut wire: CompactedCheckpointCandidateWire) -> Result<Self, Self::Error> {
        let handoff_wires = std::mem::take(&mut wire.handoffs);
        let sections = CheckpointSections::from_wire(wire)?;
        let handoffs = handoff_wires
            .into_iter()
            .map(CheckpointHandoff::try_from_wire)
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = BTreeSet::new();
        for handoff in &handoffs {
            if !seen.insert(handoff.old_id().clone()) {
                return Err(CheckpointError::DuplicateHandoff {
                    old_id: handoff.old_id().as_str().to_owned(),
                });
            }
        }
        Ok(Self { sections, handoffs })
    }
}

impl CitationBackedCheckpoint {
    pub(crate) fn persisted(&self) -> PersistedCitationBackedCheckpoint {
        PersistedCitationBackedCheckpoint {
            id: self.id().as_str().to_owned(),
            confirmed_decisions: persisted_entries(
                self.sections()
                    .entries(CheckpointSection::ConfirmedDecision),
            ),
            rejected_approaches: persisted_entries(
                self.sections().entries(CheckpointSection::RejectedApproach),
            ),
            constraints_preferences_boundaries: persisted_entries(
                self.sections()
                    .entries(CheckpointSection::ConstraintPreferenceBoundary),
            ),
            corrected_misunderstandings: persisted_entries(
                self.sections()
                    .entries(CheckpointSection::CorrectedMisunderstanding),
            ),
            durable_conclusions: persisted_entries(
                self.sections()
                    .entries(CheckpointSection::DurableConclusion),
            ),
            open_questions: persisted_entries(
                self.sections().entries(CheckpointSection::OpenQuestion),
            ),
            current_progress_and_next_steps: persisted_entries(
                self.sections()
                    .entries(CheckpointSection::CurrentProgressAndNextStep),
            ),
            exact_details: persisted_entries(
                self.sections().entries(CheckpointSection::ExactDetail),
            ),
            refs: self
                .manifest()
                .refs()
                .iter()
                .map(PersistedCheckpointRef::from)
                .collect(),
            handoffs: self
                .handoffs()
                .iter()
                .map(CheckpointHandoffWire::from)
                .collect(),
        }
    }

    pub(crate) fn from_persisted(
        persisted: PersistedCitationBackedCheckpoint,
    ) -> Result<Self, CheckpointError> {
        let PersistedCitationBackedCheckpoint {
            id,
            confirmed_decisions,
            rejected_approaches,
            constraints_preferences_boundaries,
            corrected_misunderstandings,
            durable_conclusions,
            open_questions,
            current_progress_and_next_steps,
            exact_details,
            refs,
            handoffs,
        } = persisted;
        let id = CheckpointId::new(&id)?;
        let refs = refs
            .into_iter()
            .map(CheckpointRef::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let persisted_refs = refs
            .iter()
            .map(|reference| reference.id().clone())
            .collect::<BTreeSet<_>>();
        let manifest = CheckpointRefManifest::new(id.clone(), refs)?;
        let candidate = CompactedCheckpointCandidate::try_from(CompactedCheckpointCandidateWire {
            confirmed_decisions,
            rejected_approaches,
            constraints_preferences_boundaries,
            corrected_misunderstandings,
            durable_conclusions,
            open_questions,
            current_progress_and_next_steps,
            exact_details,
            handoffs,
        })?;
        Self::from_prevalidated_persistence(id, candidate, manifest, &persisted_refs)
    }
}

fn parse_entries(
    section: CheckpointSection,
    wires: Vec<CheckpointEntryWire>,
) -> Result<Vec<CheckpointEntry>, CheckpointError> {
    wires
        .into_iter()
        .map(|wire| CheckpointEntry::try_from_wire(section, wire))
        .collect()
}

fn persisted_entries(entries: &[CheckpointEntry]) -> Vec<CheckpointEntryWire> {
    entries.iter().map(CheckpointEntryWire::from).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompactedCheckpointCandidateWire {
    confirmed_decisions: Vec<CheckpointEntryWire>,
    rejected_approaches: Vec<CheckpointEntryWire>,
    constraints_preferences_boundaries: Vec<CheckpointEntryWire>,
    corrected_misunderstandings: Vec<CheckpointEntryWire>,
    durable_conclusions: Vec<CheckpointEntryWire>,
    open_questions: Vec<CheckpointEntryWire>,
    current_progress_and_next_steps: Vec<CheckpointEntryWire>,
    exact_details: Vec<CheckpointEntryWire>,
    #[schemars(with = "Vec<CheckpointHandoffSchemaWire>")]
    handoffs: Vec<CheckpointHandoffWire>,
}

pub(crate) fn compacted_checkpoint_candidate_schema() -> Schema {
    schema_for!(CompactedCheckpointCandidateWire)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(inline)]
enum CheckpointNullableString {
    Value(String),
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(inline)]
enum CheckpointNullableEntryIds {
    Values(Vec<String>),
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CheckpointEntryWire {
    id: String,
    text: String,
    #[schemars(required, with = "CheckpointNullableString")]
    rationale: Option<String>,
    #[schemars(length(min = 1))]
    refs: Vec<String>,
}

impl From<&CheckpointEntry> for CheckpointEntryWire {
    fn from(entry: &CheckpointEntry) -> Self {
        Self {
            id: entry.id().as_str().to_owned(),
            text: entry.text().to_owned(),
            rationale: entry.rationale().map(str::to_owned),
            refs: entry
                .refs()
                .iter()
                .map(|ref_id| ref_id.as_str().to_owned())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
enum CheckpointHandoffSchemaAction {
    Keep,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
struct CheckpointHandoffSchemaWire {
    /// Preserve a prior entry by reference or record its replacement.
    action: CheckpointHandoffSchemaAction,
    /// The id of the prior checkpoint entry being handed off.
    old_id: String,
    /// Replacement entry ids for replace, or null for keep.
    #[schemars(required, with = "CheckpointNullableEntryIds")]
    new_ids: Option<Vec<String>>,
    /// Optional context for the reference; use null for keep.
    #[schemars(required, with = "CheckpointNullableString")]
    reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum CheckpointHandoffWire {
    Keep {
        old_id: String,
    },
    Replace {
        old_id: String,
        new_ids: Vec<String>,
        reason: String,
    },
    Drop {
        old_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CheckpointHandoffActionWire {
    Keep,
    Replace,
    Drop,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointHandoffFields {
    action: CheckpointHandoffActionWire,
    old_id: String,
    #[serde(default)]
    new_ids: Option<Vec<String>>,
    #[serde(default)]
    reason: Option<String>,
}

impl<'de> Deserialize<'de> for CheckpointHandoffWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = CheckpointHandoffFields::deserialize(deserializer)?;
        match fields.action {
            CheckpointHandoffActionWire::Keep => Ok(Self::Keep {
                old_id: fields.old_id,
            }),
            CheckpointHandoffActionWire::Replace => Ok(Self::Replace {
                old_id: fields.old_id,
                new_ids: fields.new_ids.unwrap_or_default(),
                reason: fields.reason.unwrap_or_default(),
            }),
            CheckpointHandoffActionWire::Drop => Ok(Self::Drop {
                old_id: fields.old_id,
                reason: fields.reason.unwrap_or_default(),
            }),
        }
    }
}

impl From<&CheckpointHandoff> for CheckpointHandoffWire {
    fn from(handoff: &CheckpointHandoff) -> Self {
        match handoff {
            CheckpointHandoff::Keep { old_id } => Self::Keep {
                old_id: old_id.as_str().to_owned(),
            },
            CheckpointHandoff::Replace {
                old_id,
                new_ids,
                reason,
            } => Self::Replace {
                old_id: old_id.as_str().to_owned(),
                new_ids: new_ids.iter().map(|id| id.as_str().to_owned()).collect(),
                reason: reason.clone(),
            },
            CheckpointHandoff::Drop { old_id, reason } => Self::Drop {
                old_id: old_id.as_str().to_owned(),
                reason: reason.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedCitationBackedCheckpoint {
    id: String,
    confirmed_decisions: Vec<CheckpointEntryWire>,
    rejected_approaches: Vec<CheckpointEntryWire>,
    constraints_preferences_boundaries: Vec<CheckpointEntryWire>,
    corrected_misunderstandings: Vec<CheckpointEntryWire>,
    durable_conclusions: Vec<CheckpointEntryWire>,
    open_questions: Vec<CheckpointEntryWire>,
    current_progress_and_next_steps: Vec<CheckpointEntryWire>,
    exact_details: Vec<CheckpointEntryWire>,
    refs: Vec<PersistedCheckpointRef>,
    handoffs: Vec<CheckpointHandoffWire>,
}
