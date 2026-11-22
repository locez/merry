//! Lightweight session-scoped checklist contracts for execution progress.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Maximum number of items retained in a session checklist.
pub const MAX_TODO_ITEMS: usize = 64;

/// Maximum UTF-8 byte length for one checklist item identifier.
pub const MAX_TODO_ID_BYTES: usize = 128;

/// Maximum UTF-8 byte length for one checklist item description.
pub const MAX_TODO_CONTENT_BYTES: usize = 1024;

/// Progress status of one checklist item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started yet.
    #[default]
    Pending,
    /// Currently being worked on.
    InProgress,
    /// Finished and verified.
    Completed,
    /// No longer needed or dropped.
    Cancelled,
}

impl TodoStatus {
    /// Compact bracket tag used in model-facing checklist summaries.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Pending => "[pending]",
            Self::InProgress => "[in_progress]",
            Self::Completed => "[completed]",
            Self::Cancelled => "[cancelled]",
        }
    }
}

/// One model-authored execution checklist item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TodoItem {
    /// Stable identifier used for partial status updates.
    pub id: String,
    /// Short human-readable task description.
    pub content: String,
    /// Current completion status.
    pub status: TodoStatus,
}
