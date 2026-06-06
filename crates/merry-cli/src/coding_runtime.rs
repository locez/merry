use crate::config::{self, MerryConfig};
use crate::debug;
use crate::sandbox::{
    ChildHandoff as SandboxChildHandoff, MERRY_SANDBOX_ENV, MERRY_SANDBOX_VERSION_ENV,
    RuntimeProfile as SandboxRuntimeProfile, read_proc_self_mountinfo,
    runtime_profile_from_evidence as sandbox_runtime_profile_from_evidence,
};
use crate::{
    CODING_LOOP_PROCESS_TOOL, CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID,
    CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES, CODING_LOOP_TASK_SMOKE_SESSION_ID, CliError,
    WORKSPACE_PATCH_TOOL, unexpected,
};
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AutomaticCompactionConfig,
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, ChildRuntimeFactory,
    ChildRuntimeInput, DEFAULT_CODING_AGENT_MAX_MODEL_TURNS, PermissionedProcessRunnerFactory,
    ProcessRunner, Runtime, RuntimeBuilder, RuntimeModelRole, RuntimeProfile, SubagentManager,
    subagent_registered_tools,
};
use merry_tool_workspace::{
    WorkspaceCodingLoopProfile, WorkspaceRuntimeProfileBuilderExt, WorkspaceToolLimits,
    WorkspaceToolsConfig,
};
use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) fn with_workspace_coding_loop_profile(
    builder: RuntimeBuilder,
    profile: WorkspaceCodingLoopProfile,
) -> Result<RuntimeBuilder, CliError> {
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .map_err(unexpected)?
        .build()
        .map_err(unexpected)?;
    builder.with_profile(profile).map_err(unexpected)
}

fn with_workspace_coding_loop_profile_for_child(
    builder: RuntimeBuilder,
    profile: WorkspaceCodingLoopProfile,
) -> Result<RuntimeBuilder, merry_runtime::RuntimeError> {
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
            reason: "child workspace coding loop profile application failed",
        })?
        .build()
        .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
            reason: "child runtime profile build failed",
        })?;
    builder.with_profile(profile)
}

pub(crate) fn coding_agent_loop_config() -> Result<AgentLoopConfig, CliError> {
    AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).map_err(unexpected)
}

pub(crate) async fn coding_loop_smoke_admission_from_current_process(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
) -> Option<AcceptedLocalWorkspaceProcessAdmission> {
    let sandbox_marker = env::var_os(MERRY_SANDBOX_ENV);
    let sandbox_version = env::var_os(MERRY_SANDBOX_VERSION_ENV);
    let home = env::var_os("HOME");
    let tmpdir = env::var_os("TMPDIR");
    let mountinfo = read_proc_self_mountinfo().await;
    let sandbox_runtime_profile = sandbox_runtime_profile_from_evidence(
        home.as_deref(),
        tmpdir.as_deref(),
        mountinfo.as_deref(),
    );
    coding_loop_smoke_admission(
        sandbox_child_handoff,
        sandbox_runtime_profile,
        sandbox_marker.as_deref(),
        sandbox_version.as_deref(),
    )
}

pub(crate) fn coding_agent_requires_sandbox_error(command: &str) -> CliError {
    CliError::DebugUsage(format!(
        "merry {command} must run via `merry --with-sandbox {command}`"
    ))
}

fn coding_loop_smoke_admission(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    sandbox_runtime_profile: Option<SandboxRuntimeProfile>,
    sandbox: Option<&OsStr>,
    version: Option<&OsStr>,
) -> Option<AcceptedLocalWorkspaceProcessAdmission> {
    debug::shell::runtime_admission(
        true,
        sandbox_child_handoff,
        sandbox_runtime_profile,
        sandbox,
        version,
    )
}

pub(crate) struct CodingLoopRuntimeOptions {
    pub(crate) allow_hidden_workspace_paths: bool,
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) automatic_compaction: AutomaticCompactionConfig,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) permissioned_process_runner_factory:
        Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) subagents: config::SubagentsConfig,
}

