use std::sync::Arc;

use merry_runtime::{RegisteredTool, ToolActionKind};

use crate::{
    config::{WorkspaceToolConfigError, WorkspaceToolsConfig},
    list::ListDirExecutor,
    patch::WorkspacePatchExecutor,
    read::ReadFileExecutor,
    schema::{list_dir_spec, read_file_spec, search_text_spec, workspace_patch_spec},
    search::SearchTextExecutor,
    state::WorkspaceToolState,
};

/// Read-only workspace tools that can be registered with `merry-runtime`.
#[derive(Debug, Clone)]
pub struct ReadOnlyWorkspaceTools {
    pub(crate) state: Arc<WorkspaceToolState>,
}

impl ReadOnlyWorkspaceTools {
    /// Validates configuration and creates read-only workspace tools.
    pub fn new(config: WorkspaceToolsConfig) -> Result<Self, WorkspaceToolConfigError> {
        let state = WorkspaceToolState::new(config)?;
        Ok(Self {
            state: Arc::new(state),
        })
    }

    /// Returns the registered read-only workspace tools.
    #[must_use]
    pub fn into_registered_tools(self) -> Vec<RegisteredTool> {
        vec![
            RegisteredTool::read_only(
                read_file_spec(),
                Arc::new(ReadFileExecutor {
                    state: Arc::clone(&self.state),
                }),
            ),
            RegisteredTool::read_only(
                list_dir_spec(),
                Arc::new(ListDirExecutor {
                    state: Arc::clone(&self.state),
                }),
            ),
            RegisteredTool::read_only(
                search_text_spec(),
                Arc::new(SearchTextExecutor { state: self.state }),
            ),
        ]
    }

    /// Returns the registered read-only workspace tools plus the opt-in patch tool.
    ///
    /// The patch tool is classified as [`ToolActionKind::WorkspaceWrite`], so
    /// current runtime default policy denies it before invoking the executor.
    #[must_use]
    pub fn into_registered_tools_with_patch(self) -> Vec<RegisteredTool> {
        let patch_state = Arc::clone(&self.state);
        let mut tools = self.into_registered_tools();
        tools.push(
            RegisteredTool::new(
                workspace_patch_spec(),
                Arc::new(WorkspacePatchExecutor { state: patch_state }),
                ToolActionKind::WorkspaceWrite,
            )
            .with_action_proposal(),
        );
        tools
    }
}

impl ReadOnlyWorkspaceTools {
    pub(crate) fn project_capability_summary(&self) -> String {
        self.state.project_capability_summary()
    }
}
