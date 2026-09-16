//! Provider-neutral coding composition for Merry.
//!
//! This crate is the single owner of coding composition: the parent runtime
//! builder, child-runtime inputs, workspace tools, project-rule and skill
//! projections, process admission lanes, tool order, and stable composition
//! identity. The facade and CLI consume these types rather than assembling a
//! second coding policy.

mod child_runtime;
mod profile_hash;
mod project_capabilities;
mod project_rules;
mod runtime;
mod search_tools;
mod workspace;

#[cfg(test)]
mod tests;

pub use profile_hash::CodingAgentProfileHash;
pub use project_rules::{
    MAX_ROOT_PROJECT_RULES_BYTES, ProjectRulesLoadError, ROOT_PROJECT_RULES_FILE,
    load_root_project_rules,
};
pub use runtime::{
    CodingApprovalPolicy, CodingModelRoleConfig, CodingModelRoleConfigError,
    CodingPermissionPolicy, CodingPermissionPolicyError, CodingRuntime, CodingRuntimeBuildError,
    CodingRuntimeBuilder, CodingRuntimeInput, CodingSubagentsConfig,
};

use merry_core::{CoreError, ToolName};
use merry_llm::ModelRetryPolicy;
use merry_process::ProcessSession;
use merry_runtime::{
    AgentLoopConfig, PermissionAdmissionError, ProcessCommandToolError, ProcessRunner,
    ProjectRules, PromptBlock, PromptError, PromptProfile, RegisteredTool, RuntimeBuilder,
    RuntimeError, RuntimeProfile, RuntimeProfileError, SkillCatalog, TaskAnchor, Tool,
};
pub use merry_tools::{WorkspaceToolConfigError, WorkspaceToolLimits};
use serde_json::Error as JsonError;
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;
use workspace::{WorkspaceCodingProfileBuildError, WorkspaceCodingProfileBuilder};

use profile_hash::coding_agent_profile_hash;

/// Stable identity of the provider-neutral coding profile contract.
pub const CODING_AGENT_PROFILE_ID: &str = "coding-agent-profile";

/// Stable provider-prefix layout owned by the runtime coding composition.
pub const CODING_AGENT_STABLE_PREFIX_LAYOUT: &str =
    "runtime-instructions|progress-commentary|skill-catalog|project-rules|tools";

/// Dynamic provider-context layout owned by the runtime coding composition.
pub const CODING_AGENT_DYNAMIC_CONTEXT_LAYOUT: &str =
    "checkpoint|task-anchor|plan-control|compiled-context|transcript|tool-results|user-input";

/// Stable coding-specific policy block inserted after runtime instructions.
pub const CODING_AGENT_POLICY_PROMPT: &str = r#"<merry_coding_policy>
This is a coding-agent run. Inspect the repository and its governing rules before changing files. Keep runtime state, task progress, artifacts, checkpoints, permissions, and tool results in their owning runtime contracts; do not treat a raw transcript as the source of truth.

Use the registered file and process tools according to their typed schemas. Use `read_text` for a bounded line range from a known text file; never request complete-file content when a focused range is enough. Use `run_process` for repository discovery and verification when it is available, preferring the installed modern search tools the workspace capability facts report, bounded commands such as `rg --files`, a focused literal `rg` search, or `sed -n '<start>,<end>p'`. Avoid broad recursive output, `cat` on large files, and repeated exploratory calls. Use `apply_patch` for edits, keep hunks localized, and include only the smallest unique context needed. Permission, phase, role, and path scope are runtime admission decisions; do not invent tools or request broader capability than the exact action needs.

A sandboxed action starts with no network access and no access beyond what trusted global configuration already granted. Decide what a command needs before running it rather than after it fails: a command that authenticates, installs, downloads, publishes, or otherwise reaches a remote service needs network requested in the same `run_process` call, while a command that only touches the workspace and the configured local baseline needs no additional capability. Paths and host integrations enabled by trusted global configuration are already available to sandboxed commands. If a process command needs access that is still missing, such as network, a reviewed path, or an unconfigured host integration, include all required capabilities in that same `run_process` call under `permissions`; Merry reviews them before execution and runs the exact command through the permissioned backend when approved. Use `reason` to explain the minimum required access. Use `request_permissions` only when the needed capability is discovered after a failed sandboxed attempt or when the action is not being retried through `run_process`. Treat the sandbox as the first explanation when a process command fails: a capability it withheld often surfaces as a credentials, authentication, or connectivity error, so re-run the same action with the missing capability before concluding anything about the user's machine, account, or local setup.

When a tool fails, preserve the failure evidence, determine whether the cause is validation, missing permission, unavailable capability, or an implementation error, and then either make a bounded recovery attempt or report the blocker. Do not repeat an identical failed action without new evidence or an explicit reviewed admission.