pub(crate) struct HeadlessCodingRuntimeInput<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) root: &'a Path,
    pub(crate) admission: AcceptedLocalWorkspaceProcessAdmission,
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
    pub(crate) runner: Arc<dyn ProcessRunner>,
    pub(crate) permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    pub(crate) allow_hidden_workspace_paths: bool,
    pub(crate) automatic_compaction: AutomaticCompactionConfig,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) subagents: config::SubagentsConfig,
}

pub(crate) fn build_headless_coding_runtime(
    input: HeadlessCodingRuntimeInput<'_>,
) -> Result<Runtime, CliError> {
    build_coding_loop_runtime(
        input.session_id,
        input.root,
        input.admission,
        input.provider,
        input.model,
        input.runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: input.allow_hidden_workspace_paths,
            approval_review: input.approval_review,
            automatic_compaction: input.automatic_compaction,
            retry_policy: input.retry_policy,
            context_compaction: input.context_compaction,
            permissioned_process_runner_factory: Some(input.permissioned_process_runner_factory),
            skill_roots: input.skill_roots,
            subagents: input.subagents,
        },
    )
}

pub(crate) struct RuntimeRoleProviderConfig {
    pub(crate) role: RuntimeModelRole,
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
}

#[derive(Clone)]
pub(crate) struct ActionProcessBackend {
    runner: Arc<dyn ProcessRunner>,
    permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
}

impl ActionProcessBackend {
    pub(crate) fn runner(&self) -> Arc<dyn ProcessRunner> {
        Arc::clone(&self.runner)
    }

    pub(crate) fn permissioned_factory(&self) -> Arc<dyn PermissionedProcessRunnerFactory> {
        Arc::clone(&self.permissioned_factory)
    }

    pub(crate) fn from_parts(
        runner: Arc<dyn ProcessRunner>,
        permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        Self {
            runner,
            permissioned_factory,
        }
    }
}

#[derive(Clone)]
struct CodingLoopChildRuntimeFactory {
    root: PathBuf,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    runner: Arc<dyn ProcessRunner>,
    permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    skill_roots: Vec<PathBuf>,
    allow_hidden_workspace_paths: bool,
}

impl CodingLoopChildRuntimeFactory {
    fn new(
        root: &Path,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        process_backend: ActionProcessBackend,
        skill_roots: Vec<PathBuf>,
        allow_hidden_workspace_paths: bool,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            admission,
            provider,
            model,
            runner: process_backend.runner(),
            permissioned_factory: process_backend.permissioned_factory(),
            skill_roots,
            allow_hidden_workspace_paths,
        }
    }
}

impl ChildRuntimeFactory for CodingLoopChildRuntimeFactory {
    fn build_child(
        &self,
        input: ChildRuntimeInput,
    ) -> Result<Runtime, merry_runtime::RuntimeError> {
        let allow_patch = input.allowed_tools.is_empty()
            || input
                .allowed_tools
                .iter()
                .any(|tool| tool.as_str() == WORKSPACE_PATCH_TOOL);
        let allow_local_workspace_process = input.allowed_tools.is_empty()
            || input
                .allowed_tools
                .iter()
                .any(|tool| tool.as_str() == CODING_LOOP_PROCESS_TOOL);
        let builder = Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(Arc::clone(&self.provider), self.model.clone());
        let mut profile = WorkspaceCodingLoopProfile::new(
            workspace_tools_config(
                coding_loop_workspace_roots(&self.root, &self.skill_roots),
                self.allow_hidden_workspace_paths,
                false,
                None,
            )
            .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
                reason: "child workspace tool config was invalid",
            })?,
        )
        .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
            reason: "child workspace coding loop profile was invalid",
        })?;
        if allow_patch {
            profile = profile.with_patch_tool();
        }
        profile = if allow_local_workspace_process {
            profile.with_cli_bwrap_permissioned_process_runner(
                self.admission,
                Arc::clone(&self.runner),
                Arc::clone(&self.permissioned_factory),
            )
        } else {
            profile.with_read_only_process_runner(Arc::clone(&self.runner))
        };
        with_workspace_coding_loop_profile_for_child(builder, profile)?.build()
    }
}

