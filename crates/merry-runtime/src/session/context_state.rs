use super::SessionState;
use crate::{
    RuntimeError,
    artifact::{ArtifactContent, ArtifactError, ArtifactRegistry},
    context::{
        ContextCompiler, ContextEntry, ContextError, ContextEvidence, ContextSummary, ProjectRules,
        SessionContextSnapshot, TaskAnchor, stable_content_hash,
    },
    memory::{ActivatedMemory, MemoryError, MemoryItem, MemoryStore},
    skill::SkillCatalog,
};
use merry_core::{ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef};

const CONTEXT_SEED_REFRESH_ARTIFACT_PREFIX: &str = "context-seed-refresh-";
const SEEDED_RUNTIME_CONTEXT_LABEL: &str = "seeded runtime context";

impl SessionState {
    pub(crate) fn reconcile_construction_context_seed(
        &mut self,
        id: &str,
        text: &str,
    ) -> Result<(), RuntimeError> {
        let mut managed_predecessor = None;
        for (index, entry) in self.context_entries.iter().enumerate() {
            if !is_managed_construction_context_seed(entry.as_summary(), id, &self.artifacts)? {
                continue;
            }
            if managed_predecessor.replace(index).is_some() {
                return Err(
                    ContextError::AmbiguousConstructionContextSeed { id: id.to_owned() }.into(),
                );
            }
        }

        if managed_predecessor
            .is_some_and(|index| self.context_entries[index].as_summary().text() == text)
        {
            return Ok(());
        }

        let mut candidate_artifacts = self.artifacts.clone();
        let mut candidate_entries = self.context_entries.clone();
        if let Some(index) = managed_predecessor {
            candidate_entries.remove(index);
        }

        let target_artifact_id = refreshed_context_seed_artifact_id(id, text)?;
        let recorded = match candidate_artifacts.read_record(&target_artifact_id) {
            Ok(record)
                if record.artifact().kind() == &ArtifactKind::Text
                    && matches!(
                        record.content(),
                        ArtifactContent::Text { content } if content == text
                    ) =>
            {
                record.artifact().clone()
            }
            Ok(_) => {
                return Err(ContextError::ConstructionContextSeedArtifactConflict {
                    id: id.to_owned(),
                    artifact_id: target_artifact_id,
                }
                .into());
            }
            Err(ArtifactError::MissingArtifact { .. }) => candidate_artifacts.record(
                ArtifactRef::new(target_artifact_id, ArtifactKind::Text),
                ArtifactContent::text(text),
            )?,
            Err(source) => return Err(source.into()),
        };
        let evidence =
            candidate_artifacts.evidence_ref(recorded.id(), EvidenceLocator::whole_artifact())?;
        let summary = ContextSummary::new(
            id,
            text,
            vec![ContextEvidence::new(
                SEEDED_RUNTIME_CONTEXT_LABEL,
                evidence,
            )?],
        )?;
        candidate_entries.push(ContextEntry::summary(summary));
        let candidate_snapshot = SessionContextSnapshot::new(
            candidate_entries.clone(),
            candidate_artifacts.clone(),
            self.activated_memories.clone(),
            self.compacted_checkpoint.clone(),
        );
        ContextCompiler::new().compile(&candidate_snapshot)?;

        self.artifacts = candidate_artifacts;
        self.context_entries = candidate_entries;
        Ok(())
    }

    pub(crate) fn set_project_rules(&mut self, project_rules: ProjectRules) {
        self.project_rules = Some(project_rules);
    }

    pub(crate) fn project_rules(&self) -> Option<ProjectRules> {
        self.project_rules.clone()
    }

    pub(crate) fn set_skill_catalog(&mut self, skill_catalog: SkillCatalog) {
        self.skill_catalog = Some(skill_catalog);
    }

    pub(crate) fn skill_catalog(&self) -> Option<SkillCatalog> {
        self.skill_catalog.clone()
    }

    pub(crate) fn set_task_anchor(&mut self, task_anchor: TaskAnchor) {
        self.task_anchor = Some(task_anchor);
    }

    pub(crate) fn task_anchor(&self) -> Option<TaskAnchor> {
        self.task_anchor.clone()
    }

    pub(crate) fn evidence_ref(
        &self,
        artifact_id: &ArtifactId,
        locator: EvidenceLocator,
    ) -> Result<EvidenceRef, ArtifactError> {
        self.artifacts.evidence_ref(artifact_id, locator)
    }

    pub(crate) fn record_context_entry(&mut self, entry: ContextEntry) -> Result<(), ContextError> {
        self.record_checked_context_entry(entry)
    }

    pub(super) fn record_checked_context_entry(
        &mut self,
        entry: ContextEntry,
    ) -> Result<(), ContextError> {
        let mut candidate_entries = self.context_entries.clone();
        candidate_entries.push(entry.clone());
        let candidate_snapshot = SessionContextSnapshot::new(
            candidate_entries,
            self.artifacts.clone(),
            self.activated_memories.clone(),
            self.compacted_checkpoint.clone(),
        );
        ContextCompiler::new().compile(&candidate_snapshot)?;

        self.context_entries.push(entry);
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn record_memory_item(&mut self, item: MemoryItem) -> Result<(), MemoryError> {
        self.memory_store.record(item)
    }

    pub(crate) fn memory_store(&self) -> &MemoryStore {
        &self.memory_store
    }

    #[allow(dead_code)]
    pub(crate) fn record_activated_memory(&mut self, memory: ActivatedMemory) {
        self.activated_memories.push(memory);
    }

    #[allow(dead_code)]
    pub(crate) fn record_activated_memories(&mut self, memories: Vec<ActivatedMemory>) {
        self.activated_memories.extend(memories);
    }

    pub(crate) fn replace_activated_memories(&mut self, memories: Vec<ActivatedMemory>) {
        self.activated_memories = memories;
    }

    pub(crate) fn context_snapshot(&self) -> SessionContextSnapshot {
        SessionContextSnapshot::new(
            self.context_entries.clone(),
            self.artifacts.clone(),
            self.activated_memories.clone(),
            self.compacted_checkpoint.clone(),
        )
    }
}

fn refreshed_context_seed_artifact_id(id: &str, text: &str) -> Result<ArtifactId, RuntimeError> {
    let mut hash_input = String::with_capacity(id.len() + 1 + text.len());
    hash_input.push_str(id);
    hash_input.push('\0');
    hash_input.push_str(text);
    ArtifactId::new(&format!(
        "{CONTEXT_SEED_REFRESH_ARTIFACT_PREFIX}{}",
        stable_content_hash(hash_input.as_bytes())
    ))
    .map_err(Into::into)
}

fn is_managed_construction_context_seed(
    summary: &ContextSummary,
    id: &str,
    artifacts: &ArtifactRegistry,
) -> Result<bool, RuntimeError> {
    if summary.id() != id || summary.evidence().len() != 1 {
        return Ok(false);
    }

    let evidence = &summary.evidence()[0];
    let reference = evidence.reference();
    if evidence.label() != SEEDED_RUNTIME_CONTEXT_LABEL || !reference.locator.is_whole_artifact() {
        return Ok(false);
    }

    let refreshed_artifact_id = refreshed_context_seed_artifact_id(summary.id(), summary.text())?;
    if reference.artifact_id != refreshed_artifact_id {
        return Ok(false);
    }

    Ok(matches!(
        artifacts.read_content(&reference.artifact_id),
        Ok(ArtifactContent::Text { content }) if content == summary.text()
    ))
}
