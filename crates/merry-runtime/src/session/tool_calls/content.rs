use super::SessionState;
use crate::{ArtifactContent, RuntimeError, ledger::LedgerFactKind, tool::ToolResultContent};
use merry_core::{
    ArtifactId, ArtifactRef, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallResult,
};

pub(super) struct ToolResultArtifacts {
    full: (ArtifactRef, ArtifactContent),
    model: Option<(ArtifactRef, ArtifactContent)>,
}

impl ToolResultArtifacts {
    pub(super) fn model_artifact_id(&self) -> Option<ArtifactId> {
        self.model
            .as_ref()
            .map(|(artifact, _)| artifact.id().clone())
    }
}

impl SessionState {
    /// Preflights both artifacts before transcript or pending-call state changes.
    pub(super) fn prepare_tool_result_artifacts(
        &self,
        result: &ToolCallResult,
        content: ToolResultContent,
    ) -> Result<ToolResultArtifacts, RuntimeError> {
        self.validate_tool_result_content(result, content.artifact())?;
        self.artifacts
            .ensure_recordable(result.artifact(), content.artifact())?;
        let model_ref = self.validate_model_result_content(content.artifact(), content.model())?;
        let (full, model) = content.into_parts();
        Ok(ToolResultArtifacts {
            full: (result.artifact().clone(), full),
            model: model_ref.zip(model),
        })
    }

    pub(super) fn validate_model_result_content(
        &self,
        full: &ArtifactContent,
        model: Option<&ArtifactContent>,
    ) -> Result<Option<ArtifactRef>, RuntimeError> {
        let Some(model) = model.filter(|model| *model != full) else {
            return Ok(None);
        };
        let kind = self.tool_result_artifact_kind(model)?;
        let artifact = ArtifactRef::new(
            super::super::artifacts::tool_result_model_id(self.next_sequence()),
            kind,
        );
        self.validate_tool_result_artifact(&artifact, model)?;
        self.artifacts.ensure_recordable(&artifact, model)?;
        Ok(Some(artifact))
    }

    /// Records both bodies before creating any artifact lifecycle event.
    pub(super) fn record_tool_result_artifacts(
        &mut self,
        artifacts: ToolResultArtifacts,
    ) -> Vec<RuntimeJournalEvent> {
        let recorded = std::iter::once(artifacts.full)
            .chain(artifacts.model)
            .map(|(artifact, content)| {
                let bytes = content.as_bytes().len();
                let recorded = self.artifacts.record_preflighted(artifact, content);
                Self::trace_artifact_record(self.session_id.as_str(), &recorded, bytes);
                recorded
            })
            .collect::<Vec<_>>();
        recorded
            .into_iter()
            .map(|artifact| {
                self.record_event(
                    RuntimeJournalPayload::ArtifactRecorded { artifact },
                    LedgerFactKind::ArtifactRecorded,
                )
            })
            .collect()
    }
}
