//! Runtime builder and step execution skeleton.
//!
//! [`Runtime`] is the MVP facade for session-owned state. Step execution and
//! direct mutation APIs admit one active operation at a time, record durable
//! session state before returning observable events where applicable, and keep
//! provider wire details behind the `merry-llm` provider boundary.

use crate::{
    CitationCompactionInput, CitationCompactionPolicy, CompactedCheckpointSummary, CompactionError,
    CompactionOutcome, FileSessionStore, RuntimeCapabilities, RuntimeError,
    events::{
        ActiveStepPermit, RuntimeEventProjector, RuntimeEventStream, RuntimeJournalEventStream,
    },
    model_config::ModelProviderConfig,
    step::{StepContext, StepInput},
    subagent::{SubagentActivityReceiver, SubagentManager},
    tool::ToolExecutionContext,
};
use merry_core::{
    PendingToolCall, QueuedInputView, RuntimeEvent, RuntimeJournalEvent, SessionId, ToolCallId,
};
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use std::{
    num::NonZeroU64,
    sync::{Arc, atomic::Ordering},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

mod auto_compaction;
mod builder;
mod config;
mod diagnostics;
mod journal_emission;
mod journal_persistence;
mod memory_activation;
mod memory_maintenance;
mod model_output;
mod model_turn_lifecycle;
mod output_rate;
mod permission_execution;
mod plan_tool_execution;
mod process_execution;
mod provider_request;
mod provider_step;
mod provider_stream;
mod session_access;
mod session_topic;
pub(crate) use self::session_topic::TopicTask;
mod state;
mod step;
mod tool_batch;
mod tool_execution;

use self::auto_compaction::{
    compact_context_once_inner, compaction_input_for_policy,
    install_summary_compaction_transactionally,
};
pub use self::builder::RuntimeBuilder;
pub use self::config::{ArchiveConfig, CompactionConfig, HistoryRetention};
use self::diagnostics::{
    DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED, DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED,
    DIAGNOSTIC_TOOL_NOT_REGISTERED, TOOL_ACTION_POLICY_DENIED_MESSAGE, diagnostic_from_text,
    runtime_error_message,
};
#[cfg(test)]
use self::journal_emission::send_cancelled_event;
#[cfg(test)]
use self::memory_activation::memory_activation_seed_from_step_input;
#[cfg(test)]
use self::provider_request::{request_context_budget, step_usage_context_snapshot};
use self::state::RuntimeInner;
#[cfg(test)]
use self::tool_execution::admit_action_to_generic_executor;

/// Merry runtime handle for one session.
///
/// A cloned handle points at the same session-owned state. [`Runtime::step`]
/// and direct mutation APIs such as [`Runtime::record_artifact`],
/// [`Runtime::record_context_entry`], [`Runtime::submit_tool_result`], and
/// [`Runtime::execute_tool_call`] acquire the active-step permit.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl Runtime {
    /// Creates a runtime builder for the provided session.
    ///
    /// The session id defines the ownership boundary for transcript state,
    /// context, pending tool calls, and emitted runtime events.
    #[must_use]
    pub fn builder(session_id: SessionId) -> RuntimeBuilder {
        RuntimeBuilder::new(session_id)
    }

    /// Subscribes to the latest UI-only activity snapshots for managed subagents.
    #[must_use]
    pub fn subscribe_subagent_activity(&self) -> SubagentActivityReceiver {
        self.inner.subagents.activity_hub.subscribe()
    }

    /// Resumes a session from the default XDG state store.
    pub async fn resume(session_id: SessionId) -> Result<Self, RuntimeError> {
        let store = FileSessionStore::default_store()?;
        Self::builder(session_id).resume_from_store(store).await
    }

    /// Starts a runtime step and returns its event stream.
    ///
    /// Only one step or direct mutation may own the runtime at a time. The
    /// step producer owns the initial active-step permit. Dropping the returned
    /// [`RuntimeJournalEventStream`] cancels and aborts the producer; the runtime
    /// becomes available after the producer and any in-flight persistence
    /// transaction have dropped their permit handles.
    ///
    /// All events emitted by the step are provider-neutral [`RuntimeJournalEvent`]
    /// values. The runtime records session transcript and pending-tool state
    /// before the corresponding event becomes observable.
    ///
    /// Cancellation records a cancelled event when the producer reaches a
    /// cancellation checkpoint. Pending tool calls remain pending unless a
    /// durable result has already been recorded. If compaction persistence is
    /// already in flight, its transaction keeps the active-step permit until
    /// the staged state is discarded or durably installed.
    pub fn step(
        &self,
        input: StepInput,
        context: StepContext,
    ) -> Result<RuntimeJournalEventStream, RuntimeError> {
        let active_permit = self.acquire_active_step_permit()?;

        self.step_with_active_permit(input, context, active_permit)
    }

    /// Starts a runtime step and returns the raw ordered journal stream.
    ///
    /// This is the explicit low-level alias for [`Runtime::step`]. Runtime
    /// internals, debugging tools, and replay inspection should use this API
    /// when they need exact journal payloads rather than SDK-facing projection.
    pub fn journal_stream(
        &self,
        input: StepInput,
        context: StepContext,
    ) -> Result<RuntimeJournalEventStream, RuntimeError> {
        self.step(input, context)
    }

    /// Starts a runtime step and returns SDK-facing public events.
    ///
    /// Public events are projected from the internal journal after the
    /// corresponding session state is recorded. They expose assistant text and
    /// tool activity directly while omitting internal bridge handoff details.
    pub fn stream(
        &self,
        input: StepInput,
        context: StepContext,
    ) -> Result<RuntimeEventStream, RuntimeError> {
        let journal_stream = self.step(input, context)?;
        Ok(RuntimeEventStream::new(
            journal_stream,
            self.clone(),
            self.inner.event_buffer_size.get(),
        ))
    }

    /// Projects raw ordered journal events into SDK-facing public events.
    ///
    /// This helper is read-only and is intended for SDK bindings that need to
    /// return public events for a completed run while preserving raw journal
    /// evidence inside Rust runtime results.
    pub async fn project_journal_events(
        &self,
        events: &[RuntimeJournalEvent],
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let mut projector = RuntimeEventProjector::new();
        let mut projected = Vec::new();
        for event in events {
            if let Some(event) = projector.project(event.clone(), self).await? {
                projected.push(event);
            }
        }
        Ok(projected)
    }

    /// Returns the latest authoritative session usage snapshot.
    pub async fn usage(&self) -> Option<merry_core::SessionUsage> {
        let session = self.inner.session.lock().await;
        session.usage()
    }

    pub(crate) fn acquire_active_step_permit(&self) -> Result<ActiveStepPermit, RuntimeError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(RuntimeError::SessionClosed {
                session_id: self.inner.session_id.clone(),
            });
        }
        let permit =
            ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step)).ok_or_else(|| {
                RuntimeError::StepAlreadyActive {
                    session_id: self.inner.session_id.clone(),
                }
            })?;
        // Re-check after acquiring the permit to close any race with `Runtime::close`.
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(RuntimeError::SessionClosed {
                session_id: self.inner.session_id.clone(),
            });
        }
        Ok(permit)
    }

    pub fn session_id(&self) -> &SessionId {
        &self.inner.session_id
    }

    /// Returns the model-written session topic, waiting briefly for the
    /// first-turn side call when it is still running.
    pub async fn session_topic(&self) -> Option<String> {
        self::session_topic::topic(&self.inner).await
    }

    /// Returns the already-recorded session topic without waiting on an
    /// in-flight background topic generation call.
    pub async fn cached_session_topic(&self) -> Option<String> {
        self.inner.session.lock().await.topic().map(str::to_owned)
    }

    #[cfg(test)]
    pub(crate) fn session_for_tests(
        &self,
    ) -> &Arc<tokio::sync::Mutex<crate::session::SessionState>> {
        &self.inner.session
    }

    pub(crate) fn schedule_memory_maintenance(&self) {
        self::memory_maintenance::schedule(&self.inner);
    }

    pub(crate) fn record_queued_input_accepted(&self, inputs: &[QueuedInputView]) {
        self.inner.trajectory.record_queued_input_accepted(inputs);
    }

    pub(crate) fn close_trajectory(&self) {
        self.inner.trajectory.close();
    }

    /// Saves the current resume-safe session state to the provided store.
    ///
    /// The in-memory cursor tracks the runtime's configured store. Another
    /// directory is written as a complete journal when it has no header, and
    /// that write does not move the cursor.
    pub async fn save_session_to(&self, store: FileSessionStore) -> Result<(), RuntimeError> {
        let active_permit = self.acquire_active_step_permit()?;
        self.save_session_to_with_active_permit(store, &active_permit)
            .await
    }

    pub(crate) async fn save_session_to_with_active_permit(
        &self,
        store: FileSessionStore,
        _active_permit: &ActiveStepPermit,
    ) -> Result<(), RuntimeError> {
        let has_header =
            crate::session::journal_destination_has_header(&store, &self.inner.session_id).await?;
        let (bundle, tracked, adopt_destination) = {
            let session = self.inner.session.lock().await;
            let tracked_dir = self
                .inner
                .persistence
                .store
                .as_ref()
                .map(FileSessionStore::sessions_dir)
                .or_else(|| session.tracked_store_dir());
            let adopt_destination = tracked_dir.is_none();
            let tracked = adopt_destination || tracked_dir == Some(store.sessions_dir());
            let cursor = session.cursor_for_destination(tracked, has_header);
            let bundle = session.persistable_bundle_with_cursor(&cursor)?;
            (bundle, tracked, adopt_destination)
        };
        let cursor = bundle.cursor.clone();
        if !bundle.document_bytes.is_empty() {
            store.write_bundle(bundle).await?;
        }
        if tracked {
            let mut session = self.inner.session.lock().await;
            if adopt_destination {
                session.set_tracked_store_dir(store.sessions_dir().to_path_buf());
            }
            session.adopt_journal_cursor(cursor);
        }
        Ok(())
    }

    /// Saves the current resume-safe session state to the configured store.
    pub async fn save_session(&self) -> Result<(), RuntimeError> {
        let store = match self.inner.persistence.store.clone() {
            Some(store) => store,
            None => FileSessionStore::default_store()?,
        };
        self.save_session_to(store).await
    }

    /// Lists background processes retained by this session.
    #[must_use]
    pub fn background_processes(&self) -> Vec<crate::BackgroundProcessSummary> {
        self.inner
            .process_lanes
            .background_control
            .as_ref()
            .map(|control| control.list())
            .unwrap_or_default()
    }

    /// Reads captured output for one retained background process.
    pub fn read_background_process(
        &self,
        id: &crate::BackgroundProcessId,
    ) -> Result<crate::BackgroundProcessOutput, crate::BackgroundProcessLookupError> {
        match &self.inner.process_lanes.background_control {
            Some(control) => control.read(id),
            None => Err(crate::BackgroundProcessLookupError::new(
                id.clone(),
                Vec::new(),
            )),
        }
    }

    /// Stops one retained background process group.
    pub fn stop_background_process(
        &self,
        id: &crate::BackgroundProcessId,
    ) -> Result<crate::BackgroundProcessStop, crate::BackgroundProcessLookupError> {
        match &self.inner.process_lanes.background_control {
            Some(control) => control.stop(id),
            None => Err(crate::BackgroundProcessLookupError::new(
                id.clone(),
                Vec::new(),
            )),
        }
    }

    /// Returns a compact snapshot of managed subagent statuses when configured.
    pub async fn subagent_snapshot(&self) -> Option<Vec<crate::SubagentStatusView>> {
        match &self.inner.subagents.manager {
            Some(manager) => Some(manager.snapshot().await),
            None => None,
        }
    }

    /// Opens a lazy transcript & live event inspection session for one subagent.
    pub async fn inspect_subagent(
        &self,
        agent_id: &merry_core::SubagentId,
    ) -> Option<crate::SubagentInspectSession> {
        match &self.inner.subagents.manager {
            Some(manager) => manager.inspect(agent_id).await,
            None => None,
        }
    }

    /// Sends a follow-up user message directly to one managed subagent.
    pub async fn send_subagent_message(
        &self,
        agent_id: &merry_core::SubagentId,
        message: String,
    ) -> Result<(), RuntimeError> {
        match &self.inner.subagents.manager {
            Some(manager) => manager.send_message(agent_id, message).await,
            None => Err(RuntimeError::InvalidSubagentSelection {
                operation: "send_subagent_message",
            }),
        }
    }

    /// Cancels one managed subagent.
    pub async fn cancel_subagent(
        &self,
        agent_id: &merry_core::SubagentId,
    ) -> Result<crate::WaitSubagentsOutput, RuntimeError> {
        match &self.inner.subagents.manager {
            Some(manager) => manager.cancel(std::slice::from_ref(agent_id)).await,
            None => Err(RuntimeError::InvalidSubagentSelection {
                operation: "cancel_subagents",
            }),
        }
    }

    pub(crate) fn subscribe_subagent_events(
        &self,
    ) -> tokio::sync::broadcast::Receiver<RuntimeJournalEvent> {
        self.inner.subagents.event_sink.subscribe()
    }

    pub(crate) async fn flush_attached_session_store(&self) -> Result<(), RuntimeError> {
        let Some(store) = self.inner.persistence.store.clone() else {
            return Ok(());
        };
        let prepared = {
            let session = self.inner.session.lock().await;
            session.persistable_bundle_if_resume_safe()?
        };
        if let Some(bundle) = prepared {
            let cursor = bundle.cursor.clone();
            store.write_bundle(bundle).await?;
            self.inner.session.lock().await.adopt_journal_cursor(cursor);
        }
        Ok(())
    }

    pub(crate) fn shutdown_background_processes(&self) {
        let Some(control) = self.inner.process_lanes.background_control.as_ref() else {
            return;
        };
        for process in control.list() {
            if matches!(
                process.state(),
                crate::BackgroundProcessState::Running { .. }
            ) {
                let _ = control.stop(process.id());
            }
        }
    }

    pub(crate) fn subagent_completion_notify(&self) -> Option<Arc<Notify>> {
        self.inner
            .subagents
            .manager
            .as_ref()
            .map(SubagentManager::completion_notify)
    }

    pub(crate) async fn take_subagent_completion_notifications(
        &self,
    ) -> Vec<crate::SubagentStatusView> {
        match &self.inner.subagents.manager {
            Some(manager) => manager.take_completion_notifications().await,
            None => Vec::new(),
        }
    }

    pub(crate) async fn has_subagent_completion_notifications(&self) -> bool {
        match &self.inner.subagents.manager {
            Some(manager) => manager.has_completion_notifications().await,
            None => false,
        }
    }

    pub(crate) async fn has_active_subagents(&self) -> bool {
        match &self.inner.subagents.manager {
            Some(manager) => manager.has_active_agents().await,
            None => false,
        }
    }

    /// Cancels all live subagents owned by this runtime and awaits their task handles.
    pub async fn shutdown_subagents(&self) {
        if let Some(manager) = &self.inner.subagents.manager {
            manager.shutdown().await;
        }
    }

    /// Returns the current Plan Mode and session plan list snapshot.
    pub async fn plan_session_snapshot(&self) -> merry_core::PlanSessionSnapshot {
        self.inner.session.lock().await.plan_state().snapshot()
    }

    /// Returns all Markdown plan documents authored in this session.
    pub async fn plan_documents(&self) -> Vec<merry_core::PlanDocument> {
        self.inner.session.lock().await.plan_state().documents()
    }

    /// Returns the current checklist items for this session.
    pub async fn todo_items(&self) -> Vec<merry_core::TodoItem> {
        self.inner.session.lock().await.todo_list().items().to_vec()
    }

    /// Returns whether a submitted plan is currently awaiting user review.
    pub async fn awaiting_plan_approval(&self) -> bool {
        self.inner
            .session
            .lock()
            .await
            .plan_state()
            .awaiting_approval_plan_id()
            .is_some()
    }

    /// Toggles Plan Mode from an explicit user control.
    ///
    /// Returns any durable journal event emitted if the mode changed.
    pub async fn set_plan_mode(
        &self,
        enabled: bool,
        turn_in_flight: bool,
    ) -> Result<Option<RuntimeJournalEvent>, RuntimeError> {
        let event = {
            let mut session = self.inner.session.lock().await;
            session
                .plan_state_mut()
                .set_mode_from_user(enabled, turn_in_flight)
                .map(|mode| {
                    session
                        .record_event(merry_core::RuntimeJournalPayload::PlanModeChanged { mode })
                })
        };
        if event.is_some() {
            self.persist_resume_safe_snapshot_if_store_attached()
                .await?;
        }
        Ok(event)
    }

    /// Activates `Pending` Plan Mode when a user prompt begins.
    pub(crate) async fn activate_pending_plan_mode_for_prompt(
        &self,
    ) -> Option<RuntimeJournalEvent> {
        let mut session = self.inner.session.lock().await;
        session
            .plan_state_mut()
            .activate_on_user_prompt()
            .map(|mode| {
                session.record_event(merry_core::RuntimeJournalPayload::PlanModeChanged { mode })
            })
    }

    /// Finalizes `ExitPending -> Inactive` when an in-flight turn completes.
    pub(crate) async fn complete_deferred_plan_mode_exit(&self) -> Option<RuntimeJournalEvent> {
        let mut session = self.inner.session.lock().await;
        session
            .plan_state_mut()
            .complete_deferred_exit()
            .map(|mode| {
                session.record_event(merry_core::RuntimeJournalPayload::PlanModeChanged { mode })
            })
    }

    /// Approves a submitted plan, exits Plan Mode, and persists the updated state.
    pub async fn approve_plan(
        &self,
        plan_id: merry_core::PlanId,
        note: Option<String>,
    ) -> Result<(merry_core::PlanDocument, Vec<RuntimeJournalEvent>), RuntimeError> {
        let now_ms = crate::plan::unix_time_ms();
        let (doc, events) = {
            let mut session = self.inner.session.lock().await;
            let (doc, decision) = session
                .plan_state_mut()
                .approve_plan(&plan_id, note, now_ms)?;
            let reviewed = session.record_event(merry_core::RuntimeJournalPayload::PlanReviewed {
                plan_id: doc.summary.id.clone(),
                decision,
            });
            let mode_changed =
                session.record_event(merry_core::RuntimeJournalPayload::PlanModeChanged {
                    mode: merry_core::PlanModeState::Inactive,
                });
            (doc, vec![reviewed, mode_changed])
        };
        self.persist_resume_safe_snapshot_if_store_attached()
            .await?;
        Ok((doc, events))
    }

    /// Requests changes on a submitted plan, keeps Plan Mode active, and persists state.
    pub async fn request_plan_changes(
        &self,
        plan_id: merry_core::PlanId,
        feedback: String,
    ) -> Result<(merry_core::PlanDocument, Vec<RuntimeJournalEvent>), RuntimeError> {
        let now_ms = crate::plan::unix_time_ms();
        let (doc, events) = {
            let mut session = self.inner.session.lock().await;
            let (doc, decision) = session
                .plan_state_mut()
                .request_changes(&plan_id, feedback, now_ms)?;
            let reviewed = session.record_event(merry_core::RuntimeJournalPayload::PlanReviewed {
                plan_id: doc.summary.id.clone(),
                decision,
            });
            let mode_changed =
                session.record_event(merry_core::RuntimeJournalPayload::PlanModeChanged {
                    mode: merry_core::PlanModeState::Active,
                });
            (doc, vec![reviewed, mode_changed])
        };
        self.persist_resume_safe_snapshot_if_store_attached()
            .await?;
        Ok((doc, events))
    }

    /// Abandons a submitted plan and exits Plan Mode without starting implementation.
    pub async fn abandon_plan(
        &self,
        plan_id: merry_core::PlanId,
    ) -> Result<(merry_core::PlanDocument, Vec<RuntimeJournalEvent>), RuntimeError> {
        let now_ms = crate::plan::unix_time_ms();
        let (doc, events) = {
            let mut session = self.inner.session.lock().await;
            let (doc, decision) = session.plan_state_mut().abandon_plan(&plan_id, now_ms)?;
            let reviewed = session.record_event(merry_core::RuntimeJournalPayload::PlanReviewed {
                plan_id: doc.summary.id.clone(),
                decision,
            });
            let mode_changed =
                session.record_event(merry_core::RuntimeJournalPayload::PlanModeChanged {
                    mode: merry_core::PlanModeState::Inactive,
                });
            (doc, vec![reviewed, mode_changed])
        };
        self.persist_resume_safe_snapshot_if_store_attached()
            .await?;
        Ok((doc, events))
    }

    async fn persist_resume_safe_snapshot_if_store_attached(&self) -> Result<(), RuntimeError> {
        let Some(store) = self.inner.persistence.store.as_ref() else {
            return Ok(());
        };
        let bundle = {
            let session = self.inner.session.lock().await;
            session.persistable_bundle_if_resume_safe()?
        };
        if let Some(bundle) = bundle
            && !bundle.document_bytes.is_empty()
        {
            let cursor = bundle.cursor.clone();
            store.write_bundle(bundle).await?;
            self.inner.session.lock().await.adopt_journal_cursor(cursor);
        }
        Ok(())
    }

    /// Returns the low-level Merry-managed capabilities configured for this runtime.
    #[must_use]
    pub fn capabilities(&self) -> &RuntimeCapabilities {
        &self.inner.capabilities
    }

    /// Returns the current runtime-owned trajectory projection.
    pub async fn trajectory_snapshot(
        &self,
    ) -> Result<merry_core::TrajectorySnapshot, RuntimeError> {
        Ok(self.inner.trajectory.snapshot())
    }

    /// Subscribes to trajectory changes and returns an atomic initial snapshot.
    pub async fn trajectory_subscription(
        &self,
    ) -> Result<
        (
            merry_core::TrajectorySnapshot,
            tokio::sync::broadcast::Receiver<merry_core::TrajectoryEvent>,
        ),
        RuntimeError,
    > {
        Ok(self.inner.trajectory.subscribe_with_snapshot())
    }

    /// Returns the configured skill metadata for UI/SDK discovery.
    ///
    /// This exposes only metadata already stored in the session. Skill bodies
    /// remain on disk and do not enter prompt context through this accessor.
    pub async fn skills(&self) -> Vec<crate::SkillMetadata> {
        let session = self.inner.session.lock().await;
        session
            .skill_catalog()
            .map(|catalog| catalog.skills().to_vec())
            .unwrap_or_default()
    }

    /// Finds one configured skill by exact metadata name.
    pub async fn find_skill(&self, name: &str) -> Option<crate::SkillMetadata> {
        self.skills()
            .await
            .into_iter()
            .find(|skill| skill.name() == name)
    }

    /// Returns whether this runtime asks the model for tool-progress commentary.
    #[must_use]
    pub fn progress_commentary(&self) -> bool {
        self.inner.models.progress_commentary
    }

    /// Executes one pending tool call through a runtime-registered executor.
    ///
    /// Runtime code owns the resulting artifact id and `ToolCallResult`.
    /// Executor infrastructure errors and cancellation leave the call pending.
    /// Tool-domain failures should be returned as failed outcomes so the
    /// runtime can still record a durable result and emit `ToolCallResolved`.
    ///
    /// This method acquires the active-step permit while the executor runs. The
    /// executor receives provider-neutral pending call data and returns
    /// provider-neutral artifact content; provider-specific tool wire formats do
    /// not enter runtime state.
    pub async fn execute_tool_call(
        &self,
        call_id: &ToolCallId,
        context: ToolExecutionContext,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let _active_permit = self.acquire_active_step_permit()?;

        self.execute_tool_call_with_active_permit(call_id, context, &_active_permit)
            .await
    }

    pub(crate) async fn execute_tool_call_with_active_permit(
        &self,
        call_id: &ToolCallId,
        context: ToolExecutionContext,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let result =
            tool_execution::execute_tool_call_with_active_permit(&self.inner, call_id, context)
                .await;
        if let Ok(events) = &result {
            self.inner.commit_journal_events(events).await;
        }
        result
    }

    pub(crate) async fn execute_tool_call_batch_with_active_permit(
        &self,
        calls: Vec<PendingToolCall>,
        context: ToolExecutionContext,
        _active_permit: &ActiveStepPermit,
    ) -> tool_batch::ToolBatchExecution {
        let execution =
            tool_batch::execute_tool_call_batch_with_active_permit(&self.inner, calls, context)
                .await;
        if execution.is_successful() {
            self.inner.commit_journal_events(execution.events()).await;
        } else {
            self.inner.project_journal_events(execution.events());
        }
        execution
    }

    pub(crate) async fn execute_tool_call_batch_streaming_with_active_permit(
        &self,
        calls: Vec<PendingToolCall>,
        context: ToolExecutionContext,
        event_tx: tokio::sync::mpsc::Sender<Vec<RuntimeJournalEvent>>,
        _active_permit: &ActiveStepPermit,
    ) -> Option<RuntimeError> {
        let error = tool_batch::execute_tool_call_batch_streaming_with_active_permit(
            &self.inner,
            calls,
            context,
            event_tx,
        )
        .await;
        if error.is_none() {
            self.inner.refresh_resume_savepoint().await;
        }
        error
    }

    /// Payload-free summary of the installed compacted checkpoint, if any.
    pub async fn compacted_checkpoint_summary(&self) -> Option<CompactedCheckpointSummary> {
        let session = self.inner.session.lock().await;
        session.compacted_checkpoint_summary()
    }

    /// Builds a model-facing citation compaction input for the compressible history prefix.
    pub async fn citation_compaction_input(
        &self,
        policy: CitationCompactionPolicy,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError> {
        let _active_permit = self.acquire_active_step_permit()?;
        compaction_input_for_policy(&self.inner, policy).await
    }

    /// Installs a prose compaction summary and drops the covered history from the prompt.
    pub async fn install_summary_compaction(
        &self,
        input: CitationCompactionInput,
        summary: &str,
    ) -> Result<CompactionOutcome, RuntimeError> {
        let active_permit = self.acquire_active_step_permit()?;
        install_summary_compaction_transactionally(
            Arc::clone(&self.inner),
            input,
            summary,
            CancellationToken::new(),
            active_permit,
        )
        .await
    }

    /// Reduces history toward the destination budget, using bounded rolling passes if needed.
    /// Returns aggregate coverage and the final checkpoint. Each pass installs durably;
    /// cancellation or a later failure preserves any previously installed checkpoint.
    pub async fn compact_context_once(
        &self,
        policy: CitationCompactionPolicy,
        context: StepContext,
    ) -> Result<Option<CompactionOutcome>, RuntimeError> {
        self.compact_context_once_with_focus(policy, None, context)
            .await
    }

    /// Reduces history toward the destination budget while incorporating optional
    /// user-supplied `focus` instructions into the summary directive.
    pub async fn compact_context_once_with_focus(
        &self,
        policy: CitationCompactionPolicy,
        focus: Option<String>,
        context: StepContext,
    ) -> Result<Option<CompactionOutcome>, RuntimeError> {
        let active_permit = self.acquire_active_step_permit()?;

        let (token, _, _) = context.into_parts();
        if token.is_cancelled() {
            return Err(RuntimeError::Compaction {
                source: CompactionError::InvalidModelResponseShape {
                    reason: "compaction cancelled before input build",
                },
            });
        }

        compact_context_once_inner(&self.inner, policy, focus, token, active_permit, None).await
    }

    pub(crate) async fn compact_context_with_active_permit(
        &self,
        focus: Option<String>,
        token: CancellationToken,
        active_permit: ActiveStepPermit,
        started_notify: tokio::sync::oneshot::Sender<()>,
    ) -> Result<Option<CompactionOutcome>, RuntimeError> {
        let policy = self.inner.models.automatic_compaction.read().await.policy();
        compact_context_once_inner(
            &self.inner,
            policy,
            focus,
            token,
            active_permit,
            Some(started_notify),
        )
        .await
    }

    pub(crate) async fn record_manual_compaction_started(&self) -> merry_core::RuntimeJournalEvent {
        let event = {
            let mut session = self.inner.session.lock().await;
            session.record_compaction_started()
        };
        self.inner
            .commit_journal_events(std::slice::from_ref(&event))
            .await;
        event
    }

    pub(crate) async fn record_manual_compaction_completed(
        &self,
        outcome: &CompactionOutcome,
    ) -> merry_core::RuntimeJournalEvent {
        let event = {
            let mut session = self.inner.session.lock().await;
            session.record_compaction_completed(
                outcome.checkpoint_id().as_str().to_owned(),
                outcome.covered_history_item_count(),
            )
        };
        self.inner
            .commit_journal_events(std::slice::from_ref(&event))
            .await;
        event
    }

    pub(crate) async fn record_manual_compaction_cancelled(
        &self,
        message: &str,
    ) -> merry_core::RuntimeJournalEvent {
        let diagnostic = diagnostic_from_text("manual_compaction_cancelled", message);
        let event = {
            let mut session = self.inner.session.lock().await;
            session.record_cancelled(diagnostic)
        };
        self.inner
            .commit_journal_events(std::slice::from_ref(&event))
            .await;
        event
    }

    pub(crate) async fn record_manual_compaction_failed(
        &self,
        message: &str,
    ) -> merry_core::RuntimeJournalEvent {
        let diagnostic = diagnostic_from_text("manual_compaction", message);
        let event = {
            let mut session = self.inner.session.lock().await;
            session.record_failed(diagnostic)
        };
        self.inner
            .commit_journal_events(std::slice::from_ref(&event))
            .await;
        event
    }
}

