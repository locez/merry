use super::{
    ModelTurnId, SessionState,
    artifacts::{assistant_output_id, process_input_id},
};
use crate::{
    RuntimeError,
    artifact::{ArtifactContent, ArtifactError},
    ledger::LedgerFactKind,
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceRef, RuntimeJournalEvent, RuntimeJournalPayload,
};

impl SessionState {
    pub(crate) fn record_artifact_state(
        &mut self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<ArtifactRef, ArtifactError> {
        self.artifacts.record(artifact, content)
    }

    pub(crate) fn record_artifact_events(
        &mut self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeJournalEvent>, ArtifactError> {
        let content_bytes = content.as_bytes().len();
        let recorded = self.record_artifact_state(artifact, content)?;
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        let mut events = Vec::with_capacity(if self.session_started { 1 } else { 2 });

        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        events.push(self.record_event(
            RuntimeJournalPayload::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ));

        Ok(events)
    }

    pub(crate) fn record_process_input_artifact(
        &mut self,
        content: ArtifactContent,
    ) -> Result<(ArtifactRef, Vec<RuntimeJournalEvent>), ArtifactError> {
        let artifact = ArtifactRef::new(process_input_id(self.next_sequence()), ArtifactKind::Json);
        let events = self.record_artifact_events(artifact.clone(), content)?;
        Ok((artifact, events))
    }

    pub(crate) fn record_assistant_text_output(
        &mut self,
        turn_id: ModelTurnId,
        text: String,
    ) -> Result<RuntimeJournalEvent, RuntimeError> {
        let artifact_sequence = self.next_sequence();
        let artifact = ArtifactRef::new(assistant_output_id(artifact_sequence), ArtifactKind::Text);
        let content = ArtifactContent::text(text);
        let content_bytes = content.as_bytes().len();
        self.artifacts.ensure_recordable(&artifact, &content)?;
        let mut transcript = self.transcript.clone();
        transcript.push_assistant_text(turn_id, artifact.id().clone())?;
        let recorded = self.artifacts.record_preflighted(artifact, content);
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        self.transcript = transcript;
        Ok(self.record_event(
            RuntimeJournalPayload::AssistantOutputRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ))
    }

    pub(crate) fn read_artifact_content(
        &self,
        artifact_id: &ArtifactId,
    ) -> Result<ArtifactContent, ArtifactError> {
        self.artifacts.read_content(artifact_id).cloned()
    }

    pub(crate) fn validate_plan_refs(
        &self,
        evidence_refs: &[EvidenceRef],
        artifact_refs: &[ArtifactRef],
    ) -> Result<(), crate::plan::PlanError> {
        for artifact in artifact_refs {
            let stored = self.artifacts.read_ref(artifact.id()).map_err(|_| {
                crate::plan::PlanError::MissingArtifactRef {
                    artifact_id: artifact.id().clone(),
                }
            })?;
            if stored != artifact {
                return Err(crate::plan::PlanError::MissingArtifactRef {
                    artifact_id: artifact.id().clone(),
                });
            }
        }
        for evidence in evidence_refs {
            self.artifacts.validate_evidence(evidence).map_err(|_| {
                crate::plan::PlanError::InvalidEvidenceRef {
                    artifact_id: evidence.artifact_id.clone(),
                }
            })?;
        }
        Ok(())
    }

    pub(super) fn trace_artifact_record(
        session_id: &str,
        artifact: &ArtifactRef,
        byte_count: usize,
    ) {
        tracing::info!(
            event = "runtime.artifact.record",
            session_id,
            artifact_id = artifact.id().as_str(),
            artifact_kind = ?artifact.kind(),
            byte_count,
            "runtime artifact recorded"
        );
    }
}
