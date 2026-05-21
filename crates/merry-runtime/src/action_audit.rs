//! Internal runtime action audit records.
//!
//! Action audits are runtime-owned facts about policy decisions for registered
//! tool actions. They are not provider-visible tool result payloads and do not
//! store provider wire formats.

use crate::{
    ToolActionKind,
    action_policy::{ActionPolicyDecision, ActionPolicyDisposition, ActionRiskTier},
};
use merry_core::{PendingToolCall, ToolCallId, ToolName};
use std::fmt;

const ACTION_AUDIT_ID_PREFIX: &str = "action-audit-";

/// Deterministic identifier for an internal action audit record.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// Final status for an audited runtime action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionAuditStatus {
    /// The action was denied by runtime policy.
    Denied,
}

/// Compact internal policy decision recorded with an action audit.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActionAuditPolicy {
    risk_tier: ActionRiskTier,
    disposition: ActionPolicyDisposition,
    reason: &'static str,
}

impl ActionAuditPolicy {
    fn from_decision(decision: &ActionPolicyDecision) -> Self {
        Self {
            risk_tier: decision.risk_tier(),
            disposition: decision.disposition(),
            reason: decision.reason(),
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
    pub(crate) const fn reason(&self) -> &'static str {
        self.reason
    }
}

/// Internal audit record for a runtime action policy decision.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionAuditRecord {
    id: ActionAuditId,
    order: u64,
    tool_call_id: ToolCallId,
    tool_name: ToolName,
    action_kind: ToolActionKind,
    policy: ActionAuditPolicy,
    status: ActionAuditStatus,
}

impl ActionAuditRecord {
    fn denied(order: u64, call: &PendingToolCall, decision: &ActionPolicyDecision) -> Self {
        Self {
            id: ActionAuditId::from_order(order),
            order,
            tool_call_id: call.id().clone(),
            tool_name: call.name().clone(),
            action_kind: decision.action_kind(),
            policy: ActionAuditPolicy::from_decision(decision),
            status: ActionAuditStatus::Denied,
        }
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
    pub(crate) const fn policy(&self) -> ActionAuditPolicy {
        self.policy
    }

    #[cfg(test)]
    pub(crate) const fn status(&self) -> ActionAuditStatus {
        self.status
    }
}

/// Append-only registry for internal action audit records.
#[derive(Debug, Default)]
pub(crate) struct ActionAuditRegistry {
    records: Vec<ActionAuditRecord>,
    next_order: u64,
}

impl ActionAuditRegistry {
    pub(crate) fn record_denied_tool_action(
        &mut self,
        call: &PendingToolCall,
        decision: &ActionPolicyDecision,
    ) {
        let record = ActionAuditRecord::denied(self.next_order, call, decision);
        self.next_order += 1;
        self.records.push(record);
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> ActionAuditRegistrySnapshot {
        ActionAuditRegistrySnapshot {
            records: self.records.clone(),
        }
    }
}

/// Detached read model for action audit tests.
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