impl Runtime {
    /// Returns the automatic compaction policy used by subsequent requests.
    pub async fn automatic_compaction_config(&self) -> CompactionConfig {
        self.inner.models.automatic_compaction.read().await.clone()
    }

    pub(crate) async fn update_interactive_primary_model(
        &self,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        retry_policy: ModelRetryPolicy,
    ) {
        self.update_interactive_role_model(
            crate::RuntimeModelRole::Primary,
            Some(crate::InteractivePrimaryModel::new(
                provider,
                model,
                retry_policy,
            )),
        )
        .await;
    }

    pub(crate) async fn update_interactive_role_model(
        &self,
        role: crate::RuntimeModelRole,
        model: Option<crate::InteractivePrimaryModel>,
    ) {
        let entry = model.map(|selected| {
            ModelProviderConfig::new(selected.provider, selected.model, selected.retry_policy)
                .with_reasoning_effort(selected.reasoning_effort)
        });
        if role == crate::RuntimeModelRole::Primary
            && let Some(ref cfg) = entry
            && let Some(manager) = self.inner.subagents.manager.as_ref()
        {
            manager.update_parent_primary_model(crate::ParentPrimaryModel {
                provider: cfg.provider(),
                model: cfg.model().clone(),
                retry_policy: cfg.retry_policy(),
            });
        }
        self.inner
            .models
            .role_overrides
            .write()
            .await
            .insert(role, entry);
    }

