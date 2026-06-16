//! Internal runtime action audit records.
//!
//! Action audits are runtime-owned facts about policy decisions for registered
//! tool actions. They are not provider-visible tool result payloads and do not
//! store provider wire formats.

use crate::{
    ActionExecutionEvidence, ActionProposal, ToolActionKind,
    action_policy::{ActionPolicyDecision, ActionPolicyDisposition, ActionRiskTier},
};
use merry_core::{PendingToolCall, ToolCallId, ToolName};
use serde::{Deserialize, Serialize};
use std::fmt;

const ACTION_AUDIT_ID_PREFIX: &str = "action-audit-";

/// Deterministic identifier for an internal action audit record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ActionAuditId(String);

impl ActionAuditId {
    fn from_order(order: u64) -> Self {
        Self(format!("{ACTION_AUDIT_ID_PREFIX}{order:020}"))
    }

    #[cfg(test)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ActionAuditId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Runtime-owned lifecycle status for an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActionAuditStatus {
    /// The action was proposed with deterministic runtime-owned evidence.
    Proposed,
    /// The action was applied and recorded with execute-time evidence.
    Executed,
    /// The action was denied by runtime policy.
    Denied,
    /// The action was blocked by runtime admission until commit lifecycle exists.
    Guarded,
}

/// Compact runtime-owned policy decision recorded with an action audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActionAuditPolicy {
    risk_tier: ActionRiskTier,
    disposition: ActionPolicyDisposition,
    reason: String,
}

impl ActionAuditPolicy {
    pub(crate) fn from_decision(decision: &ActionPolicyDecision) -> Self {
        Self {
            risk_tier: decision.risk_tier(),
            disposition: decision.disposition(),
            reason: decision.reason().to_owned(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new(
        risk_tier: ActionRiskTier,
        disposition: ActionPolicyDisposition,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            risk_tier,
            disposition,
            reason: reason.into(),
        }
    }

    #[cfg(test)]
    pub(crate) const fn risk_tier(&self) -> ActionRiskTier {
        self.risk_tier
    }

    #[cfg(test)]
    pub(crate) const fn disposition(&self) -> ActionPolicyDisposition {
        self.disposition
    }

    #[cfg(test)]
    pub(crate) fn reason(&self) -> &str {
        &self.reason
    }
}

/// Runtime-owned audit record for an action proposal or policy decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActionAuditRecord {
    id: ActionAuditId,
    order: u64,
    tool_call_id: ToolCallId,
    tool_name: ToolName,
    action_kind: ToolActionKind,
    policy: Option<ActionAuditPolicy>,
    status: ActionAuditStatus,
    proposal: Option<ActionProposal>,
    execution_evidence: Option<ActionExecutionEvidence>,
}

impl ActionAuditRecord {
    fn proposed(order: u64, proposal: ActionProposal) -> Self {
        let proposal = proposal.audit_sanitized();
        Self {
            id: ActionAuditId::from_order(order),
            order,
            tool_call_id: proposal.tool_call_id().clone(),
            tool_name: proposal.tool_name().clone(),
            action_kind: proposal.action_kind(),
            policy: None,
            status: ActionAuditStatus::Proposed,
            proposal: Some(proposal),
            execution_evidence: None,
        }
    }

    fn executed(
        order: u64,
        call: &PendingToolCall,
        action_kind: ToolActionKind,
        policy: ActionAuditPolicy,
        evidence: ActionExecutionEvidence,
    ) -> Self {
        Self {
            id: ActionAuditId::from_order(order),
            order,
            tool_call_id: call.id().clone(),
            tool_name: call.name().clone(),
            action_kind,
            policy: Some(policy),
            status: ActionAuditStatus::Executed,
            proposal: None,
            execution_evidence: Some(evidence),
        }
    }

    fn denied(order: u64, call: &PendingToolCall, decision: &ActionPolicyDecision) -> Self {
        Self {
            id: ActionAuditId::from_order(order),
            order,
            tool_call_id: call.id().clone(),
            tool_name: call.name().clone(),
            action_kind: decision.action_kind(),
            policy: Some(ActionAuditPolicy::from_decision(decision)),
            status: ActionAuditStatus::Denied,
            proposal: None,
            execution_evidence: None,
        }
    }

