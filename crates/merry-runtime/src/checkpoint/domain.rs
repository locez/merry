use super::{
    CheckpointError, CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest,
    CheckpointValidationPolicy, candidate::CompactedCheckpointCandidate, format_ref_list,
};
use crate::checkpoint::candidate::{CheckpointHandoff, CheckpointSection, CheckpointSections};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationBackedCheckpoint {
    id: CheckpointId,
    sections: CheckpointSections,
    manifest: CheckpointRefManifest,
    handoffs: Vec<CheckpointHandoff>,
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

    pub fn from_rolling_candidate(
        id: CheckpointId,
        candidate: CompactedCheckpointCandidate,
        previous: &Self,
        manifest: CheckpointRefManifest,
        policy: CheckpointValidationPolicy,
    ) -> Result<Self, CheckpointError> {
        Self::from_rolling_candidate_with_pinned_refs(
            id,
            candidate,
            manifest,
            previous,
            policy,
            &BTreeSet::new(),
        )
    }

    pub(crate) fn from_candidate_with_pinned_refs(
        id: CheckpointId,
        candidate: CompactedCheckpointCandidate,
        manifest: CheckpointRefManifest,
        policy: CheckpointValidationPolicy,
        pinned_refs: &BTreeSet<CheckpointRefId>,
    ) -> Result<Self, CheckpointError> {
        Self::build(
            id,
            candidate,
            manifest,
            policy,
            pinned_refs,
            PreviousCheckpointValidation::Initial,
        )
    }

    pub(crate) fn from_rolling_candidate_with_pinned_refs(
        id: CheckpointId,
        candidate: CompactedCheckpointCandidate,
        manifest: CheckpointRefManifest,
        previous: &Self,
        policy: CheckpointValidationPolicy,
        pinned_refs: &BTreeSet<CheckpointRefId>,
    ) -> Result<Self, CheckpointError> {
        Self::build(
            id,
            candidate,
            manifest,
            policy,
            pinned_refs,
            PreviousCheckpointValidation::Rolling(previous),
        )
    }

    pub(super) fn from_prevalidated_persistence(
        id: CheckpointId,
        candidate: CompactedCheckpointCandidate,
        manifest: CheckpointRefManifest,
        persisted_refs: &BTreeSet<CheckpointRefId>,
    ) -> Result<Self, CheckpointError> {
        Self::build(
            id,
            candidate,
            manifest,
            CheckpointValidationPolicy::default(),
            persisted_refs,
            PreviousCheckpointValidation::PrevalidatedPersistence,
        )
    }

    fn build(
        id: CheckpointId,
        candidate: CompactedCheckpointCandidate,
        manifest: CheckpointRefManifest,
        _policy: CheckpointValidationPolicy,
        pinned_refs: &BTreeSet<CheckpointRefId>,
        previous: PreviousCheckpointValidation<'_>,
    ) -> Result<Self, CheckpointError> {
        let mut candidate = candidate;
        if let PreviousCheckpointValidation::Rolling(previous) = &previous {
            candidate.materialize_kept_entries(previous);
        }

        let manifest_refs = manifest
            .refs()
            .iter()
            .map(|reference| reference.id().clone())
            .collect::<BTreeSet<_>>();
        let mut used_refs = pinned_refs.clone();
        for ref_id in pinned_refs {
            if !manifest_refs.contains(ref_id) {
                return Err(CheckpointError::PinnedRefNotFound {
                    ref_id: ref_id.as_str().to_owned(),
                });
            }
        }
        for (_, entry) in candidate.sections().iter() {
            for ref_id in entry.refs() {
                if !manifest_refs.contains(ref_id) {
                    return Err(CheckpointError::UnknownRef {
                        entry_id: entry.id().as_str().to_owned(),
                        ref_id: ref_id.as_str().to_owned(),
                    });
                }
                used_refs.insert(ref_id.clone());
            }
        }

        match previous {
            PreviousCheckpointValidation::Initial if !candidate.handoffs().is_empty() => {
                return Err(CheckpointError::InitialCheckpointHasHandoffs);
            }
            PreviousCheckpointValidation::Initial
            | PreviousCheckpointValidation::Rolling(_)
            | PreviousCheckpointValidation::PrevalidatedPersistence => {}
        }

        let (sections, handoffs) = candidate.into_parts();
        Ok(Self {
            id: id.clone(),
            sections,
            manifest: manifest.filtered_for_checkpoint(id, &used_refs)?,
            handoffs,
        })
    }

    #[must_use]
    pub fn id(&self) -> &CheckpointId {
        &self.id
    }

    #[must_use]
    pub fn sections(&self) -> &CheckpointSections {
        &self.sections
    }

    #[must_use]
    pub fn manifest(&self) -> &CheckpointRefManifest {
        &self.manifest
    }

    #[must_use]
    pub fn handoffs(&self) -> &[CheckpointHandoff] {
        &self.handoffs
    }

    #[must_use]
    pub fn render_prompt_text(&self) -> String {
        let mut lines = Vec::with_capacity(self.sections.entry_count().saturating_mul(3) + 8);
        for section in CheckpointSection::ALL {
            lines.push(format!("{}:", section.as_str()));
            for entry in self.sections.entries(section) {
                lines.push(format!("- [{}] {}", entry.id().as_str(), entry.text()));
                if let Some(rationale) = entry.rationale() {
                    lines.push(format!("  reason: {rationale}"));
                }
                lines.push(format!("  refs: [{}]", format_ref_list(entry.refs())));
            }
        }
        lines.join("\n")
    }

    pub fn read_ref(&self, ref_id: &CheckpointRefId) -> Result<&CheckpointRef, CheckpointError> {
        self.manifest
            .get(ref_id)
            .ok_or_else(|| CheckpointError::RefNotFound {
                checkpoint_id: self.id.as_str().to_owned(),
                ref_id: ref_id.as_str().to_owned(),
            })
    }
}

enum PreviousCheckpointValidation<'a> {
    Initial,
    Rolling(&'a CitationBackedCheckpoint),
    PrevalidatedPersistence,
}
