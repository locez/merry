use super::{
    PlanError,
    control::PlanControlOutput,
    execution::{
        PlanAttemptActor, PlanAttemptReportOutput, PlanAttemptStartOutput,
        PlanDirectiveDeliveryOutput, PlanDirectiveOutput, PlanProgressOutput,
    },
    protocol::{
        BeginPlanInput, BeginPlanOutput, ControlPlanAttemptInput, PlanApprovalInput,
        PlanUpdateOutput, ReportPlanAttemptInput, ReportPlanProgressInput, UpdatePlanInput,
    },
    recovery::{PlanAttemptCancellationOutput, PlanProgressReviewOutput, PlanRecoveryOutput},
};
use crate::{FileSessionStore, RuntimeError, SessionStoreError, session::SessionState};
use merry_core::{
    PlanCapabilityEnvelopeSnapshot, PlanLeaseId, PlanNodeId, PlanSnapshot, RuntimeJournalEvent,
    ToolCallId,
};
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex as StdMutex},
};
use thiserror::Error;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

mod actor;
mod transactions;
pub(crate) use actor::PlanCommandResult;
use actor::{PlanCommand, PlanControlRequest, run_controller};

pub(crate) type PlanControllerEventReceiver = broadcast::Receiver<RuntimeJournalEvent>;

#[derive(Debug, Error)]
pub enum PlanControllerError {
    #[error(transparent)]
    Plan {
        #[from]
        source: PlanError,
    },
    #[error("plan controller session store error: {source}")]
    SessionStore {
        #[from]
        source: SessionStoreError,
    },
    #[error("plan controller command channel is closed")]
    CommandChannelClosed,
    #[error("no active plan exists")]
    NoActivePlan,
    #[error("plan transaction became stale while persistence was in flight")]
    StaleTransaction,
    #[error("plan controller runtime transaction error: {source}")]
    Runtime {
        #[from]
        source: RuntimeError,
    },
}

#[derive(Clone)]
pub(crate) struct PlanController {
    sender: mpsc::Sender<PlanCommand>,
    events: broadcast::Sender<RuntimeJournalEvent>,
    bootstrap: Arc<StdMutex<Option<PlanControllerBootstrap>>>,
}

struct PlanControllerBootstrap {
    session: Arc<Mutex<SessionState>>,
    store: Option<FileSessionStore>,
    receiver: mpsc::Receiver<PlanCommand>,
    events: broadcast::Sender<RuntimeJournalEvent>,
}

impl PlanController {
    pub(crate) fn start(
        session: Arc<Mutex<SessionState>>,
        store: Option<FileSessionStore>,
        buffer_size: NonZeroUsize,
    ) -> (Self, PlanControllerEventReceiver) {
        let (sender, receiver) = mpsc::channel(buffer_size.get());
        let (events, event_receiver) = broadcast::channel(buffer_size.get());
        let controller = Self {
            sender,
            events: events.clone(),
            bootstrap: Arc::new(StdMutex::new(Some(PlanControllerBootstrap {
                session,
                store,
                receiver,
                events,
            }))),
        };
        (controller, event_receiver)
    }

    pub(crate) fn subscribe(&self) -> PlanControllerEventReceiver {
        self.events.subscribe()
    }

    pub(crate) async fn begin(
        &self,
        input: BeginPlanInput,
    ) -> Result<BeginPlanOutput, PlanControllerError> {
        Ok(self.begin_with_events(input).await?.output)
    }

