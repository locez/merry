use super::{Runtime, RuntimeError, diagnostic_from_text};
use crate::{
    ArtifactContent, ArtifactContentPreview, ContextEntry, ContextSummary,
    LedgerProjectionSnapshot, SessionContextSnapshot, SessionTranscriptItem,
    events::ActiveStepPermit, session::is_runtime_reserved_artifact_id,
    tool_input_validation::ToolInputValidationError,
};
use merry_core::{
    ArtifactId, ArtifactRef, ErrorInfo, EvidenceLocator, EvidenceRef, PendingToolCall,
    RuntimeJournalEvent, TOOL_CANCELLED_BY_USER_CODE, ToolCallId, ToolCallResult,
    ToolCallResultStatus,
};
use std::sync::Arc;

/// Diagnostic code recorded for a tool call a settling run gave up on.
const TOOL_ABANDONED_BY_SETTLEMENT_CODE: &str = "tool_abandoned_by_run_settlement";

impl Runtime {
    /// Records exact artifact state into the owning session and returns observable events.
    ///
    /// When this is the first observable action in the session, `SessionStarted`
    /// is returned before `ArtifactRecorded`.
    ///
    /// This direct mutation path acquires the active-step permit and therefore
    /// cannot run concurrently with [`Runtime::step`],
    /// [`Runtime::submit_tool_result`], or [`Runtime::execute_tool_call`]. State
    /// is written before returned events are handed to the caller.
    ///
    /// Artifact ids with runtime-reserved prefixes are rejected. Runtime-owned
    /// ids are used for internally generated artifacts such as assistant output
    /// and registered tool execution results.
    pub async fn record_artifact(
        &self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        if is_runtime_reserved_artifact_id(artifact.id()) {
            return Err(RuntimeError::ReservedArtifactId {
                artifact_id: artifact.id().clone(),
            });
        }

        let events = {
            let mut session = self.inner.session.lock().await;
            session
                .record_artifact_events(artifact, content)
                .map_err(RuntimeError::from)?
        };
        self.observe_recorded_journal_events(&events);
        Ok(events)
    }

    /// Resolves one pending tool call with an artifact-backed result.
    ///
    /// The artifact content is durably recorded before `ToolCallResolved` is
    /// emitted. The event carries only the artifact reference, not the payload.
    ///
    /// This is the manual result path for external tool runners. Callers choose
    /// the artifact id and must not use runtime-reserved artifact ids. The
    /// registered executor path is [`Runtime::execute_tool_call`], where runtime
    /// code owns the generated artifact id and result envelope.
    ///
    /// Cancellation or executor infrastructure failures do not resolve the call;
    /// a pending tool call remains pending until this method or
    /// [`Runtime::execute_tool_call`] records a durable result.
    pub async fn submit_tool_result(
        &self,
        result: ToolCallResult,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        self.submit_tool_result_with_active_permit(result, content, &_active_permit)
            .await
    }

