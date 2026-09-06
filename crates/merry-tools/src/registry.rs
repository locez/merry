use crate::{
    config::{WorkspaceToolConfigError, WorkspaceToolsConfig},
    patch::{self, ApplyPatchExecutor},
    read::{self, ReadTextExecutor},
    state::WorkspaceToolState,
};
use merry_core::ToolSpec;
use merry_runtime::{RegisteredTool, ToolActionKind};
use std::sync::Arc;

/// Built-in filesystem tools with validated, session-stable definitions.
#[derive(Debug, Clone)]
pub struct WorkspaceTools {
    pub(crate) state: Arc<WorkspaceToolState>,
    read_spec: ToolSpec,
    patch_spec: ToolSpec,
}

impl WorkspaceTools {
    /// Validates paths, resource limits, and both built-in tool definitions.
    pub fn new(config: WorkspaceToolsConfig) -> Result<Self, WorkspaceToolConfigError> {
        let state = WorkspaceToolState::new(config)?;
        let read_spec = read::spec(&state.limits)?;
        let patch_spec = patch::spec(&state.limits)?;
        Ok(Self {
            state: Arc::new(state),
            read_spec,
            patch_spec,
        })
    }

    /// Returns the bounded text-read tool with parallel-safe admission.
    #[must_use]
    pub fn into_registered_tools(self) -> Vec<RegisteredTool> {
        vec![register_read(self.read_spec, self.state)]
    }

    /// Adds opt-in patches with explicit write admission and proposal evidence.
    #[must_use]
    pub fn into_registered_tools_with_patch(self) -> Vec<RegisteredTool> {
        vec![
            register_read(self.read_spec, Arc::clone(&self.state)),
            RegisteredTool::new(
                self.patch_spec,
                Arc::new(ApplyPatchExecutor { state: self.state }),
                ToolActionKind::WorkspaceWrite,
            )
            .with_action_proposal(),
        ]
    }
}

fn register_read(spec: ToolSpec, state: Arc<WorkspaceToolState>) -> RegisteredTool {
    RegisteredTool::read_only(spec, Arc::new(ReadTextExecutor { state }))
        .with_parallel_safe_execution()
}
