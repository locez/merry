use super::PlanControllerError;
use super::transactions::{
    authorize_execution, begin_plan, begin_user_plan, bind_subagent, cancel_attempt, control_plan,
    deliver_directives, heartbeat, issue_directive, record_runtime_effect, recover_attempts,
    report_attempt, report_progress, review_progress, start_attempt, start_local_attempt,
    update_plan, update_subagent, update_subagent_link,
};
use crate::{
    FileSessionStore,
    plan::{
        PlanArtifactPromotion,
        control::PlanControlOutput,
        execution::{
            PlanAttemptActor, PlanAttemptReportOutput, PlanAttemptStartOutput,
            PlanDirectiveDeliveryOutput, PlanDirectiveOutput, PlanLocalAttemptStartOutput,
            PlanProgressOutput,
        },
        protocol::{
            BeginPlanInput, BeginPlanOutput, ControlPlanAttemptInput, PlanApprovalInput,
            PlanUpdateOutput, ReportPlanAttemptInput, ReportPlanProgressInput,
            SubagentPlanUpdateInput, UpdatePlanInput,
        },
        recovery::{PlanAttemptCancellationOutput, PlanProgressReviewOutput, PlanRecoveryOutput},
    },
    session::SessionState,
};
use merry_core::{
    PlanBindingId, PlanCapabilityEnvelopeSnapshot, PlanId, PlanLeaseId, PlanLinkSnapshot,
    PlanLinkStatus, PlanNodeId, PlanSnapshot, RuntimeJournalEvent, SubagentId, SubagentTaskId,
    ToolCallId,
};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

#[allow(dead_code)]
pub(super) enum PlanCommand {
    Begin {
        input: BeginPlanInput,
        reply: oneshot::Sender<Result<PlanCommandResult<BeginPlanOutput>, PlanControllerError>>,
    },
    BeginTool {
        input: BeginPlanInput,
        call_id: ToolCallId,
        reply: oneshot::Sender<Result<Vec<RuntimeJournalEvent>, PlanControllerError>>,
    },
    BeginUser {
        reason: String,
        reply: oneshot::Sender<Result<PlanCommandResult<BeginPlanOutput>, PlanControllerError>>,
    },
    Update {
        input: UpdatePlanInput,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanUpdateOutput>, PlanControllerError>>,
    },
    UpdateSubagent {
        plan_id: PlanId,
        root_node_id: PlanNodeId,
        binding_id: PlanBindingId,
        input: SubagentPlanUpdateInput,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanUpdateOutput>, PlanControllerError>>,
    },
    UpdateTool {
        input: UpdatePlanInput,
        call_id: ToolCallId,
        persist_tool_resolution: bool,
        reply: oneshot::Sender<Result<Vec<RuntimeJournalEvent>, PlanControllerError>>,
    },
    BindSubagent {
        client_key: String,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        now_ms: u64,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanLinkSnapshot>, PlanControllerError>>,
    },
    UpdateSubagentLink {
        binding_id: PlanBindingId,
        status: PlanLinkStatus,
        now_ms: u64,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanSnapshot>, PlanControllerError>>,
    },
    StartAttempt {
        node_id: PlanNodeId,
        actor: PlanAttemptActor,
        now_ms: u64,
        reply:
            oneshot::Sender<Result<PlanCommandResult<PlanAttemptStartOutput>, PlanControllerError>>,
    },
    StartLocalAttempt {
        node_id: PlanNodeId,
        actor: PlanAttemptActor,
        now_ms: u64,
        reply: oneshot::Sender<
            Result<PlanCommandResult<PlanLocalAttemptStartOutput>, PlanControllerError>,
        >,
    },
    DirectiveTool {
        input: ControlPlanAttemptInput,
        call_id: ToolCallId,
        now_ms: u64,
        reply: oneshot::Sender<Result<Vec<RuntimeJournalEvent>, PlanControllerError>>,
    },
    Directive {
        input: ControlPlanAttemptInput,
        now_ms: u64,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanDirectiveOutput>, PlanControllerError>>,
    },
    Progress {
        actor: PlanAttemptActor,
        input: ReportPlanProgressInput,
        artifact_promotions: Vec<PlanArtifactPromotion>,
        now_ms: u64,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError>>,
    },
    ProgressTool {
        actor: PlanAttemptActor,
        input: ReportPlanProgressInput,
        call_id: ToolCallId,
        now_ms: u64,
        reply: oneshot::Sender<Result<Vec<RuntimeJournalEvent>, PlanControllerError>>,
    },
    AttemptReport {
        actor: PlanAttemptActor,
        input: ReportPlanAttemptInput,
        artifact_promotions: Vec<PlanArtifactPromotion>,
        now_ms: u64,
        reply: oneshot::Sender<
            Result<PlanCommandResult<PlanAttemptReportOutput>, PlanControllerError>,
        >,
    },
    AttemptReportTool {
        actor: PlanAttemptActor,
        input: ReportPlanAttemptInput,
        call_id: ToolCallId,
        now_ms: u64,
        reply: oneshot::Sender<Result<Vec<RuntimeJournalEvent>, PlanControllerError>>,
    },
    Heartbeat {
        actor: PlanAttemptActor,
        lease_id: PlanLeaseId,
        now_ms: u64,
        provider_request_in_flight: bool,
        tool_call_in_flight: bool,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError>>,
    },
    RecordRuntimeEffect {
        actor: PlanAttemptActor,
        changed_paths: Vec<String>,
        now_ms: u64,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError>>,
    },
    DeliverDirectives {
        actor: PlanAttemptActor,
        lease_id: PlanLeaseId,
        now_ms: u64,
        reply: oneshot::Sender<
            Result<PlanCommandResult<PlanDirectiveDeliveryOutput>, PlanControllerError>,
        >,
    },
    RecoverAttempts {
        now_ms: u64,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanRecoveryOutput>, PlanControllerError>>,
    },
    ReviewProgress {
        actor: PlanAttemptActor,
        lease_id: PlanLeaseId,
        now_ms: u64,
        reply: oneshot::Sender<
            Result<PlanCommandResult<PlanProgressReviewOutput>, PlanControllerError>,
        >,
    },
    Control {
        request: PlanControlRequest,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanControlOutput>, PlanControllerError>>,
    },
    CancelAttempt {
        actor: PlanAttemptActor,
        lease_id: PlanLeaseId,
        reason: String,
        now_ms: u64,
        reply: oneshot::Sender<
            Result<PlanCommandResult<PlanAttemptCancellationOutput>, PlanControllerError>,
        >,
    },
    AuthorizeExecution {
        envelope: PlanCapabilityEnvelopeSnapshot,
        authorization_refs: Vec<String>,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanSnapshot>, PlanControllerError>>,
    },
    Snapshot {
        reply: oneshot::Sender<Option<PlanSnapshot>>,
    },
}

