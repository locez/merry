//! Provider-neutral coding composition for Merry.
//!
//! This crate is the single owner of coding composition: the parent runtime
//! builder, child-runtime inputs, workspace tools, project-rule and skill
//! projections, process admission lanes, tool order, and stable composition
//! identity. The facade and CLI consume these types rather than assembling a
//! second coding policy.

mod child_runtime;
mod project_rules;
mod runtime;
mod workspace;

#[cfg(test)]
mod tests;

pub use project_rules::{
    MAX_ROOT_PROJECT_RULES_BYTES, ProjectRulesLoadError, ROOT_PROJECT_RULES_FILE,
    load_root_project_rules,
};
pub use runtime::{
    CodingModelRoleConfig, CodingModelRoleConfigError, CodingPermissionPolicy, CodingRuntime,
    CodingRuntimeBuildError, CodingRuntimeBuilder, CodingRuntimeInput, CodingSubagentsConfig,
};

use merry_core::{CoreError, ToolName};
use merry_llm::ModelRetryPolicy;
use merry_process::ProcessSession;
use merry_runtime::{
    AgentLoopConfig, PermissionAdmissionError, ProcessCommandToolError, ProcessRunner,
    ProjectRules, PromptBlock, PromptError, PromptProfile, RegisteredTool, RuntimeBuilder,
    RuntimeError, RuntimeProfile, RuntimeProfileError, SkillCatalog, TaskAnchor, ToolActionKind,
    ToolConcurrency, ToolRunner,
};
pub use merry_tool_workspace::{WorkspaceToolConfigError, WorkspaceToolLimits};
use serde_json::Error as JsonError;
use std::{fmt, path::PathBuf, sync::Arc};
use thiserror::Error;
use workspace::{WorkspaceCodingProfileBuildError, WorkspaceCodingProfileBuilder};

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

Use the registered workspace and process tools according to their typed schemas. Permission, phase, role, and workspace scope are runtime admission decisions. Do not try to obtain authority by inventing tools, changing tool schemas, or asking for a broader capability than the exact action needs.

When a tool fails, preserve the failure evidence, determine whether the cause is validation, missing permission, unavailable capability, or an implementation error, and then either make a bounded recovery attempt or report the blocker. Do not repeat an identical failed action without new evidence or an explicit reviewed admission.

Before finishing, verify the requested behavior with the narrowest deterministic checks that prove it. The final report is an evidence-backed summary: state what changed or was answered, name the checks that actually ran and their outcomes, distinguish skipped or blocked checks, and call out remaining risks. Never claim an unrun check succeeded.
</merry_coding_policy>"#;

/// Stable default coding loop budget owned by the coding composition layer.
pub const DEFAULT_CODING_AGENT_MAX_MODEL_TURNS: usize = 1024;

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

/// Stable identity of a shared coding-agent composition profile.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CodingAgentProfileHash(String);

impl CodingAgentProfileHash {
    /// Borrows the stable profile hash label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CodingAgentProfileHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
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
            registered_tools: Vec::new(),
        }
    }

    /// Creates a coding-agent profile builder with explicit workspace roots.
    pub fn with_roots<I, P>(roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            workspace: WorkspaceCodingProfileBuilder::with_roots(roots),
            retry_policy: None,
            run_policy: CodingAgentRunPolicy::default(),
            allow_bridge_tools: false,
            skill_catalog: None,
            project_rules: None,
            task_anchor: None,
            registered_tools: Vec::new(),
        }
    }

    /// Adds another workspace root.
    #[must_use]
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace = self.workspace.root(root);
        self
    }

    /// Adds read-only resource roots that are not writable workspace roots.
    #[must_use]
    pub fn readonly_resource_roots<I, P>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.workspace = self.workspace.readonly_resource_roots(roots);
        self
    }

    /// Controls whether hidden path components are allowed.
    #[must_use]
    pub fn allow_hidden(mut self, allow_hidden: bool) -> Self {
        self.workspace = self.workspace.allow_hidden(allow_hidden);
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

    /// Sets workspace-relative paths that `workspace_patch` may write.
    #[must_use]
    pub fn patch_write_scope<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.workspace = self.workspace.patch_write_scope(paths);
        self
    }

    /// Denies all `workspace_patch` writes.
    #[must_use]
    pub fn read_only_patch_scope(mut self) -> Self {
        self.workspace = self.workspace.read_only_patch_scope();
        self
    }

    /// Sets workspace-relative paths that `workspace_patch` must never write.
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

    /// Adds a runtime-owned tool after the canonical workspace tool catalog.
    #[must_use]
    pub fn register_tool(mut self, tool: RegisteredTool) -> Self {
        self.registered_tools.push(tool);
        self
    }

    /// Adds runtime-owned tools in the supplied stable order.
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

