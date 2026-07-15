use super::{PlanCommandResult, PlanControlRequest, PlanControllerError};
use crate::{
    ArtifactContent, FileSessionStore,
    plan::{
        PlanArtifactPromotion, PlanError,
        control::PlanControlOutput,
        domain::PlanState,
        execution::{
            PlanAttemptActor, PlanAttemptReportOutput, PlanAttemptStartOutput,
            PlanDirectiveDeliveryOutput, PlanDirectiveOutput, PlanLocalAttemptStartOutput,
            PlanProgressOutput,
        },
        protocol::{
            BeginPlanInput, BeginPlanOutput, ControlPlanAttemptInput, PlanAttemptToolOutput,
            PlanDirectiveToolOutput, PlanProgressToolOutput, PlanUpdateOutput,
            PlanUpdateToolOutput, ReportPlanAttemptInput, ReportPlanProgressInput,
            SubagentPlanUpdateInput, UpdatePlanInput,
        },
        recovery::{PlanAttemptCancellationOutput, PlanProgressReviewOutput, PlanRecoveryOutput},
        validation,
    },
    session::{PreparedPlanToolCommit, SessionState},
};
use merry_core::{
    PlanActivationSource, PlanBindingId, PlanCapabilityEnvelopeSnapshot, PlanId, PlanLeaseId,
    PlanLinkSnapshot, PlanLinkStatus, PlanNodeId, PlanNodeStatus, PlanPhase, PlanRevisionSummary,
    PlanSchedulerStatus, PlanSnapshot, RuntimeJournalEvent, RuntimeJournalPayload, SubagentId,
    SubagentTaskId, ToolCallId, ToolCallResultStatus,
};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

mod control;
pub(super) use control::{cancel_attempt, control_plan, recover_attempts, review_progress};

