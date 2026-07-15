//! Runtime capability policy and complete runtime profiles.

use crate::{
    AcceptedLocalWorkspaceProcessAdmission, PermissionAdmissionSource, PermissionReviewMode,
    PermissionedProcessRunnerFactory, ProcessRunner, ProjectRules, RegisteredTool,
    RuntimeTrustLevel, SkillCatalog, SubagentManager, TaskAnchor,
};
use merry_core::ToolName;
use merry_llm::ModelRetryPolicy;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

/// Filesystem access granted or denied for one configured path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathAccess {
    /// The path may be read but not written by Merry-managed runtime backends.
    ReadOnly,
    /// The path may be read and written by Merry-managed runtime backends.
    ReadWrite,
    /// The path must not be made available to Merry-managed runtime backends.
    Deny,
}

impl PathAccess {
    /// Returns the stable config/API spelling for this access mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "ro",
            Self::ReadWrite => "rw",
            Self::Deny => "deny",
        }
    }
}

/// Trust source for a path access rule.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathAccessRuleSource {
    /// Rule came from the user's global Merry config.
    ///
    /// This is intentionally higher trust than project-local configuration,
    /// because normal coding-agent runs may edit files inside the project.
    TrustedGlobalConfig,
    /// Rule materialized from the current permission admission result.
    PermissionReview,
}

/// One platform-neutral path access rule consumed by sandbox backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathAccessRule {
    path: PathBuf,
    access: PathAccess,
    source: PathAccessRuleSource,
}

impl PathAccessRule {
    /// Creates a path access rule.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, access: PathAccess, source: PathAccessRuleSource) -> Self {
        Self {
            path: path.into(),
            access,
            source,
        }
    }

    /// Returns the configured path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the requested access.
    #[must_use]
    pub const fn access(&self) -> PathAccess {
        self.access
    }

    /// Returns the trust source for this rule.
    #[must_use]
    pub const fn source(&self) -> PathAccessRuleSource {
        self.source
    }
}

/// Merry-managed low-level capability policy.
///
/// This constrains capabilities owned by Merry-managed runners, such as file
/// and process access lanes. It is not a complete product runtime profile or a
/// trust label for arbitrary host code or in-process tool executors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    network_allowed: bool,
    path_rules: Vec<PathAccessRule>,
}

impl RuntimeCapabilities {
    /// Creates the default capability policy.
    ///
    /// The default denies network and starts with no path grants.
    #[must_use]
    pub fn new() -> Self {
        Self {
            network_allowed: false,
            path_rules: Vec::new(),
        }
    }

    /// Allows Merry-managed network capability for this runtime.
    #[must_use]
    pub fn allow_network(mut self) -> Self {
        self.network_allowed = true;
        self
    }

    /// Denies Merry-managed network capability for this runtime.
    #[must_use]
    pub fn deny_network(mut self) -> Self {
        self.network_allowed = false;
        self
    }

    /// Adds one path access rule to this profile.
    #[must_use]
    pub fn with_path_rule(mut self, rule: PathAccessRule) -> Self {
        self.path_rules.push(rule);
        self
    }

    /// Replaces all path access rules for this profile.
    #[must_use]
    pub fn with_path_rules(mut self, rules: Vec<PathAccessRule>) -> Self {
        self.path_rules = rules;
        self
    }

    /// Returns whether Merry-managed network capability is allowed.
    #[must_use]
    pub fn network_allowed(&self) -> bool {
        self.network_allowed
    }

    /// Returns platform-neutral path access rules for Merry-managed backends.
    #[must_use]
    pub fn path_rules(&self) -> &[PathAccessRule] {
        &self.path_rules
    }
}

impl Default for RuntimeCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors raised while building a complete runtime profile.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RuntimeProfileError {
    /// A profile tried to register the same tool more than once.
    #[error("tool {name} is already registered in runtime profile")]
    DuplicateToolRegistration {
        /// Duplicate tool name.
        name: ToolName,
    },
}

/// Accepted local workspace process lane carried by a runtime profile.
#[derive(Clone)]
pub struct AcceptedLocalWorkspaceProcessRunnerProfile {
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
}

impl AcceptedLocalWorkspaceProcessRunnerProfile {
    /// Creates a profile-owned accepted local workspace process lane.
    #[must_use]
    pub fn new(
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
    ) -> Self {
        Self { admission, runner }
    }

