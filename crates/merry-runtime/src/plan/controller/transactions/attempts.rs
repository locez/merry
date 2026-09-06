use crate::{
    ArtifactContent, FileSessionStore,
    plan::{
        PlanArtifactPromotion,
        controller::{
            PlanCommandResult, PlanControllerError,
            transactions::{
                persistence::{
                    SessionBase, persist_and_install, prepare_plan_commit,
                    prepare_plan_commit_with_artifact_promotions,
                },
                plan_updated_payload,
            },
        },
        execution::{
            PlanAttemptActor, PlanAttemptReportOutput, PlanAttemptStartOutput,
            PlanDirectiveDeliveryOutput, PlanDirectiveOutput, PlanLocalAttemptStartOutput,
            PlanProgressOutput,
        },
        protocol::{
            ControlPlanAttemptInput, PlanAttemptToolOutput, PlanDirectiveToolOutput,
            PlanProgressToolOutput, ReportPlanAttemptInput, ReportPlanProgressInput,
        },
    },
    session::SessionState,
};
use merry_core::{PlanLeaseId, PlanNodeId, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallId};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

pub(in crate::plan::controller) async fn record_runtime_effect(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    actor: PlanAttemptActor,
    changed_paths: Vec<String>,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = candidate.record_runtime_effect(&actor, changed_paths, now_ms)?;
        let payloads = vec![RuntimeJournalPayload::PlanProgressUpdated {
            progress: output.progress.clone(),
        }];
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            None,
        )?;
        (base, output, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

pub(in crate::plan::controller) async fn start_attempt(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    node_id: PlanNodeId,
    actor: PlanAttemptActor,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanAttemptStartOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = candidate.start_attempt(&node_id, actor, now_ms)?;
        let payloads = vec![
            plan_updated_payload(&output.snapshot),
            RuntimeJournalPayload::PlanLeaseStarted {
                lease: output.lease.clone(),
            },
            RuntimeJournalPayload::PlanProgressUpdated {
                progress: output.progress.clone(),
            },
        ];
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            None,
        )?;
        (base, output, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

pub(in crate::plan::controller) async fn start_local_attempt(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    node_id: PlanNodeId,
    actor: PlanAttemptActor,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanLocalAttemptStartOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = candidate.start_local_attempt(&node_id, actor, now_ms)?;
        let payloads = vec![
            plan_updated_payload(&output.snapshot),
            RuntimeJournalPayload::PlanProgressUpdated {
                progress: output.progress.clone(),
            },
        ];
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            None,
        )?;
        (base, output, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

pub(in crate::plan::controller) async fn issue_directive(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    input: ControlPlanAttemptInput,
    tool_call_id: Option<ToolCallId>,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanDirectiveOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = candidate.issue_directive(input, now_ms)?;
        let payloads = vec![
            plan_updated_payload(&output.snapshot),
            RuntimeJournalPayload::PlanDirectiveUpdated {
                directive: output.directive.clone(),
            },
        ];
        let tool_resolution = tool_call_id.map(|call_id| {
            let tool_output = PlanDirectiveToolOutput {
                plan_id: output.snapshot.plan_id.clone(),
                revision: output.snapshot.revision,
                directive: output.directive.clone(),
            };
            (
                call_id,
                ArtifactContent::json(
                    serde_json::to_string(&tool_output)
                        .expect("control_plan_attempt output serializes"),
                ),
            )
        });
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            tool_resolution,
        )?;
        (base, output, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::plan::controller) async fn report_progress(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    actor: PlanAttemptActor,
    input: ReportPlanProgressInput,
    artifact_promotions: Vec<PlanArtifactPromotion>,
    tool_call_id: Option<ToolCallId>,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let artifacts = session.artifacts_with_plan_promotions(&artifact_promotions)?;
        SessionState::validate_plan_refs_in(
            &artifacts,
            &input.evidence_refs,
            &input.artifact_refs,
        )?;
        let output = candidate.report_progress(&actor, input, now_ms)?;
        let mut payloads = vec![plan_updated_payload(&output.snapshot)];
        payloads.extend(
            output
                .updated_directives
                .iter()
                .cloned()
                .map(|directive| RuntimeJournalPayload::PlanDirectiveUpdated { directive }),
        );
        payloads.push(RuntimeJournalPayload::PlanAttemptProgressReported {
            progress: output.progress.clone(),
        });
        if output.progress.request_coordinator_review {
            payloads.push(RuntimeJournalPayload::PlanProgressReviewRequested {
                plan_id: output.snapshot.plan_id.clone(),
                attempt_id: output.progress.attempt_id.clone(),
                reason: "subagent requested coordinator review".to_owned(),
            });
        }
        let tool_resolution = tool_call_id.map(|call_id| {
            let tool_output = PlanProgressToolOutput {
                plan_id: output.snapshot.plan_id.clone(),
                revision: output.snapshot.revision,
                progress: output.progress.clone(),
                updated_directives: output.updated_directives.clone(),
            };
            (
                call_id,
                ArtifactContent::json(
                    serde_json::to_string(&tool_output)
                        .expect("report_plan_progress output serializes"),
                ),
            )
        });
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit_with_artifact_promotions(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            artifact_promotions,
            tool_resolution,
            true,
        )?;
        (base, output, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::plan::controller) async fn report_attempt(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    actor: PlanAttemptActor,
    input: ReportPlanAttemptInput,
    artifact_promotions: Vec<PlanArtifactPromotion>,
    tool_call_id: Option<ToolCallId>,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanAttemptReportOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let artifacts = session.artifacts_with_plan_promotions(&artifact_promotions)?;
        if let Some(result) = input.result.as_ref() {
            SessionState::validate_plan_refs_in(
                &artifacts,
                &result.evidence_refs,
                &result.artifact_refs,
            )?;
        }
        let output = candidate.report_attempt(&actor, input, now_ms)?;
        let mut payloads = vec![plan_updated_payload(&output.snapshot)];
        payloads.extend(
            output
                .updated_directives
                .iter()
                .cloned()
                .map(|directive| RuntimeJournalPayload::PlanDirectiveUpdated { directive }),
        );
        payloads.push(RuntimeJournalPayload::PlanAttemptFinished {
            attempt: output.attempt.clone(),
        });
        payloads.extend(output.ready_node_ids.iter().cloned().map(|node_id| {
            let node_revision = output
                .snapshot
                .nodes
                .iter()
                .find(|node| node.id == node_id)
                .expect("ready node exists in output snapshot")
                .updated_revision;
            RuntimeJournalPayload::PlanNodeReady {
                plan_id: output.snapshot.plan_id.clone(),
                node_id,
                node_revision,
            }
        }));
        if output.previous_phase != output.snapshot.phase {
            payloads.push(RuntimeJournalPayload::PlanPhaseChanged {
                plan_id: output.snapshot.plan_id.clone(),
                phase: output.snapshot.phase,
            });
        }
        let tool_resolution = tool_call_id.map(|call_id| {
            let tool_output = PlanAttemptToolOutput {
                plan_id: output.snapshot.plan_id.clone(),
                revision: output.snapshot.revision,
                phase: output.snapshot.phase,
                attempt: output.attempt.clone(),
                ready_node_ids: output.ready_node_ids.clone(),
                client_key_to_runtime_node_id: output.client_key_to_runtime_node_id.clone(),
            };
            (
                call_id,
                ArtifactContent::json(
                    serde_json::to_string(&tool_output)
                        .expect("report_plan_attempt output serializes"),
                ),
            )
        });
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit_with_artifact_promotions(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            artifact_promotions,
            tool_resolution,
            true,
        )?;
        (base, output, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::plan::controller) async fn heartbeat(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    actor: PlanAttemptActor,
    lease_id: PlanLeaseId,
    now_ms: u64,
    provider_request_in_flight: bool,
    tool_call_in_flight: bool,
) -> Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = candidate.heartbeat(
            &actor,
            &lease_id,
            now_ms,
            provider_request_in_flight,
            tool_call_in_flight,
        )?;
        let payloads = vec![RuntimeJournalPayload::PlanProgressUpdated {
            progress: output.progress.clone(),
        }];
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            None,
        )?;
        (base, output, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

pub(in crate::plan::controller) async fn deliver_directives(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    actor: PlanAttemptActor,
    lease_id: PlanLeaseId,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanDirectiveDeliveryOutput>, PlanControllerError> {
    let prepared = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = candidate.deliver_queued_directives(&actor, &lease_id, now_ms)?;
        if output.updated_directives.is_empty() {
            return Ok(PlanCommandResult {
                output,
                events: Vec::new(),
            });
        }
        let mut payloads = vec![plan_updated_payload(&output.snapshot)];
        payloads.extend(
            output
                .updated_directives
                .iter()
                .cloned()
                .map(|directive| RuntimeJournalPayload::PlanDirectiveUpdated { directive }),
        );
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            None,
        )?;
        (base, output, prepared)
    };
    let (base, output, prepared) = prepared;
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}
