//! Latest-value subagent activity snapshots shared by providers, runtime, and TUI.

use crate::{SubagentId, SubagentTaskId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Current UI-facing phase of a subagent activity snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubagentActivityPhase {
    Starting,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
}

/// Latest-value activity projection for one subagent task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubagentActivitySnapshot {
    pub subagent_id: SubagentId,
    pub task_id: SubagentTaskId,
    pub phase: SubagentActivityPhase,
    pub summary: String,
    pub updated_at_ms: u64,
}
