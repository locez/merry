use super::{ContextEntry, SessionContextSnapshot};
use crate::{
    artifact::{ArtifactContent, ArtifactRegistry},
    memory::{
        ActivatedMemory, MemoryActivationProvenance, MemoryActivationReason, MemoryActivationScore,
        MemoryActivationSourceKind, MemoryEvidence, MemoryId, MemoryItem, MemoryItemSelection,
        MemoryScope,
    },
};
use merry_core::{ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef};

mod budget;
mod input;
mod projection;

pub(super) fn activated_memory(
    id: &str,
    scope: MemoryScope,
    text: &str,
    score: MemoryActivationScore,
    reasons: Vec<MemoryActivationReason>,
) -> ActivatedMemory {
    activated_memory_with_evidence(
        id,
        scope,
        text,
        vec![memory_evidence(
            "primary source",
            &format!("{id}-artifact"),
            EvidenceLocator::whole_artifact(),
        )],
        score,
        reasons,
        provenance(),
    )
}

pub(super) fn activated_memory_with_evidence(
    id: &str,
    scope: MemoryScope,
    text: &str,
    evidence: Vec<MemoryEvidence>,
    score: MemoryActivationScore,
    reasons: Vec<MemoryActivationReason>,
    provenance: MemoryActivationProvenance,
) -> ActivatedMemory {
    let item = MemoryItem::new(
        memory_id(id),
        scope,
        text,
        evidence,
        memory_selection(score.confidence().as_f32(), score.priority()),
    )
    .expect("memory item is valid");
    ActivatedMemory::new(item, score, reasons, provenance).expect("activated memory is valid")
}

pub(super) fn ranked_reasons(
    matches: usize,
    priority: i32,
    confidence: f32,
) -> Vec<MemoryActivationReason> {
    vec![
        MemoryActivationReason::ScopeAllowed,
        MemoryActivationReason::trigger_matched("topic").expect("valid trigger"),
        MemoryActivationReason::ranked(score(matches, priority, confidence)),
    ]
}

pub(super) fn score(matches: usize, priority: i32, confidence: f32) -> MemoryActivationScore {
    MemoryActivationScore::new(matches, priority, confidence).expect("score is valid")
}

pub(super) fn provenance() -> MemoryActivationProvenance {
    labeled_provenance("user request")
}

pub(super) fn labeled_provenance(label: &str) -> MemoryActivationProvenance {
    MemoryActivationProvenance::new(
        "topic",
        vec![MemoryScope::Session, MemoryScope::Task, MemoryScope::Step],
        MemoryActivationSourceKind::UserQuery,
        label,
    )
    .expect("provenance is valid")
}

pub(super) fn memory_id(value: &str) -> MemoryId {
    MemoryId::new(value).expect("memory id is valid")
}

pub(super) fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("artifact id is valid")
}

pub(super) fn memory_evidence(
    label: &str,
    artifact: &str,
    locator: EvidenceLocator,
) -> MemoryEvidence {
    MemoryEvidence::new(label, EvidenceRef::new(artifact_id(artifact), locator))
        .expect("memory evidence is valid")
}

pub(super) fn memory_selection(confidence: f32, priority: i32) -> MemoryItemSelection {
    MemoryItemSelection::new(vec!["topic".to_owned()], confidence, priority, None)
        .expect("memory selection is valid")
}

pub(super) fn memory_snapshot(memories: Vec<ActivatedMemory>) -> SessionContextSnapshot {
    snapshot_with_memories(Vec::new(), memories)
}

pub(super) fn snapshot_with_memories(
    entries: Vec<ContextEntry>,
    memories: Vec<ActivatedMemory>,
) -> SessionContextSnapshot {
    let mut artifacts = ArtifactRegistry::default();
    record_text_artifacts(&mut artifacts, &memories);
    SessionContextSnapshot::new(entries, artifacts, memories, None)
}

pub(super) fn record_text_artifacts(
    artifacts: &mut ArtifactRegistry,
    memories: &[ActivatedMemory],
) {
    let mut seen = std::collections::BTreeSet::new();

    for memory in memories {
        for evidence in memory.item().evidence() {
            if !seen.insert(evidence.reference().artifact_id.clone()) {
                continue;
            }

            artifacts
                .record(
                    ArtifactRef::new(evidence.reference().artifact_id.clone(), ArtifactKind::Text),
                    ArtifactContent::text(format!(
                        "evidence for {}\n{}",
                        memory.item().id(),
                        memory.item().text()
                    )),
                )
                .expect("memory artifact records");
        }
    }
}
