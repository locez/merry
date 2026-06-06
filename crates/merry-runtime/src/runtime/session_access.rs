use super::{Runtime, RuntimeError};
use crate::{
    ArtifactContent, ContextEntry, ContextSummary, LedgerProjectionSnapshot,
    SessionContextSnapshot, event_stream::ActiveStepPermit,
    session::is_runtime_reserved_artifact_id,
};
use merry_core::{
    ArtifactId, ArtifactRef, EvidenceLocator, EvidenceRef, PendingToolCall, RuntimeEvent,
    ToolCallResult,
};
use std::sync::Arc;

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
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        if is_runtime_reserved_artifact_id(artifact.id()) {
            return Err(RuntimeError::ReservedArtifactId {
                artifact_id: artifact.id().clone(),
            });
        }

        let mut session = self.inner.session.lock().await;
        session
            .record_artifact_events(artifact, content)
            .map_err(Into::into)
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
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
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
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        if is_runtime_reserved_artifact_id(result.artifact().id()) {
            return Err(RuntimeError::ReservedArtifactId {
                artifact_id: result.artifact().id().clone(),
            });
        }

        let mut session = self.inner.session.lock().await;
        session.submit_tool_result(result, content)
    }

    pub(crate) async fn record_final_output_tool_call(
        &self,
        call: PendingToolCall,
    ) -> Result<(crate::FinalOutput, Vec<RuntimeEvent>), RuntimeError> {
        let json = serde_json::to_string(call.arguments().as_object())
            .expect("tool call arguments are JSON object values and must serialize");
        let mut session = self.inner.session.lock().await;
        session.record_final_output(call.id().clone(), json)
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

    /// Records a structured context entry into the owning session.
    ///
    /// This is the raw/manual MVP direct context mutation surface. It appends
    /// summary-only context entries today after acquiring the active-step
    /// permit. It does not validate evidence readability, reject duplicate
    /// summary ids, emit runtime events, or write ledger facts.
    ///
    /// Direct writes are validated later when a session snapshot is compiled by
    /// [`crate::ContextCompiler`]. They are not summary-draft promotion, do not
    /// record promotion lifecycle state, and are not governed by the internal
    /// summary-draft promotion acceptance/replay rules.
    pub async fn record_context_entry(&self, entry: ContextEntry) -> Result<(), RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        let mut session = self.inner.session.lock().await;
        session.record_context_entry(entry);
        Ok(())
    }

    /// Records a summary context entry into the owning session.
    ///
    /// Summaries are navigation only; exact supporting evidence must remain
    /// readable through session-owned artifacts before the summary can enter
    /// compiled context. This helper is the raw/manual MVP direct write path:
    /// it delegates to [`Runtime::record_context_entry`], so it records with
    /// the same active-step admission guard and without immediate evidence
    /// readability validation, duplicate-id rejection, runtime events, or
    /// ledger facts.
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
}