fn coding_agent_profile_hash(
    profile: &RuntimeProfile,
    run_policy: CodingAgentRunPolicy,
    workspace_hash_material: &[u8],
) -> Result<CodingAgentProfileHash, JsonError> {
    let mut material = Vec::new();
    append_hash_field(&mut material, "profile-id", CODING_AGENT_PROFILE_ID);
    material.extend_from_slice(workspace_hash_material);
    append_hash_field(
        &mut material,
        "stable-layout",
        CODING_AGENT_STABLE_PREFIX_LAYOUT,
    );
    append_hash_field(
        &mut material,
        "dynamic-layout",
        CODING_AGENT_DYNAMIC_CONTEXT_LAYOUT,
    );
    append_hash_field(
        &mut material,
        "prompt-base",
        profile.prompt_profile().base_instructions(),
    );
    append_hash_field(
        &mut material,
        "prompt-progress",
        profile.prompt_profile().progress_commentary_instructions(),
    );
    for block in profile.prompt_profile().stable_blocks() {
        append_hash_field(&mut material, "prompt-block-tag", block.tag());
        append_hash_field(&mut material, "prompt-block-text", block.text());
    }
    append_hash_field(
        &mut material,
        "run-max-model-turns",
        &run_policy.max_model_turns().to_string(),
    );
    append_hash_field(
        &mut material,
        "run-final-report",
        run_policy.final_report().as_str(),
    );
    if let Some(retry_policy) = profile.model_retry_policy() {
        append_hash_field(
            &mut material,
            "retry-enabled",
            if retry_policy.enabled() { "on" } else { "off" },
        );
        append_hash_field(
            &mut material,
            "retry-max-attempts",
            &retry_policy.max_attempts().to_string(),
        );
        append_hash_field(
            &mut material,
            "retry-initial-delay-nanos",
            &retry_policy.initial_delay().as_nanos().to_string(),
        );
        append_hash_field(
            &mut material,
            "retry-max-delay-nanos",
            &retry_policy.max_delay().as_nanos().to_string(),
        );
        append_hash_field(
            &mut material,
            "retry-max-elapsed-nanos",
            &retry_policy.max_elapsed().as_nanos().to_string(),
        );
        append_hash_field(
            &mut material,
            "retry-jitter",
            if retry_policy.jitter() { "on" } else { "off" },
        );
    } else {
        append_hash_field(&mut material, "retry-policy", "runtime-default");
    }
    append_hash_field(
        &mut material,
        "progress-commentary",
        if profile.progress_commentary() {
            "on"
        } else {
            "off"
        },
    );
    append_hash_field(
        &mut material,
        "bridge-tools",
        if profile.allow_bridge_tools() {
            "on"
        } else {
            "off"
        },
    );
    append_hash_field(
        &mut material,
        "workspace-patches",
        if profile.allow_low_risk_workspace_patches() {
            "on"
        } else {
            "off"
        },
    );
    append_hash_field(
        &mut material,
        "low-risk-process",
        if profile.low_risk_process_runner().is_some() {
            "on"
        } else {
            "off"
        },
    );
    append_hash_field(
        &mut material,
        "read-only-process",
        if profile.read_only_shell_process_runner().is_some() {
            "on"
        } else {
            "off"
        },
    );
    append_hash_field(
        &mut material,
        "accepted-process",
        if profile.accepted_local_workspace_process_runner().is_some() {
            "on"
        } else {
            "off"
        },
    );
    append_hash_field(
        &mut material,
        "permissioned-process",
        if profile.permissioned_process_runner_factory().is_some() {
            "on"
        } else {
            "off"
        },
    );

    for (id, text) in profile.initial_context_summaries() {
        append_hash_field(&mut material, "initial-context-id", id);
        append_hash_field(&mut material, "initial-context-text", text);
    }
    if let Some(project_rules) = profile.project_rules() {
        append_hash_field(
            &mut material,
            "project-rules-source",
            project_rules.source_path(),
        );
        append_hash_field(
            &mut material,
            "project-rules-hash",
            project_rules.content_hash(),
        );
        append_hash_field(
            &mut material,
            "project-rules-stable-text",
            &project_rules.to_stable_prefix_message_text(),
        );
    }
    if let Some(skill_catalog) = profile.skill_catalog()
        && let Some(text) = skill_catalog.to_stable_prefix_message_text()
    {
        append_hash_field(&mut material, "skill-catalog", &text);
    }

    // Task anchors and checkpoints are intentionally excluded: they are dynamic
    // runtime context and must not invalidate the stable profile identity.
    for tool in profile.registered_tools() {
        let spec = serde_json::to_string(tool.spec())?;
        append_hash_field(&mut material, "tool-spec", &spec);
        append_hash_field(
            &mut material,
            "tool-action-kind",
            tool_action_kind_label(tool.action_kind()),
        );
        append_hash_field(
            &mut material,
            "tool-runner",
            tool_runner_label(tool.runner()),
        );
        append_hash_field(
            &mut material,
            "tool-concurrency",
            tool_concurrency_label(tool.concurrency()),
        );
        append_hash_field(
            &mut material,
            "tool-proposals",
            if tool.proposals_enabled() {
                "on"
            } else {
                "off"
            },
        );
    }

    Ok(CodingAgentProfileHash(format!(
        "fnv1a64:{:016x}",
        fnv1a64(&material)
    )))
}

fn append_hash_field(material: &mut Vec<u8>, name: &str, value: &str) {
    material.extend_from_slice(name.as_bytes());
    material.push(0);
    material.extend_from_slice(&(value.len() as u64).to_be_bytes());
    material.extend_from_slice(value.as_bytes());
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

fn tool_action_kind_label(kind: ToolActionKind) -> &'static str {
    match kind {
        ToolActionKind::ReadOnly => "read_only",
        ToolActionKind::RuntimeControl => "runtime_control",
        ToolActionKind::WorkspaceWrite => "workspace_write",
        ToolActionKind::CommandExec => "command_exec",
        ToolActionKind::Network => "network",
        ToolActionKind::TrustedExternal => "trusted_external",
    }
}

fn tool_runner_label(runner: ToolRunner) -> &'static str {
    match runner {
        ToolRunner::Runtime => "runtime",
        ToolRunner::Bridge => "bridge",
    }
}

fn tool_concurrency_label(concurrency: ToolConcurrency) -> &'static str {
    match concurrency {
        ToolConcurrency::ParallelSafe => "parallel_safe",
        ToolConcurrency::Exclusive => "exclusive",
    }
}
