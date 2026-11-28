use super::{Runtime, RuntimeError, diagnostic_from_text};
use crate::{
    ContextEntry, ContextSummary, SessionContextSnapshot, SessionTranscriptItem,
    events::ActiveStepPermit, tool_input_validation::ToolInputValidationError,
};
use merry_core::{
    ErrorInfo, PendingToolCall, RuntimeJournalEvent, TOOL_CANCELLED_BY_USER_CODE, ToolCallId,
    ToolCallResult, ToolCallResultStatus, ToolOutput, ToolOutputPreview,
};
use std::sync::atomic::Ordering;

/// Diagnostic code recorded for a tool call a settling run gave up on.
const TOOL_ABANDONED_BY_SETTLEMENT_CODE: &str = "tool_abandoned_by_run_settlement";

impl Runtime {
    /// Returns whether [`Self::close`] has closed this runtime session.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    /// Closes this runtime session, releasing any held OS-level session lock
    /// and removing an untouched empty session directory from the store.
    ///
    /// Closing is idempotent once completed. Calling `close` while an active
    /// step, run, or interactive session still owns the runtime fails with
    /// [`RuntimeError::StepAlreadyActive`] so in-flight persistence cannot
    /// continue without the session lock.
    pub async fn close(&self) -> Result<(), RuntimeError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let _active_permit = match self.acquire_active_step_permit() {
            Ok(permit) => permit,
            Err(RuntimeError::SessionClosed { .. }) => return Ok(()),
            Err(error) => return Err(error),
        };
        self.inner.closed.store(true, Ordering::Release);
        let reservation = self
            .inner
            .persistence
            .reservation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let (Some(reservation), Some(store)) =
            (reservation, self.inner.persistence.store.as_ref())
        {
            store
                .release_empty_reservation(&self.inner.session_id, reservation)
                .await;
        }
        Ok(())
    }

    /// Returns the session's frozen external definitions, independently of availability.
    pub async fn external_tool_catalog(&self) -> merry_core::SessionToolCatalog {
        self.inner
            .session
            .lock()
            .await
            .external_tool_catalog()
            .clone()
    }

    /// Resolves one pending tool call with its recorded output.
    ///
    /// The output is durably recorded in the session transcript before
    /// `ToolCallResolved` is emitted.
    ///
    /// Cancellation or executor infrastructure failures do not resolve the call;
    /// a pending tool call remains pending until this method or
    /// [`Runtime::execute_tool_call`] records a durable result.
    pub async fn submit_tool_result(
        &self,
        result: ToolCallResult,
        output: ToolOutput,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let _active_permit = self.acquire_active_step_permit()?;

        self.submit_tool_result_with_active_permit(result, output, &_active_permit)
            .await
    }

    pub(crate) async fn submit_tool_result_with_active_permit(
        &self,
        result: ToolCallResult,
        output: ToolOutput,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let events = {
            let mut session = self.inner.session.lock().await;
            session.submit_tool_result(result, output)?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    pub(crate) async fn validate_tool_execution_outcomes_with_active_permit(
        &self,
        outcomes: &[(ToolCallId, crate::ToolExecutionOutcome)],
        _active_permit: &ActiveStepPermit,
    ) -> Result<(), RuntimeError> {
        let session = self.inner.session.lock().await;
        session.validate_tool_execution_outcomes(outcomes)
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
                ToolOutput::text("Tool execution was cancelled by user interrupt."),
                Some(diagnostic),
                None,
            )?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    /// Resolves every pending tool call with a durable failed result so the
    /// session reaches a resume-safe boundary, returning how many were resolved.
    pub async fn abandon_pending_tool_calls(&self, reason: &str) -> Result<usize, RuntimeError> {
        let active_permit = self.acquire_active_step_permit()?;
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
                ToolOutput::text(format!("Tool execution was abandoned: {reason}")),
                Some(diagnostic),
                None,
            )?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    pub(crate) async fn replace_pending_tool_call_arguments_with_active_permit(
        &self,
        call_id: &ToolCallId,
        arguments: merry_core::ToolCallArguments,
        _active_permit: &ActiveStepPermit,
    ) -> Result<PendingToolCall, RuntimeError> {
        let mut session = self.inner.session.lock().await;
        session.replace_pending_tool_call_arguments(call_id, arguments)
    }

    pub(crate) async fn replace_tool_result_output_with_active_permit(
        &self,
        call_id: &ToolCallId,
        output: ToolOutput,
        _active_permit: &ActiveStepPermit,
    ) -> Result<(), RuntimeError> {
        {
            let mut session = self.inner.session.lock().await;
            session.replace_tool_result_output(call_id, output)?;
        }
        self.inner.refresh_resume_savepoint().await;
        Ok(())
    }

    pub(crate) async fn submit_hook_denied_tool_call_with_active_permit(
        &self,
        call: &PendingToolCall,
        message: &str,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let diagnostic = diagnostic_from_text("hook_tool_denied", message);
        let content = ToolOutput::json(
            serde_json::json!({
                "ok": false,
                "tool": call.name().as_str(),
                "error": {
                    "code": "hook_tool_denied",
                    "message": message,
                }
            })
            .to_string(),
        );
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

    pub(crate) async fn submit_hook_short_circuit_tool_call_with_active_permit(
        &self,
        call_id: &ToolCallId,
        output: ToolOutput,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let events = {
            let mut session = self.inner.session.lock().await;
            session.submit_tool_execution_outcome(
                call_id,
                ToolCallResultStatus::Succeeded,
                output,
                None,
                None,
            )?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    /// Durably settles all remaining pending tool calls when a hook stops the turn,
    /// keeping the session transcript valid for subsequent turns and `resume`.
    pub(crate) async fn settle_pending_tool_calls_stopped_by_hook_with_active_permit(
        &self,
        reason: &str,
        active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        let pending = {
            let session = self.inner.session.lock().await;
            session.pending_tool_calls()
        };
        let mut all_events = Vec::new();
        for call in pending {
            let diagnostic = diagnostic_from_text("hook_stopped_turn", reason);
            let mut events = {
                let mut session = self.inner.session.lock().await;
                session.submit_tool_execution_outcome(
                    call.id(),
                    ToolCallResultStatus::Failed,
                    ToolOutput::text(format!(
                        "Tool execution skipped because hook stopped turn: {reason}"
                    )),
                    Some(diagnostic),
                    None,
                )?
            };
            self.inner.commit_journal_events(&events).await;
            all_events.append(&mut events);
        }
        let _ = active_permit;
        Ok(all_events)
    }

    /// Converts an executor infrastructure failure into a durable failed
    /// result for an interactive continuation.
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
                ToolOutput::text(format!("Tool execution failed: {message}")),
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
                ToolOutput::text(format!(
                    "Structured output was invalid: {message}. Call merry_final_output again with a value matching the requested schema."
                )),
                Some(diagnostic),
                None,
            )?
        };
        self.inner.commit_journal_events(&events).await;
        Ok(events)
    }

    /// Reads the full recorded output for one resolved tool call.
    ///
    /// Checks hot in-memory session state first, then falls back to archived
    /// compacted segments on disk when a session store is attached.
    /// Returns `Ok(None)` when `call_id` has no recorded result in this session.
    pub async fn read_tool_output(
        &self,
        call_id: &ToolCallId,
    ) -> Result<Option<ToolOutput>, RuntimeError> {
        let hot = {
            let session = self.inner.session.lock().await;
            session.read_tool_output(call_id)
        };
        if hot.is_some() {
            return Ok(hot);
        }
        let Some(store) = self.inner.persistence.store.as_ref() else {
            return Ok(None);
        };
        Ok(
            crate::session::read_archived_tool_output(store, &self.inner.session_id, call_id)
                .await?,
        )
    }

    /// Reads a bounded UTF-8 preview of one resolved tool call without cloning
    /// the full output when it is still in hot memory.
    ///
    /// Falls back to archived compacted segments on disk when a session store
    /// is attached. Returns `Ok(None)` when `call_id` has no recorded result
    /// in this session.
    pub async fn read_tool_output_preview(
        &self,
        call_id: &ToolCallId,
        max_bytes: usize,
    ) -> Result<Option<ToolOutputPreview>, RuntimeError> {
        let hot = {
            let session = self.inner.session.lock().await;
            session.read_tool_output_preview(call_id, max_bytes)
        };
        if hot.is_some() {
            return Ok(hot);
        }
        let Some(store) = self.inner.persistence.store.as_ref() else {
            return Ok(None);
        };
        let archived =
            crate::session::read_archived_tool_output(store, &self.inner.session_id, call_id)
                .await?;
        Ok(archived.map(|output| output.bounded_preview(max_bytes)))
    }

    /// Records a structured context entry into the owning session.
    pub async fn record_context_entry(&self, entry: ContextEntry) -> Result<(), RuntimeError> {
        let _active_permit = self.acquire_active_step_permit()?;

        let mut session = self.inner.session.lock().await;
        session.record_context_entry(entry)?;
        Ok(())
    }

    /// Records a summary context entry into the owning session.
    pub async fn record_context_summary(
        &self,
        summary: ContextSummary,
    ) -> Result<(), RuntimeError> {
        self.record_context_entry(ContextEntry::summary(summary))
            .await
    }

    /// Builds a sealed context snapshot from session-owned context state.
    pub async fn context_snapshot(&self) -> SessionContextSnapshot {
        let session = self.inner.session.lock().await;
        session.context_snapshot()
    }

    /// Returns a snapshot of provider-neutral tool calls currently awaiting results.
    pub async fn pending_tool_calls(&self) -> Vec<PendingToolCall> {
        let session = self.inner.session.lock().await;
        session.pending_tool_calls()
    }

    /// Returns the persisted transcript as a UI/SDK-friendly read-only view.
    pub async fn session_transcript(&self) -> Result<Vec<SessionTranscriptItem>, RuntimeError> {
        let snapshots = {
            let session = self.inner.session.lock().await;
            session.full_transcript_snapshot()
        };
        Ok(snapshots
            .into_iter()
            .map(SessionTranscriptItem::from)
            .collect())
    }

    /// Lists surviving archived compaction segment numbers in ascending order
    /// (`1, 2, ...`) when a session store is attached.
    pub async fn archived_segment_numbers(&self) -> Result<Vec<u32>, RuntimeError> {
        let Some(store) = self.inner.persistence.store.as_ref() else {
            return Ok(Vec::new());
        };
        Ok(crate::session::list_archived_segment_numbers(store, &self.inner.session_id).await?)
    }

    /// Reads one archived compaction segment as UI/SDK transcript items for
    /// on-demand upward scrollback loading.
    pub async fn read_archived_segment_transcript(
        &self,
        segment_number: u32,
    ) -> Result<Vec<SessionTranscriptItem>, RuntimeError> {
        let Some(store) = self.inner.persistence.store.as_ref() else {
            return Ok(Vec::new());
        };
        Ok(crate::session::read_archived_segment_transcript(
            store,
            &self.inner.session_id,
            segment_number,
        )
        .await?)
    }
}
