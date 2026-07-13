use super::*;

pub(crate) async fn recover_leases(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    now_ms: u64,
    all_live: bool,
) -> Result<PlanCommandResult<PlanRecoveryOutput>, PlanControllerError> {
    let prepared = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = if all_live {
            candidate.interrupt_live_leases_after_resume(now_ms)?
        } else {
            candidate.interrupt_expired_leases(now_ms)?
        };
        if output.interrupted_attempts.is_empty() {
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
        payloads.extend(
            output
                .interrupted_attempts
                .iter()
                .cloned()
                .map(|attempt| RuntimeJournalPayload::PlanAttemptFinished { attempt }),
        );
        payloads.extend(output.ready_node_ids.iter().cloned().map(|node_id| {
            let node_revision = output
                .snapshot
                .nodes
                .iter()
                .find(|node| node.id == node_id)
                .expect("ready node exists in recovery snapshot")
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

pub(crate) async fn review_progress(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    actor: PlanAttemptActor,
    lease_id: PlanLeaseId,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanProgressReviewOutput>, PlanControllerError> {
    let prepared = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = candidate.review_progress_at_boundary(&actor, &lease_id, now_ms)?;
        let Some(progress) = output.updated_progress.clone() else {
            return Ok(PlanCommandResult {
                output,
                events: Vec::new(),
            });
        };
        let payloads = vec![
            plan_updated_payload(&output.snapshot),
            RuntimeJournalPayload::PlanProgressUpdated {
                progress: progress.clone(),
            },
            RuntimeJournalPayload::PlanProgressReviewRequested {
                plan_id: output.snapshot.plan_id.clone(),
                attempt_id: progress.attempt_id,
                reason: "no durable progress reached the configured semantic review window"
                    .to_owned(),
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
    let (base, output, prepared) = prepared;
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

pub(crate) async fn control_plan(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    request: PlanControlRequest,
) -> Result<PlanCommandResult<PlanControlOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = match request {
            PlanControlRequest::Approve(input) => candidate.approve(input)?,
            PlanControlRequest::Pause(reason) => candidate.pause_scheduling(&reason)?,
            PlanControlRequest::Resume(reason) => candidate.resume_scheduling(&reason)?,
            PlanControlRequest::Revise(reason) => candidate.revise(&reason)?,
            PlanControlRequest::Cancel(reason) => candidate.request_cancellation(&reason)?,
        };
        let mut payloads = vec![plan_updated_payload(&output.snapshot)];
        if output.previous_phase != output.snapshot.phase {
            payloads.push(RuntimeJournalPayload::PlanPhaseChanged {
                plan_id: output.snapshot.plan_id.clone(),
                phase: output.snapshot.phase,
            });
        }
        if output.snapshot.phase == PlanPhase::Executing
            && output.snapshot.scheduler_status == merry_core::PlanSchedulerStatus::Active
        {
            payloads.extend(candidate.ready_node_ids().into_iter().map(|node_id| {
                let node_revision = output
                    .snapshot
                    .nodes
                    .iter()
                    .find(|node| node.id == node_id)
                    .expect("ready node exists after plan control")
                    .updated_revision;
                RuntimeJournalPayload::PlanNodeReady {
                    plan_id: output.snapshot.plan_id.clone(),
                    node_id,
                    node_revision,
                }
            }));
        }
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn cancel_attempt(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    actor: PlanAttemptActor,
    lease_id: PlanLeaseId,
    reason: &str,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanAttemptCancellationOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = candidate.cancel_attempt(&actor, &lease_id, reason, now_ms)?;
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
        if output.previous_phase != output.snapshot.phase {
            payloads.push(RuntimeJournalPayload::PlanPhaseChanged {
                plan_id: output.snapshot.plan_id.clone(),
                phase: output.snapshot.phase,
            });
        }
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
