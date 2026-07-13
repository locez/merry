use super::{
    PlanError,
    domain::PlanState,
    protocol::{
        BeginPlanInput, BeginPlanOutput, PlanUpdateOutput, PlanUpdateToolOutput, UpdatePlanInput,
    },
    validation,
};
use crate::{
    ArtifactContent, FileSessionStore, RuntimeError, SessionStoreError,
    session::{PreparedPlanToolCommit, SessionState},
};
use merry_core::{
    PlanActivationSource, PlanId, PlanPhase, PlanRevisionSummary, PlanSnapshot,
    RuntimeJournalEvent, RuntimeJournalPayload, ToolCallId, ToolCallResultStatus,
};
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex as StdMutex},
};
use thiserror::Error;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

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

    pub(crate) async fn update(
        &self,
        input: UpdatePlanInput,
    ) -> Result<PlanUpdateOutput, PlanControllerError> {
        Ok(self.update_with_events(input).await?.output)
    }

    pub(crate) async fn update_with_events(
        &self,
        input: UpdatePlanInput,
    ) -> Result<PlanCommandResult<PlanUpdateOutput>, PlanControllerError> {
        self.ensure_started()?;
        let (reply, response) = oneshot::channel();
        self.sender
            .send(PlanCommand::Update { input, reply })
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?;
        response
            .await
            .map_err(|_| PlanControllerError::CommandChannelClosed)?
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

enum PlanCommand {
    Begin {
        input: BeginPlanInput,
        reply: oneshot::Sender<Result<PlanCommandResult<BeginPlanOutput>, PlanControllerError>>,
    },
    BeginTool {
        input: BeginPlanInput,
        call_id: ToolCallId,
        reply: oneshot::Sender<Result<Vec<RuntimeJournalEvent>, PlanControllerError>>,
    },
    Update {
        input: UpdatePlanInput,
        reply: oneshot::Sender<Result<PlanCommandResult<PlanUpdateOutput>, PlanControllerError>>,
    },
    UpdateTool {
        input: UpdatePlanInput,
        call_id: ToolCallId,
        reply: oneshot::Sender<Result<Vec<RuntimeJournalEvent>, PlanControllerError>>,
    },
    Snapshot {
        reply: oneshot::Sender<Option<PlanSnapshot>>,
    },
}

pub(crate) struct PlanCommandResult<T> {
    pub(crate) output: T,
    pub(crate) events: Vec<RuntimeJournalEvent>,
}

async fn run_controller(
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
            PlanCommand::Update { input, reply } => {
                let result = update_plan(&session, store.as_ref(), &events, input, None).await;
                let _ = reply.send(result);
            }
            PlanCommand::UpdateTool {
                input,
                call_id,
                reply,
            } => {
                let result = update_plan(&session, store.as_ref(), &events, input, Some(call_id))
                    .await
                    .map(|committed| committed.events);
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

async fn begin_plan(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    next_plan_sequence: &mut u64,
    input: BeginPlanInput,
    tool_call_id: Option<ToolCallId>,
) -> Result<PlanCommandResult<BeginPlanOutput>, PlanControllerError> {
    validation::validate_reason(&input.reason)?;
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
                PlanActivationSource::Coordinator {
                    reason: input.reason.clone(),
                    governing_skill_id: input.governing_skill_id,
                },
                session
                    .active_plan()
                    .map(|plan| plan.snapshot().resource_policy_snapshot.clone())
                    .unwrap_or_default(),
            );
            let summary = PlanRevisionSummary::new(0, &input.reason).map_err(|_| {
                PlanControllerError::Plan {
                    source: PlanError::InvalidText {
                        field: "reason",
                        reason: "is invalid",
                    },
                }
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

async fn update_plan(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    input: UpdatePlanInput,
    tool_call_id: Option<ToolCallId>,
) -> Result<PlanCommandResult<PlanUpdateOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let previous_phase = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .snapshot()
            .phase;
        let mut candidate = session.active_plan().expect("checked active plan").clone();
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

struct PreparedPlanCommit {
    install: PreparedPlanInstall,
    bundle: crate::session::PersistableSessionBundle,
}

enum PreparedPlanInstall {
    PlanOnly {
        candidate: PlanState,
        terminal_plans: Vec<PlanSnapshot>,
        payloads: Vec<RuntimeJournalPayload>,
    },
    Tool(PreparedPlanToolCommit),
}

fn prepare_plan_commit(
    session: &SessionState,
    candidate: PlanState,
    terminal_plans: Vec<PlanSnapshot>,
    payloads: Vec<RuntimeJournalPayload>,
    tool_resolution: Option<(ToolCallId, ArtifactContent)>,
) -> Result<PreparedPlanCommit, PlanControllerError> {
    if let Some((call_id, content)) = tool_resolution {
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
            install: PreparedPlanInstall::Tool(prepared),
            bundle,
        });
    }

    let next_sequence = session.next_sequence() + payloads.len() as u64;
    let bundle = session.persistable_bundle_with_plan_candidate(
        Some(&candidate),
        &terminal_plans,
        next_sequence,
    )?;
    Ok(PreparedPlanCommit {
        install: PreparedPlanInstall::PlanOnly {
            candidate,
            terminal_plans,
            payloads,
        },
        bundle,
    })
}

async fn persist_and_install(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    base: SessionBase,
    prepared: PreparedPlanCommit,
) -> Result<Vec<RuntimeJournalEvent>, PlanControllerError> {
    let PreparedPlanCommit { install, bundle } = prepared;
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

    let committed_events = {
        let mut session = session.lock().await;
        if !base.matches(&session) {
            return Err(PlanControllerError::StaleTransaction);
        }
        match install {
            PreparedPlanInstall::PlanOnly {
                candidate,
                terminal_plans,
                payloads,
            } => {
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
                payloads
                    .into_iter()
                    .map(|payload| session.record_transient_event(payload))
                    .collect::<Vec<_>>()
            }
            PreparedPlanInstall::Tool(prepared) => {
                let committed_events = prepared.events().to_vec();
                session.install_plan_tool_commit(prepared);
                committed_events
            }
        }
    };
    for event in &committed_events {
        let _ = events.send(event.clone());
    }
    Ok(committed_events)
}

#[derive(Debug, Clone)]
struct SessionBase {
    next_sequence: u64,
    active_plan: Option<(PlanId, u64)>,
    terminal_plan_count: usize,
}

impl SessionBase {
    fn capture(session: &SessionState) -> Self {
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
