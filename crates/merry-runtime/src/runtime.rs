//! Runtime builder and step execution skeleton.
//!
//! [`Runtime`] is the MVP facade for session-owned state. Step execution and
//! direct mutation APIs admit one active operation at a time, record durable
//! session state before returning observable events where applicable, and keep
//! provider wire details behind the `merry-llm` provider boundary.

use crate::{
    CheckpointRefId, CitationCompactionInput, CitationCompactionPolicy, CompactedCheckpointSummary,
    CompactionError, CompactionOutcome, FileSessionStore, RuntimeCapabilities, RuntimeError,
    TextEvidencePage,
    events::{
        ActiveStepPermit, RuntimeEventProjector, RuntimeEventStream, RuntimeJournalEventStream,
    },
    judgment::{JudgmentContext, JudgmentError, JudgmentRecord, JudgmentRequest, JudgmentSource},
    model_config::ModelProviderConfig,
    plan::{
        BeginPlanInput, BeginPlanOutput, PlanControllerError, PlanControllerEventReceiver,
        PlanUpdateOutput, UpdatePlanInput,
    },
    step::{StepContext, StepInput},
    subagent::{SubagentActivityReceiver, SubagentManager},
    tool::ToolExecutionContext,
};
use merry_core::{
    PendingToolCall, QueuedInputView, RuntimeEvent, RuntimeJournalEvent, SessionId, ToolCallId,
};
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use std::{num::NonZeroU64, sync::Arc};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

mod auto_compaction;
mod builder;
mod checkpoint_ref_tool;
mod config;
mod diagnostics;
mod journal_emission;
mod journal_persistence;
mod memory_activation;
mod model_output;
mod permission_execution;
mod plan_tool_execution;
mod process_execution;
mod provider_request;
mod provider_step;
mod session_access;
mod state;
mod step;
mod tool_batch;
mod tool_execution;

