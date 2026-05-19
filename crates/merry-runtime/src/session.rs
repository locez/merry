//! Runtime session state and state-before-event helpers.

use crate::{
    RuntimeError,
    artifact::{ArtifactContent, ArtifactError, ArtifactRegistry},
    context::{ContextEntry, SessionContextSnapshot},
    ledger::{LedgerFactKind, TaskLedger},
    memory::{ActivatedMemory, MemoryError, MemoryItem, MemoryStore},
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, EvidenceLocator, EvidenceRef,
    PendingToolCall, RuntimeEvent, RuntimeEventKind, SessionId, ToolCallId, ToolCallResult,
    ToolCallResultStatus,
};
use std::collections::BTreeSet;

const ASSISTANT_OUTPUT_ARTIFACT_PREFIX: &str = "assistant-output-";
const TOOL_RESULT_ARTIFACT_PREFIX: &str = "tool-result-";

/// Resolved tool call state that has not yet been compiled into a provider request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolContinuation {
    call: PendingToolCall,
    result: ToolCallResult,
}

impl ResolvedToolContinuation {
    fn new(call: PendingToolCall, result: ToolCallResult) -> Self {
        Self { call, result }
    }
}

/// Tool continuation data read from session state for one request compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolContinuationSnapshot {
    call: PendingToolCall,
    result: ToolCallResult,
    content: ArtifactContent,
}

impl ResolvedToolContinuationSnapshot {
    fn new(call: PendingToolCall, result: ToolCallResult, content: ArtifactContent) -> Self {
        Self {
            call,
            result,
            content,
        }
    }

    pub(crate) fn call(&self) -> &PendingToolCall {
        &self.call
    }

    pub(crate) fn result(&self) -> &ToolCallResult {
        &self.result
    }

    pub(crate) fn content(&self) -> &ArtifactContent {
        &self.content
    }
}

/// Mutable runtime state for one session.
#[derive(Debug)]
pub(crate) struct SessionState {
    session_id: SessionId,
    next_sequence: u64,
    session_started: bool,
    ledger: TaskLedger,
    artifacts: ArtifactRegistry,
    memory_store: MemoryStore,
    context_entries: Vec<ContextEntry>,
    activated_memories: Vec<ActivatedMemory>,
    pending_tool_calls: Vec<PendingToolCall>,
    resolved_tool_calls: BTreeSet<ToolCallId>,
    unconsumed_tool_continuations: Vec<ResolvedToolContinuation>,
}

