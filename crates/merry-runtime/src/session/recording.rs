use super::{
    ModelTurnId, SessionState,
    artifacts::{assistant_output_id, process_input_id},
};
use crate::{
    RuntimeError,
    artifact::{ArtifactContent, ArtifactContentPreview, ArtifactError, ArtifactRegistry},
    ledger::LedgerFactKind,
    plan::{PlanArtifactPromotion, PlanError},
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceRef, RuntimeJournalEvent, RuntimeJournalPayload,
};
use std::collections::BTreeSet;

#[allow(dead_code)]
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

    pub(crate) fn read_artifact_preview(
        &self,
        artifact_id: &ArtifactId,
        max_bytes: usize,
    ) -> Result<ArtifactContentPreview, ArtifactError> {
        self.artifacts.read_content_preview(artifact_id, max_bytes)
    }

    pub(crate) fn validate_plan_refs(
        &self,
        evidence_refs: &[EvidenceRef],
        artifact_refs: &[ArtifactRef],
    ) -> Result<(), PlanError> {
        Self::validate_plan_refs_in(&self.artifacts, evidence_refs, artifact_refs)
    }

    pub(crate) fn collect_plan_artifact_records(
        &self,
        evidence_refs: &[EvidenceRef],
        artifact_refs: &[ArtifactRef],
    ) -> Result<Vec<(ArtifactRef, ArtifactContent)>, PlanError> {
        self.validate_plan_refs(evidence_refs, artifact_refs)?;
        let ids = artifact_refs
            .iter()
            .map(|artifact| artifact.id().clone())
            .chain(
                evidence_refs
                    .iter()
                    .map(|evidence| evidence.artifact_id.clone()),
            )
            .collect::<BTreeSet<_>>();
        ids.into_iter()
            .map(|id| {
                let artifact = self
                    .artifacts
                    .read_ref(&id)
                    .expect("validated plan artifact remains present")
                    .clone();
                let content = self
                    .artifacts
                    .read_content(&id)
                    .expect("validated plan artifact content remains present")
                    .clone();
                Ok((artifact, content))
            })
            .collect()
    }

    pub(crate) fn artifacts_with_plan_promotions(
        &self,
        promotions: &[PlanArtifactPromotion],
    ) -> Result<ArtifactRegistry, PlanError> {
        let mut artifacts = self.artifacts.clone();
        for promotion in promotions {
            match artifacts.read_record(promotion.artifact.id()) {
                Ok(existing)
                    if existing.artifact() == &promotion.artifact
                        && existing.content() == &promotion.content => {}
                Ok(_) => {
                    return Err(PlanError::ArtifactPromotionConflict {
                        artifact_id: promotion.artifact.id().clone(),
                    });
                }
                Err(ArtifactError::MissingArtifact { .. }) => {
                    artifacts
                        .record(promotion.artifact.clone(), promotion.content.clone())
                        .map_err(|_| PlanError::ArtifactPromotionConflict {
                            artifact_id: promotion.artifact.id().clone(),
                        })?;
                }
                Err(_) => {
                    return Err(PlanError::ArtifactPromotionConflict {
                        artifact_id: promotion.artifact.id().clone(),
                    });
                }
            }
        }
        Ok(artifacts)
    }

    pub(crate) fn validate_plan_refs_in(
        artifacts: &ArtifactRegistry,
        evidence_refs: &[EvidenceRef],
        artifact_refs: &[ArtifactRef],
    ) -> Result<(), PlanError> {
        for artifact in artifact_refs {
            let stored =
                artifacts
                    .read_ref(artifact.id())
                    .map_err(|_| PlanError::MissingArtifactRef {
                        artifact_id: artifact.id().clone(),
                    })?;
            if stored != artifact {
                return Err(PlanError::MissingArtifactRef {
                    artifact_id: artifact.id().clone(),
                });
            }
        }
        for evidence in evidence_refs {
            artifacts
                .validate_evidence(evidence)
                .map_err(|_| PlanError::InvalidEvidenceRef {
                    artifact_id: evidence.artifact_id.clone(),
                })?;
        }
        Ok(())
    }

    pub(crate) fn replace_artifacts_for_plan_commit(&mut self, artifacts: ArtifactRegistry) {
        self.artifacts = artifacts;
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
