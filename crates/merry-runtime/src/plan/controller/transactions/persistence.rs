use crate::{
    ArtifactContent, FileSessionStore,
    plan::{PlanArtifactPromotion, controller::PlanControllerError, domain::PlanState, validation},
    session::{PreparedPlanToolCommit, SessionState},
};
use merry_core::{
    PlanId, PlanSnapshot, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallId,
    ToolCallResultStatus,
};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

pub(super) struct PreparedPlanCommit {
    install: PreparedPlanInstall,
}

enum PreparedPlanInstall {
    PlanOnly {
        candidate: Box<PlanState>,
        terminal_plans: Vec<PlanSnapshot>,
        payloads: Vec<RuntimeJournalPayload>,
        artifact_promotions: Vec<PlanArtifactPromotion>,
    },
    Tool {
        prepared: Box<PreparedPlanToolCommit>,
        bundle: Option<crate::session::PersistableSessionBundle>,
    },
}

pub(super) fn prepare_plan_commit(
    session: &SessionState,
    candidate: PlanState,
    terminal_plans: Vec<PlanSnapshot>,
    payloads: Vec<RuntimeJournalPayload>,
    tool_resolution: Option<(ToolCallId, ArtifactContent)>,
) -> Result<PreparedPlanCommit, PlanControllerError> {
    prepare_plan_commit_with_tool_persistence(
        session,
        candidate,
        terminal_plans,
        payloads,
        tool_resolution,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_plan_commit_with_tool_persistence(
    session: &SessionState,
    candidate: PlanState,
    terminal_plans: Vec<PlanSnapshot>,
    payloads: Vec<RuntimeJournalPayload>,
    tool_resolution: Option<(ToolCallId, ArtifactContent)>,
    persist_tool_resolution: bool,
) -> Result<PreparedPlanCommit, PlanControllerError> {
    prepare_plan_commit_with_artifact_promotions(
        session,
        candidate,
        terminal_plans,
        payloads,
        Vec::new(),
        tool_resolution,
        persist_tool_resolution,
    )
}

pub(super) fn prepare_plan_commit_with_artifact_promotions(
    session: &SessionState,
    candidate: PlanState,
    terminal_plans: Vec<PlanSnapshot>,
    payloads: Vec<RuntimeJournalPayload>,
    artifact_promotions: Vec<PlanArtifactPromotion>,
    tool_resolution: Option<(ToolCallId, ArtifactContent)>,
    persist_tool_resolution: bool,
) -> Result<PreparedPlanCommit, PlanControllerError> {
    validation::validate_snapshot_limits(candidate.snapshot())?;
    for terminal in &terminal_plans {
        validation::validate_snapshot_limits(terminal)?;
    }
    if let Some((call_id, content)) = tool_resolution {
        debug_assert!(artifact_promotions.is_empty());
        let prepared = session.prepare_plan_tool_commit(
            candidate,
            terminal_plans,
            payloads,
            &call_id,
            ToolCallResultStatus::Succeeded,
            content,
            None,
        )?;
        let bundle = persist_tool_resolution
            .then(|| session.persistable_bundle_with_plan_tool_commit(&prepared))
            .transpose()?;
        return Ok(PreparedPlanCommit {
            install: PreparedPlanInstall::Tool {
                prepared: Box::new(prepared),
                bundle,
            },
        });
    }

    Ok(PreparedPlanCommit {
        install: PreparedPlanInstall::PlanOnly {
            candidate: Box::new(candidate),
            terminal_plans,
            payloads,
            artifact_promotions,
        },
    })
}

pub(super) async fn persist_and_install(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    base: SessionBase,
    prepared: PreparedPlanCommit,
) -> Result<Vec<RuntimeJournalEvent>, PlanControllerError> {
    let committed_events = match prepared.install {
        PreparedPlanInstall::PlanOnly {
            candidate,
            terminal_plans,
            payloads,
            artifact_promotions,
        } => {
            persist_and_install_plan_only(
                session,
                store,
                base,
                *candidate,
                terminal_plans,
                payloads,
                artifact_promotions,
            )
            .await?
        }
        PreparedPlanInstall::Tool { prepared, bundle } => {
            if let (Some(store), Some(bundle)) = (store, bundle) {
                let staged = store.stage_bundle(bundle).await?;
                let is_current = {
                    let session = session.lock().await;
                    base.matches(&session)
                };
                if !is_current {
                    staged.discard().await?;
                    return Err(PlanControllerError::StaleTransaction);
                }
                staged.commit().await?.require_durable()?;
            }

            let mut session = session.lock().await;
            if !base.matches(&session) {
                return Err(PlanControllerError::StaleTransaction);
            }
            let committed_events = prepared.events().to_vec();
            session.install_plan_tool_commit(*prepared);
            committed_events
        }
    };
    for event in &committed_events {
        let _ = events.send(event.clone());
    }
    Ok(committed_events)
}

#[allow(clippy::too_many_arguments)]
async fn persist_and_install_plan_only(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    base: SessionBase,
    candidate: PlanState,
    terminal_plans: Vec<PlanSnapshot>,
    payloads: Vec<RuntimeJournalPayload>,
    artifact_promotions: Vec<PlanArtifactPromotion>,
) -> Result<Vec<RuntimeJournalEvent>, PlanControllerError> {
    // Reserve event identities before disk IO. A failed commit may leave a gap,
    // but a later attempt can never reuse an event identity that was prepared here.
    let committed_events = {
        let mut session = session.lock().await;
        if !base.plan_matches(&session) {
            return Err(PlanControllerError::StaleTransaction);
        }
        payloads
            .into_iter()
            .map(|payload| session.record_transient_event(payload))
            .collect::<Vec<_>>()
    };

    let Some(store) = store else {
        let mut session = session.lock().await;
        if !base.plan_matches(&session) {
            return Err(PlanControllerError::StaleTransaction);
        }
        install_plan_only_candidate(
            &mut session,
            candidate,
            terminal_plans,
            &artifact_promotions,
        )?;
        return Ok(committed_events);
    };

    loop {
        // Session activity may advance the global sequence while the sidecar is
        // staging. Rewrite until the persisted frontier and install point agree.
        let (overlay, persisted_next_sequence) = {
            let session = session.lock().await;
            if !base.plan_matches(&session) {
                return Err(PlanControllerError::StaleTransaction);
            }
            let artifacts = session.artifacts_with_plan_promotions(&artifact_promotions)?;
            let next_sequence = session.next_sequence();
            let overlay = session.persistable_plan_overlay(
                &candidate,
                &terminal_plans,
                next_sequence,
                &artifacts,
            )?;
            (overlay, next_sequence)
        };
        store
            .stage_plan_overlay(overlay)
            .await?
            .commit()
            .await?
            .require_durable()?;

        let mut session = session.lock().await;
        if !base.plan_matches(&session) {
            return Err(PlanControllerError::StaleTransaction);
        }
        if session.next_sequence() != persisted_next_sequence {
            continue;
        }
        install_plan_only_candidate(
            &mut session,
            candidate,
            terminal_plans,
            &artifact_promotions,
        )?;
        return Ok(committed_events);
    }
}

fn install_plan_only_candidate(
    session: &mut SessionState,
    candidate: PlanState,
    terminal_plans: Vec<PlanSnapshot>,
    artifact_promotions: &[PlanArtifactPromotion],
) -> Result<(), PlanControllerError> {
    let artifacts = session.artifacts_with_plan_promotions(artifact_promotions)?;
    session.replace_artifacts_for_plan_commit(artifacts);
    session.take_active_plan();
    for snapshot in terminal_plans {
        if !session
            .terminal_plans()
            .iter()
            .any(|existing| existing.plan_id == snapshot.plan_id)
        {
            session.push_terminal_plan(snapshot);
        }
    }
    session.set_active_plan(candidate);
    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct SessionBase {
    pub(super) next_sequence: u64,
    pub(super) active_plan: Option<(PlanId, u64)>,
    pub(super) terminal_plan_count: usize,
}

impl SessionBase {
    pub(super) fn capture(session: &SessionState) -> Self {
        Self {
            next_sequence: session.next_sequence(),
            active_plan: session
                .active_plan()
                .map(|plan| (plan.snapshot().plan_id.clone(), plan.snapshot().revision)),
            terminal_plan_count: session.terminal_plans().len(),
        }
    }

    fn matches(&self, session: &SessionState) -> bool {
        self.next_sequence == session.next_sequence()
            && self.terminal_plan_count == session.terminal_plans().len()
            && self.active_plan
                == session
                    .active_plan()
                    .map(|plan| (plan.snapshot().plan_id.clone(), plan.snapshot().revision))
    }

    fn plan_matches(&self, session: &SessionState) -> bool {
        self.terminal_plan_count == session.terminal_plans().len()
            && self.active_plan
                == session
                    .active_plan()
                    .map(|plan| (plan.snapshot().plan_id.clone(), plan.snapshot().revision))
    }
}