impl SessionState {
    pub(crate) fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            next_sequence: 0,
            session_started: false,
            ledger: TaskLedger::default(),
            artifacts: ArtifactRegistry::default(),
            memory_store: MemoryStore::new(),
            context_entries: Vec::new(),
            activated_memories: Vec::new(),
            pending_tool_calls: Vec::new(),
            resolved_tool_calls: BTreeSet::new(),
            unconsumed_tool_continuations: Vec::new(),
        }
    }

    pub(crate) fn record_artifact_state(
        &mut self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<ArtifactRef, ArtifactError> {
        self.artifacts.record(artifact, content)
    }

    pub(crate) fn record_artifact_events(
        &mut self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeEvent>, ArtifactError> {
        let recorded = self.record_artifact_state(artifact, content)?;
        let mut events = Vec::with_capacity(if self.session_started { 1 } else { 2 });

        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        events.push(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ));

        Ok(events)
    }

    pub(crate) fn record_assistant_text_output(
        &mut self,
        text: String,
    ) -> Result<RuntimeEvent, ArtifactError> {
        let artifact_sequence = self.next_sequence();
        let artifact = ArtifactRef::new(assistant_output_id(artifact_sequence), ArtifactKind::Text);
        let recorded = self.record_artifact_state(artifact, ArtifactContent::text(text))?;
        Ok(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ))
    }

    pub(crate) fn evidence_ref(
        &self,
        artifact_id: &ArtifactId,
        locator: EvidenceLocator,
    ) -> Result<EvidenceRef, ArtifactError> {
        self.artifacts.evidence_ref(artifact_id, locator)
    }

    pub(crate) fn record_context_entry(&mut self, entry: ContextEntry) {
        self.context_entries.push(entry);
    }

    #[allow(dead_code)]
    pub(crate) fn record_memory_item(&mut self, item: MemoryItem) -> Result<(), MemoryError> {
        self.memory_store.record(item)
    }

    pub(crate) fn memory_store(&self) -> &MemoryStore {
        &self.memory_store
    }

    #[allow(dead_code)]
    pub(crate) fn record_activated_memory(&mut self, memory: ActivatedMemory) {
        self.activated_memories.push(memory);
    }

    #[allow(dead_code)]
    pub(crate) fn record_activated_memories(&mut self, memories: Vec<ActivatedMemory>) {
        self.activated_memories.extend(memories);
    }

    pub(crate) fn replace_activated_memories(&mut self, memories: Vec<ActivatedMemory>) {
        self.activated_memories = memories;
    }

    pub(crate) fn context_snapshot(&self) -> SessionContextSnapshot {
        SessionContextSnapshot::new(
            self.context_entries.clone(),
            self.artifacts.clone(),
            self.activated_memories.clone(),
        )
    }

    pub(crate) fn ledger_projection(&self) -> crate::ledger::LedgerProjectionSnapshot {
        self.ledger.project()
    }

    pub(crate) fn pending_tool_calls(&self) -> Vec<PendingToolCall> {
        self.pending_tool_calls.clone()
    }

    pub(crate) fn pending_tool_call(&self, call_id: &ToolCallId) -> Option<PendingToolCall> {
        self.pending_tool_calls
            .iter()
            .find(|call| call.id() == call_id)
            .cloned()
    }

    pub(crate) fn has_pending_tool_calls(&self) -> bool {
        !self.pending_tool_calls.is_empty()
    }

    pub(crate) fn unconsumed_tool_continuation_snapshots(
        &self,
    ) -> Result<Vec<ResolvedToolContinuationSnapshot>, ArtifactError> {
        self.unconsumed_tool_continuations
            .iter()
            .map(|continuation| {
                let content = self
                    .artifacts
                    .read_content(continuation.result.artifact().id())?
                    .clone();
                Ok(ResolvedToolContinuationSnapshot::new(
                    continuation.call.clone(),
                    continuation.result.clone(),
                    content,
                ))
            })
            .collect()
    }

    pub(crate) fn consume_tool_continuations(&mut self, count: usize) {
        let count = count.min(self.unconsumed_tool_continuations.len());
        self.unconsumed_tool_continuations.drain(..count);
    }

    #[cfg(test)]
    pub(crate) fn read_artifact_content(
        &self,
        artifact_id: &ArtifactId,
    ) -> Result<ArtifactContent, ArtifactError> {
        self.artifacts.read_content(artifact_id).cloned()
    }

    pub(crate) fn record_session_started_if_needed(&mut self) -> Option<RuntimeEvent> {
        if self.session_started {
            return None;
        }

        self.session_started = true;
        Some(self.record_event(
            RuntimeEventKind::SessionStarted,
            LedgerFactKind::SessionStarted,
        ))
    }

    pub(crate) fn record_step_started(&mut self) -> RuntimeEvent {
        self.record_event(RuntimeEventKind::StepStarted, LedgerFactKind::StepStarted)
    }

    pub(crate) fn record_step_completed(&mut self) -> RuntimeEvent {
        self.record_event(
            RuntimeEventKind::StepCompleted,
            LedgerFactKind::StepCompleted,
        )
    }

    pub(crate) fn record_tool_call_pending(
        &mut self,
        call: PendingToolCall,
    ) -> Result<RuntimeEvent, ErrorInfo> {
        if self
            .pending_tool_calls
            .iter()
            .any(|pending| pending.id() == call.id())
        {
            return Err(duplicate_tool_call_diagnostic(call.id(), "already pending"));
        }

        if self.resolved_tool_calls.contains(call.id()) {
            return Err(duplicate_tool_call_diagnostic(
                call.id(),
                "already resolved",
            ));
        }

        self.pending_tool_calls.push(call.clone());
        Ok(self.record_event(
            RuntimeEventKind::ToolCallPending { call },
            LedgerFactKind::ToolCallPending,
        ))
    }

    pub(crate) fn submit_tool_result(
        &mut self,
        result: ToolCallResult,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let Some(pending_index) = self
            .pending_tool_calls
            .iter()
            .position(|call| call.id() == result.call_id())
        else {
            return if self.resolved_tool_calls.contains(result.call_id()) {
                Err(RuntimeError::ToolCallAlreadyResolved {
                    session_id: self.session_id.clone(),
                    call_id: result.call_id().clone(),
                })
            } else {
                Err(RuntimeError::UnknownToolCall {
                    session_id: self.session_id.clone(),
                    call_id: result.call_id().clone(),
                })
            };
        };

        self.validate_tool_result_content(&result, &content)?;
        let recorded = self.record_artifact_state(result.artifact().clone(), content)?;
        debug_assert_eq!(&recorded, result.artifact());

        let pending = self.pending_tool_calls.remove(pending_index);
        self.resolved_tool_calls.insert(result.call_id().clone());
        self.unconsumed_tool_continuations
            .push(ResolvedToolContinuation::new(pending, result.clone()));

        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        events.push(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ));
        events.push(self.record_event(
            RuntimeEventKind::ToolCallResolved { result },
            LedgerFactKind::ToolCallResolved,
        ));

        Ok(events)
    }

    pub(crate) fn submit_tool_execution_outcome(
        &mut self,
        call_id: &ToolCallId,
        status: ToolCallResultStatus,
        content: ArtifactContent,
        diagnostic: Option<ErrorInfo>,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let artifact_kind = match &content {
            ArtifactContent::Text(_) => ArtifactKind::Text,
            ArtifactContent::Json(_) => ArtifactKind::Json,
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                return Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: self.next_tool_result_artifact_id(),
                    content_kind: content.kind(),
                });
            }
        };
        let artifact = ArtifactRef::new(self.next_tool_result_artifact_id(), artifact_kind);
        let result = match status {
            ToolCallResultStatus::Succeeded => ToolCallResult::new(
                call_id.clone(),
                ToolCallResultStatus::Succeeded,
                artifact,
                diagnostic,
            )?,
            ToolCallResultStatus::Failed => {
                let diagnostic = diagnostic.ok_or(RuntimeError::Core {
                    source: merry_core::CoreError::InvalidToolCallResult {
                        reason: "failed tool execution outcome must include a diagnostic",
                    },
                })?;
                ToolCallResult::failed(call_id.clone(), artifact, diagnostic)
            }
        };

        self.submit_tool_result(result, content)
    }

    pub(crate) fn record_cancelled(&mut self, diagnostic: ErrorInfo) -> RuntimeEvent {
        self.record_event(
            RuntimeEventKind::Cancelled { diagnostic },
            LedgerFactKind::Cancelled,
        )
    }

    pub(crate) fn record_failed(&mut self, diagnostic: ErrorInfo) -> RuntimeEvent {
        self.record_event(
            RuntimeEventKind::Failed { diagnostic },
            LedgerFactKind::Failed,
        )
    }

    fn record_event(&mut self, kind: RuntimeEventKind, fact_kind: LedgerFactKind) -> RuntimeEvent {
        let sequence = self.next_sequence;
        self.ledger.record(sequence, fact_kind);
        self.next_sequence += 1;
        RuntimeEvent::new(self.session_id.clone(), sequence, kind)
    }

    fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    fn next_tool_result_artifact_id(&self) -> ArtifactId {
        tool_result_id(self.next_sequence())
    }

    fn validate_tool_result_content(
        &self,
        result: &ToolCallResult,
        content: &ArtifactContent,
    ) -> Result<(), RuntimeError> {
        let supported = matches!(content, ArtifactContent::Text(_) | ArtifactContent::Json(_));
        let compatible = matches!(
            (result.artifact().kind(), content),
            (ArtifactKind::Text, ArtifactContent::Text(_))
                | (ArtifactKind::Json, ArtifactContent::Json(_))
        );

        if !supported {
            return Err(RuntimeError::UnsupportedToolResultContent {
                artifact_id: result.artifact().id().clone(),
                content_kind: content.kind(),
            });
        }

        if !compatible {
            return Err(ArtifactError::IncompatibleContent {
                id: result.artifact().id().clone(),
                artifact_kind: result.artifact().kind().clone(),
                content_kind: content.kind(),
            }
            .into());
        }

        match content {
            ArtifactContent::Text(text) | ArtifactContent::Json(text) if text.trim().is_empty() => {
                Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: result.artifact().id().clone(),
                    content_kind: content.kind(),
                })
            }
            ArtifactContent::Text(_) | ArtifactContent::Json(_) => Ok(()),
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                unreachable!("unsupported tool result content is rejected before blank validation")
            }
        }
    }
}

