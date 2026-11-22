//! Public Plan Mode lifecycle and multi-plan document contracts.

use crate::PlanId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Session-scoped lifecycle state of Plan Mode.
///
/// While [`PlanModeState::Active`], runtime tool admission blocks workspace
/// file mutations (`ToolActionKind::WorkspaceWrite`) so exploration and design
/// remain read-only except for session plan documents authored via `write_plan`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanModeState {
    /// Normal operating mode with no Plan Mode edit restrictions.
    #[default]
    Inactive,
    /// User toggled Plan Mode on before submitting the next prompt.
    Pending,
    /// Plan Mode is active: workspace edits are blocked until approved or exited.
    Active,
    /// User toggled Plan Mode off mid-turn; exits cleanly once the turn finishes.
    ExitPending,
}

impl PlanModeState {
    /// Returns whether the read-only Plan Mode edit gate is currently enforced.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Lifecycle status of one Markdown plan document inside a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanDocumentStatus {
    /// Currently authored or being revised by the agent.
    #[default]
    Draft,
    /// Submitted via `exit_plan_mode` and awaiting user review.
    AwaitingApproval,
    /// Approved by the user for implementation.
    Approved,
    /// Abandoned without approval.
    Abandoned,
}

/// Bounded metadata for one Markdown plan document in `<session_dir>/plans/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanDocumentSummary {
    /// Stable runtime-allocated identifier for this plan (`plan-1`, `plan-2`, ...).
    pub id: PlanId,
    /// Human-readable title displayed in UI rails and headers.
    pub title: String,
    /// Sanitized file name under `<session_dir>/plans/` (e.g., `001-auth-refactor.md`).
    pub file_name: String,
    /// Current lifecycle status of this plan document.
    pub status: PlanDocumentStatus,
    /// Monotonic revision count incremented each time this document is saved.
    pub revision: u32,
    /// Creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// Last modification timestamp in Unix milliseconds.
    pub updated_at_ms: u64,
}

/// Complete Markdown plan document combining durable metadata and content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanDocument {
    /// Durable metadata for this plan.
    pub summary: PlanDocumentSummary,
    /// Full Markdown body of the plan.
    pub content: String,
}

/// User decision submitted from a Plan review boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanReviewDecision {
    /// Approve the plan, exit Plan Mode, and proceed to implementation.
    Approved {
        /// Optional user note or inline comments attached alongside approval.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Keep Plan Mode active and return structured feedback for plan revision.
    ChangesRequested {
        /// User feedback or formatted inline comments explaining requested edits.
        feedback: String,
    },
    /// Abandon the proposed plan and exit Plan Mode without starting implementation.
    Abandoned,
}

/// Compact session-level view of Plan Mode and all plans authored in the session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanSessionSnapshot {
    /// Current session Plan Mode state.
    pub mode: PlanModeState,
    /// Plan currently being drafted or most recently active, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_plan_id: Option<PlanId>,
    /// Plan currently parked waiting for user approval, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_approval_plan_id: Option<PlanId>,
    /// Ordered metadata for all plans authored in this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plans: Vec<PlanDocumentSummary>,
}