For delegated coding work, each child max_model_turns covers its full lifecycle and must be at least 2048. The configured maximum may be larger. Reaching the limit is recoverable when that maximum leaves room: inspect the child status, then spawn a replacement with the same plan_client_key and a larger budget so it can continue from the shared workspace.

Before finishing, verify the requested behavior with the narrowest deterministic checks that prove it. The final report is an evidence-backed summary: state what changed or was answered, name the checks that actually ran and their outcomes, distinguish skipped or blocked checks, and call out remaining risks. Never claim an unrun check succeeded.
</merry_coding_policy>"#;

/// Minimum model-turn budget for a coding-profile child agent.
pub const MIN_CODING_SUBAGENT_MODEL_TURNS: u32 = 2048;

/// Default coding-profile ceiling for one child agent.
pub const DEFAULT_CODING_SUBAGENT_MAX_MODEL_TURNS: u32 = 2048;

/// Maximum model-turn ceiling for the coding-profile main agent.
pub const DEFAULT_CODING_AGENT_MAX_MODEL_TURNS: usize = usize::MAX;

/// Final-report behavior for a coding-agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodingFinalReportPolicy {
    /// Report changes, evidence, verification, blockers, and remaining risks.
    EvidenceBackedSummary,
}

impl CodingFinalReportPolicy {
    /// Returns the stable policy identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EvidenceBackedSummary => "evidence_backed_summary",
        }
    }
}

/// Coding-loop recovery and final-report policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CodingAgentRunPolicy {
    max_model_turns: usize,
    final_report: CodingFinalReportPolicy,
}

impl CodingAgentRunPolicy {
    /// Creates a validated coding run policy.
    pub fn new(
        max_model_turns: usize,
        final_report: CodingFinalReportPolicy,
    ) -> Result<Self, CodingAgentRunPolicyError> {
        if max_model_turns == 0 {
            return Err(CodingAgentRunPolicyError::MaxModelTurnsMustBeNonZero);
        }
        Ok(Self {
            max_model_turns,
            final_report,
        })
    }

    /// Returns the maximum number of model turns.
    #[must_use]
    pub const fn max_model_turns(self) -> usize {
        self.max_model_turns
    }

    /// Returns the final-report policy.
    #[must_use]
    pub const fn final_report(self) -> CodingFinalReportPolicy {
        self.final_report
    }

    /// Creates the runtime loop configuration owned by this policy.
    pub fn loop_config(self) -> Result<AgentLoopConfig, merry_runtime::AgentLoopConfigError> {
        AgentLoopConfig::new(self.max_model_turns)
    }
}

impl Default for CodingAgentRunPolicy {
    fn default() -> Self {
        Self {
            max_model_turns: DEFAULT_CODING_AGENT_MAX_MODEL_TURNS,
            final_report: CodingFinalReportPolicy::EvidenceBackedSummary,
        }
    }
}

/// Invalid coding-loop policy.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentRunPolicyError {
    /// A coding loop without a turn budget cannot make progress.
    #[error("coding agent max_model_turns must be greater than zero")]
    MaxModelTurnsMustBeNonZero,
}

/// Returns the shared default coding-agent loop configuration.
pub fn coding_agent_loop_config() -> Result<AgentLoopConfig, merry_runtime::AgentLoopConfigError> {
    CodingAgentRunPolicy::default().loop_config()
}

/// Creates the shared coding-agent composition builder with one workspace root.
#[must_use]
pub fn coding_agent(root: impl Into<PathBuf>) -> CodingAgentProfileBuilder {
    CodingAgentProfileBuilder::new(root)
}

/// One provider-neutral coding-agent composition applied to a runtime builder.
///
/// The profile owns the coding tool catalog and its runtime policy settings.
/// Provider adapters see only the resulting Merry-owned tool specifications;
/// provider wire types never enter this API. Task anchors, checkpoints,
/// transcript items, and tool results remain runtime-owned dynamic context.
#[derive(Clone)]
pub struct CodingAgentProfile {
    runtime_profile: RuntimeProfile,
    profile_hash: CodingAgentProfileHash,
    run_policy: CodingAgentRunPolicy,
}

impl CodingAgentProfile {
    /// Creates a shared coding-agent profile builder.
    #[must_use]
    pub fn builder(root: impl Into<PathBuf>) -> CodingAgentProfileBuilder {
        CodingAgentProfileBuilder::new(root)
    }

    /// Applies this profile to a runtime builder.
    pub fn apply_to(&self, builder: RuntimeBuilder) -> Result<RuntimeBuilder, RuntimeError> {
        builder.with_profile(self.runtime_profile.clone())
    }