pub(crate) fn is_runtime_reserved_artifact_id(artifact_id: &ArtifactId) -> bool {
    artifact_id
        .as_str()
        .starts_with(ASSISTANT_OUTPUT_ARTIFACT_PREFIX)
        || artifact_id
            .as_str()
            .starts_with(TOOL_RESULT_ARTIFACT_PREFIX)
}

fn assistant_output_id(sequence: u64) -> ArtifactId {
    ArtifactId::new(&format!("{ASSISTANT_OUTPUT_ARTIFACT_PREFIX}{sequence}"))
        .expect("assistant output artifact id uses a valid static prefix and sequence")
}

fn tool_result_id(sequence: u64) -> ArtifactId {
    ArtifactId::new(&format!("{TOOL_RESULT_ARTIFACT_PREFIX}{sequence}"))
        .expect("tool result artifact id uses a valid static prefix and sequence")
}

fn duplicate_tool_call_diagnostic(call_id: &ToolCallId, state: &'static str) -> ErrorInfo {
    ErrorInfo::new(
        "tool_call_duplicate",
        &format!("tool call {call_id} is {state}; duplicate pending admission rejected"),
    )
    .expect("duplicate tool call diagnostic uses static code and validated call id")
}

#[cfg(test)]
mod tests {
    use super::SessionState;
    use crate::{
        artifact::ArtifactContent,
        context::ContextCompiler,
        memory::{
            ActivatedMemory, MemoryActivationProvenance, MemoryActivationReason,
            MemoryActivationScore, MemoryActivationSourceKind, MemoryEvidence, MemoryId,
            MemoryItem, MemoryItemSelection, MemoryScope,
        },
    };
    use merry_core::{
        ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, PendingToolCall,
        RuntimeEventKind, SessionId, ToolCallArguments, ToolCallId, ToolCallResult, ToolName,
    };
    use serde_json::json;

