use super::SessionState;
use crate::{
    FileSessionStore, SessionStoreError,
    artifact::{ArtifactContent, ArtifactError, ArtifactRegistry},
    plan::{PersistedPlanState, PlanState},
};
use merry_core::{ArtifactId, ArtifactRef, PlanSnapshot, SessionId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const PLAN_OVERLAY_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPlanOverlayDocument {
    format_version: u32,
    session_id: SessionId,
    next_sequence: u64,
    base_active_plan: Option<StoredPlanVersion>,
    base_terminal_plan_count: usize,
    active_plan: PersistedPlanState,
    terminal_plans: Vec<PlanSnapshot>,
    artifacts: Vec<StoredPlanArtifact>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredPlanVersion {
    plan_id: merry_core::PlanId,
    revision: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPlanArtifact {
    artifact: ArtifactRef,
    content: ArtifactContent,
}

#[derive(Debug, Clone)]
pub(crate) struct PersistablePlanOverlayBundle {
    session_id: SessionId,
    document_bytes: Vec<u8>,
}

impl SessionState {
    pub(crate) fn persistable_plan_overlay(
        &self,
        active_plan: &PlanState,
        terminal_plans: &[PlanSnapshot],
        next_sequence: u64,
        artifacts: &ArtifactRegistry,
    ) -> Result<PersistablePlanOverlayBundle, SessionStoreError> {
        validate_plan_snapshot_refs(artifacts, active_plan.snapshot())?;
        for terminal in terminal_plans {
            validate_plan_snapshot_refs(artifacts, terminal)?;
        }
        let artifact_ids = plan_artifact_ids(active_plan.snapshot())
            .into_iter()
            .chain(terminal_plans.iter().flat_map(plan_artifact_ids))
            .collect::<BTreeSet<_>>();
        let stored_artifacts = artifact_ids
            .into_iter()
            .map(|artifact_id| {
                let artifact = artifacts
                    .read_ref(&artifact_id)
                    .map_err(|_| invalid_document("plan overlay artifact is missing"))?
                    .clone();
                let content = artifacts
                    .read_content(&artifact_id)
                    .map_err(|_| invalid_document("plan overlay artifact content is missing"))?
                    .clone();
                Ok(StoredPlanArtifact { artifact, content })
            })
            .collect::<Result<Vec<_>, SessionStoreError>>()?;
        let document = StoredPlanOverlayDocument {
            format_version: PLAN_OVERLAY_FORMAT_VERSION,
            session_id: self.session_id.clone(),
            next_sequence,
            base_active_plan: self.active_plan.as_ref().map(|plan| StoredPlanVersion {
                plan_id: plan.snapshot().plan_id.clone(),
                revision: plan.snapshot().revision,
            }),
            base_terminal_plan_count: self.terminal_plans.len(),
            active_plan: active_plan.persisted(),
            terminal_plans: terminal_plans.to_vec(),
            artifacts: stored_artifacts,
        };
        Ok(PersistablePlanOverlayBundle {
            session_id: self.session_id.clone(),
            document_bytes: serde_json::to_vec_pretty(&document)?,
        })
    }

    pub(super) fn apply_plan_overlay(
        &mut self,
        bytes: &[u8],
        allow_without_base: bool,
    ) -> Result<(), SessionStoreError> {
        let document: StoredPlanOverlayDocument = serde_json::from_slice(bytes)?;
        if document.format_version != PLAN_OVERLAY_FORMAT_VERSION {
            return Err(SessionStoreError::UnsupportedFormatVersion {
                actual: document.format_version,
            });
        }
        if document.session_id != self.session_id {
            return Err(SessionStoreError::SessionIdMismatch {
                requested: self.session_id.clone(),
                actual: document.session_id,
            });
        }
        if document.terminal_plans.len() > 8 {
            return Err(invalid_document(
                "plan overlay contains too many terminal plans",
            ));
        }
        let active_plan = PlanState::from_persisted(document.active_plan)
            .map_err(|_| invalid_document("stored plan overlay is invalid"))?;
        let current_version = self.active_plan.as_ref().map(|plan| StoredPlanVersion {
            plan_id: plan.snapshot().plan_id.clone(),
            revision: plan.snapshot().revision,
        });
        let candidate_version = StoredPlanVersion {
            plan_id: active_plan.snapshot().plan_id.clone(),
            revision: active_plan.snapshot().revision,
        };
        let base_matches = current_version == document.base_active_plan
            && self.terminal_plans.len() == document.base_terminal_plan_count;
        let candidate_is_current_or_newer = current_version.as_ref().is_some_and(|current| {
            current.plan_id == candidate_version.plan_id
                && current.revision >= candidate_version.revision
        });
        if !allow_without_base && !base_matches && !candidate_is_current_or_newer {
            return Ok(());
        }
        if candidate_is_current_or_newer
            && current_version
                .as_ref()
                .is_some_and(|current| current.revision > candidate_version.revision)
        {
            return Ok(());
        }

        let mut artifacts = self.artifacts.clone();
        for stored in document.artifacts {
            match artifacts.read_record(stored.artifact.id()) {
                Ok(existing)
                    if existing.artifact() == &stored.artifact
                        && existing.content() == &stored.content => {}
                Ok(_) => {
                    return Err(invalid_document(
                        "plan overlay artifact conflicts with session state",
                    ));
                }
                Err(ArtifactError::MissingArtifact { .. }) => {
                    artifacts
                        .record(stored.artifact, stored.content)
                        .map_err(|_| invalid_document("plan overlay artifact is invalid"))?;
                }
                Err(_) => {
                    return Err(invalid_document("plan overlay artifact is invalid"));
                }
            }
        }
        validate_plan_snapshot_refs(&artifacts, active_plan.snapshot())?;
        for terminal in &document.terminal_plans {
            validate_plan_snapshot_refs(&artifacts, terminal)?;
        }

        self.next_sequence = self.next_sequence.max(document.next_sequence);
        self.artifacts = artifacts;
        self.active_plan = Some(active_plan);
        self.terminal_plans = document.terminal_plans;
        Ok(())
    }
}

pub(super) fn validate_plan_snapshot_refs(
    artifacts: &ArtifactRegistry,
    snapshot: &PlanSnapshot,
) -> Result<(), SessionStoreError> {
    for result in snapshot
        .nodes
        .iter()
        .filter_map(|node| node.result.as_ref())
        .chain(
            snapshot
                .attempts
                .iter()
                .filter_map(|attempt| attempt.result.as_ref()),
        )
    {
        SessionState::validate_plan_refs_in(
            artifacts,
            &result.evidence_refs,
            &result.artifact_refs,
        )
        .map_err(|_| invalid_document("stored plan result references invalid evidence"))?;
    }
    for progress in &snapshot.attempt_progress {
        SessionState::validate_plan_refs_in(
            artifacts,
            &progress.acceptance_evidence,
            &progress.artifact_refs,
        )
        .map_err(|_| invalid_document("stored plan progress references invalid evidence"))?;
    }
    Ok(())
}

fn plan_artifact_ids(snapshot: &PlanSnapshot) -> Vec<ArtifactId> {
    let mut ids = BTreeSet::new();
    for result in snapshot
        .nodes
        .iter()
        .filter_map(|node| node.result.as_ref())
        .chain(
            snapshot
                .attempts
                .iter()
                .filter_map(|attempt| attempt.result.as_ref()),
        )
    {
        ids.extend(
            result
                .artifact_refs
                .iter()
                .map(|artifact| artifact.id().clone()),
        );
        ids.extend(
            result
                .evidence_refs
                .iter()
                .map(|evidence| evidence.artifact_id.clone()),
        );
    }
    for progress in &snapshot.attempt_progress {
        ids.extend(
            progress
                .artifact_refs
                .iter()
                .map(|artifact| artifact.id().clone()),
        );
        ids.extend(
            progress
                .acceptance_evidence
                .iter()
                .map(|evidence| evidence.artifact_id.clone()),
        );
    }
    ids.into_iter().collect()
}

impl FileSessionStore {
    pub(crate) async fn stage_plan_overlay(
        &self,
        bundle: PersistablePlanOverlayBundle,
    ) -> Result<crate::session_store::StagedSessionBundle, SessionStoreError> {
        self.stage_plan_overlay_bytes(&bundle.session_id, &bundle.document_bytes)
            .await
    }
}

fn invalid_document(reason: &'static str) -> SessionStoreError {
    SessionStoreError::InvalidDocument { reason }
}
