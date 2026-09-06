use crate::{CODING_LOOP_PROCESS_TOOL, project_capabilities::project_capability_summary_for_root};
use merry_core::ToolName;
use merry_llm::ModelRetryPolicy;
use merry_process::ProcessSession;
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, PermissionAdmissionError,
    PermissionedProcessRunnerFactory, ProcessCommandToolError, ProcessRunner,
    RuntimeProfileBuilder, StaticPermissionedProcessRunnerFactory, process_command_tool,
    request_permissions_tool,
};
use merry_tools::{
    WorkspaceToolConfigError, WorkspaceToolLimits, WorkspaceTools, WorkspaceToolsConfig,
};
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

const PROJECT_CAPABILITY_CONTEXT_ID: &str = "project-capabilities";
const CODING_WORKSPACE_CAPABILITY_SUMMARY: &str = "\
Coding file capabilities:\n- `read_text` reads a bounded one-based line range from a known UTF-8 text path. Omit the range only to use the small configured default; use multiple focused reads instead of requesting a whole file. Paths are relative to configured roots, and skill/resource roots are read-only and separate from write scope.\n- `apply_patch` is the only file-edit tool. Use one patch envelope with localized Add File or Update File hunks; do not submit whole-file content for a small edit. Runtime admission, write scope, forbidden paths, and current-file preimages are enforced before writes.\n- `run_process` is the discovery and verification lane when configured. Prefer `rg --files`, focused literal `rg` searches, and bounded `sed -n '<start>,<end>p'` reads. Avoid broad recursive output and unbounded file reads.\n- Process execution runs through Merry runtime policy and the configured sandbox/profile, so filesystem and network access may be intentionally restricted; environment and host IPC access may also be intentionally restricted.\n- A failed process action is the signal to recover. If the failure appears caused by unavailable network, filesystem, or host-integration access, call `request_permissions` for that exact action and request only the corresponding minimum capability before retrying it. Approved paths and host integrations remain available for later actions in this runtime session; network access must be requested again for every action that needs it.\n- Linux Unix sockets are filesystem paths. If a host resource is not represented by a named integration, request its exact socket/file path through `requested.paths`; the outer sandbox must already expose the path.\n- `request_permissions` must name the exact planned action and request only the minimum needed capability; the runtime may approve, deny, or fail the request.";

#[derive(Clone)]
pub(crate) enum WorkspaceProcessRunnerConfig {
    ReadOnly(Arc<dyn ProcessRunner>),
    Accepted(ProcessSession),
}

/// Workspace tool and process-lane inputs used by the coding profile.
#[derive(Clone)]
pub(crate) struct WorkspaceCodingProfileBuilder {
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) readonly_resource_roots: Vec<PathBuf>,
    pub(crate) allow_hidden: bool,
    pub(crate) limits: WorkspaceToolLimits,
    pub(crate) patch_write_scope: Option<Vec<PathBuf>>,
    pub(crate) forbidden_paths: Vec<PathBuf>,
    pub(crate) enable_patch_tool: bool,
    pub(crate) process_runner: Option<WorkspaceProcessRunnerConfig>,
}

