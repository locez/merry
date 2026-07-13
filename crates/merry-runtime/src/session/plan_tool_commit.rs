use super::{SessionState, transcript::ToolResultPromptProjection};
use crate::{
    RuntimeError,
    artifact::{ArtifactContent, ArtifactRegistry},
    ledger::{LedgerFactKind, TaskLedger},
    plan::PlanState,
};
use merry_core::{
    ArtifactRef, ErrorInfo, PendingToolCall, PlanSnapshot, RuntimeJournalEvent,
    RuntimeJournalPayload, ToolCallId, ToolCallResult, ToolCallResultStatus,
};
use std::collections::BTreeSet;

pub(crate) struct PreparedPlanToolCommit {
    next_sequence: u64,
    session_started: bool,
    ledger: TaskLedger,
    artifacts: ArtifactRegistry,
    active_plan: PlanState,
    terminal_plans: Vec<PlanSnapshot>,
    transcript: super::Transcript,
    pending_tool_calls: Vec<PendingToolCall>,
    resolved_tool_calls: BTreeSet<ToolCallId>,
    recorded_artifact: ArtifactRef,
    recorded_content_bytes: usize,
    events: Vec<RuntimeJournalEvent>,
}

impl PreparedPlanToolCommit {
    pub(crate) fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub(crate) fn session_started(&self) -> bool {
        self.session_started
    }

    pub(crate) fn ledger(&self) -> &TaskLedger {
        &self.ledger
    }

    pub(crate) fn artifacts(&self) -> &ArtifactRegistry {
        &self.artifacts
    }

    pub(crate) fn active_plan(&self) -> &PlanState {
        &self.active_plan
    }

    pub(crate) fn terminal_plans(&self) -> &[PlanSnapshot] {
        &self.terminal_plans
    }

    pub(crate) fn transcript(&self) -> &super::Transcript {
        &self.transcript
    }

    pub(crate) fn pending_tool_calls(&self) -> &[PendingToolCall] {
        &self.pending_tool_calls
    }

    pub(crate) fn resolved_tool_calls(&self) -> &BTreeSet<ToolCallId> {
        &self.resolved_tool_calls
    }

    pub(crate) fn events(&self) -> &[RuntimeJournalEvent] {
        &self.events
    }
}

impl SessionState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_plan_tool_commit(
        &self,
        active_plan: PlanState,
        terminal_plans: Vec<PlanSnapshot>,
        plan_payloads: Vec<RuntimeJournalPayload>,
        call_id: &ToolCallId,
        status: ToolCallResultStatus,
        content: ArtifactContent,
        diagnostic: Option<ErrorInfo>,
    ) -> Result<PreparedPlanToolCommit, RuntimeError> {
        let pending_index = self
            .pending_tool_calls
            .iter()
            .position(|call| call.id() == call_id)
            .ok_or_else(|| {
                if self.resolved_tool_calls.contains(call_id) {
                    RuntimeError::ToolCallAlreadyResolved {
                        session_id: self.session_id.clone(),
                        call_id: call_id.clone(),
                    }
                } else {
                    RuntimeError::UnknownToolCall {
                        session_id: self.session_id.clone(),
                        call_id: call_id.clone(),
                    }
                }
            })?;
        self.transcript
            .model_turn_id_for_tool_call(call_id)
            .ok_or_else(|| RuntimeError::TranscriptToolCallMissing {
                call_id: call_id.clone(),
            })?;

        let mut next_sequence = self.next_sequence;
        let mut session_started = self.session_started;
        let mut ledger = self.ledger.clone();
        let mut events = Vec::with_capacity(plan_payloads.len() + 3);
        if !session_started {
            session_started = true;
            events.push(record_candidate_event(
                &self.session_id,
                &mut next_sequence,
                &mut ledger,
                RuntimeJournalPayload::SessionStarted,
                Some(LedgerFactKind::SessionStarted),
            ));
        }
        for payload in plan_payloads {
            events.push(record_candidate_event(
                &self.session_id,
                &mut next_sequence,
                &mut ledger,
                payload,
                None,
            ));
        }

        let artifact_kind = self.tool_result_artifact_kind(&content)?;
        let artifact = ArtifactRef::new(
            super::artifacts::tool_result_id(next_sequence),
            artifact_kind,
        );
        let result = match status {
            ToolCallResultStatus::Succeeded => {
                ToolCallResult::new(call_id.clone(), status, artifact.clone(), diagnostic)?
            }
            ToolCallResultStatus::Failed => {
                let diagnostic = diagnostic.ok_or(RuntimeError::Core {
                    source: merry_core::CoreError::InvalidToolCallResult {
                        reason: "failed plan tool outcome must include a diagnostic",
                    },
                })?;
                ToolCallResult::failed(call_id.clone(), artifact.clone(), diagnostic)
            }
        };

        let mut artifacts = self.artifacts.clone();
        artifacts.ensure_recordable(&artifact, &content)?;
        let content_bytes = content.as_bytes().len();
        let recorded_artifact = artifacts.record_preflighted(artifact, content);
        let mut transcript = self.transcript.clone();
        transcript.push_tool_result(
            call_id.clone(),
            result.clone(),
            recorded_artifact.id().clone(),
            ToolResultPromptProjection::Full,
        )?;
        let mut pending_tool_calls = self.pending_tool_calls.clone();
        pending_tool_calls.remove(pending_index);
        let mut resolved_tool_calls = self.resolved_tool_calls.clone();
        resolved_tool_calls.insert(call_id.clone());

        events.push(record_candidate_event(
            &self.session_id,
            &mut next_sequence,
            &mut ledger,
            RuntimeJournalPayload::ArtifactRecorded {
                artifact: recorded_artifact.clone(),
            },
            Some(LedgerFactKind::ArtifactRecorded),
        ));
        events.push(record_candidate_event(
            &self.session_id,
            &mut next_sequence,
            &mut ledger,
            RuntimeJournalPayload::ToolCallResolved { result },
            Some(LedgerFactKind::ToolCallResolved),
        ));

        Ok(PreparedPlanToolCommit {
            next_sequence,
            session_started,
            ledger,
            artifacts,
            active_plan,
            terminal_plans,
            transcript,
            pending_tool_calls,
            resolved_tool_calls,
            recorded_artifact,
            recorded_content_bytes: content_bytes,
            events,
        })
    }

    pub(crate) fn install_plan_tool_commit(&mut self, prepared: PreparedPlanToolCommit) {
        Self::trace_artifact_record(
            self.session_id.as_str(),
            &prepared.recorded_artifact,
            prepared.recorded_content_bytes,
        );
        self.next_sequence = prepared.next_sequence;
        self.session_started = prepared.session_started;
        self.ledger = prepared.ledger;
        self.artifacts = prepared.artifacts;
        self.active_plan = Some(prepared.active_plan);
        self.terminal_plans = prepared.terminal_plans;
        self.transcript = prepared.transcript;
        self.pending_tool_calls = prepared.pending_tool_calls;
        self.resolved_tool_calls = prepared.resolved_tool_calls;
    }
}

fn record_candidate_event(
    session_id: &merry_core::SessionId,
    next_sequence: &mut u64,
    ledger: &mut TaskLedger,
    payload: RuntimeJournalPayload,
    fact_kind: Option<LedgerFactKind>,
) -> RuntimeJournalEvent {
    let sequence = *next_sequence;
    if let Some(fact_kind) = fact_kind {
        ledger.record(sequence, fact_kind);
    }
    *next_sequence += 1;
    RuntimeJournalEvent::new(session_id.clone(), sequence, payload)
}
