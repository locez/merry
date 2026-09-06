mod activation;
mod source;
mod validation;

use super::{
    ActivatedMemory, MemoryActivationProvenance, MemoryActivationSeed, MemoryActivationSourceKind,
    MemoryEvidence, MemoryId, MemoryItem, MemoryItemSelection, MemoryScope,
};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};

fn memory_id(value: &str) -> MemoryId {
    MemoryId::new(value).expect("memory id is valid")
}

fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("artifact id is valid")
}

fn evidence_ref(id: &str) -> EvidenceRef {
    EvidenceRef::new(artifact_id(id), EvidenceLocator::whole_artifact())
}

fn evidence(label: &str) -> MemoryEvidence {
    MemoryEvidence::new(label, evidence_ref(&format!("artifact-{label}")))
        .expect("memory evidence is valid")
}

fn provenance() -> MemoryActivationProvenance {
    MemoryActivationProvenance::new(
        "topic",
        vec![MemoryScope::Session],
        MemoryActivationSourceKind::UserQuery,
        "user request",
    )
    .expect("provenance is valid")
}

fn seed(query: &str, scopes: Vec<MemoryScope>) -> MemoryActivationSeed {
    MemoryActivationSeed::new(
        query,
        scopes,
        MemoryActivationSourceKind::UserQuery,
        "user request",
    )
    .expect("seed is valid")
}

fn item(
    id: &str,
    scope: MemoryScope,
    triggers: &[&str],
    confidence: f32,
    priority: i32,
    conflict_key: Option<&str>,
) -> MemoryItem {
    MemoryItem::new(
        memory_id(id),
        scope,
        format!("{id} text"),
        vec![evidence("source")],
        selection(triggers, confidence, priority, conflict_key),
    )
    .expect("memory item is valid")
}

fn selection(
    triggers: &[&str],
    confidence: f32,
    priority: i32,
    conflict_key: Option<&str>,
) -> MemoryItemSelection {
    MemoryItemSelection::new(
        triggers
            .iter()
            .map(|trigger| (*trigger).to_owned())
            .collect(),
        confidence,
        priority,
        conflict_key.map(str::to_owned),
    )
    .expect("memory item selection is valid")
}

fn ids(activated: &[ActivatedMemory]) -> Vec<&str> {
    activated
        .iter()
        .map(|memory| memory.item().id().as_str())
        .collect()
}

fn ids_from_memory_ids(ids: &[MemoryId]) -> Vec<&str> {
    ids.iter().map(MemoryId::as_str).collect()
}
