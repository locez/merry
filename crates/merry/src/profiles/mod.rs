//! Runtime profile components.

pub use merry_coding::{
    CODING_AGENT_DYNAMIC_CONTEXT_LAYOUT, CODING_AGENT_POLICY_PROMPT, CODING_AGENT_PROFILE_ID,
    CODING_AGENT_STABLE_PREFIX_LAYOUT, CodingAgentProfile, CodingAgentProfileBuildError,
    CodingAgentProfileBuilder, CodingAgentProfileHash, CodingAgentRunPolicy,
    CodingAgentRunPolicyError, CodingFinalReportPolicy, CodingModelRoleConfig,
    CodingModelRoleConfigError, CodingPermissionPolicy, CodingPermissionPolicyError,
    CodingProcessBoundary, CodingRuntime, CodingRuntimeBuildError, CodingRuntimeBuilder,
    CodingRuntimeInput, CodingSubagentsConfig, CodingTrustMode,
    DEFAULT_CODING_AGENT_MAX_MODEL_TURNS, MAX_ROOT_PROJECT_RULES_BYTES, NoSandboxReviewMode,
    ProjectRulesLoadError, ROOT_PROJECT_RULES_FILE, WorkspaceToolLimits, coding_agent,
    load_root_project_rules,
};

pub use merry_process::{ProcessBackend, ProcessSession};

pub use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, PathAccess, PathAccessRule, PathAccessRuleSource,
    RuntimeCapabilities, RuntimeProfile, RuntimeProfileBuilder, RuntimeProfileError,
};

#[cfg(test)]
mod tests {
    use super::coding_agent;

    #[test]
    fn facade_uses_the_shared_coding_agent_profile() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let profile = coding_agent(temp.path())
            .build()
            .expect("shared coding profile should build");

        assert_eq!(
            profile
                .tool_names()
                .into_iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>(),
            [
                "workspace_read_file",
                "workspace_list_dir",
                "workspace_search_text"
            ]
        );
        assert!(profile.profile_hash().as_str().starts_with("fnv1a64:"));
    }
}