    /// Returns the accepted local workspace process admission evidence.
    #[must_use]
    pub const fn admission(&self) -> AcceptedLocalWorkspaceProcessAdmission {
        self.admission
    }

    /// Returns the process runner for this lane.
    #[must_use]
    pub fn runner(&self) -> Arc<dyn ProcessRunner> {
        Arc::clone(&self.runner)
    }
}

/// Complete runtime shape applied by [`crate::RuntimeBuilder::with_profile`].
#[derive(Clone)]
pub struct RuntimeProfile {
    capabilities: RuntimeCapabilities,
    model_retry_policy: Option<ModelRetryPolicy>,
    progress_commentary: bool,
    initial_context_summaries: BTreeMap<String, String>,
    registered_tools: Vec<RegisteredTool>,
    allow_bridge_tools: bool,
    allow_low_risk_workspace_patches: bool,
    low_risk_process_runner: Option<Arc<dyn ProcessRunner>>,
    read_only_shell_process_runner: Option<Arc<dyn ProcessRunner>>,
    accepted_local_workspace_process_runner: Option<AcceptedLocalWorkspaceProcessRunnerProfile>,
    runtime_trust_level: Option<RuntimeTrustLevel>,
    permission_review_mode: Option<PermissionReviewMode>,
    permission_admission_source: Option<Arc<dyn PermissionAdmissionSource>>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    skill_catalog: Option<SkillCatalog>,
    project_rules: Option<ProjectRules>,
    task_anchor: Option<TaskAnchor>,
    subagent_manager: Option<SubagentManager>,
}

impl RuntimeProfile {
    /// Creates a profile builder.
    #[must_use]
    pub fn builder() -> RuntimeProfileBuilder {
        RuntimeProfileBuilder::new()
    }

    /// Returns low-level Merry-managed capabilities.
    #[must_use]
    pub const fn capabilities(&self) -> &RuntimeCapabilities {
        &self.capabilities
    }

    /// Returns the profile model retry policy override.
    #[must_use]
    pub const fn model_retry_policy(&self) -> Option<ModelRetryPolicy> {
        self.model_retry_policy
    }

    /// Returns whether this profile asks the model for tool-progress commentary.
    #[must_use]
    pub const fn progress_commentary(&self) -> bool {
        self.progress_commentary
    }

    /// Returns startup context summaries keyed by stable id.
    #[must_use]
    pub fn initial_context_summaries(&self) -> &BTreeMap<String, String> {
        &self.initial_context_summaries
    }

    /// Returns registered tools carried by the profile.
    #[must_use]
    pub fn registered_tools(&self) -> &[RegisteredTool] {
        &self.registered_tools
    }

    /// Consumes the profile and returns registered tools.
    #[must_use]
    pub fn into_registered_tools(self) -> Vec<RegisteredTool> {
        self.registered_tools
    }

    /// Returns whether bridge tools are allowed.
    #[must_use]
    pub const fn allow_bridge_tools(&self) -> bool {
        self.allow_bridge_tools
    }

    /// Returns whether low-risk workspace patches are allowed.
    #[must_use]
    pub const fn allow_low_risk_workspace_patches(&self) -> bool {
        self.allow_low_risk_workspace_patches
    }

    /// Returns the low-risk process action runner.
    #[must_use]
    pub fn low_risk_process_runner(&self) -> Option<Arc<dyn ProcessRunner>> {
        self.low_risk_process_runner.as_ref().map(Arc::clone)
    }

    /// Returns the read-only shell process runner.
    #[must_use]
    pub fn read_only_shell_process_runner(&self) -> Option<Arc<dyn ProcessRunner>> {
        self.read_only_shell_process_runner.as_ref().map(Arc::clone)
    }

    /// Returns the accepted local workspace process runner lane.
    #[must_use]
    pub fn accepted_local_workspace_process_runner(
        &self,
    ) -> Option<&AcceptedLocalWorkspaceProcessRunnerProfile> {
        self.accepted_local_workspace_process_runner.as_ref()
    }

    /// Returns the profile runtime trust level override.
    #[must_use]
    pub const fn runtime_trust_level(&self) -> Option<RuntimeTrustLevel> {
        self.runtime_trust_level
    }

