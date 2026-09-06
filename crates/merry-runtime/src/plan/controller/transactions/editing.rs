use crate::{
    ArtifactContent, FileSessionStore,
    plan::{
        PlanError,
        controller::{
            PlanCommandResult, PlanControllerError,
            transactions::{
                persistence::{
                    SessionBase, persist_and_install, prepare_plan_commit,
                    prepare_plan_commit_with_tool_persistence,
                },
                plan_updated_payload,
            },
        },
        domain::PlanState,
        protocol::{
            BeginPlanInput, BeginPlanOutput, PlanChangeInput, PlanUpdateOutput,
            PlanUpdateToolOutput, UpdatePlanInput,
        },
        validation,
    },
    session::SessionState,
};
use merry_core::{
    PlanActivationSource, PlanCapabilityEnvelopeSnapshot, PlanId, PlanLeaseStatus, PlanLinkStatus,
    PlanPhase, PlanRevisionSummary, PlanSchedulerStatus, PlanSnapshot, RuntimeJournalEvent,
    RuntimeJournalPayload, ToolCallId,
};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

pub(in crate::plan::controller) async fn begin_plan(
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

pub(in crate::plan::controller) async fn begin_user_plan(
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
pub(super) async fn begin_plan_with_source(
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

pub(in crate::plan::controller) async fn update_plan(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    input: UpdatePlanInput,
    tool_call_id: Option<ToolCallId>,
    next_plan_sequence: Option<&mut u64>,
    persist_tool_resolution: bool,
) -> Result<PlanCommandResult<PlanUpdateOutput>, PlanControllerError> {
    let mut next_plan_sequence = next_plan_sequence;
    let (base, output, prepared, created_new_plan) = {
        let session = session.lock().await;
        let replacing_active = session.active_plan().is_some_and(|active| {
            active.snapshot().root_node_id.is_some()
                && active.snapshot().phase != PlanPhase::Planning
        }) && matches!(&input.change, PlanChangeInput::DefinePlan { .. });
        let (previous_phase, mut candidate, terminal_plans, mut payloads, input, created_new_plan) =
            if replacing_active {
                let active = session
                    .active_plan()
                    .expect("active plan exists when replacing it");
                if active_plan_has_live_work(active.snapshot()) {
                    return Err(PlanControllerError::Plan {
                        source: PlanError::ActiveAttemptsPreventControl {
                            operation: "replace active plan",
                        },
                    });
                }
                let resource_policy_snapshot = active.snapshot().resource_policy_snapshot.clone();
                let mut terminal_plans = session.terminal_plans().to_vec();
                let mut archived = active.clone();
                let mut payloads = Vec::new();
                if !is_terminal(archived.snapshot().phase) {
                    let previous_phase = archived.snapshot().phase;
                    archived.snapshot.scheduler_status = PlanSchedulerStatus::Draining;
                    archived.advance_revision("plan archived for a new definition")?;
                    archived.snapshot.phase = PlanPhase::Cancelled;
                    payloads.push(plan_updated_payload(archived.snapshot()));
                    if archived.snapshot().phase != previous_phase {
                        payloads.push(RuntimeJournalPayload::PlanPhaseChanged {
                            plan_id: archived.snapshot().plan_id.clone(),
                            phase: archived.snapshot().phase,
                        });
                    }
                }
                push_bounded_terminal(&mut terminal_plans, archived.snapshot().clone());
                let Some(sequence) = next_plan_sequence.as_deref_mut() else {
                    return Err(PlanControllerError::NoActivePlan);
                };
                let plan_id = PlanId::new(&format!("plan-{}", *sequence))
                    .expect("runtime-generated plan id is valid");
                let candidate = PlanState::empty(
                    plan_id,
                    PlanActivationSource::Coordinator {
                        reason: input.reason.clone(),
                        governing_skill_id: None,
                    },
                    resource_policy_snapshot,
                );
                // The new Plan starts at revision zero. The caller's old
                // revision is not an internal identity of the new run.
                (
                    PlanPhase::Planning,
                    candidate,
                    terminal_plans,
                    payloads,
                    reset_define_revision(input),
                    true,
                )
            } else if let Some(active) = session.active_plan() {
                (
                    active.snapshot().phase,
                    active.clone(),
                    session.terminal_plans().to_vec(),
                    Vec::new(),
                    input,
                    false,
                )
            } else {
                let Some(sequence) = next_plan_sequence.as_deref_mut() else {
                    return Err(PlanControllerError::NoActivePlan);
                };
                let plan_id = PlanId::new(&format!("plan-{}", *sequence))
                    .expect("runtime-generated plan id is valid");
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
                    session.terminal_plans().to_vec(),
                    Vec::new(),
                    input,
                    true,
                )
            };
        let output = candidate.update(input)?;
        let summary = output
            .snapshot
            .revision_summaries
            .last()
            .cloned()
            .expect("successful update records a revision summary");
        payloads.push(RuntimeJournalPayload::PlanUpdated {
            snapshot: output.snapshot.clone(),
            summary,
        });
        if output.snapshot.phase != previous_phase {
            payloads.push(RuntimeJournalPayload::PlanPhaseChanged {
                plan_id: output.snapshot.plan_id.clone(),
                phase: output.snapshot.phase,
            });
        }
        let base = SessionBase::capture(&session);
        let tool_resolution = tool_call_id.map(|call_id| {
            let tool_output = PlanUpdateToolOutput::from(&output);
            let content = ArtifactContent::json(
                serde_json::to_string(&tool_output).expect("update_plan output serializes"),
            );
            (call_id, content)
        });
        let prepared = prepare_plan_commit_with_tool_persistence(
            &session,
            candidate,
            terminal_plans,
            payloads,
            tool_resolution,
            persist_tool_resolution,
        )?;
        (base, output, prepared, created_new_plan)
    };

    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    if created_new_plan && let Some(sequence) = next_plan_sequence {
        *sequence = (*sequence).saturating_add(1);
    }
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

pub(super) fn reset_define_revision(input: UpdatePlanInput) -> UpdatePlanInput {
    let UpdatePlanInput {
        reason,
        execution_intent,
        coordinator_node_id,
        max_concurrency_hint,
        change:
            PlanChangeInput::DefinePlan {
                root,
                expected_plan_revision: _,
            },
    } = input
    else {
        unreachable!("only define_plan updates can create a new active plan")
    };
    UpdatePlanInput {
        reason,
        execution_intent,
        coordinator_node_id,
        max_concurrency_hint,
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root,
        },
    }
}

pub(in crate::plan::controller) async fn authorize_execution(
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

pub(super) fn begin_output(snapshot: &PlanSnapshot) -> BeginPlanOutput {
    BeginPlanOutput {
        plan_id: snapshot.plan_id.clone(),
        phase: snapshot.phase,
        revision: snapshot.revision,
    }
}

pub(super) fn is_terminal(phase: PlanPhase) -> bool {
    matches!(
        phase,
        PlanPhase::Completed | PlanPhase::Blocked | PlanPhase::Cancelled
    )
}

pub(super) fn active_plan_has_live_work(snapshot: &PlanSnapshot) -> bool {
    snapshot
        .attempts
        .iter()
        .any(|attempt| attempt.outcome.is_none())
        || snapshot
            .leases
            .iter()
            .any(|lease| lease.status == PlanLeaseStatus::Live)
        || snapshot.nodes.iter().any(|node| {
            node.links
                .iter()
                .any(|link| link.status == PlanLinkStatus::Active)
        })
}

pub(super) fn push_bounded_terminal(terminal: &mut Vec<PlanSnapshot>, snapshot: PlanSnapshot) {
    const MAX_TERMINAL_PLANS: usize = 8;
    if terminal.len() == MAX_TERMINAL_PLANS {
        terminal.remove(0);
    }
    terminal.push(snapshot);
}