pub(super) enum PlanControlRequest {
    Approve(PlanApprovalInput),
    Pause(String),
    Resume(String),
    Revise(String),
    RetryInterrupted { node_id: PlanNodeId, reason: String },
    Cancel { reason: String, now_ms: u64 },
}

pub(crate) struct PlanCommandResult<T> {
    pub(crate) output: T,
    pub(crate) events: Vec<RuntimeJournalEvent>,
}

pub(super) async fn run_controller(
    session: Arc<Mutex<SessionState>>,
    store: Option<FileSessionStore>,
    mut receiver: mpsc::Receiver<PlanCommand>,
    events: broadcast::Sender<RuntimeJournalEvent>,
) {
    let mut next_plan_sequence = {
        let session = session.lock().await;
        session.terminal_plans().len() as u64 + u64::from(session.active_plan().is_some()) + 1
    };
    while let Some(command) = receiver.recv().await {
        match command {
            PlanCommand::Begin { input, reply } => {
                let result = begin_plan(
                    &session,
                    store.as_ref(),
                    &events,
                    &mut next_plan_sequence,
                    input,
                    None,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::BeginTool {
                input,
                call_id,
                reply,
            } => {
                let result = begin_plan(
                    &session,
                    store.as_ref(),
                    &events,
                    &mut next_plan_sequence,
                    input,
                    Some(call_id),
                )
                .await
                .map(|committed| committed.events);
                let _ = reply.send(result);
            }
            PlanCommand::BeginUser { reason, reply } => {
                let result = begin_user_plan(
                    &session,
                    store.as_ref(),
                    &events,
                    &mut next_plan_sequence,
                    reason,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::Update { input, reply } => {
                let result =
                    update_plan(&session, store.as_ref(), &events, input, None, None, true).await;
                let _ = reply.send(result);
            }
            PlanCommand::UpdateSubagent {
                plan_id,
                root_node_id,
                binding_id,
                input,
                reply,
            } => {
                let result = update_subagent(
                    &session,
                    store.as_ref(),
                    &events,
                    plan_id,
                    root_node_id,
                    binding_id,
                    input,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::UpdateTool {
                input,
                call_id,
                persist_tool_resolution,
                reply,
            } => {
                let result = update_plan(
                    &session,
                    store.as_ref(),
                    &events,
                    input,
                    Some(call_id),
                    Some(&mut next_plan_sequence),
                    persist_tool_resolution,
                )
                .await
                .map(|committed| committed.events);
                let _ = reply.send(result);
            }
            PlanCommand::BindSubagent {
                client_key,
                agent_id,
                task_id,
                now_ms,
                reply,
            } => {
                let result = bind_subagent(
                    &session,
                    store.as_ref(),
                    &events,
                    client_key,
                    agent_id,
                    task_id,
                    now_ms,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::UpdateSubagentLink {
                binding_id,
                status,
                now_ms,
                reply,
            } => {
                let result = update_subagent_link(
                    &session,
                    store.as_ref(),
                    &events,
                    binding_id,
                    status,
                    now_ms,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::StartAttempt {
                node_id,
                actor,
                now_ms,
                reply,
            } => {
                let result =
                    start_attempt(&session, store.as_ref(), &events, node_id, actor, now_ms).await;
                let _ = reply.send(result);
            }
            PlanCommand::StartLocalAttempt {
                node_id,
                actor,
                now_ms,
                reply,
            } => {
                let result =
                    start_local_attempt(&session, store.as_ref(), &events, node_id, actor, now_ms)
                        .await;
                let _ = reply.send(result);
            }
            PlanCommand::DirectiveTool {
                input,
                call_id,
                now_ms,
                reply,
            } => {
                let result = issue_directive(
                    &session,
                    store.as_ref(),
                    &events,
                    input,
                    Some(call_id),
                    now_ms,
                )
                .await
                .map(|committed| committed.events);
                let _ = reply.send(result);
            }
            PlanCommand::Directive {
                input,
                now_ms,
                reply,
            } => {
                let result =
                    issue_directive(&session, store.as_ref(), &events, input, None, now_ms).await;
                let _ = reply.send(result);
            }
            PlanCommand::Progress {
                actor,
                input,
                artifact_promotions,
                now_ms,
                reply,
            } => {
                let result = report_progress(
                    &session,
                    store.as_ref(),
                    &events,
                    actor,
                    input,
                    artifact_promotions,
                    None,
                    now_ms,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::ProgressTool {
                actor,
                input,
                call_id,
                now_ms,
                reply,
            } => {
                let result = report_progress(
                    &session,
                    store.as_ref(),
                    &events,
                    actor,
                    input,
                    Vec::new(),
                    Some(call_id),
                    now_ms,
                )
                .await
                .map(|committed| committed.events);
                let _ = reply.send(result);
            }
            PlanCommand::AttemptReport {
                actor,
                input,
                artifact_promotions,
                now_ms,
                reply,
            } => {
                let result = report_attempt(
                    &session,
                    store.as_ref(),
                    &events,
                    actor,
                    input,
                    artifact_promotions,
                    None,
                    now_ms,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::AttemptReportTool {
                actor,
                input,
                call_id,
                now_ms,
                reply,
            } => {
                let result = report_attempt(
                    &session,
                    store.as_ref(),
                    &events,
                    actor,
                    input,
                    Vec::new(),
                    Some(call_id),
                    now_ms,
                )
                .await
                .map(|committed| committed.events);
                let _ = reply.send(result);
            }
            PlanCommand::Heartbeat {
                actor,
                lease_id,
                now_ms,
                provider_request_in_flight,
                tool_call_in_flight,
                reply,
            } => {
                let result = heartbeat(
                    &session,
                    store.as_ref(),
                    &events,
                    actor,
                    lease_id,
                    now_ms,
                    provider_request_in_flight,
                    tool_call_in_flight,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::RecordRuntimeEffect {
                actor,
                changed_paths,
                now_ms,
                reply,
            } => {
                let result = record_runtime_effect(
                    &session,
                    store.as_ref(),
                    &events,
                    actor,
                    changed_paths,
                    now_ms,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::DeliverDirectives {
                actor,
                lease_id,
                now_ms,
                reply,
            } => {
                let result =
                    deliver_directives(&session, store.as_ref(), &events, actor, lease_id, now_ms)
                        .await;
                let _ = reply.send(result);
            }
            PlanCommand::RecoverAttempts { now_ms, reply } => {
                let result = recover_attempts(&session, store.as_ref(), &events, now_ms).await;
                let _ = reply.send(result);
            }
            PlanCommand::ReviewProgress {
                actor,
                lease_id,
                now_ms,
                reply,
            } => {
                let result =
                    review_progress(&session, store.as_ref(), &events, actor, lease_id, now_ms)
                        .await;
                let _ = reply.send(result);
            }
            PlanCommand::Control { request, reply } => {
                let result = control_plan(&session, store.as_ref(), &events, request).await;
                let _ = reply.send(result);
            }
            PlanCommand::CancelAttempt {
                actor,
                lease_id,
                reason,
                now_ms,
                reply,
            } => {
                let result = cancel_attempt(
                    &session,
                    store.as_ref(),
                    &events,
                    actor,
                    lease_id,
                    &reason,
                    now_ms,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::AuthorizeExecution {
                envelope,
                authorization_refs,
                reply,
            } => {
                let result = authorize_execution(
                    &session,
                    store.as_ref(),
                    &events,
                    envelope,
                    authorization_refs,
                )
                .await;
                let _ = reply.send(result);
            }
            PlanCommand::Snapshot { reply } => {
                let snapshot = session
                    .lock()
                    .await
                    .active_plan()
                    .map(|plan| plan.snapshot().clone());
                let _ = reply.send(snapshot);
            }
        }
    }
}
