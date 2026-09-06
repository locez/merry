use super::{ContextError, input::ContextEvidence};
use crate::{artifact::ArtifactRegistry, memory::ActivatedMemory};
use merry_core::EvidenceLocator;

pub(super) fn validate_evidence(
    summary_id: &str,
    evidence: &[ContextEvidence],
    artifacts: &ArtifactRegistry,
) -> Result<(), ContextError> {
    for item in evidence {
        artifacts
            .validate_evidence(item.reference())
            .map_err(|source| ContextError::UnreadableEvidence {
                summary_id: summary_id.to_owned(),
                artifact_id: item.reference().artifact_id.clone(),
                source,
            })?;
    }

    Ok(())
}

pub(super) fn validate_memory_evidence(
    memories: &[ActivatedMemory],
    artifacts: &ArtifactRegistry,
) -> Result<(), ContextError> {
    for memory in memories {
        if memory.item().evidence().is_empty() {
            return Err(ContextError::MemoryWithoutEvidence {
                memory_id: memory.item().id().as_str().to_owned(),
            });
        }

        for item in memory.item().evidence() {
            artifacts
                .validate_evidence(item.reference())
                .map_err(|source| ContextError::UnreadableMemoryEvidence {
                    memory_id: memory.item().id().as_str().to_owned(),
                    artifact_id: item.reference().artifact_id.clone(),
                    source,
                })?;
        }
    }

    Ok(())
}

pub(super) fn format_locator(locator: &EvidenceLocator) -> String {
    if locator.is_whole_artifact() {
        return "whole".to_owned();
    }

    if let Some((start, end)) = locator.as_line_range() {
        return format!("line:{start}-{end}");
    }

    if let Some((start, end)) = locator.as_byte_range() {
        return format!("byte:{start}-{end}");
    }

    if let Some(pointer) = locator.as_json_pointer() {
        return format!("json:{pointer}");
    }

    if let Some(name) = locator.as_named_section() {
        return format!("section:{name}");
    }

    unreachable!("all evidence locator variants are covered by public accessors")
}