    fn guarded(
        order: u64,
        call: &PendingToolCall,
        action_kind: ToolActionKind,
        policy: ActionAuditPolicy,
    ) -> Self {
        Self {
            id: ActionAuditId::from_order(order),
            order,
            tool_call_id: call.id().clone(),
            tool_name: call.name().clone(),
            action_kind,
            policy: Some(policy),
            status: ActionAuditStatus::Guarded,
            proposal: None,
            execution_evidence: None,
        }
    }

    fn is_guarded_for(&self, call_id: &ToolCallId, action_kind: ToolActionKind) -> bool {
        self.status == ActionAuditStatus::Guarded
            && &self.tool_call_id == call_id
            && self.action_kind == action_kind
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> &ActionAuditId {
        &self.id
    }

    #[cfg(test)]
    pub(crate) const fn order(&self) -> u64 {
        self.order
    }

    #[cfg(test)]
    pub(crate) fn tool_call_id(&self) -> &ToolCallId {
        &self.tool_call_id
    }

    #[cfg(test)]
    pub(crate) fn tool_name(&self) -> &ToolName {
        &self.tool_name
    }

    #[cfg(test)]
    pub(crate) const fn action_kind(&self) -> ToolActionKind {
        self.action_kind
    }

    #[cfg(test)]
    pub(crate) fn policy(&self) -> Option<ActionAuditPolicy> {
        self.policy.clone()
    }

    #[cfg(test)]
    pub(crate) const fn status(&self) -> ActionAuditStatus {
        self.status
    }

    #[cfg(test)]
    pub(crate) fn proposal(&self) -> Option<&ActionProposal> {
        self.proposal.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn execution_evidence(&self) -> Option<&ActionExecutionEvidence> {
        self.execution_evidence.as_ref()
    }
}

/// Append-only registry for internal action audit records.
#[derive(Debug, Default)]
pub(crate) struct ActionAuditRegistry {
    records: Vec<ActionAuditRecord>,
    next_order: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedActionAuditRegistry {
    records: Vec<ActionAuditRecord>,
}

impl ActionAuditRegistry {
    pub(crate) fn record_proposed_tool_action(&mut self, proposal: ActionProposal) {
        let record = ActionAuditRecord::proposed(self.next_order, proposal);
        self.next_order += 1;
        self.records.push(record);
    }

    pub(crate) fn record_denied_tool_action(
        &mut self,
        call: &PendingToolCall,
        decision: &ActionPolicyDecision,
    ) {
        let record = ActionAuditRecord::denied(self.next_order, call, decision);
        self.next_order += 1;
        self.records.push(record);
    }

    pub(crate) fn record_executed_tool_action(
        &mut self,
        call: &PendingToolCall,
        action_kind: ToolActionKind,
        policy: ActionAuditPolicy,
        evidence: ActionExecutionEvidence,
    ) {
        let record =
            ActionAuditRecord::executed(self.next_order, call, action_kind, policy, evidence);
        self.next_order += 1;
        self.records.push(record);
    }

    pub(crate) fn record_guarded_tool_action(
        &mut self,
        call: &PendingToolCall,
        action_kind: ToolActionKind,
        policy: ActionAuditPolicy,
    ) -> bool {
        if self
            .records
            .iter()
            .any(|record| record.is_guarded_for(call.id(), action_kind))
        {
            return false;
        }

        let record = ActionAuditRecord::guarded(self.next_order, call, action_kind, policy);
        self.next_order += 1;
        self.records.push(record);
        true
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> ActionAuditRegistrySnapshot {
        ActionAuditRegistrySnapshot {
            records: self.records.clone(),
        }
    }

    #[must_use]
    pub(crate) fn persisted(&self) -> PersistedActionAuditRegistry {
        PersistedActionAuditRegistry {
            records: self.records.clone(),
        }
    }

    pub(crate) fn from_persisted(persisted: PersistedActionAuditRegistry) -> Self {
        let next_order = persisted
            .records
            .iter()
            .map(|record| record.order.saturating_add(1))
            .max()
            .unwrap_or(0);
        Self {
            records: persisted.records,
            next_order,
        }
    }
}

/// Detached read model for internal runtime action audit records.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionAuditRegistrySnapshot {
    records: Vec<ActionAuditRecord>,
}

#[cfg(test)]
impl ActionAuditRegistrySnapshot {
    pub(crate) fn records(&self) -> &[ActionAuditRecord] {
        &self.records
    }
}