    /// Returns the profile permission review mode override.
    #[must_use]
    pub const fn permission_review_mode(&self) -> Option<PermissionReviewMode> {
        self.permission_review_mode
    }

    /// Returns the host-owned permission admission source.
    #[must_use]
    pub fn permission_admission_source(&self) -> Option<Arc<dyn PermissionAdmissionSource>> {
        self.permission_admission_source.as_ref().map(Arc::clone)
    }

    /// Returns the permissioned process runner factory.
    #[must_use]
    pub fn permissioned_process_runner_factory(
        &self,
    ) -> Option<Arc<dyn PermissionedProcessRunnerFactory>> {
        self.permissioned_process_runner_factory
            .as_ref()
            .map(Arc::clone)
    }

    /// Returns the profile skill catalog.
    #[must_use]
    pub const fn skill_catalog(&self) -> Option<&SkillCatalog> {
        self.skill_catalog.as_ref()
    }

    /// Returns the profile project rules.
    #[must_use]
    pub const fn project_rules(&self) -> Option<&ProjectRules> {
        self.project_rules.as_ref()
    }

    /// Returns the profile task anchor.
    #[must_use]
    pub const fn task_anchor(&self) -> Option<&TaskAnchor> {
        self.task_anchor.as_ref()
    }

    /// Returns the profile subagent manager.
    #[must_use]
    pub const fn subagent_manager(&self) -> Option<&SubagentManager> {
        self.subagent_manager.as_ref()
    }

    pub(crate) fn into_parts(self) -> RuntimeProfileParts {
        RuntimeProfileParts {
            capabilities: self.capabilities,
            model_retry_policy: self.model_retry_policy,
            progress_commentary: self.progress_commentary,
            initial_context_summaries: self.initial_context_summaries,
            registered_tools: self.registered_tools,
            allow_bridge_tools: self.allow_bridge_tools,
            allow_low_risk_workspace_patches: self.allow_low_risk_workspace_patches,
            low_risk_process_runner: self.low_risk_process_runner,
            read_only_shell_process_runner: self.read_only_shell_process_runner,
            accepted_local_workspace_process_runner: self.accepted_local_workspace_process_runner,
            runtime_trust_level: self.runtime_trust_level,
            permission_review_mode: self.permission_review_mode,
            permission_admission_source: self.permission_admission_source,
            permissioned_process_runner_factory: self.permissioned_process_runner_factory,
            skill_catalog: self.skill_catalog,
            project_rules: self.project_rules,
            task_anchor: self.task_anchor,
            subagent_manager: self.subagent_manager,
        }
    }
}

pub(crate) struct RuntimeProfileParts {
    pub(crate) capabilities: RuntimeCapabilities,
    pub(crate) model_retry_policy: Option<ModelRetryPolicy>,
    pub(crate) progress_commentary: bool,
    pub(crate) initial_context_summaries: BTreeMap<String, String>,
    pub(crate) registered_tools: Vec<RegisteredTool>,
    pub(crate) allow_bridge_tools: bool,
    pub(crate) allow_low_risk_workspace_patches: bool,
    pub(crate) low_risk_process_runner: Option<Arc<dyn ProcessRunner>>,
    pub(crate) read_only_shell_process_runner: Option<Arc<dyn ProcessRunner>>,
    pub(crate) accepted_local_workspace_process_runner:
        Option<AcceptedLocalWorkspaceProcessRunnerProfile>,
    pub(crate) runtime_trust_level: Option<RuntimeTrustLevel>,
    pub(crate) permission_review_mode: Option<PermissionReviewMode>,
    pub(crate) permission_admission_source: Option<Arc<dyn PermissionAdmissionSource>>,
    pub(crate) permissioned_process_runner_factory:
        Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    pub(crate) skill_catalog: Option<SkillCatalog>,
    pub(crate) project_rules: Option<ProjectRules>,
    pub(crate) task_anchor: Option<TaskAnchor>,
    pub(crate) subagent_manager: Option<SubagentManager>,
}