use self::auto_compaction::{
    compact_context_once_inner, compaction_input_for_policy,
    install_citation_compaction_candidate_transactionally,
};
pub use self::builder::RuntimeBuilder;
#[cfg(test)]
use self::checkpoint_ref_tool::merry_read_checkpoint_ref_tool_name;
pub use self::config::AutomaticCompactionConfig;
use self::diagnostics::{
    APPLY_PATCH_TOOL_NAME, DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
    DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED, DIAGNOSTIC_TOOL_NOT_REGISTERED,
    TOOL_ACTION_POLICY_DENIED_MESSAGE, diagnostic_from_text, runtime_error_message,
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
    /// The session id defines the ownership boundary for artifacts, context,
    /// ledger facts, pending tool calls, and emitted runtime events.
    #[must_use]
    pub fn builder(session_id: SessionId) -> RuntimeBuilder {
        RuntimeBuilder::new(session_id)
    }

    /// Subscribes to the latest UI-only activity snapshots for managed subagents.
    #[must_use]
    pub fn subscribe_subagent_activity(&self) -> SubagentActivityReceiver {
        self.inner.activity_hub.subscribe()
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
    /// values. The runtime records session, ledger, artifact, and pending-tool
    /// state before the corresponding event becomes observable.
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
        let active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

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
        ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step)).ok_or_else(|| {
            RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            }
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.inner.session_id
    }

    fn observe_recorded_journal_events(&self, events: &[RuntimeJournalEvent]) {
        self.inner.project_journal_events(events);
    }

    pub(crate) fn record_queued_input_accepted(&self, inputs: &[QueuedInputView]) {
        self.inner.trajectory.record_queued_input_accepted(inputs);
    }

    pub(crate) fn close_trajectory(&self) {
        self.inner.trajectory.close();
    }

    /// Saves the current resume-safe session state to the provided store.
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
        let trajectory = self.inner.trajectory.snapshot();
        let bundle = {
            let mut session = self.inner.session.lock().await;
            session.set_trajectory_snapshot(trajectory);
            session.persistable_bundle()?
        };
        store.write_bundle(bundle).await?;
        Ok(())
    }

    /// Saves the current resume-safe session state to the configured store.
    pub async fn save_session(&self) -> Result<(), RuntimeError> {
        let store = match self.inner.session_store.clone() {
            Some(store) => store,
            None => FileSessionStore::default_store()?,
        };
        self.save_session_to(store).await
    }

    /// Returns a compact snapshot of managed subagent statuses when configured.
    pub async fn subagent_snapshot(&self) -> Option<Vec<crate::SubagentStatusView>> {
        match &self.inner.subagent_manager {
            Some(manager) => Some(manager.snapshot().await),
            None => None,
        }
    }

    pub(crate) fn subagent_completion_notify(&self) -> Option<Arc<Notify>> {
        self.inner
            .subagent_manager
            .as_ref()
            .map(SubagentManager::completion_notify)
    }

    pub(crate) async fn take_subagent_completion_notifications(
        &self,
    ) -> Vec<crate::SubagentStatusView> {
        match &self.inner.subagent_manager {
            Some(manager) => manager.take_completion_notifications().await,
            None => Vec::new(),
        }
    }

    pub(crate) async fn has_subagent_completion_notifications(&self) -> bool {
        match &self.inner.subagent_manager {
            Some(manager) => manager.has_completion_notifications().await,
            None => false,
        }
    }

    /// Returns the latest committed active plan snapshot.
    pub async fn plan_snapshot(
        &self,
    ) -> Result<Option<merry_core::PlanSnapshot>, PlanControllerError> {
        self.inner.plan_controller.snapshot().await
    }

    /// Activates Plan Mode through the runtime PlanController API.
    ///
    /// The provider-facing coordinator surface is `update_plan`; this Rust
    /// method is an internal runtime control entry point, not a registered
    /// provider tool.
    pub async fn begin_plan(
        &self,
        input: BeginPlanInput,
    ) -> Result<BeginPlanOutput, PlanControllerError> {
        let output = self.inner.plan_controller.begin(input).await?;
        Ok(output)
    }

    /// Activates Plan Mode from an explicit user control without changing permissions.
    pub async fn enter_plan_mode(
        &self,
        reason: &str,
    ) -> Result<BeginPlanOutput, PlanControllerError> {
        let committed = self
            .inner
            .plan_controller
            .begin_from_user(reason.to_owned())
            .await?;
        Ok(committed.output)
    }

    /// Replaces the complete planning tree or one mutable future subtree.
    pub async fn update_plan(
        &self,
        input: UpdatePlanInput,
    ) -> Result<PlanUpdateOutput, PlanControllerError> {
        let output = self.inner.plan_controller.update(input).await?;
        Ok(output)
    }

    /// Persists an attempt-scoped coordinator directive for one live subagent.
    /// This is a runtime control API and is not provider-visible.
    pub async fn control_plan_attempt(
        &self,
        input: crate::ControlPlanAttemptInput,
    ) -> Result<merry_core::CoordinatorDirectiveSnapshot, PlanControllerError> {
        Ok(self
            .inner
            .plan_controller
            .directive(input, crate::plan::unix_time_ms())
            .await?
            .output
            .directive)
    }

    /// Authorizes the current non-empty plan under an explicit capability envelope.
    pub async fn authorize_plan_execution(
        &self,
        envelope: merry_core::PlanCapabilityEnvelopeSnapshot,
        authorization_refs: Vec<String>,
    ) -> Result<merry_core::PlanSnapshot, PlanControllerError> {
        let committed = self
            .inner
            .plan_controller
            .authorize_execution(envelope, authorization_refs)
            .await?;
        Ok(committed.output)
    }

    /// Resolves the current plan's typed approval requirements and starts execution.
    pub async fn approve_plan(
        &self,
        input: crate::PlanApprovalInput,
    ) -> Result<merry_core::PlanSnapshot, PlanControllerError> {
        let committed = self.inner.plan_controller.approve(input).await?;
        Ok(committed.output.snapshot)
    }

    /// Pauses admission of new plan attempts without cancelling live subagents.
    pub async fn pause_plan_scheduling(
        &self,
        reason: &str,
    ) -> Result<merry_core::PlanSnapshot, PlanControllerError> {
        Ok(self
            .inner
            .plan_controller
            .pause_scheduling(reason.to_owned())
            .await?
            .output
            .snapshot)
    }

    /// Re-enables deterministic admission of ready plan nodes.
    pub async fn resume_plan_scheduling(
        &self,
        reason: &str,
    ) -> Result<merry_core::PlanSnapshot, PlanControllerError> {
        let committed = self
            .inner
            .plan_controller
            .resume_scheduling(reason.to_owned())
            .await?;
        Ok(committed.output.snapshot)
    }

    /// Returns an idle approved or executing plan to planning for revision.
    pub async fn revise_plan(
        &self,
        reason: &str,
    ) -> Result<merry_core::PlanSnapshot, PlanControllerError> {
        Ok(self
            .inner
            .plan_controller
            .revise(reason.to_owned())
            .await?
            .output
            .snapshot)
    }

    /// Reopens one blocked node whose latest terminal attempt was interrupted.
    pub async fn retry_interrupted_plan_node(
        &self,
        node_id: merry_core::PlanNodeId,
        reason: &str,
    ) -> Result<merry_core::PlanSnapshot, PlanControllerError> {
        let committed = self
            .inner
            .plan_controller
            .retry_interrupted_node(node_id, reason.to_owned())
            .await?;
        Ok(committed.output.snapshot)
    }

    /// Stops new attempt admission and cooperatively cancels live plan subagents.
    pub async fn cancel_plan(
        &self,
        reason: &str,
    ) -> Result<merry_core::PlanSnapshot, PlanControllerError> {
        let committed = self
            .inner
            .plan_controller
            .request_cancellation(reason.to_owned())
            .await?;
        Ok(committed.output.snapshot)
    }

    pub(crate) fn subscribe_plan_events(&self) -> PlanControllerEventReceiver {
        self.inner.plan_controller.subscribe()
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
        self.inner.progress_commentary
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
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

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

    /// Payload-free summary of the installed compacted checkpoint, if any.
    pub async fn compacted_checkpoint_summary(&self) -> Option<CompactedCheckpointSummary> {
        let session = self.inner.session.lock().await;
        session.compacted_checkpoint_summary()
    }

    /// Reads one bounded page from a checkpoint ref's original artifact evidence.
    pub async fn read_checkpoint_ref_page(
        &self,
        ref_id: &CheckpointRefId,
        offset: usize,
        max_bytes: usize,
    ) -> Result<TextEvidencePage, RuntimeError> {
        let session = self.inner.session.lock().await;
        session.read_checkpoint_ref_page(ref_id, offset, max_bytes)
    }

    /// Builds a model-facing citation compaction input for the compressible history prefix.
    pub async fn citation_compaction_input(
        &self,
        policy: CitationCompactionPolicy,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;
        compaction_input_for_policy(&self.inner, policy).await
    }

    /// Installs a validated citation compaction candidate and removes the covered history prefix.
    pub async fn install_citation_compaction_candidate(
        &self,
        input: CitationCompactionInput,
        candidate_json: &str,
    ) -> Result<CompactionOutcome, RuntimeError> {
        let active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;
        install_citation_compaction_candidate_transactionally(
            Arc::clone(&self.inner),
            input,
            candidate_json,
            CancellationToken::new(),
            active_permit,
        )
        .await
    }

    /// Runs one model-backed compaction pass when a compressible history prefix exists.
    pub async fn compact_context_once(
        &self,
        policy: CitationCompactionPolicy,
        context: StepContext,
    ) -> Result<Option<CompactionOutcome>, RuntimeError> {
        let active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        let (token, _, _) = context.into_parts();
        if token.is_cancelled() {
            return Err(RuntimeError::Compaction {
                source: CompactionError::InvalidModelResponseShape {
                    reason: "compaction cancelled before input build",
                },
            });
        }

        compact_context_once_inner(&self.inner, policy, token, active_permit).await
    }

    #[allow(dead_code)]
    pub(crate) async fn run_uncertainty_review(
        &self,
        source: &dyn JudgmentSource,
        request: JudgmentRequest,
        token: CancellationToken,
    ) -> Result<JudgmentRecord, JudgmentError> {
        if token.is_cancelled() {
            return Err(JudgmentError::Cancelled);
        }

        {
            let session = self.inner.session.lock().await;
            if token.is_cancelled() {
                return Err(JudgmentError::Cancelled);
            }
            session.preflight_judgment_request(&request)?;
        }

        if token.is_cancelled() {
            return Err(JudgmentError::Cancelled);
        }

        let context = JudgmentContext::new(token.clone());
        let outcome = tokio::select! {
            biased;
            () = token.cancelled() => {
                return Err(JudgmentError::Cancelled);
            }
            outcome = source.judge(request.clone(), context) => outcome?,
        };

        if token.is_cancelled() {
            return Err(JudgmentError::Cancelled);
        }

        let mut session = self.inner.session.lock().await;
        if token.is_cancelled() {
            return Err(JudgmentError::Cancelled);
        }
        session.record_judgment(request, outcome)
    }
}

