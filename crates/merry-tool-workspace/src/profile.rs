use std::sync::Arc;

use merry_core::ToolName;
use merry_llm::ModelRetryPolicy;
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, PermissionAdmissionError,
    PermissionedProcessRunnerFactory, ProcessCommandToolError, ProcessRunner,
    RuntimeProfileBuilder, StaticPermissionedProcessRunnerFactory, process_command_tool,
    request_permissions_tool,
};
use thiserror::Error;

use crate::{
    CODING_LOOP_PROCESS_TOOL, ReadOnlyWorkspaceTools, WorkspaceToolConfigError,
    WorkspaceToolsConfig,
};

const PROJECT_CAPABILITY_CONTEXT_ID: &str = "project-capabilities";

/// Errors raised while building a reusable workspace coding-loop profile.
#[derive(Debug, Error)]
pub enum WorkspaceCodingLoopProfileError {
    /// Workspace tool configuration was invalid.
    #[error(transparent)]
    WorkspaceTools {
        /// Source workspace tool configuration error.
        #[from]
        source: WorkspaceToolConfigError,
    },
    /// The process command tool could not be constructed.
    #[error(transparent)]
    ProcessTool {
        /// Source process command tool error.
        #[from]
        source: ProcessCommandToolError,
    },
    /// The request_permissions tool could not be constructed.
    #[error(transparent)]
    PermissionTool {
        /// Source permission tool error.
        #[from]
        source: PermissionAdmissionError,
    },
}

/// Reusable tool/profile registration for Merry's workspace coding loop.
///
/// This profile keeps upper layers from assembling the same workspace
/// read/search, optional patch, process tool, and process permission lanes by
/// hand. It does not change runtime policy by itself: patch support remains
/// opt-in through [`WorkspaceCodingLoopProfile::with_patch_tool`], and local
/// workspace process effects require an injected runner plus explicit CLI
/// bwrap admission through
/// [`WorkspaceCodingLoopProfile::with_cli_bwrap_process_runner`].
///
/// Apply this to [`merry_runtime::RuntimeProfileBuilder`] through
/// [`WorkspaceRuntimeProfileBuilderExt::with_workspace_coding_loop`], then pass
/// the built runtime profile to `RuntimeBuilder::with_profile`.
#[derive(Clone)]
enum WorkspaceProcessRunner {
    ReadOnly(Arc<dyn ProcessRunner>),
    CliBwrap {
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
        permissioned_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    },
}

#[derive(Clone)]
pub struct WorkspaceCodingLoopProfile {
    workspace_tools: ReadOnlyWorkspaceTools,
    include_patch_tool: bool,
    process_runner: Option<WorkspaceProcessRunner>,
}

impl WorkspaceCodingLoopProfile {
    /// Validates workspace tool configuration and creates the reusable profile.
    pub fn new(config: WorkspaceToolsConfig) -> Result<Self, WorkspaceToolConfigError> {
        Ok(Self {
            workspace_tools: ReadOnlyWorkspaceTools::new(config)?,
            include_patch_tool: false,
            process_runner: None,
        })
    }

    /// Includes the constrained workspace patch tool and low-risk patch lane.
    #[must_use]
    pub fn with_patch_tool(mut self) -> Self {
        self.include_patch_tool = true;
        self
    }

    /// Includes read-only process execution lanes.
    ///
    /// This registers the process tool for shell commands that fit the
    /// read-only process policy. It does not admit local workspace effects.
    #[must_use]
    pub fn with_read_only_process_runner(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.process_runner = Some(WorkspaceProcessRunner::ReadOnly(runner));
        self
    }

    /// Includes process execution lanes for the declared CLI bubblewrap profile.
    ///
    /// This covers all validated process commands under the injected runner.
    #[must_use]
    pub fn with_cli_bwrap_process_runner(
        mut self,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
    ) -> Self {
        self.process_runner = Some(WorkspaceProcessRunner::CliBwrap {
            admission,
            runner,
            permissioned_factory: None,
        });
        self
    }

