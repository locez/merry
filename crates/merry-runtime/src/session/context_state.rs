use super::SessionState;
use crate::{
    RuntimeError,
    artifact::{ArtifactContent, ArtifactError},
    context::{
        ContextCompiler, ContextEntry, ContextError, ContextEvidence, ContextSummary, ProjectRules,
        SessionContextSnapshot, TaskAnchor,
    },
    memory::{ActivatedMemory, MemoryError, MemoryItem, MemoryStore},
    skill::SkillCatalog,
};
use merry_core::{ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef};

impl SessionState {
    pub(crate) fn seed_context_summary(
        &mut self,
        id: &str,
        text: &str,
    ) -> Result<(), RuntimeError> {
        let artifact_id = ArtifactId::new(&format!("context-seed-{id}"))?;
        let artifact = ArtifactRef::new(artifact_id.clone(), ArtifactKind::Text);
        let content = ArtifactContent::text(text);
        let recorded = self.artifacts.record(artifact, content)?;
        let evidence = self
            .artifacts
            .evidence_ref(recorded.id(), EvidenceLocator::whole_artifact())?;
        let summary = ContextSummary::new(
            id,
            text,
            vec![ContextEvidence::new("seeded runtime context", evidence)?],
        )?;
        self.record_checked_context_entry(ContextEntry::summary(summary))?;
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