    fn session_id() -> SessionId {
        SessionId::new("session-state-test").expect("valid session id")
    }

    fn artifact_id(value: &str) -> ArtifactId {
        ArtifactId::new(value).expect("valid artifact id")
    }

    fn tool_call_id(value: &str) -> ToolCallId {
        ToolCallId::new(value).expect("valid tool call id")
    }

    fn pending_tool_call(id: &str) -> PendingToolCall {
        PendingToolCall::new(
            tool_call_id(id),
            ToolName::new("lookup").expect("valid tool name"),
            ToolCallArguments::try_from(json!({ "query": "value" }))
                .expect("object arguments are valid"),
        )
    }

    fn memory_id(value: &str) -> MemoryId {
        MemoryId::new(value).expect("valid memory id")
    }

    fn activated_memory(id: &str) -> ActivatedMemory {
        activated_memory_with_details(id, format!("{id} text"), 1, 0, 0.5)
    }

    fn activated_memory_with_details(
        id: &str,
        text: impl Into<String>,
        matches: usize,
        priority: i32,
        confidence: f32,
    ) -> ActivatedMemory {
        let item = MemoryItem::new(
            memory_id(id),
            MemoryScope::Session,
            text,
            vec![memory_evidence("primary source", &format!("{id}-artifact"))],
            MemoryItemSelection::new(vec!["topic".to_owned()], confidence, priority, None)
                .expect("valid memory selection"),
        )
        .expect("valid memory item");
        let score =
            MemoryActivationScore::new(matches, priority, confidence).expect("valid memory score");
        ActivatedMemory::new(
            item,
            score,
            vec![
                MemoryActivationReason::ScopeAllowed,
                MemoryActivationReason::trigger_matched("topic").expect("valid trigger"),
                MemoryActivationReason::ranked(score),
            ],
            provenance(),
        )
        .expect("valid activated memory")
    }

    fn provenance() -> MemoryActivationProvenance {
        MemoryActivationProvenance::new(
            "topic",
            vec![MemoryScope::Session, MemoryScope::Task, MemoryScope::Step],
            MemoryActivationSourceKind::UserQuery,
            "user request",
        )
        .expect("valid memory provenance")
    }

    fn memory_evidence(label: &str, artifact: &str) -> MemoryEvidence {
        MemoryEvidence::new(
            label,
            EvidenceRef::new(artifact_id(artifact), EvidenceLocator::whole_artifact()),
        )
        .expect("valid memory evidence")
    }

