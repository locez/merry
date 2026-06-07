use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, PermissionedProcessRunnerFactory, ProcessRunner,
    RuntimeProfile, RuntimeProfileError,
};
use merry_tool_workspace::{
    WorkspaceCodingLoopProfile, WorkspaceCodingLoopProfileError, WorkspaceRuntimeProfileBuilderExt,
    WorkspaceToolConfigError, WorkspaceToolLimits, WorkspaceToolsConfig,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

#[derive(Clone)]
enum WorkspaceProcessRunnerConfig {
    ReadOnly(Arc<dyn ProcessRunner>),
    CliBwrap {
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
        permissioned_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    },
}

/// Creates a workspace coding profile builder with one workspace root.
#[must_use]
pub fn workspace_coding(root: impl Into<PathBuf>) -> WorkspaceCodingProfileBuilder {
    WorkspaceCodingProfileBuilder::new(root)
}

/// Facade builder for the common workspace coding profile.
#[derive(Clone)]
pub struct WorkspaceCodingProfileBuilder {
    roots: Vec<PathBuf>,
    allow_hidden: bool,
    limits: WorkspaceToolLimits,
    patch_write_scope: Option<Vec<PathBuf>>,
    forbidden_paths: Vec<PathBuf>,
    enable_patch_tool: bool,
    process_runner: Option<WorkspaceProcessRunnerConfig>,
}

impl WorkspaceCodingProfileBuilder {
    /// Creates a profile builder with one workspace root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_roots([root])
    }

    /// Creates a profile builder with explicit workspace roots.
    pub fn with_roots<I, P>(roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            roots: roots.into_iter().map(Into::into).collect(),
            allow_hidden: false,
            limits: WorkspaceToolLimits::default(),
            patch_write_scope: None,
            forbidden_paths: Vec::new(),
            enable_patch_tool: false,
            process_runner: None,
        }
    }

    /// Adds another workspace root.
    #[must_use]
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.roots.push(root.into());
        self
    }

    /// Controls whether hidden path components are allowed.
    #[must_use]
    pub fn allow_hidden(mut self, allow_hidden: bool) -> Self {
        self.allow_hidden = allow_hidden;
        self
    }

    /// Sets workspace tool limits.
    #[must_use]
    pub fn limits(mut self, limits: WorkspaceToolLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Enables the workspace patch tool.
    #[must_use]
    pub fn patch_tool(mut self) -> Self {
        self.enable_patch_tool = true;
        self
    }

    /// Sets workspace-relative paths that `workspace_patch` may write.
    #[must_use]
    pub fn patch_write_scope<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.patch_write_scope = Some(paths.into_iter().map(Into::into).collect());
        self
    }

    /// Denies all `workspace_patch` writes while still allowing patch proposals
    /// to be rejected with explicit runtime feedback.
    #[must_use]
    pub fn read_only_patch_scope(mut self) -> Self {
        self.patch_write_scope = Some(Vec::new());
        self
    }

    /// Sets workspace-relative paths that `workspace_patch` must never write.
    #[must_use]
    pub fn forbidden_paths<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.forbidden_paths = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Includes read-only process execution lanes.
    #[must_use]
    pub fn read_only_process_runner(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.process_runner = Some(WorkspaceProcessRunnerConfig::ReadOnly(runner));
        self
    }

    /// Includes process execution lanes for an accepted CLI bubblewrap profile.
    #[must_use]
    pub fn cli_bwrap_process_runner(
        mut self,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
    ) -> Self {
        self.process_runner = Some(WorkspaceProcessRunnerConfig::CliBwrap {
            admission,
            runner,
            permissioned_factory: None,
        });
        self
    }

    /// Includes process execution lanes for an accepted CLI bubblewrap profile
    /// and a per-action permissioned process runner factory.
    #[must_use]
    pub fn cli_bwrap_permissioned_process_runner(
        mut self,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
        permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        self.process_runner = Some(WorkspaceProcessRunnerConfig::CliBwrap {
            admission,
            runner,
            permissioned_factory: Some(permissioned_factory),
        });
        self
    }

    /// Builds a complete runtime profile.
    pub fn build(self) -> Result<RuntimeProfile, WorkspaceCodingProfileBuildError> {
        let config = WorkspaceToolsConfig::new(self.roots)
            .with_allow_hidden(self.allow_hidden)
            .with_limits(self.limits)
            .with_patch_write_scope(self.patch_write_scope)
            .with_forbidden_paths(self.forbidden_paths);

        let mut coding_profile = WorkspaceCodingLoopProfile::new(config)?;
        if self.enable_patch_tool {
            coding_profile = coding_profile.with_patch_tool();
        }
        if let Some(process_runner) = self.process_runner {
            coding_profile = match process_runner {
                WorkspaceProcessRunnerConfig::ReadOnly(runner) => {
                    coding_profile.with_read_only_process_runner(runner)
                }
                WorkspaceProcessRunnerConfig::CliBwrap {
                    admission,
                    runner,
                    permissioned_factory,
                } => match permissioned_factory {
                    Some(factory) => coding_profile
                        .with_cli_bwrap_permissioned_process_runner(admission, runner, factory),
                    None => coding_profile.with_cli_bwrap_process_runner(admission, runner),
                },
            };
        }

        Ok(RuntimeProfile::builder()
            .with_workspace_coding_loop(coding_profile)?
            .build()?)
    }

    /// Convenience helper for path-like callers that already have a root path reference.
    #[must_use]
    pub fn from_path(root: &Path) -> Self {
        Self::new(root)
    }
}

/// Errors raised while constructing a workspace coding profile.
#[derive(Debug, Error)]
pub enum WorkspaceCodingProfileBuildError {
    /// Workspace tool configuration was invalid.
    #[error(transparent)]
    WorkspaceTools(#[from] WorkspaceToolConfigError),
    /// Workspace coding-loop profile assembly failed.
    #[error(transparent)]
    WorkspaceCodingLoop(#[from] WorkspaceCodingLoopProfileError),
    /// The final runtime profile was invalid.
    #[error(transparent)]
    RuntimeProfile(#[from] RuntimeProfileError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_llm::ModelRetryPolicy;

    #[test]
    fn workspace_profile_builds_read_tools_and_coding_defaults() {
        let temp = tempfile::tempdir().expect("tempdir should be created");

        let profile = workspace_coding(temp.path())
            .build()
            .expect("workspace profile should build");

        let tool_names = profile
            .registered_tools()
            .iter()
            .map(|tool| tool.spec().name().as_str())
            .collect::<Vec<_>>();
        assert!(tool_names.contains(&"workspace_read_file"));
        assert!(tool_names.contains(&"workspace_list_dir"));
        assert!(tool_names.contains(&"workspace_search_text"));
        assert!(!tool_names.contains(&"workspace_patch"));
        assert!(profile.progress_commentary());
        assert_eq!(
            profile.model_retry_policy(),
            Some(ModelRetryPolicy::coding_agent_default())
        );
    }

    #[test]
    fn workspace_profile_can_enable_patch_tool() {
        let temp = tempfile::tempdir().expect("tempdir should be created");

        let profile = workspace_coding(temp.path())
            .patch_tool()
            .read_only_patch_scope()
            .build()
            .expect("workspace profile should build");

        assert!(
            profile
                .registered_tools()
                .iter()
                .any(|tool| tool.spec().name().as_str() == "workspace_patch")
        );
    }
}