    /// Includes process lanes for the declared CLI bubblewrap profile and
    /// materializes approved permission requests through a per-action factory.
    #[must_use]
    pub fn with_cli_bwrap_permissioned_process_runner(
        mut self,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
        permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        self.process_runner = Some(WorkspaceProcessRunner::CliBwrap {
            admission,
            runner,
            permissioned_factory: Some(permissioned_factory),
        });
        self
    }

    fn apply_to_runtime_profile_builder(
        self,
        mut builder: RuntimeProfileBuilder,
    ) -> Result<RuntimeProfileBuilder, WorkspaceCodingLoopProfileError> {
        builder = builder
            .model_retry_policy(ModelRetryPolicy::coding_agent_default())
            .progress_commentary(true);

        builder = builder.initial_context_summary(
            PROJECT_CAPABILITY_CONTEXT_ID,
            &self.workspace_tools.project_capability_summary(),
        );

        if self.include_patch_tool {
            builder = builder.allow_low_risk_workspace_patches();
        }

        if let Some(process_runner) = self.process_runner {
            let (runner, accepted_admission, permissioned_factory) = match process_runner {
                WorkspaceProcessRunner::ReadOnly(runner) => {
                    let permissioned_factory = Arc::new(
                        StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
                    );
                    (
                        runner,
                        None,
                        permissioned_factory as Arc<dyn PermissionedProcessRunnerFactory>,
                    )
                }
                WorkspaceProcessRunner::CliBwrap {
                    admission,
                    runner,
                    permissioned_factory,
                } => {
                    let permissioned_factory = permissioned_factory.unwrap_or_else(|| {
                        Arc::new(StaticPermissionedProcessRunnerFactory::new(Arc::clone(
                            &runner,
                        )))
                    });
                    (runner, Some(admission), permissioned_factory)
                }
            };
            builder = builder
                .allow_low_risk_process_actions(Arc::clone(&runner))
                .allow_read_only_shell_process_actions(Arc::clone(&runner))
                .permissioned_process_runner_factory(permissioned_factory);
            if let Some(admission) = accepted_admission {
                builder = builder.allow_accepted_local_workspace_process_actions(admission, runner);
            }
            builder = builder
                .register_tool(process_command_tool(
                    ToolName::new(CODING_LOOP_PROCESS_TOOL).expect("static tool name is valid"),
                    "Run one shell command through Merry's configured process runner. Provide command as one JSON string, with cwd set to null or a workspace-relative directory such as \".\"; do not add a bash field or pass argv. The runtime validates command and cwd byte/control-character limits. Workspace files and directories are writable in the sandbox; .git, external paths, network, and host integrations begin restricted and may require a reviewed session capability. A failed action may indicate unavailable access; after observing the failure, request the corresponding minimum capability for the exact same action before retrying. Linux Unix sockets can be requested as exact filesystem paths when no named integration applies.",
                )?)
                .register_tool(request_permissions_tool()?);
        }

        let tools = if self.include_patch_tool {
            self.workspace_tools.into_registered_tools_with_patch()
        } else {
            self.workspace_tools.into_registered_tools()
        };
        for tool in tools {
            builder = builder.register_tool(tool);
        }

        Ok(builder)
    }
}

/// Extension methods for adding workspace coding-loop tools to runtime profiles.
pub trait WorkspaceRuntimeProfileBuilderExt {
    /// Adds workspace coding-loop context, tools, and process lanes to a runtime profile.
    fn with_workspace_coding_loop(
        self,
        profile: WorkspaceCodingLoopProfile,
    ) -> Result<RuntimeProfileBuilder, WorkspaceCodingLoopProfileError>;
}

impl WorkspaceRuntimeProfileBuilderExt for RuntimeProfileBuilder {
    fn with_workspace_coding_loop(
        self,
        profile: WorkspaceCodingLoopProfile,
    ) -> Result<RuntimeProfileBuilder, WorkspaceCodingLoopProfileError> {
        profile.apply_to_runtime_profile_builder(self)
    }
}