impl Runtime {
    /// Returns the automatic compaction policy used by subsequent requests.
    pub async fn automatic_compaction_config(&self) -> AutomaticCompactionConfig {
        *self.inner.automatic_compaction.read().await
    }

    pub(crate) async fn update_interactive_primary_model(
        &self,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        retry_policy: ModelRetryPolicy,
    ) {
        *self.inner.primary_model_override.write().await =
            Some(ModelProviderConfig::new(provider, model, retry_policy));
    }

    pub(crate) async fn update_interactive_subagents(
        &self,
        enabled: bool,
        config: crate::SubagentConfig,
    ) -> Result<(), RuntimeError> {
        let manager =
            self.inner
                .subagent_manager
                .as_ref()
                .ok_or(RuntimeError::InvalidStepInput {
                    reason: "interactive subagent runtime control is unavailable",
                })?;
        manager.update_policy(enabled, config).await
    }

    pub(crate) async fn update_interactive_automatic_compaction(
        &self,
        config: AutomaticCompactionConfig,
    ) {
        *self.inner.automatic_compaction.write().await = config;
    }

    pub(crate) async fn update_interactive_context_window_tokens(
        &self,
        context_window_tokens: Option<NonZeroU64>,
    ) {
        *self.inner.context_window_tokens.write().await = context_window_tokens;
    }
}
#[cfg(test)]
mod tests {
    mod bridge_tool_flow;
    mod builder_checkpoint;
    mod checkpoint_ref_tool;
    mod compaction_transaction;
    mod context_cache;
    mod event_cancellation;
    mod memory_activation_flow;
    mod model_role_flow;
    mod permission_execution;
    mod plan_surface;
    mod process_cancellation;
    mod process_execution;
    mod process_shell_execution;
    mod provider_step_flow;
    mod provider_step_turn_lifecycle;
    mod rolling_compaction;
    mod session_resume;
    mod tool_catalog;
    mod tool_execution;
    mod tool_submit_cancellation;
    mod uncertainty_review;

    include!("runtime/tests/support.rs");
}
