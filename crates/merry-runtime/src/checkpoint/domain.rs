use super::{
    CheckpointError, CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest,
    CheckpointValidationPolicy, candidate::CompactedCheckpointCandidate, format_ref_list,
};
use crate::checkpoint::candidate::{
    CheckpointEntry, CheckpointEntryId, CheckpointHandoff, CheckpointSection, CheckpointSections,
};
use std::collections::{BTreeMap, BTreeSet};

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
            PreviousCheckpointValidation::Rolling(previous) => {
                validate_rolling_handoffs(previous, &candidate)?;
            }
            PreviousCheckpointValidation::PrevalidatedPersistence => {
                validate_persisted_handoffs(&candidate)?;
            }
            PreviousCheckpointValidation::Initial => {}
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

fn validate_rolling_handoffs(
    previous: &CitationBackedCheckpoint,
    candidate: &CompactedCheckpointCandidate,
) -> Result<(), CheckpointError> {
    let previous_entries = entry_map(previous.sections());
    let candidate_entries = entry_map(candidate.sections());
    let handoffs = candidate
        .handoffs()
        .iter()
        .map(|handoff| (handoff.old_id().clone(), handoff))
        .collect::<BTreeMap<_, _>>();

    for old_id in handoffs.keys() {
        if !previous_entries.contains_key(old_id) {
            return Err(CheckpointError::UnknownHandoffOldId {
                old_id: old_id.as_str().to_owned(),
            });
        }
    }
    for old_id in previous_entries.keys() {
        if !handoffs.contains_key(old_id) {
            return Err(CheckpointError::MissingHandoff {
                old_id: old_id.as_str().to_owned(),
            });
        }
    }

    for (old_id, handoff) in handoffs {
        let (old_section, old_entry) = previous_entries
            .get(&old_id)
            .expect("unknown handoff ids were rejected");
        match handoff {
            CheckpointHandoff::Keep { .. } => {
                let Some((new_section, new_entry)) = candidate_entries.get(&old_id) else {
                    return Err(CheckpointError::InvalidKeep {
                        old_id: old_id.as_str().to_owned(),
                    });
                };
                if old_section != new_section || old_entry != new_entry {
                    return Err(CheckpointError::InvalidKeep {
                        old_id: old_id.as_str().to_owned(),
                    });
                }
            }
            CheckpointHandoff::Replace { new_ids, .. } => {
                if candidate_entries.contains_key(&old_id) {
                    return Err(CheckpointError::InvalidReplace {
                        old_id: old_id.as_str().to_owned(),
                    });
                }
                for new_id in new_ids {
                    if previous_entries.contains_key(new_id) {
                        return Err(CheckpointError::ReplacementEntryNotNew {
                            old_id: old_id.as_str().to_owned(),
                            new_id: new_id.as_str().to_owned(),
                        });
                    }
                    if !candidate_entries.contains_key(new_id) {
                        return Err(CheckpointError::ReplacementEntryNotFound {
                            old_id: old_id.as_str().to_owned(),
                            new_id: new_id.as_str().to_owned(),
                        });
                    }
                }
            }
            CheckpointHandoff::Drop { .. } => {
                if candidate_entries.contains_key(&old_id) {
                    return Err(CheckpointError::InvalidDrop {
                        old_id: old_id.as_str().to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_persisted_handoffs(
    candidate: &CompactedCheckpointCandidate,
) -> Result<(), CheckpointError> {
    let candidate_entries = entry_map(candidate.sections());
    let declared_old_ids = candidate
        .handoffs()
        .iter()
        .map(|handoff| handoff.old_id().clone())
        .collect::<BTreeSet<_>>();
    for handoff in candidate.handoffs() {
        let old_id = handoff.old_id();
        match handoff {
            CheckpointHandoff::Keep { .. } => {
                if !candidate_entries.contains_key(old_id) {
                    return Err(CheckpointError::InvalidKeep {
                        old_id: old_id.as_str().to_owned(),
                    });
                }
            }
            CheckpointHandoff::Replace { new_ids, .. } => {
                if candidate_entries.contains_key(old_id) {
                    return Err(CheckpointError::InvalidReplace {
                        old_id: old_id.as_str().to_owned(),
                    });
                }
                for new_id in new_ids {
                    if declared_old_ids.contains(new_id) {
                        return Err(CheckpointError::ReplacementEntryNotNew {
                            old_id: old_id.as_str().to_owned(),
                            new_id: new_id.as_str().to_owned(),
                        });
                    }
                    if !candidate_entries.contains_key(new_id) {
                        return Err(CheckpointError::ReplacementEntryNotFound {
                            old_id: old_id.as_str().to_owned(),
                            new_id: new_id.as_str().to_owned(),
                        });
                    }
                }
            }
            CheckpointHandoff::Drop { .. } => {
                if candidate_entries.contains_key(old_id) {
                    return Err(CheckpointError::InvalidDrop {
                        old_id: old_id.as_str().to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn entry_map(
    sections: &CheckpointSections,
) -> BTreeMap<CheckpointEntryId, (CheckpointSection, &CheckpointEntry)> {
    sections
        .iter()
        .map(|(section, entry)| (entry.id().clone(), (section, entry)))
        .collect()
}