/// Builder for a complete runtime profile.
pub struct RuntimeProfileBuilder {
    capabilities: RuntimeCapabilities,
    model_retry_policy: Option<ModelRetryPolicy>,
    progress_commentary: bool,
    initial_context_summaries: BTreeMap<String, String>,
    registered_tools: Vec<RegisteredTool>,
    allow_bridge_tools: bool,
    allow_low_risk_workspace_patches: bool,
    low_risk_process_runner: Option<Arc<dyn ProcessRunner>>,
    read_only_shell_process_runner: Option<Arc<dyn ProcessRunner>>,
    accepted_local_workspace_process_runner: Option<AcceptedLocalWorkspaceProcessRunnerProfile>,
    runtime_trust_level: Option<RuntimeTrustLevel>,
    permission_review_mode: Option<PermissionReviewMode>,
    permission_admission_source: Option<Arc<dyn PermissionAdmissionSource>>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    skill_catalog: Option<SkillCatalog>,
    project_rules: Option<ProjectRules>,
    task_anchor: Option<TaskAnchor>,
    subagent_manager: Option<SubagentManager>,
}

impl RuntimeProfileBuilder {
    /// Creates an empty runtime profile builder with default-deny capabilities.
    #[must_use]
    pub fn new() -> Self {
        Self {
            capabilities: RuntimeCapabilities::default(),
            model_retry_policy: None,
            progress_commentary: false,
            initial_context_summaries: BTreeMap::new(),
            registered_tools: Vec::new(),
            allow_bridge_tools: false,
            allow_low_risk_workspace_patches: false,
            low_risk_process_runner: None,
            read_only_shell_process_runner: None,
            accepted_local_workspace_process_runner: None,
            runtime_trust_level: None,
            permission_review_mode: None,
            permission_admission_source: None,
            permissioned_process_runner_factory: None,
            skill_catalog: None,
            project_rules: None,
            task_anchor: None,
            subagent_manager: None,
        }
    }