    /// Creates the shared coding loop configuration for this profile.
    pub fn loop_config(&self) -> Result<AgentLoopConfig, merry_runtime::AgentLoopConfigError> {
        self.run_policy.loop_config()
    }

    /// Returns the recovery and final-report policy carried by this profile.
    #[must_use]
    pub const fn run_policy(&self) -> CodingAgentRunPolicy {
        self.run_policy
    }

    /// Returns the complete runtime profile carried by this composition.
    #[must_use]
    pub fn runtime_profile(&self) -> RuntimeProfile {
        self.runtime_profile.clone()
    }

    /// Returns the provider-neutral registered tools in their advertised order.
    #[must_use]
    pub fn registered_tools(&self) -> &[RegisteredTool] {
        self.runtime_profile.registered_tools()
    }

    /// Returns the ordered provider-neutral tool names for diagnostics/tests.
    #[must_use]
    pub fn tool_names(&self) -> Vec<&ToolName> {
        self.registered_tools()
            .iter()
            .map(|tool| tool.spec().name())
            .collect()
    }

    /// Returns the deterministic identity of the profile's stable composition.
    #[must_use]
    pub fn profile_hash(&self) -> &CodingAgentProfileHash {
        &self.profile_hash
    }
}

/// Builder for the single shared coding-agent composition profile.
#[derive(Clone)]
pub struct CodingAgentProfileBuilder {
    workspace: WorkspaceCodingProfileBuilder,
    retry_policy: Option<ModelRetryPolicy>,
    run_policy: CodingAgentRunPolicy,
    allow_bridge_tools: bool,
    skill_catalog: Option<SkillCatalog>,
    project_rules: Option<ProjectRules>,
    task_anchor: Option<TaskAnchor>,
    tools: Vec<Tool>,
    registered_tools: Vec<RegisteredTool>,
}

