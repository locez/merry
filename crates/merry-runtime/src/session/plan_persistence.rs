use super::SessionState;
use crate::{
    FileSessionStore, PlanPersistenceLocation, SessionStoreError,
    plan::{PersistedPlanState, PlanState},
};
use merry_core::{PlanSnapshot, SessionId};
use serde::{Deserialize, Serialize};

const PLAN_OVERLAY_FORMAT_VERSION: u32 = 2;

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
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredPlanVersion {
    plan_id: merry_core::PlanId,
    revision: u64,
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
    ) -> Result<PersistablePlanOverlayBundle, SessionStoreError> {
        crate::session_store::validate_plan_snapshot(
            active_plan.snapshot(),
            PlanPersistenceLocation::OverlayActive,
        )?;
        for (index, terminal) in terminal_plans.iter().enumerate() {
            crate::session_store::validate_plan_snapshot(
                terminal,
                PlanPersistenceLocation::OverlayTerminal { index },
            )?;
        }
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
        let active_plan = PlanState::from_persisted(document.active_plan).map_err(|source| {
            crate::session_store::invalid_plan(PlanPersistenceLocation::OverlayActive, source)
        })?;
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

        crate::session_store::validate_plan_snapshot(
            active_plan.snapshot(),
            PlanPersistenceLocation::OverlayActive,
        )?;
        for (index, terminal) in document.terminal_plans.iter().enumerate() {
            crate::session_store::validate_plan_snapshot(
                terminal,
                PlanPersistenceLocation::OverlayTerminal { index },
            )?;
        }

        self.next_sequence = self.next_sequence.max(document.next_sequence);
        self.active_plan = Some(active_plan);
        self.terminal_plans = document.terminal_plans;
        Ok(())
    }
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