    /// Sets low-level Merry-managed capabilities.
    #[must_use]
    pub fn capabilities(mut self, capabilities: RuntimeCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Sets provider-neutral model retry policy for runtimes using this profile.
    #[must_use]
    pub fn model_retry_policy(mut self, policy: ModelRetryPolicy) -> Self {
        self.model_retry_policy = Some(policy);
        self
    }

    /// Controls whether the profile asks the model to emit brief tool-progress commentary.
    ///
    /// This is prompt guidance only. Runtime state and final output remain
    /// separate from commentary, and model-authored commentary is still recorded
    /// if a provider emits it while this is disabled.
    #[must_use]
    pub fn progress_commentary(mut self, enabled: bool) -> Self {
        self.progress_commentary = enabled;
        self
    }

    /// Adds a startup context summary.
    #[must_use]
    pub fn initial_context_summary(mut self, id: &str, text: &str) -> Self {
        self.initial_context_summaries
            .insert(id.to_owned(), text.to_owned());
        self
    }

    /// Registers a runtime-owned tool in this profile.
    #[must_use]
    pub fn register_tool(mut self, tool: RegisteredTool) -> Self {
        self.registered_tools.push(tool);
        self
    }

    /// Allows bridge tools registered through this profile.
    #[must_use]
    pub fn allow_bridge_tools(mut self) -> Self {
        self.allow_bridge_tools = true;
        self
    }

    /// Allows validated low-risk workspace patch proposals.
    #[must_use]
    pub fn allow_low_risk_workspace_patches(mut self) -> Self {
        self.allow_low_risk_workspace_patches = true;
        self
    }

    /// Sets the low-risk process action runner.
    #[must_use]
    pub fn allow_low_risk_process_actions(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.low_risk_process_runner = Some(runner);
        self
    }

    /// Sets the read-only shell process runner.
    #[must_use]
    pub fn allow_read_only_shell_process_actions(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.read_only_shell_process_runner = Some(runner);
        self
    }

    /// Sets the accepted local workspace process runner lane.
    #[must_use]
    pub fn allow_accepted_local_workspace_process_actions(
        mut self,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
    ) -> Self {
        self.accepted_local_workspace_process_runner = Some(
            AcceptedLocalWorkspaceProcessRunnerProfile::new(admission, runner),
        );
        self
    }

    /// Sets the profile trust level.
    #[must_use]
    pub fn runtime_trust_level(mut self, trust_level: RuntimeTrustLevel) -> Self {
        self.runtime_trust_level = Some(trust_level);
        self
    }

    /// Sets the profile permission review mode.
    #[must_use]
    pub fn permission_review_mode(mut self, mode: PermissionReviewMode) -> Self {
        self.permission_review_mode = Some(mode);
        self
    }

    /// Sets the host-owned permission admission source.
    #[must_use]
    pub fn permission_admission_source(
        mut self,
        source: Arc<dyn PermissionAdmissionSource>,
    ) -> Self {
        self.permission_admission_source = Some(source);
        self
    }

    /// Sets the permissioned process runner factory.
    #[must_use]
    pub fn permissioned_process_runner_factory(
        mut self,
        factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        self.permissioned_process_runner_factory = Some(factory);
        self
    }

    /// Sets the profile skill catalog.
    #[must_use]
    pub fn skill_catalog(mut self, catalog: SkillCatalog) -> Self {
        self.skill_catalog = Some(catalog);
        self
    }

    /// Sets profile project rules.
    #[must_use]
    pub fn project_rules(mut self, project_rules: ProjectRules) -> Self {
        self.project_rules = Some(project_rules);
        self
    }

    /// Sets the profile task anchor.
    #[must_use]
    pub fn task_anchor(mut self, task_anchor: TaskAnchor) -> Self {
        self.task_anchor = Some(task_anchor);
        self
    }

    /// Sets the profile subagent manager.
    #[must_use]
    pub fn subagent_manager(mut self, manager: SubagentManager) -> Self {
        self.subagent_manager = Some(manager);
        self
    }

    /// Builds the runtime profile.
    pub fn build(self) -> Result<RuntimeProfile, RuntimeProfileError> {
        let mut names = BTreeSet::new();
        for tool in &self.registered_tools {
            let name = tool.spec().name().clone();
            if !names.insert(name.clone()) {
                return Err(RuntimeProfileError::DuplicateToolRegistration { name });
            }
        }

        Ok(RuntimeProfile {
            capabilities: self.capabilities,
            model_retry_policy: self.model_retry_policy,
            progress_commentary: self.progress_commentary,
            initial_context_summaries: self.initial_context_summaries,
            registered_tools: self.registered_tools,
            allow_bridge_tools: self.allow_bridge_tools,
            allow_low_risk_workspace_patches: self.allow_low_risk_workspace_patches,
            low_risk_process_runner: self.low_risk_process_runner,
            read_only_shell_process_runner: self.read_only_shell_process_runner,
            accepted_local_workspace_process_runner: self.accepted_local_workspace_process_runner,
            runtime_trust_level: self.runtime_trust_level,
            permission_review_mode: self.permission_review_mode,
            permission_admission_source: self.permission_admission_source,
            permissioned_process_runner_factory: self.permissioned_process_runner_factory,
            skill_catalog: self.skill_catalog,
            project_rules: self.project_rules,
            task_anchor: self.task_anchor,
            subagent_manager: self.subagent_manager,
        })
    }
}

impl Default for RuntimeProfileBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{PathAccess, PathAccessRule, PathAccessRuleSource, RuntimeCapabilities};
    use std::path::Path;

    #[test]
    fn runtime_capabilities_control_network_without_tool_network_field() {
        let capabilities = RuntimeCapabilities::default().allow_network();

        assert!(capabilities.network_allowed());
    }

    #[test]
    fn runtime_capabilities_deny_network_by_default() {
        let capabilities = RuntimeCapabilities::default();

        assert!(!capabilities.network_allowed());
    }

    #[test]
    fn runtime_capabilities_carry_platform_neutral_path_rules() {
        let rule = PathAccessRule::new(
            "/var/log/foo",
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        );
        let capabilities = RuntimeCapabilities::default().with_path_rule(rule);

        assert_eq!(capabilities.path_rules().len(), 1);
        assert_eq!(
            capabilities.path_rules()[0].path(),
            Path::new("/var/log/foo")
        );
        assert_eq!(capabilities.path_rules()[0].access(), PathAccess::ReadOnly);
        assert_eq!(
            capabilities.path_rules()[0].source(),
            PathAccessRuleSource::TrustedGlobalConfig
        );
    }

    #[test]
    fn path_access_has_stable_config_spelling() {
        assert_eq!(PathAccess::ReadOnly.as_str(), "ro");
        assert_eq!(PathAccess::ReadWrite.as_str(), "rw");
        assert_eq!(PathAccess::Deny.as_str(), "deny");
    }
}