    pub(crate) fn update_interactive_max_parallel_tool_calls(&self, limit: std::num::NonZeroUsize) {
        self.inner
            .tools
            .max_parallel_calls
            .store(limit.get(), std::sync::atomic::Ordering::Release);
    }

    pub(crate) async fn update_interactive_permission_review_mode(
        &self,
        mode: crate::PermissionReviewMode,
    ) {
        *self.inner.permissions.review_mode.write().await = mode;
    }

    pub(crate) async fn update_interactive_permission_admission_source(
        &self,
        source: Arc<dyn crate::PermissionAdmissionSource>,
    ) {
        *self.inner.permissions.admission_source.write().await = Some(source);
    }

    pub(crate) fn update_interactive_generation_config(
        &self,
        generation_config: merry_llm::GenerationConfig,
    ) {
        if let Some(manager) = self.inner.subagents.manager.as_ref() {
            manager.update_parent_generation_config(generation_config);
        }
    }

    pub(crate) async fn update_interactive_subagents(
        &self,
        enabled: bool,
        config: crate::SubagentConfig,
    ) -> Result<(), RuntimeError> {
        let manager =
            self.inner
                .subagents
                .manager
                .as_ref()
                .ok_or(RuntimeError::InvalidStepInput {
                    reason: "interactive subagent runtime control is unavailable",
                })?;
        manager.update_policy(enabled, config).await
    }