pub(super) async fn record_runtime_effect(
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

pub(super) async fn begin_plan(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    next_plan_sequence: &mut u64,
    input: BeginPlanInput,
    tool_call_id: Option<ToolCallId>,
) -> Result<PlanCommandResult<BeginPlanOutput>, PlanControllerError> {
    validation::validate_reason(&input.reason)?;
    let reason = input.reason.clone();
    begin_plan_with_source(
        session,
        store,
        events,
        next_plan_sequence,
        PlanActivationSource::Coordinator {
            reason,
            governing_skill_id: input.governing_skill_id,
        },
        input.reason,
        tool_call_id,
    )
    .await
}

pub(super) async fn begin_user_plan(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    next_plan_sequence: &mut u64,
    reason: String,
) -> Result<PlanCommandResult<BeginPlanOutput>, PlanControllerError> {
    validation::validate_reason(&reason)?;
    begin_plan_with_source(
        session,
        store,
        events,
        next_plan_sequence,
        PlanActivationSource::User,
        reason,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn begin_plan_with_source(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    next_plan_sequence: &mut u64,
    activation_source: PlanActivationSource,
    reason: String,
    tool_call_id: Option<ToolCallId>,
) -> Result<PlanCommandResult<BeginPlanOutput>, PlanControllerError> {
    let plan_id = PlanId::new(&format!("plan-{}", *next_plan_sequence))
        .expect("runtime-generated plan id is valid");
    let (output, base, prepared, created_new) = {
        let session = session.lock().await;
        if let Some(active) = session.active_plan()
            && !is_terminal(active.snapshot().phase)
        {
            let output = begin_output(active.snapshot());
            let Some(call_id) = tool_call_id else {
                return Ok(PlanCommandResult {
                    output,
                    events: Vec::new(),
                });
            };
            let base = SessionBase::capture(&session);
            let content = ArtifactContent::json(
                serde_json::to_string(&output).expect("begin_plan output serializes"),
            );
            let prepared = prepare_plan_commit(
                &session,
                active.clone(),
                session.terminal_plans().to_vec(),
                Vec::new(),
                Some((call_id, content)),
            )?;
            (output, base, prepared, false)
        } else {
            let mut terminal_plans = session.terminal_plans().to_vec();
            if let Some(active) = session.active_plan()
                && is_terminal(active.snapshot().phase)
            {
                push_bounded_terminal(&mut terminal_plans, active.snapshot().clone());
            }
            let candidate = PlanState::empty(
                plan_id,
                activation_source,
                session
                    .active_plan()
                    .map(|plan| plan.snapshot().resource_policy_snapshot.clone())
                    .unwrap_or_default(),
            );
            let summary =
                PlanRevisionSummary::new(0, &reason).map_err(|_| PlanControllerError::Plan {
                    source: PlanError::InvalidText {
                        field: "reason",
                        reason: "is invalid",
                    },
                })?;
            let payloads = vec![RuntimeJournalPayload::PlanUpdated {
                snapshot: candidate.snapshot().clone(),
                summary,
            }];
            let output = begin_output(candidate.snapshot());
            let tool_resolution = tool_call_id.map(|call_id| {
                let content = ArtifactContent::json(
                    serde_json::to_string(&output).expect("begin_plan output serializes"),
                );
                (call_id, content)
            });
            let base = SessionBase::capture(&session);
            let prepared = prepare_plan_commit(
                &session,
                candidate,
                terminal_plans,
                payloads,
                tool_resolution,
            )?;
            (output, base, prepared, true)
        }
    };

    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    if created_new {
        *next_plan_sequence += 1;
    }
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

pub(super) async fn update_plan(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    input: UpdatePlanInput,
    tool_call_id: Option<ToolCallId>,
    next_plan_sequence: Option<&mut u64>,
) -> Result<PlanCommandResult<PlanUpdateOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let (previous_phase, mut candidate) = if let Some(active) = session.active_plan() {
            (active.snapshot().phase, active.clone())
        } else {
            let Some(next_plan_sequence) = next_plan_sequence else {
                return Err(PlanControllerError::NoActivePlan);
            };
            let plan_id = PlanId::new(&format!("plan-{}", *next_plan_sequence))
                .expect("runtime-generated plan id is valid");
            *next_plan_sequence = next_plan_sequence.saturating_add(1);
            let reason = input.reason.clone();
            (
                PlanPhase::Planning,
                PlanState::empty(
                    plan_id,
                    PlanActivationSource::Coordinator {
                        reason,
                        governing_skill_id: None,
                    },
                    Default::default(),
                ),
            )
        };
        let output = candidate.update(input)?;
        let summary = output
            .snapshot
            .revision_summaries
            .last()
            .cloned()
            .expect("successful update records a revision summary");
        let mut payloads = vec![RuntimeJournalPayload::PlanUpdated {
            snapshot: output.snapshot.clone(),
            summary,
        }];
        if output.snapshot.phase != previous_phase {
            payloads.push(RuntimeJournalPayload::PlanPhaseChanged {
                plan_id: output.snapshot.plan_id.clone(),
                phase: output.snapshot.phase,
            });
        }
        let terminal_plans = session.terminal_plans().to_vec();
        let base = SessionBase::capture(&session);
        let tool_resolution = tool_call_id.map(|call_id| {
            let tool_output = PlanUpdateToolOutput::from(&output);
            let content = ArtifactContent::json(
                serde_json::to_string(&tool_output).expect("update_plan output serializes"),
            );
            (call_id, content)
        });
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            terminal_plans,
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

pub(super) async fn update_subagent(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    plan_id: PlanId,
    root_node_id: PlanNodeId,
    binding_id: PlanBindingId,
    input: SubagentPlanUpdateInput,
) -> Result<PlanCommandResult<PlanUpdateOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = candidate.update_subagent(plan_id, root_node_id, binding_id, input)?;
        let payloads = vec![plan_updated_payload(&output.snapshot)];
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

pub(super) async fn bind_subagent(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    client_key: String,
    agent_id: SubagentId,
    task_id: SubagentTaskId,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanLinkSnapshot>, PlanControllerError> {
    let (base, link, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let revision = candidate.advance_revision("runtime linked a subagent to this Plan task")?;
        let plan_id = candidate.snapshot.plan_id.clone();
        let node_id = candidate
            .snapshot
            .nodes
            .iter()
            .find(|node| node.client_key.as_deref() == Some(client_key.as_str()))
            .map(|node| node.id.clone())
            .ok_or(PlanControllerError::Plan {
                source: PlanError::UnknownClientKey { client_key },
            })?;
        let mut ancestor_ids = Vec::new();
        let mut ancestor_id = candidate
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .and_then(|node| node.parent_id.clone());
        while let Some(current_id) = ancestor_id {
            ancestor_id = candidate
                .snapshot
                .nodes
                .iter()
                .find(|candidate| candidate.id == current_id)
                .and_then(|candidate| candidate.parent_id.clone());
            ancestor_ids.push(current_id);
        }
        let node = candidate
            .snapshot
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
            .expect("plan node remains present");
        // The plan revision is monotonic across the whole plan, unlike the
        // per-node link count. Use it to keep binding identities unique when
        // several nodes each receive their first delegated child.
        let binding_id = PlanBindingId::new(&format!("plan-binding-{revision}"))
            .expect("runtime-generated plan binding id is valid");
        let link = PlanLinkSnapshot {
            plan_id,
            node_id: node_id.clone(),
            binding_id: binding_id.clone(),
            subagent_id: agent_id,
            task_id,
            status: PlanLinkStatus::Active,
            linked_at_ms: now_ms,
            terminal_at_ms: None,
            superseded_by: None,
        };
        // A new binding after a terminal link is a retry/replacement for this
        // Plan node. Keep the old lifecycle record for auditability, but stop
        // it from contributing a stale failure to the live projection.
        for existing in &mut node.links {
            if existing.superseded_by.is_none()
                && matches!(
                    existing.status,
                    PlanLinkStatus::Completed | PlanLinkStatus::Failed | PlanLinkStatus::Cancelled
                )
            {
                existing.status = PlanLinkStatus::Superseded;
                existing.superseded_by = Some(binding_id.clone());
            }
        }
        node.links.push(link.clone());
        node.updated_revision = revision;
        recompute_link_projection(node);
        for ancestor_id in ancestor_ids {
            let ancestor = candidate
                .snapshot
                .nodes
                .iter_mut()
                .find(|candidate| candidate.id == ancestor_id)
                .expect("plan ancestor remains present");
            if matches!(
                ancestor.status,
                PlanNodeStatus::Blocked | PlanNodeStatus::Failed
            ) {
                ancestor.status = PlanNodeStatus::Expanded;
                ancestor.updated_revision = revision;
            }
        }
        if candidate.snapshot.phase == PlanPhase::Blocked {
            candidate.snapshot.phase = PlanPhase::Executing;
            candidate.snapshot.scheduler_status = PlanSchedulerStatus::Active;
        }
        candidate.refresh_parent_states(revision);
        let summary =
            PlanRevisionSummary::new(revision, "runtime linked a subagent to this Plan task")
                .map_err(|_| PlanControllerError::Plan {
                    source: PlanError::InvalidText {
                        field: "summary",
                        reason: "is invalid",
                    },
                })?;
        let payloads = vec![RuntimeJournalPayload::PlanUpdated {
            snapshot: candidate.snapshot.clone(),
            summary,
        }];
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            None,
        )?;
        (base, link, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output: link,
        events: committed_events,
    })
}

pub(super) async fn update_subagent_link(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    binding_id: PlanBindingId,
    status: PlanLinkStatus,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanSnapshot>, PlanControllerError> {
    let (base, snapshot, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let revision = candidate.advance_revision("runtime updated linked subagent state")?;
        let node = candidate
            .snapshot
            .nodes
            .iter_mut()
            .find(|node| node.links.iter().any(|link| link.binding_id == binding_id))
            .ok_or_else(|| PlanControllerError::Plan {
                source: PlanError::UnknownNode {
                    node_id: PlanNodeId::new("linked-node-not-found")
                        .expect("valid diagnostic node id"),
                },
            })?;
        let link = node
            .links
            .iter_mut()
            .find(|link| link.binding_id == binding_id)
            .expect("link lookup above succeeded");
        link.status = status;
        link.terminal_at_ms = matches!(
            status,
            PlanLinkStatus::Completed
                | PlanLinkStatus::Failed
                | PlanLinkStatus::Cancelled
                | PlanLinkStatus::Superseded
        )
        .then_some(now_ms);
        node.updated_revision = revision;
        recompute_link_projection(node);
        candidate.refresh_parent_states(revision);
        candidate.refresh_terminal_phase();
        let summary = PlanRevisionSummary::new(revision, "runtime updated linked subagent state")
            .map_err(|_| PlanControllerError::Plan {
            source: PlanError::InvalidText {
                field: "summary",
                reason: "is invalid",
            },
        })?;
        let payloads = vec![RuntimeJournalPayload::PlanUpdated {
            snapshot: candidate.snapshot.clone(),
            summary,
        }];
        let snapshot = candidate.snapshot.clone();
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            None,
        )?;
        (base, snapshot, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output: snapshot,
        events: committed_events,
    })
}

fn recompute_link_projection(node: &mut merry_core::PlanNodeSnapshot) {
    let mut summary = merry_core::PlanExecutionSummary::default();
    for link in node
        .links
        .iter()
        .filter(|link| link.status != PlanLinkStatus::Superseded && link.superseded_by.is_none())
    {
        match link.status {
            PlanLinkStatus::Active => summary.active += 1,
            PlanLinkStatus::Completed => summary.completed += 1,
            PlanLinkStatus::Failed => summary.failed += 1,
            PlanLinkStatus::Cancelled => summary.cancelled += 1,
            PlanLinkStatus::Superseded => {}
        }
    }
    node.execution_summary = summary.clone();
    node.status = if summary.active > 0 {
        PlanNodeStatus::InProgress
    } else if summary.failed > 0 || summary.cancelled > 0 {
        PlanNodeStatus::Failed
    } else if summary.completed > 0 {
        PlanNodeStatus::Completed
    } else {
        node.declared_status
    };
}

pub(super) async fn start_attempt(
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

pub(super) async fn start_local_attempt(
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

pub(super) async fn issue_directive(
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
pub(super) async fn report_progress(
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
pub(super) async fn report_attempt(
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
                client_key_ids: output.client_key_ids.clone(),
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
pub(super) async fn heartbeat(
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

pub(super) async fn deliver_directives(
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

pub(super) async fn authorize_execution(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    envelope: PlanCapabilityEnvelopeSnapshot,
    authorization_refs: Vec<String>,
) -> Result<PlanCommandResult<PlanSnapshot>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let previous_phase = candidate.snapshot().phase;
        let output = candidate.enter_execution(envelope, authorization_refs)?;
        let mut payloads = vec![plan_updated_payload(&output)];
        if previous_phase != output.phase {
            payloads.push(RuntimeJournalPayload::PlanPhaseChanged {
                plan_id: output.plan_id.clone(),
                phase: output.phase,
            });
        }
        payloads.extend(candidate.ready_node_ids().into_iter().map(|node_id| {
            let node_revision = output
                .nodes
                .iter()
                .find(|node| node.id == node_id)
                .expect("ready node exists")
                .updated_revision;
            RuntimeJournalPayload::PlanNodeReady {
                plan_id: output.plan_id.clone(),
                node_id,
                node_revision,
            }
        }));
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

pub(super) fn plan_updated_payload(snapshot: &PlanSnapshot) -> RuntimeJournalPayload {
    let summary = snapshot
        .revision_summaries
        .last()
        .cloned()
        .expect("plan mutation records a revision summary");
    RuntimeJournalPayload::PlanUpdated {
        snapshot: snapshot.clone(),
        summary,
    }
}

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
        bundle: crate::session::PersistableSessionBundle,
    },
}

pub(super) fn prepare_plan_commit(
    session: &SessionState,
    candidate: PlanState,
    terminal_plans: Vec<PlanSnapshot>,
    payloads: Vec<RuntimeJournalPayload>,
    tool_resolution: Option<(ToolCallId, ArtifactContent)>,
) -> Result<PreparedPlanCommit, PlanControllerError> {
    prepare_plan_commit_with_artifact_promotions(
        session,
        candidate,
        terminal_plans,
        payloads,
        Vec::new(),
        tool_resolution,
    )
}

fn prepare_plan_commit_with_artifact_promotions(
    session: &SessionState,
    candidate: PlanState,
    terminal_plans: Vec<PlanSnapshot>,
    payloads: Vec<RuntimeJournalPayload>,
    artifact_promotions: Vec<PlanArtifactPromotion>,
    tool_resolution: Option<(ToolCallId, ArtifactContent)>,
) -> Result<PreparedPlanCommit, PlanControllerError> {
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
        let bundle = session.persistable_bundle_with_plan_tool_commit(&prepared)?;
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
            if let Some(store) = store {
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
    next_sequence: u64,
    active_plan: Option<(PlanId, u64)>,
    terminal_plan_count: usize,
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

fn begin_output(snapshot: &PlanSnapshot) -> BeginPlanOutput {
    BeginPlanOutput {
        plan_id: snapshot.plan_id.clone(),
        phase: snapshot.phase,
        revision: snapshot.revision,
    }
}

fn is_terminal(phase: PlanPhase) -> bool {
    matches!(
        phase,
        PlanPhase::Completed | PlanPhase::Blocked | PlanPhase::Cancelled
    )
}

fn push_bounded_terminal(terminal: &mut Vec<PlanSnapshot>, snapshot: PlanSnapshot) {
    const MAX_TERMINAL_PLANS: usize = 8;
    if terminal.len() == MAX_TERMINAL_PLANS {
        terminal.remove(0);
    }
    terminal.push(snapshot);
}