pub(crate) fn coding_loop_workspace_roots(root: &Path, skill_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = vec![root.to_path_buf()];
    roots.extend(skill_roots.iter().filter(|root| root.is_dir()).cloned());
    roots
}

pub(crate) fn workspace_tools_config(
    roots: Vec<PathBuf>,
    allow_hidden_workspace_paths: bool,
    task_smoke_patch_limit: bool,
    max_patch_bytes_override: Option<usize>,
) -> Result<WorkspaceToolsConfig, CliError> {
    let max_patch_bytes = max_patch_bytes_override.unwrap_or_else(|| {
        if task_smoke_patch_limit {
            CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES
        } else {
            WorkspaceToolLimits::default().max_patch_bytes
        }
    });
    Ok(WorkspaceToolsConfig::new(roots)
        .with_allow_hidden(allow_hidden_workspace_paths)
        .with_limits(WorkspaceToolLimits {
            max_patch_bytes,
            ..WorkspaceToolLimits::default()
        }))
}

pub(crate) fn build_coding_loop_runtime(
    session_id: &str,
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    runner: Arc<dyn ProcessRunner>,
    options: CodingLoopRuntimeOptions,
) -> Result<Runtime, CliError> {
    let parent_session_id = merry_core::SessionId::new(session_id).map_err(unexpected)?;
    let permissioned_factory = options
        .permissioned_process_runner_factory
        .unwrap_or_else(|| {
            Arc::new(merry_runtime::StaticPermissionedProcessRunnerFactory::new(
                Arc::clone(&runner),
            ))
        });
    let mut builder = Runtime::builder(parent_session_id.clone())
        .automatic_compaction(options.automatic_compaction)
        .model_provider(Arc::clone(&provider), model.clone());
    if let Some(role_provider) = options.context_compaction {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    if let Some(role_provider) = options.approval_review {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    if !options.skill_roots.is_empty() {
        let catalog = merry_runtime::SkillCatalog::load_from_roots(options.skill_roots.clone())
            .map_err(unexpected)?;
        let skill_names = catalog
            .skills()
            .iter()
            .map(|skill| skill.name())
            .collect::<Vec<_>>();
        let skill_paths = catalog
            .skills()
            .iter()
            .map(|skill| skill.skill_md_path().display().to_string())
            .collect::<Vec<_>>();
        tracing::info!(
            event = "runtime.skill_catalog.load",
            session_id,
            configured_root_count = options.skill_roots.len(),
            readable_root_count = options
                .skill_roots
                .iter()
                .filter(|root| root.is_dir())
                .count(),
            skill_count = catalog.skills().len(),
            warning_count = catalog.warnings().len(),
            skill_names = ?skill_names,
            skill_paths = ?skill_paths,
            "runtime skill catalog loaded"
        );
        builder = builder.skill_catalog(catalog);
    }

    if options.subagents.is_enabled() {
        let factory = CodingLoopChildRuntimeFactory::new(
            root,
            admission,
            Arc::clone(&provider),
            model.clone(),
            ActionProcessBackend {
                runner: Arc::clone(&runner),
                permissioned_factory: Arc::clone(&permissioned_factory),
            },
            options.skill_roots.clone(),
            options.allow_hidden_workspace_paths,
        );
        let manager = SubagentManager::new(
            parent_session_id.clone(),
            options.subagents.limits(),
            Arc::new(factory),
        );
        let [spawn_tool, wait_tool, cancel_tool] =
            subagent_registered_tools(manager.clone()).map_err(unexpected)?;
        builder = builder
            .subagent_manager(manager)
            .register_tool(spawn_tool)
            .register_tool(wait_tool)
            .register_tool(cancel_tool);
        tracing::info!(
            event = "runtime.subagents.enabled",
            session_id,
            max_threads = options.subagents.limits().max_threads(),
            max_depth = options.subagents.limits().max_depth(),
            "runtime subagent tools registered"
        );
    }

    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(root, &options.skill_roots),
        options.allow_hidden_workspace_paths,
        session_id == CODING_LOOP_TASK_SMOKE_SESSION_ID
            || session_id == CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID,
        None,
    )?)
    .map_err(unexpected)?
    .with_patch_tool()
    .with_cli_bwrap_permissioned_process_runner(admission, runner, permissioned_factory);
    let mut builder = with_workspace_coding_loop_profile(builder, profile)?;
    if let Some(policy) = options.retry_policy {
        builder = builder.model_retry_policy(policy);
    }
    builder.build().map_err(unexpected)
}

