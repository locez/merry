use super::{ContextEntry, SessionContextSnapshot};
use crate::memory::{
    ActivatedMemory, MemoryActivationProvenance, MemoryActivationReason, MemoryActivationScore,
    MemoryActivationSourceKind, MemoryEvidence, MemoryId, MemoryItem, MemoryItemSelection,
    MemoryScope,
};

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
        vec![memory_evidence("primary source", &format!("{id}-source"))],
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

pub(super) fn memory_evidence(label: &str, source: &str) -> MemoryEvidence {
    MemoryEvidence::new(label, source).expect("memory evidence is valid")
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
    SessionContextSnapshot::new(entries, memories, None)
}
