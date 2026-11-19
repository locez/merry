use super::SessionState;
use crate::{
    RuntimeError,
    context::{
        ContextCompiler, ContextEntry, ContextError, ContextSummary, ProjectRules,
        SessionContextSnapshot, TaskAnchor,
    },
    memory::{ActivatedMemory, MemoryError, MemoryItem, MemoryStore},
    skill::SkillCatalog,
};

impl SessionState {
    pub(crate) fn reconcile_construction_context_seed(
        &mut self,
        id: &str,
        text: &str,
    ) -> Result<(), RuntimeError> {
        if self
            .context_entries
            .iter()
            .any(|entry| entry.as_summary().id() == id && entry.as_summary().text() == text)
        {
            return Ok(());
        }

        let summary = ContextSummary::new(id, text)?;
        let mut candidate_entries = self
            .context_entries
            .iter()
            .filter(|entry| entry.as_summary().id() != id)
            .cloned()
            .collect::<Vec<_>>();
        candidate_entries.push(ContextEntry::summary(summary));
        let candidate_snapshot = SessionContextSnapshot::new(
            candidate_entries.clone(),
            self.activated_memories.clone(),
            self.compacted_checkpoint.clone(),
        );
        ContextCompiler::new().compile(&candidate_snapshot)?;

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
            self.activated_memories.clone(),
            self.compacted_checkpoint.clone(),
        )
    }
}
