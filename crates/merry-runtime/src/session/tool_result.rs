use crate::{
    ActionExecutionEvidence, ActionProposal,
    action_audit::ActionAuditPolicy,
    ledger::{CompactLedgerText, LedgerScope, LedgerUpdateKind},
    tool::ToolResultContent,
};
use merry_core::{ErrorInfo, ToolCallId, ToolCallResultStatus};

/// Compact ledger fact to record after a tool result is durable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolResultLedgerObservation {
    scope: LedgerScope,
    summary: CompactLedgerText,
}

impl ToolResultLedgerObservation {
    pub(crate) fn new(
        scope: LedgerScope,
        summary: impl Into<String>,
    ) -> Result<Self, crate::ledger::LedgerValidationError> {
        Ok(Self {
            scope,
            summary: CompactLedgerText::try_from(summary.into())?,
        })
    }

    pub(super) fn into_update_for_call(self, call_id: &ToolCallId) -> LedgerUpdateKind {
        let summary = CompactLedgerText::try_from(format!(
            "{}; call_id={}",
            self.summary.as_str(),
            call_id.as_str()
        ))
        .expect("validated compact ledger text remains non-empty after appending call id");

        LedgerUpdateKind::Observation {
            scope: self.scope,
            summary,
        }
    }
}

/// Complete proposed action execution outcome before session state is mutated.
#[derive(Debug)]
pub(crate) struct ProposedToolExecutionOutcome {
    pub(super) proposal: ActionProposal,
    pub(super) status: ToolCallResultStatus,
    pub(super) content: ToolResultContent,
    pub(super) diagnostic: Option<ErrorInfo>,
    pub(super) execution_evidence: Option<ActionExecutionEvidence>,
    pub(super) policy: ActionAuditPolicy,
    pub(super) observation: Option<ToolResultLedgerObservation>,
}

impl ProposedToolExecutionOutcome {
    pub(crate) fn new(
        proposal: ActionProposal,
        status: ToolCallResultStatus,
        content: impl Into<ToolResultContent>,
        diagnostic: Option<ErrorInfo>,
        execution_evidence: Option<ActionExecutionEvidence>,
        policy: ActionAuditPolicy,
    ) -> Self {
        Self {
            proposal,
            status,
            content: content.into(),
            diagnostic,
            execution_evidence,
            policy,
            observation: None,
        }
    }

    pub(crate) fn with_observation(mut self, observation: ToolResultLedgerObservation) -> Self {
        self.observation = Some(observation);
        self
    }
}