pub(crate) fn action_process_runner(
    workspace_root: &Path,
    merry_config: Option<&MerryConfig>,
) -> Result<ActionProcessBackend, CliError> {
    let path_rules = merry_config
        .map(MerryConfig::trusted_global_path_rules)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let network_allowed = merry_config
        .map(MerryConfig::permissions_network_allowed)
        .unwrap_or(false);
    let mut runner = BwrapProcessRunner::new_at_workspace_root(workspace_root)
        .with_path_rules(path_rules.clone());
    if network_allowed {
        runner = runner.allow_network();
    }
    let mut permissioned_factory =
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(workspace_root)
            .with_path_rules(path_rules);
    if network_allowed {
        permissioned_factory = permissioned_factory.allow_base_network();
    }
    Ok(ActionProcessBackend {
        runner: Arc::new(runner),
        permissioned_factory: Arc::new(permissioned_factory),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CodingLoopRuntimeOptions, HeadlessCodingRuntimeInput, build_coding_loop_runtime,
        build_headless_coding_runtime,
    };
    use crate::debug::coding_loop::coding_loop_workspace_call;
    use crate::runtime_events::{collect_runtime_step_events, first_pending_tool_call};
    use crate::test_support::{FakeProcessRunner, ScriptedProvider, model_name};
    use crate::{CODING_LOOP_PROCESS_TOOL, WORKSPACE_READ_FILE_TOOL};
    use merry_core::{RuntimeEvent, ToolCallResult, ToolCallResultStatus, ToolName};
    use merry_llm::{
        FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
        ToolArguments,
    };
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AgentLoopStatus, ProcessRunner,
        StepContext, StepInput, SubagentConfig, ToolExecutionContext,
    };
    use serde_json::{Map, Value};
    use std::sync::Arc;

    #[tokio::test(flavor = "current_thread")]
    async fn projects_skill_metadata_without_body() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let skill_root = temp.path().join("skills");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        std::fs::create_dir_all(skill_root.join("demo")).expect("mkdir skill");
        std::fs::write(
            skill_root.join("demo/SKILL.md"),
            "---\nname: demo-skill\ndescription: Use for demo tasks.\n---\n# Demo\nbody sentinel\n",
        )
        .expect("write skill");

        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);
        let runtime = build_coding_loop_runtime(
            "coding-loop-skill-prefix",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            model_name(),
            Arc::new(FakeProcessRunner::succeeding("")),
            CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: vec![skill_root.clone()],
                subagents: crate::config::SubagentsConfig::default(),
            },
        )
        .expect("runtime should build");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Inspect skills.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");
        let request = provider.recorded_requests()[0].clone();
        let stable_text = request
            .stable_prefix_messages()
            .iter()
            .map(|message| message.content().as_text())
            .collect::<Vec<_>>()
            .join("\n");
        let request_text = request
            .messages()
            .iter()
            .map(|message| message.content().as_text())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(stable_text.contains("demo-skill"));
        assert!(stable_text.contains("Use for demo tasks."));
        assert!(stable_text.contains("demo/SKILL.md"));
        assert!(request_text.contains("workspace_read_file"));
        assert!(request_text.contains("Workspace coding profile"));
        assert!(request_text.contains("user's current input language"));
        assert!(request_text.contains("configured sandbox/profile"));
        assert!(request_text.contains("network access may be intentionally restricted"));
        assert!(request_text.contains("call request_permissions for that exact action"));
        assert!(!stable_text.contains("body sentinel"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn headless_runtime_uses_coding_agent_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);
        let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
        let permissioned_factory = Arc::new(
            merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
        );

        let runtime = build_headless_coding_runtime(HeadlessCodingRuntimeInput {
            session_id: "headless-coding-runtime-profile",
            root: &workspace,
            admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            provider: Arc::new(provider.clone()),
            model: model_name(),
            runner,
            permissioned_process_runner_factory: permissioned_factory,
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: crate::config::SubagentsConfig::default(),
        })
        .expect("headless coding runtime should build");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Inspect workspace.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");

        let request = provider.recorded_requests()[0].clone();
        let request_text = request
            .messages()
            .iter()
            .map(|message| message.content().as_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(request_text.contains("Workspace coding profile"));
        assert!(request_text.contains("user's current input language"));
        assert!(
            request
                .tools()
                .iter()
                .any(|tool| tool.name().as_str() == CODING_LOOP_PROCESS_TOOL)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn includes_skill_roots_in_workspace_read_tools() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let skill_root = temp.path().join("skills");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        std::fs::create_dir_all(skill_root.join("demo")).expect("mkdir skill");
        std::fs::write(
            skill_root.join("demo/SKILL.md"),
            "---\nname: demo-skill\ndescription: Demo skill.\n---\n# Demo\n",
        )
        .expect("write skill");

        let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_workspace_call(
            "call-read-skill",
            WORKSPACE_READ_FILE_TOOL,
            [(
                "path",
                serde_json::Value::String("demo/SKILL.md".to_owned()),
            )],
        )
        .expect("workspace read call should build"))]]);
        let runtime = build_coding_loop_runtime(
            "coding-loop-skill-root-read",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            model_name(),
            Arc::new(FakeProcessRunner::succeeding("")),
            CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: vec![skill_root.clone()],
                subagents: crate::config::SubagentsConfig::default(),
            },
        )
        .expect("runtime should build");

        let events = collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Read demo skill.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should collect pending skill read");
        let pending = first_pending_tool_call(&events).expect("pending skill read");
        let execution_events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("skill read should execute");
        let result = resolved_tool_result(&execution_events);
        assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn allows_missing_default_skill_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let missing_skill_root = temp.path().join("config/merry/skills");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");

        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);
        let runtime = build_coding_loop_runtime(
            "coding-loop-missing-default-skill-root",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            model_name(),
            Arc::new(FakeProcessRunner::succeeding("")),
            CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: vec![missing_skill_root],
                subagents: crate::config::SubagentsConfig::default(),
            },
        )
        .expect("missing default skill root should not block runtime");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Run without configured skills.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");

        let request = provider.recorded_requests()[0].clone();
        assert!(
            request
                .stable_prefix_messages()
                .iter()
                .all(|message| !message.content().as_text().contains("## Skills"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hides_subagent_tools_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");

        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);
        let runtime = build_coding_loop_runtime(
            "coding-loop-subagents-default-off",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            model_name(),
            Arc::new(FakeProcessRunner::succeeding("")),
            CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: Vec::new(),
                subagents: crate::config::SubagentsConfig::default(),
            },
        )
        .expect("runtime should build");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Inspect available tools.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");
        let requests = provider.recorded_requests();
        let tool_names = requests[0]
            .tools()
            .iter()
            .map(|tool| tool.name().as_str())
            .collect::<Vec<_>>();

        assert!(!tool_names.contains(&"spawn_subagents"));
        assert!(!tool_names.contains(&"wait_subagents"));
        assert!(!tool_names.contains(&"cancel_subagents"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exposes_subagent_tools_when_enabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");

        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);
        let runtime = build_coding_loop_runtime(
            "coding-loop-subagents-enabled",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            model_name(),
            Arc::new(FakeProcessRunner::succeeding("")),
            CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: Vec::new(),
                subagents: crate::config::SubagentsConfig::enabled_for_test(
                    SubagentConfig::new(2, 1).expect("valid subagent config"),
                ),
            },
        )
        .expect("runtime should build");

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("Inspect available tools.").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("runtime step should complete");
        let requests = provider.recorded_requests();
        let tool_names = requests[0]
            .tools()
            .iter()
            .map(|tool| tool.name().as_str())
            .collect::<Vec<_>>();

        assert!(tool_names.contains(&"spawn_subagents"));
        assert!(tool_names.contains(&"wait_subagents"));
        assert!(tool_names.contains(&"cancel_subagents"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subagent_with_narrow_tools_keeps_read_only_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        std::fs::write(workspace.join("README.md"), "child fixture\n").expect("write fixture");

        let provider = ScriptedProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(ModelToolCall::new(
                        ModelToolCallId::new("call-spawn").expect("valid call id"),
                        ToolName::new("spawn_subagents").expect("valid tool name"),
                        ToolArguments::try_from(Value::Object(Map::from_iter([(
                            "tasks".to_owned(),
                            Value::Array(vec![Value::Object(Map::from_iter([
                                (
                                    "task".to_owned(),
                                    Value::String("Inspect the fixture.".to_owned()),
                                ),
                                (
                                    "max_model_turns".to_owned(),
                                    Value::Number(serde_json::Number::from(1)),
                                ),
                                (
                                    "allowed_tools".to_owned(),
                                    Value::Array(vec![Value::String(
                                        "workspace_read_file".to_owned(),
                                    )]),
                                ),
                            ]))]),
                        )])))
                        .expect("valid spawn args"),
                    ))],
                    FinishReason::ToolCalls,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("child done")],
                    FinishReason::Stop,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("parent done")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime = build_coding_loop_runtime(
            "coding-loop-subagent-narrow-tools",
            &workspace,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(provider.clone()),
            model_name(),
            Arc::new(FakeProcessRunner::succeeding("")),
            CodingLoopRuntimeOptions {
                allow_hidden_workspace_paths: false,
                approval_review: None,
                automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
                retry_policy: None,
                context_compaction: None,
                permissioned_process_runner_factory: None,
                skill_roots: Vec::new(),
                subagents: crate::config::SubagentsConfig::enabled_for_test(
                    SubagentConfig::new(2, 1).expect("valid subagent config"),
                ),
            },
        )
        .expect("runtime should build");

        let result = runtime
            .run_agent_loop(
                StepInput::user_text("Delegate fixture inspection.").expect("valid input"),
                StepContext::default(),
                AgentLoopConfig::new(3).expect("valid loop config"),
            )
            .await
            .expect("agent loop should run");

        assert_eq!(result.status(), &AgentLoopStatus::Completed);
        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 3);
        let child_request = requests
            .iter()
            .find(|request| {
                request
                    .dynamic_messages()
                    .iter()
                    .any(|message| message.content().as_text().contains("Inspect the fixture."))
            })
            .expect("child request should be recorded");
        let child_tool_names = child_request
            .tools()
            .iter()
            .map(|tool| tool.name().as_str())
            .collect::<Vec<_>>();
        assert!(child_tool_names.contains(&"workspace_read_file"));
        assert!(child_tool_names.contains(&"workspace_list_dir"));
        assert!(child_tool_names.contains(&"workspace_search_text"));
        assert!(child_tool_names.contains(&"run_process"));
        assert!(!child_tool_names.contains(&"workspace_patch"));
        assert!(!child_tool_names.contains(&"spawn_subagents"));
        assert!(!child_tool_names.contains(&"wait_subagents"));
        assert!(!child_tool_names.contains(&"cancel_subagents"));
    }

    fn resolved_tool_result(events: &[RuntimeEvent]) -> &ToolCallResult {
        events
            .iter()
            .find_map(|event| match &event.kind {
                merry_core::RuntimeEventKind::ToolCallResolved { result } => Some(result),
                _ => None,
            })
            .expect("events should include a resolved tool result")
    }
}