    pub(crate) async fn begin_with_events(
        &self,
        input: BeginPlanInput,
    ) -> Result<PlanCommandResult<BeginPlanOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::Begin { input, reply })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn begin_from_tool(
        &self,
        input: BeginPlanInput,
        call_id: ToolCallId,
    ) -> Result<Vec<RuntimeJournalEvent>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::BeginTool {
                input,
                call_id,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn begin_from_user(
        &self,
        reason: String,
    ) -> Result<PlanCommandResult<BeginPlanOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::BeginUser { reason, reply })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn update(
        &self,
        input: UpdatePlanInput,
    ) -> Result<PlanUpdateOutput, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::Update { input, reply })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        let committed = response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)??;
        Ok(committed.output)
    }

    pub(crate) async fn update_from_tool(
        &self,
        input: UpdatePlanInput,
        call_id: ToolCallId,
    ) -> Result<Vec<RuntimeJournalEvent>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::UpdateTool {
                input,
                call_id,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn start_attempt(
        &self,
        node_id: PlanNodeId,
        actor: PlanAttemptActor,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanAttemptStartOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::StartAttempt {
                node_id,
                actor,
                now_ms,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn directive_from_tool(
        &self,
        input: ControlPlanAttemptInput,
        call_id: ToolCallId,
        now_ms: u64,
    ) -> Result<Vec<RuntimeJournalEvent>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::DirectiveTool {
                input,
                call_id,
                now_ms,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn directive(
        &self,
        input: ControlPlanAttemptInput,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanDirectiveOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::Directive {
                input,
                now_ms,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn progress_from_tool(
        &self,
        actor: PlanAttemptActor,
        input: ReportPlanProgressInput,
        call_id: ToolCallId,
        now_ms: u64,
    ) -> Result<Vec<RuntimeJournalEvent>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::ProgressTool {
                actor,
                input,
                call_id,
                now_ms,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn progress(
        &self,
        actor: PlanAttemptActor,
        input: ReportPlanProgressInput,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::Progress {
                actor,
                input,
                now_ms,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn attempt_report_from_tool(
        &self,
        actor: PlanAttemptActor,
        input: ReportPlanAttemptInput,
        call_id: ToolCallId,
        now_ms: u64,
    ) -> Result<Vec<RuntimeJournalEvent>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::AttemptReportTool {
                actor,
                input,
                call_id,
                now_ms,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn attempt_report(
        &self,
        actor: PlanAttemptActor,
        input: ReportPlanAttemptInput,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanAttemptReportOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::AttemptReport {
                actor,
                input,
                now_ms,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn heartbeat(
        &self,
        actor: PlanAttemptActor,
        lease_id: PlanLeaseId,
        now_ms: u64,
        provider_request_in_flight: bool,
        tool_call_in_flight: bool,
    ) -> Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::Heartbeat {
                actor,
                lease_id,
                now_ms,
                provider_request_in_flight,
                tool_call_in_flight,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn deliver_directives(
        &self,
        actor: PlanAttemptActor,
        lease_id: PlanLeaseId,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanDirectiveDeliveryOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::DeliverDirectives {
                actor,
                lease_id,
                now_ms,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn recover_expired_leases(
        &self,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanRecoveryOutput>, PlanControllerError> {
        self.recover_leases(now_ms, false).await
    }

    pub(crate) async fn recover_live_leases_after_resume(
        &self,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanRecoveryOutput>, PlanControllerError> {
        self.recover_leases(now_ms, true).await
    }

    async fn recover_leases(
        &self,
        now_ms: u64,
        all_live: bool,
    ) -> Result<PlanCommandResult<PlanRecoveryOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::RecoverLeases {
                now_ms,
                all_live,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn review_progress_at_boundary(
        &self,
        actor: PlanAttemptActor,
        lease_id: PlanLeaseId,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanProgressReviewOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::ReviewProgress {
                actor,
                lease_id,
                now_ms,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn approve(
        &self,
        input: PlanApprovalInput,
    ) -> Result<PlanCommandResult<PlanControlOutput>, PlanControllerError> {
        self.control(PlanControlRequest::Approve(input)).await
    }

    pub(crate) async fn pause_scheduling(
        &self,
        reason: String,
    ) -> Result<PlanCommandResult<PlanControlOutput>, PlanControllerError> {
        self.control(PlanControlRequest::Pause(reason)).await
    }

    pub(crate) async fn resume_scheduling(
        &self,
        reason: String,
    ) -> Result<PlanCommandResult<PlanControlOutput>, PlanControllerError> {
        self.control(PlanControlRequest::Resume(reason)).await
    }

    pub(crate) async fn revise(
        &self,
        reason: String,
    ) -> Result<PlanCommandResult<PlanControlOutput>, PlanControllerError> {
        self.control(PlanControlRequest::Revise(reason)).await
    }

    pub(crate) async fn retry_interrupted_node(
        &self,
        node_id: PlanNodeId,
        reason: String,
    ) -> Result<PlanCommandResult<PlanControlOutput>, PlanControllerError> {
        self.control(PlanControlRequest::RetryInterrupted { node_id, reason })
            .await
    }

    pub(crate) async fn request_cancellation(
        &self,
        reason: String,
    ) -> Result<PlanCommandResult<PlanControlOutput>, PlanControllerError> {
        self.control(PlanControlRequest::Cancel(reason)).await
    }

    async fn control(
        &self,
        request: PlanControlRequest,
    ) -> Result<PlanCommandResult<PlanControlOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::Control { request, reply })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn cancel_attempt(
        &self,
        actor: PlanAttemptActor,
        lease_id: PlanLeaseId,
        reason: String,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanAttemptCancellationOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::CancelAttempt {
                actor,
                lease_id,
                reason,
                now_ms,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn authorize_execution(
        &self,
        envelope: PlanCapabilityEnvelopeSnapshot,
        authorization_refs: Vec<String>,
    ) -> Result<PlanCommandResult<PlanSnapshot>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::AuthorizeExecution {
                envelope,
                authorization_refs,
                reply,
            })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
    }

    pub(crate) async fn snapshot(&self) -> Result<Option<PlanSnapshot>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::Snapshot { reply })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)
    }

    fn ensure_started(&self) -> Result<(), PlanControllerError> {
        let bootstrap = self
            .bootstrap
            .lock()
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
            .take();
        if let Some(bootstrap) = bootstrap {
            tokio::spawn(run_controller(
                bootstrap.session,
                bootstrap.store,
                bootstrap.receiver,
                bootstrap.events,
            ));
        }
        Ok(())
    }
}