impl CodingAgentProfileBuilder {
    /// Creates a coding-agent profile builder with one workspace root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            workspace: WorkspaceCodingProfileBuilder::new(root),
            retry_policy: None,
            run_policy: CodingAgentRunPolicy::default(),
            allow_bridge_tools: false,
            skill_catalog: None,
            project_rules: None,
            task_anchor: None,
            tools: Vec::new(),
            registered_tools: Vec::new(),
        }
    }

    /// Replaces the workspace root.
    #[must_use]
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace = self.workspace.root(root);
        self
    }

    /// Adds read-only resource roots that are not the writable workspace root.
    #[must_use]
    pub fn readonly_resource_roots<I, P>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.workspace = self.workspace.readonly_resource_roots(roots);
        self
    }

    /// Sets workspace tool limits.
    #[must_use]
    pub fn limits(mut self, limits: WorkspaceToolLimits) -> Self {
        self.workspace = self.workspace.limits(limits);
        self
    }

    /// Enables the constrained workspace patch tool.
    #[must_use]
    pub fn patch_tool(mut self) -> Self {
        self.workspace = self.workspace.patch_tool();
        self
    }

    /// Sets workspace-relative paths that `apply_patch` may write.
    #[must_use]
    pub fn patch_write_scope<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.workspace = self.workspace.patch_write_scope(paths);
        self
    }

    /// Denies all `apply_patch` writes.
    #[must_use]
    pub fn read_only_patch_scope(mut self) -> Self {
        self.workspace = self.workspace.read_only_patch_scope();
        self
    }

    /// Sets workspace-relative paths that `apply_patch` must never write.
    #[must_use]
    pub fn forbidden_paths<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.workspace = self.workspace.forbidden_paths(paths);
        self
    }

    /// Includes read-only process execution lanes.
    #[must_use]
    pub fn read_only_process_runner(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.workspace = self.workspace.read_only_process_runner(runner);
        self
    }

    /// Includes one host-process session and its reviewed-action capability.
    #[must_use]
    pub fn accepted_process_session(mut self, session: ProcessSession) -> Self {
        self.workspace = self.workspace.accepted_process_session(session);
        self
    }

    /// Sets the provider-neutral retry/recovery policy for model turns.
    #[must_use]
    pub fn retry_policy(mut self, retry_policy: ModelRetryPolicy) -> Self {
        self.retry_policy = Some(retry_policy);
        self
    }

    /// Sets coding-loop recovery and final-report policy.
    #[must_use]
    pub fn run_policy(mut self, run_policy: CodingAgentRunPolicy) -> Self {
        self.run_policy = run_policy;
        self
    }

    /// Allows explicitly registered bridge tools in this coding profile.
    #[doc(hidden)]
    #[must_use]
    pub fn allow_bridge_tools(mut self) -> Self {
        self.allow_bridge_tools = true;
        self
    }

    /// Adds the stable project rules projection.
    #[must_use]
    pub fn project_rules(mut self, project_rules: ProjectRules) -> Self {
        self.project_rules = Some(project_rules);
        self
    }

    /// Adds stable skill metadata without embedding skill bodies.
    #[must_use]
    pub fn skill_catalog(mut self, skill_catalog: SkillCatalog) -> Self {
        self.skill_catalog = Some(skill_catalog);
        self
    }

    /// Adds the dynamic task anchor projection.
    #[must_use]
    pub fn task_anchor(mut self, task_anchor: TaskAnchor) -> Self {
        self.task_anchor = Some(task_anchor);
        self
    }

    /// Adds a typed application tool after the canonical Merry-managed tools.
    #[must_use]
    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Adds a runtime-owned tool after the canonical workspace tool catalog.
    #[doc(hidden)]
    #[must_use]
    pub fn register_tool(mut self, tool: RegisteredTool) -> Self {
        self.registered_tools.push(tool);
        self
    }

    /// Adds runtime-owned tools in the supplied stable order.
    #[doc(hidden)]
    #[must_use]
    pub fn register_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = RegisteredTool>,
    {
        self.registered_tools.extend(tools);
        self
    }

    /// Builds the shared coding-agent profile.
    pub fn build(self) -> Result<CodingAgentProfile, CodingAgentProfileBuildError> {
        let Self {
            workspace,
            retry_policy,
            run_policy,
            allow_bridge_tools,
            skill_catalog,
            project_rules,
            task_anchor,
            tools,
            registered_tools,
        } = self;
        let workspace_hash_material = workspace.hash_material();
        let prompt_profile = PromptProfile::default().with_stable_block(PromptBlock::new(
            "merry_coding_policy",
            CODING_AGENT_POLICY_PROMPT,
        )?)?;
        let mut builder = RuntimeProfile::builder().prompt_profile(prompt_profile);
        builder = workspace
            .apply_to_runtime_profile(builder)
            .map_err(|error| match error {
                WorkspaceCodingProfileBuildError::WorkspaceTools(source) => {
                    CodingAgentProfileBuildError::WorkspaceTools(source)
                }
                WorkspaceCodingProfileBuildError::ProcessTool(source) => {
                    CodingAgentProfileBuildError::ProcessTool(source)
                }
                WorkspaceCodingProfileBuildError::PermissionTool(source) => {
                    CodingAgentProfileBuildError::PermissionTool(source)
                }
                WorkspaceCodingProfileBuildError::Core(source) => {
                    CodingAgentProfileBuildError::Core(source)
                }
            })?;
        if allow_bridge_tools {
            builder = builder.allow_bridge_tools();
        }
        if let Some(retry_policy) = retry_policy {
            builder = builder.model_retry_policy(retry_policy);
        }
        if let Some(skill_catalog) = skill_catalog {
            builder = builder.skill_catalog(skill_catalog);
        }
        if let Some(project_rules) = project_rules {
            builder = builder.project_rules(project_rules);
        }
        if let Some(task_anchor) = task_anchor {
            builder = builder.task_anchor(task_anchor);
        }
        for tool in tools {
            builder = builder.register_tool(tool.into_registered_tool());
        }
        for tool in registered_tools {
            builder = builder.register_tool(tool);
        }
        let runtime_profile = builder.build()?;
        let profile_hash =
            coding_agent_profile_hash(&runtime_profile, run_policy, &workspace_hash_material)?;
        Ok(CodingAgentProfile {
            runtime_profile,
            profile_hash,
            run_policy,
        })
    }
}

/// Errors raised while building the shared coding-agent profile.
#[derive(Debug, Error)]
pub enum CodingAgentProfileBuildError {
    /// Workspace tool configuration was invalid.
    #[error(transparent)]
    WorkspaceTools(#[from] WorkspaceToolConfigError),
    /// The process command tool could not be constructed.
    #[error(transparent)]
    ProcessTool(#[from] ProcessCommandToolError),
    /// The permission request tool could not be constructed.
    #[error(transparent)]
    PermissionTool(#[from] PermissionAdmissionError),
    /// A provider-neutral protocol value failed validation.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// The final runtime profile was invalid.
    #[error(transparent)]
    RuntimeProfile(#[from] RuntimeProfileError),
    /// Profile hash material could not be serialized.
    #[error("coding agent profile hash material could not be serialized: {0}")]
    HashSerialization(#[from] JsonError),
    /// Stable prompt composition was invalid.
    #[error(transparent)]
    Prompt(#[from] PromptError),
}

/// Coding-profile name for runtime-owned process execution.
pub const CODING_LOOP_PROCESS_TOOL: &str = "run_process";