impl WorkspaceCodingProfileBuilder {
    /// Creates a profile builder with one workspace root.
    #[must_use]
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_roots([root])
    }

    /// Creates a profile builder with explicit workspace roots.
    pub(crate) fn with_roots<I, P>(roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            roots: roots.into_iter().map(Into::into).collect(),
            readonly_resource_roots: Vec::new(),
            allow_hidden: false,
            limits: WorkspaceToolLimits::default(),
            patch_write_scope: None,
            forbidden_paths: Vec::new(),
            enable_patch_tool: false,
            process_runner: None,
        }
    }

    pub(crate) fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.roots.push(root.into());
        self
    }

    pub(crate) fn readonly_resource_roots<I, P>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.readonly_resource_roots = roots.into_iter().map(Into::into).collect();
        self
    }

    pub(crate) fn allow_hidden(mut self, allow_hidden: bool) -> Self {
        self.allow_hidden = allow_hidden;
        self
    }

    pub(crate) fn limits(mut self, limits: WorkspaceToolLimits) -> Self {
        self.limits = limits;
        self
    }

    pub(crate) fn patch_tool(mut self) -> Self {
        self.enable_patch_tool = true;
        self
    }

    pub(crate) fn patch_write_scope<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.patch_write_scope = Some(paths.into_iter().map(Into::into).collect());
        self
    }

    pub(crate) fn read_only_patch_scope(mut self) -> Self {
        self.patch_write_scope = Some(Vec::new());
        self
    }

    pub(crate) fn forbidden_paths<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.forbidden_paths = paths.into_iter().map(Into::into).collect();
        self
    }

    pub(crate) fn read_only_process_runner(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.process_runner = Some(WorkspaceProcessRunnerConfig::ReadOnly(runner));
        self
    }

    pub(crate) fn accepted_process_session(mut self, session: ProcessSession) -> Self {
        self.process_runner = Some(WorkspaceProcessRunnerConfig::Accepted(session));
        self
    }

    pub(crate) fn apply_to_runtime_profile(
        self,
        mut builder: RuntimeProfileBuilder,
    ) -> Result<RuntimeProfileBuilder, WorkspaceCodingProfileBuildError> {
        let config = WorkspaceToolsConfig::new(self.roots)
            .with_readonly_resource_roots(self.readonly_resource_roots)
            .with_allow_hidden(self.allow_hidden)
            .with_limits(self.limits)
            .with_patch_write_scope(self.patch_write_scope)
            .with_forbidden_paths(self.forbidden_paths);
        let project_summary = config
            .roots()
            .iter()
            .find_map(|root| project_capability_summary_for_root(root));
        let workspace_tools = WorkspaceTools::new(config)?;

        let capability_summary = project_summary.map_or_else(
            || CODING_WORKSPACE_CAPABILITY_SUMMARY.to_owned(),
            |facts| format!("{CODING_WORKSPACE_CAPABILITY_SUMMARY}\n{facts}"),
        );
        builder = builder
            .model_retry_policy(ModelRetryPolicy::coding_agent_default())
            .progress_commentary(true)
            .initial_context_summary(PROJECT_CAPABILITY_CONTEXT_ID, &capability_summary);

        if self.enable_patch_tool {
            builder = builder.allow_low_risk_apply_patches();
        }

        if let Some(process_runner) = self.process_runner {
            let (runner, accepted_admission, permissioned_factory) = match process_runner {
                WorkspaceProcessRunnerConfig::ReadOnly(runner) => {
                    let permissioned_factory = Arc::new(
                        StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
                    );
                    (
                        runner,
                        None,
                        permissioned_factory as Arc<dyn PermissionedProcessRunnerFactory>,
                    )
                }
                WorkspaceProcessRunnerConfig::Accepted(session) => {
                    let admission = session.admission();
                    let runner = session.runner();
                    let permissioned_factory = session.permissioned_factory();
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

            let process_tool = process_command_tool(
                ToolName::new(CODING_LOOP_PROCESS_TOOL)?,
                "Run one shell command through Merry's configured process runner. Provide command as one JSON string, with cwd set to null or a workspace-relative directory such as \".\"; do not add a bash field or pass argv. The runtime validates command and cwd byte/control-character limits. Workspace files and directories are writable in the sandbox; .git, external paths, network, and host integrations begin restricted and may require a reviewed capability. A failed action may indicate unavailable access; after observing the failure, request the corresponding minimum capability for the exact same action before retrying it. Network access must be requested again for each action that needs it. Linux Unix sockets can be requested as exact filesystem paths when no named integration applies.",
            )?;
            builder = builder
                .register_tool(process_tool)
                .register_tool(request_permissions_tool()?);
        }

        let tools = if self.enable_patch_tool {
            workspace_tools.into_registered_tools_with_patch()
        } else {
            workspace_tools.into_registered_tools()
        };
        for tool in tools {
            builder = builder.register_tool(tool);
        }

        Ok(builder)
    }

    pub(crate) fn hash_material(&self) -> Vec<u8> {
        let mut material = Vec::new();
        for root in &self.roots {
            append_hash_field(&mut material, "workspace-root", &root.to_string_lossy());
        }
        for root in &self.readonly_resource_roots {
            append_hash_field(
                &mut material,
                "readonly-resource-root",
                &root.to_string_lossy(),
            );
        }
        append_hash_field(
            &mut material,
            "allow-hidden",
            if self.allow_hidden { "on" } else { "off" },
        );
        append_hash_field(
            &mut material,
            "patch-tool",
            if self.enable_patch_tool { "on" } else { "off" },
        );
        match self.patch_write_scope.as_ref() {
            None => append_hash_field(&mut material, "patch-write-scope-state", "unrestricted"),
            Some(paths) => {
                append_hash_field(&mut material, "patch-write-scope-state", "restricted");
                for path in paths {
                    append_hash_field(&mut material, "patch-write-scope", &path.to_string_lossy());
                }
            }
        }
        for path in &self.forbidden_paths {
            append_hash_field(&mut material, "forbidden-path", &path.to_string_lossy());
        }
        for (name, value) in [
            ("max-read-bytes", self.limits.max_read_bytes),
            ("max-read-lines", self.limits.max_read_lines),
            ("max-write-bytes", self.limits.max_write_bytes),
            ("max-patch-bytes", self.limits.max_patch_bytes),
        ] {
            append_hash_field(&mut material, name, &value.to_string());
        }
        let process_runner_label = match &self.process_runner {
            None => "none",
            Some(WorkspaceProcessRunnerConfig::ReadOnly(_)) => "read-only",
            Some(WorkspaceProcessRunnerConfig::Accepted(session)) => {
                let admission = session.admission();
                append_hash_field(
                    &mut material,
                    "process-sandbox",
                    sandbox_profile_label(admission),
                );
                append_hash_field(
                    &mut material,
                    "process-permission-profile",
                    admission.permission_profile_id().as_str(),
                );
                "accepted-process-permissioned"
            }
        };
        append_hash_field(&mut material, "process-runner", process_runner_label);
        material
    }
}

fn sandbox_profile_label(admission: AcceptedLocalWorkspaceProcessAdmission) -> &'static str {
    admission.sandbox_profile().as_str()
}

fn append_hash_field(material: &mut Vec<u8>, name: &str, value: &str) {
    material.extend_from_slice(name.as_bytes());
    material.push(0);
    material.extend_from_slice(&(value.len() as u64).to_be_bytes());
    material.extend_from_slice(value.as_bytes());
}

/// Errors raised while composing workspace tools into the coding profile.
#[derive(Debug, Error)]
pub(crate) enum WorkspaceCodingProfileBuildError {
    /// Workspace tool configuration was invalid.
    #[error(transparent)]
    WorkspaceTools(#[from] WorkspaceToolConfigError),
    /// The process command tool could not be constructed.
    #[error(transparent)]
    ProcessTool(#[from] ProcessCommandToolError),
    /// The permission request tool could not be constructed.
    #[error(transparent)]
    PermissionTool(#[from] PermissionAdmissionError),
    /// A static workspace tool name failed validation.
    #[error(transparent)]
    Core(#[from] merry_core::CoreError),
}