    fn record_memory_artifacts(session: &mut SessionState, memories: &[&ActivatedMemory]) {
        let mut seen = std::collections::BTreeSet::new();

        for memory in memories {
            for evidence in memory.item().evidence() {
                if !seen.insert(evidence.reference().artifact_id.clone()) {
                    continue;
                }

                session
                    .record_artifact_state(
                        ArtifactRef::new(
                            evidence.reference().artifact_id.clone(),
                            ArtifactKind::Text,
                        ),
                        ArtifactContent::text(format!(
                            "evidence for {}\n{}",
                            memory.item().id(),
                            memory.item().text()
                        )),
                    )
                    .expect("memory artifact records");
            }
        }
    }

    #[test]
    fn session_start_is_recorded_once_before_step_lifecycle() {
        let mut session = SessionState::new(session_id());

        let first = session
            .record_session_started_if_needed()
            .expect("first start should emit");
        let second = session.record_session_started_if_needed();
        let started = session.record_step_started();
        let completed = session.record_step_completed();

        assert!(matches!(first.kind, RuntimeEventKind::SessionStarted));
        assert!(second.is_none());
        assert_eq!(started.sequence, 1);
        assert_eq!(completed.sequence, 2);
    }

    #[test]
    fn assistant_output_artifact_id_uses_artifact_event_sequence() {
        let mut session = SessionState::new(session_id());
        let _started = session
            .record_session_started_if_needed()
            .expect("start should emit");
        let _step_started = session.record_step_started();

        let artifact = session
            .record_assistant_text_output("hello".to_owned())
            .expect("assistant output should record");
        let completed = session.record_step_completed();

        match artifact.kind {
            RuntimeEventKind::ArtifactRecorded { artifact } => {
                assert_eq!(artifact.id().as_str(), "assistant-output-2");
                assert_eq!(artifact.kind(), &ArtifactKind::Text);
            }
            other => panic!("expected artifact event, got {other:?}"),
        }
        assert_eq!(artifact.sequence, 2);
        assert_eq!(completed.sequence, 3);
    }

    #[test]
    fn submit_tool_result_stores_exact_content_before_resolved_event() {
        let mut session = SessionState::new(session_id());
        let call = pending_tool_call("call-1");
        session
            .record_tool_call_pending(call.clone())
            .expect("pending call should record");
        let artifact = ArtifactRef::new(artifact_id("tool-result-exact"), ArtifactKind::Text);
        let result = ToolCallResult::succeeded(call.id().clone(), artifact.clone());

        let events = session
            .submit_tool_result(result.clone(), ArtifactContent::text("exact result\n"))
            .expect("tool result should submit");

        assert_eq!(
            session
                .read_artifact_content(artifact.id())
                .expect("recorded content should be readable"),
            ArtifactContent::text("exact result\n")
        );
        assert!(matches!(
            events.last().map(|event| &event.kind),
            Some(RuntimeEventKind::ToolCallResolved { result: resolved }) if resolved == &result
        ));
    }

    #[test]
    fn submit_tool_result_starts_session_before_artifact_and_resolution_when_needed() {
        let mut session = SessionState::new(session_id());
        session
            .pending_tool_calls
            .push(pending_tool_call("pre-start-call"));
        let artifact = ArtifactRef::new(artifact_id("pre-start-result"), ArtifactKind::Json);
        let result = ToolCallResult::succeeded(tool_call_id("pre-start-call"), artifact.clone());

        let events = session
            .submit_tool_result(result.clone(), ArtifactContent::json(r#"{"ok":true}"#))
            .expect("tool result should submit");

        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(matches!(events[0].kind, RuntimeEventKind::SessionStarted));
        assert!(matches!(
            &events[1].kind,
            RuntimeEventKind::ArtifactRecorded { artifact: recorded } if recorded == &artifact
        ));
        assert!(matches!(
            &events[2].kind,
            RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == &result
        ));
    }

    #[test]
    fn duplicate_tool_call_pending_is_rejected_without_second_pending_or_sequence() {
        let mut session = SessionState::new(session_id());
        let call = pending_tool_call("duplicate-call");
        let first = session
            .record_tool_call_pending(call.clone())
            .expect("first pending call should record");

        let err = session
            .record_tool_call_pending(call.clone())
            .expect_err("duplicate pending call id should be rejected");

        assert_eq!(err.code(), "tool_call_duplicate");
        assert_eq!(session.pending_tool_calls(), vec![call]);
        assert_eq!(first.sequence, 0);
        assert_eq!(
            session
                .ledger_projection()
                .entries()
                .iter()
                .map(|entry| match entry {
                    crate::ledger::LedgerProjection::Lifecycle { sequence, .. } => *sequence,
                    crate::ledger::LedgerProjection::Fact { sequence, .. } => *sequence,
                })
                .collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[test]
    fn context_snapshot_is_independent_from_later_recorded_memory() {
        let mut session = SessionState::new(session_id());

        let stale = session.context_snapshot();
        let memory = activated_memory("memory-later");
        session.record_activated_memory(memory.clone());
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("memory-later-artifact"), ArtifactKind::Text),
                ArtifactContent::text("first exact memory evidence\n"),
            )
            .expect("memory artifact records");
        let current = session.context_snapshot();
        let second_memory =
            activated_memory_with_details("memory-later-2", "later text", 1, 2, 0.5);
        session.record_activated_memory(second_memory);
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("memory-later-2-artifact"), ArtifactKind::Text),
                ArtifactContent::text("second exact memory evidence\n"),
            )
            .expect("later memory artifact records");

        let stale = ContextCompiler::new()
            .compile(&stale)
            .expect("stale snapshot compiles");
        let current = ContextCompiler::new()
            .compile(&current)
            .expect("current snapshot compiles");

        assert_eq!(stale.to_snapshot(), "");
        assert!(current.to_snapshot().contains("memory:memory-later"));
        assert!(
            current
                .to_snapshot()
                .contains("memory-evidence:primary source:memory-later-artifact:whole")
        );
        assert!(
            current
                .to_snapshot()
                .contains("memory-activation-source-label:user request")
        );
        assert!(!current.to_snapshot().contains("memory-later-2"));
    }