    pub(crate) async fn submit_tool_result_with_active_permit(
        &self,
        result: ToolCallResult,
        content: ArtifactContent,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        if is_runtime_reserved_artifact_id(result.artifact().id()) {
            return Err(RuntimeError::ReservedArtifactId {
                artifact_id: result.artifact().id().clone(),
            });
        }

        let events = {
            let mut session = self.inner.session.lock().await;
            session.submit_tool_result(result, content)?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    pub(crate) async fn submit_tool_execution_outcomes_with_active_permit(
        &self,
        outcomes: Vec<(ToolCallId, crate::ToolExecutionOutcome)>,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let events = {
            let mut session = self.inner.session.lock().await;
            session.submit_tool_execution_outcomes(outcomes)?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    pub(crate) async fn record_final_output_tool_call(
        &self,
        call: PendingToolCall,
    ) -> Result<(crate::FinalOutput, Vec<RuntimeJournalEvent>), RuntimeError> {
        let json = serde_json::to_string(call.arguments().as_object())
            .expect("tool call arguments are JSON object values and must serialize");
        let output = {
            let mut session = self.inner.session.lock().await;
            session.record_final_output(call.id().clone(), json)?
        };
        self.inner.commit_journal_events(&output.1).await;
        Ok(output)
    }

    pub(crate) async fn submit_tool_input_validation_failure_with_active_permit(
        &self,
        call: &PendingToolCall,
        error: ToolInputValidationError,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let content = error.content_for_call(call);
        let diagnostic = error.diagnostic();
        let events = {
            let mut session = self.inner.session.lock().await;
            session.submit_tool_execution_outcome(
                call.id(),
                ToolCallResultStatus::Failed,
                content,
                Some(diagnostic),
                None,
            )?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    pub(crate) async fn submit_tool_interrupt_failure_with_active_permit(
        &self,
        call_id: &ToolCallId,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let diagnostic = ErrorInfo::new(
            TOOL_CANCELLED_BY_USER_CODE,
            &format!("tool call {call_id} was cancelled by user interrupt"),
        )
        .expect("static diagnostic code and runtime-owned call id are valid");
        let events = {
            let mut session = self.inner.session.lock().await;
            session.submit_tool_execution_outcome(
                call_id,
                ToolCallResultStatus::Failed,
                ArtifactContent::text("Tool execution was cancelled by user interrupt."),
                Some(diagnostic),
                None,
            )?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    /// Resolves every pending tool call with a durable failed result so the
    /// session reaches a resume-safe boundary, returning how many were resolved.
    ///
    /// A run that settles while a tool call is still pending cannot be saved at
    /// all: [`crate::SessionStoreError::UnsafePendingToolCalls`] rejects the
    /// bundle, so the partial session is lost instead of persisted. Owners of
    /// run settlement call this before saving to record why each call never
    /// produced a result, which is what keeps a failed run resumable.
    ///
    /// This acquires the active-step permit and therefore cannot run while a
    /// step, tool submission, or tool execution is in flight.
    pub async fn abandon_pending_tool_calls(&self, reason: &str) -> Result<usize, RuntimeError> {
        let active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;
        let pending = {
            let session = self.inner.session.lock().await;
            session.pending_tool_calls()
        };
        for call in &pending {
            self.submit_tool_abandoned_failure_with_active_permit(
                call.id(),
                reason,
                &active_permit,
            )
            .await?;
        }
        Ok(pending.len())
    }

    pub(crate) async fn submit_tool_abandoned_failure_with_active_permit(
        &self,
        call_id: &ToolCallId,
        reason: &str,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let diagnostic = diagnostic_from_text(TOOL_ABANDONED_BY_SETTLEMENT_CODE, reason);
        let events = {
            let mut session = self.inner.session.lock().await;
            session.submit_tool_execution_outcome(
                call_id,
                ToolCallResultStatus::Failed,
                ArtifactContent::text(format!("Tool execution was abandoned: {reason}")),
                Some(diagnostic),
                None,
            )?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    /// Converts an executor infrastructure failure into a durable failed
    /// result for an interactive continuation. Direct executor callers keep
    /// the lower-level contract of leaving infrastructure failures pending.
    pub(crate) async fn submit_tool_execution_failure_with_active_permit(
        &self,
        call_id: &ToolCallId,
        message: &str,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let diagnostic = diagnostic_from_text("tool_execution_failed", message);
        let events = {
            let mut session = self.inner.session.lock().await;
            session.submit_tool_execution_outcome(
                call_id,
                ToolCallResultStatus::Failed,
                ArtifactContent::text(format!("Tool execution failed: {message}")),
                Some(diagnostic),
                None,
            )?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    pub(crate) async fn submit_structured_output_failure_with_active_permit(
        &self,
        call_id: &ToolCallId,
        message: &str,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let diagnostic = diagnostic_from_text("structured_output_invalid", message);
        let events = {
            let mut session = self.inner.session.lock().await;
            session.submit_tool_execution_outcome(
                call_id,
                ToolCallResultStatus::Failed,
                ArtifactContent::text(format!(
                    "Structured output was invalid: {message}. Call merry_final_output again with a value matching the requested schema."
                )),
                Some(diagnostic),
                None,
            )?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    /// Creates an exact evidence reference from artifact state owned by this session.
    ///
    /// Prefer this facade over reading [`crate::ArtifactRegistry`] directly. The
    /// returned reference is valid only for artifact content already owned by
    /// this runtime session.
    pub async fn evidence_ref(
        &self,
        artifact_id: &ArtifactId,
        locator: EvidenceLocator,
    ) -> Result<EvidenceRef, RuntimeError> {
        let session = self.inner.session.lock().await;
        session
            .evidence_ref(artifact_id, locator)
            .map_err(Into::into)
    }

    /// Reads exact artifact content already owned by this runtime session.
    ///
    /// This is an inspection facade over session-owned artifact state. It does
    /// not mutate runtime state, advance event sequence, or expose provider
    /// wire formats.
    pub async fn read_artifact_content(
        &self,
        artifact_id: &ArtifactId,
    ) -> Result<ArtifactContent, RuntimeError> {
        let session = self.inner.session.lock().await;
        session
            .read_artifact_content(artifact_id)
            .map_err(Into::into)
    }

    /// Reads bounded artifact inspection data without cloning the full payload.
    pub async fn read_artifact_preview(
        &self,
        artifact_id: &ArtifactId,
        max_bytes: usize,
    ) -> Result<ArtifactContentPreview, RuntimeError> {
        let session = self.inner.session.lock().await;
        session
            .read_artifact_preview(artifact_id, max_bytes)
            .map_err(Into::into)
    }

    /// Records a structured context entry into the owning session.
    ///
    /// This is the raw/manual MVP direct context mutation surface. It appends
    /// summary-only context entries today after acquiring the active-step
    /// permit. It validates evidence readability and duplicate summary ids, but
    /// does not emit runtime events or write ledger facts.
    ///
    /// Direct writes are not summary-draft promotion, do not record promotion
    /// lifecycle state, and are not governed by the internal summary-draft
    /// promotion acceptance/replay rules.
    pub async fn record_context_entry(&self, entry: ContextEntry) -> Result<(), RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        let mut session = self.inner.session.lock().await;
        session.record_context_entry(entry)?;
        Ok(())
    }

    /// Records a summary context entry into the owning session.
    ///
    /// Summaries are navigation only; exact supporting evidence must remain
    /// readable through session-owned artifacts before the summary can enter
    /// compiled context. This helper is the raw/manual MVP direct write path:
    /// it delegates to [`Runtime::record_context_entry`], so it records with
    /// the same active-step admission guard, immediate evidence readability
    /// validation, duplicate-id rejection, and without runtime events or ledger
    /// facts.
    ///
    /// This API is independent of the internal summary-draft promotion
    /// lifecycle. Calling it does not create promotion records, perform
    /// acceptance/replay checks, or authorize context mutation from judgment
    /// output.
    pub async fn record_context_summary(
        &self,
        summary: ContextSummary,
    ) -> Result<(), RuntimeError> {
        self.record_context_entry(ContextEntry::summary(summary))
            .await
    }

    /// Builds a sealed context snapshot from session-owned context and artifacts.
    ///
    /// The snapshot is opaque and session-owned. It exists so
    /// [`crate::ContextCompiler`] can validate summaries against the matching
    /// artifact view without accepting arbitrary caller-assembled state.
    pub async fn context_snapshot(&self) -> SessionContextSnapshot {
        let session = self.inner.session.lock().await;
        session.context_snapshot()
    }

    /// Builds a read-only deterministic projection of the task ledger.
    ///
    /// This is the preferred public read path for lifecycle and compact ledger
    /// facts. Direct [`crate::TaskLedger`] access is a low-level in-memory MVP
    /// primitive and should not be treated as the stable application-facing
    /// ledger API.
    pub async fn ledger_projection(&self) -> LedgerProjectionSnapshot {
        let session = self.inner.session.lock().await;
        session.ledger_projection()
    }

    /// Returns a snapshot of provider-neutral tool calls currently awaiting results.
    ///
    /// The returned calls are normalized Merry runtime state, not provider wire
    /// payloads. A call remains listed until a durable result is submitted or
    /// executed through a registered executor.
    pub async fn pending_tool_calls(&self) -> Vec<PendingToolCall> {
        let session = self.inner.session.lock().await;
        session.pending_tool_calls()
    }

    /// Returns the persisted transcript as a UI/SDK-friendly read-only view.
    pub async fn session_transcript(&self) -> Result<Vec<SessionTranscriptItem>, RuntimeError> {
        let snapshots = {
            let session = self.inner.session.lock().await;
            session.full_transcript_snapshot()?
        };
        Ok(snapshots
            .into_iter()
            .map(SessionTranscriptItem::from)
            .collect())
    }
}