    pub(crate) async fn update_interactive_automatic_compaction(&self, config: CompactionConfig) {
        *self.inner.models.automatic_compaction.write().await = config;
    }

    pub(crate) async fn update_interactive_context_window_tokens(
        &self,
        context_window_tokens: Option<NonZeroU64>,
    ) {
        *self.inner.models.context_window_tokens.write().await = context_window_tokens;
    }

    pub(crate) fn native_hook_handler(&self) -> Option<&Arc<dyn crate::RuntimeHookHandler>> {
        self.inner.tools.hook_handler.as_ref()
    }

    pub(crate) fn bridge_hook_subscriptions(&self) -> &crate::HookSubscriptions {
        &self.inner.tools.bridge_hook_subscriptions
    }

    pub(crate) fn validate_registered_tool_input(
        &self,
        call: &merry_core::PendingToolCall,
    ) -> Option<Result<(), crate::tool_input_validation::ToolInputValidationError>> {
        self.inner.tools.registry.validate_tool_input(call)
    }

    pub(crate) async fn compact_context_for_hook_with_active_permit(
        &self,
        cancellation_token: tokio_util::sync::CancellationToken,
        active_permit: &ActiveStepPermit,
    ) -> Result<Option<CompactionOutcome>, RuntimeError> {
        let policy = self.inner.models.automatic_compaction.read().await.policy();
        auto_compaction::compact_context_once_inner(
            &self.inner,
            policy,
            None,
            cancellation_token,
            active_permit.clone(),
            None,
        )
        .await
    }
}
#[cfg(test)]
mod tests;