    #[test]
    fn record_activated_memories_appends_to_memory_projection() {
        let mut session = SessionState::new(session_id());
        let memory_a = activated_memory("memory-a");
        let memory_b = activated_memory("memory-b");
        record_memory_artifacts(&mut session, &[&memory_a, &memory_b]);
        session.record_activated_memories(vec![memory_a, memory_b]);

        let compiled = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("snapshot compiles");

        assert!(compiled.to_snapshot().contains("memory:memory-a"));
        assert!(compiled.to_snapshot().contains("memory:memory-b"));
    }

    #[test]
    fn replace_activated_memories_updates_current_memory_projection() {
        let mut session = SessionState::new(session_id());
        let stale = activated_memory("memory-stale");
        let current = activated_memory("memory-current");
        record_memory_artifacts(&mut session, &[&stale, &current]);

        session.record_activated_memory(stale);
        session.replace_activated_memories(vec![current]);

        let compiled = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("snapshot compiles");

        assert!(!compiled.to_snapshot().contains("memory:memory-stale"));
        assert!(compiled.to_snapshot().contains("memory:memory-current"));
    }

    #[test]
    fn replace_activated_memories_with_empty_clears_current_memory_projection() {
        let mut session = SessionState::new(session_id());
        let memory = activated_memory("memory-cleared");
        record_memory_artifacts(&mut session, &[&memory]);

        session.record_activated_memory(memory);
        session.replace_activated_memories(Vec::new());

        let compiled = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("snapshot compiles");

        assert_eq!(compiled.to_snapshot(), "");
    }

    #[test]
    fn duplicate_recorded_activated_memories_compile_once_deterministically() {
        let lower_duplicate =
            activated_memory_with_details("memory-duplicate", "Lower duplicate.", 1, 0, 0.5);
        let higher_duplicate =
            activated_memory_with_details("memory-duplicate", "Higher duplicate.", 2, 0, 0.5);

        let mut first = SessionState::new(session_id());
        record_memory_artifacts(&mut first, &[&lower_duplicate, &higher_duplicate]);
        first.record_activated_memories(vec![lower_duplicate.clone(), higher_duplicate.clone()]);
        let first = ContextCompiler::new()
            .compile(&first.context_snapshot())
            .expect("first snapshot compiles")
            .to_snapshot();

        let mut second = SessionState::new(session_id());
        record_memory_artifacts(&mut second, &[&higher_duplicate, &lower_duplicate]);
        second.record_activated_memories(vec![higher_duplicate, lower_duplicate]);
        let second = ContextCompiler::new()
            .compile(&second.context_snapshot())
            .expect("second snapshot compiles")
            .to_snapshot();

        assert_eq!(first, second);
        assert_eq!(first.matches("memory:memory-duplicate").count(), 1);
        assert!(first.contains("memory-text:Higher duplicate."));
        assert!(!first.contains("memory-text:Lower duplicate."));
    }
}
