use super::*;
use merry_core::{ToolInputSchema, ToolName, ToolSpec};
use merry_llm::ModelRetryPolicy;
use merry_process::{ProcessSession, TokioProcessRunner};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, PermissionedProcessRunnerFactory, ProcessRunner,
    RegisteredTool, StaticPermissionedProcessRunnerFactory,
};
use schemars::Schema;
use serde_json::json;

fn bridge_tool(name: &str) -> RegisteredTool {
    bridge_tool_with_description(name, "Test bridge tool")
}

fn bridge_tool_with_description(name: &str, description: &str) -> RegisteredTool {
    let schema =
        Schema::try_from(json!({ "type": "object" })).expect("test schema should be valid");
    let spec = ToolSpec::new(
        ToolName::new(name).expect("test tool name should be valid"),
        description,
        ToolInputSchema::new(schema).expect("test input schema should be valid"),
    )
    .expect("test tool spec should be valid");
    RegisteredTool::bridge(spec)
}

fn process_session(
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
) -> ProcessSession {
    let permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory> = Arc::new(
        StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );
    ProcessSession::from_parts(admission, runner, permissioned_factory)
}

#[test]
fn workspace_profile_builds_read_tools_and_coding_defaults() {
    let temp = tempfile::tempdir().expect("tempdir should be created");

    let profile = coding_agent(temp.path())
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
    assert!(profile.runtime_profile().progress_commentary());
    assert_eq!(
        profile.runtime_profile().model_retry_policy(),
        Some(ModelRetryPolicy::coding_agent_default())
    );
}

#[test]
fn coding_agent_profile_has_one_canonical_workspace_tool_order() {
    let temp = tempfile::tempdir().expect("tempdir should be created");

    let profile = coding_agent(temp.path())
        .build()
        .expect("coding-agent profile should build");
    let tool_names = profile
        .tool_names()
        .into_iter()
        .map(ToolName::as_str)
        .collect::<Vec<_>>();

    assert_eq!(
        tool_names,
        vec![
            "workspace_read_file",
            "workspace_list_dir",
            "workspace_search_text",
        ]
    );
}

#[test]
fn coding_agent_profile_owns_process_permission_and_patch_order() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let runner: Arc<dyn merry_runtime::ProcessRunner> = Arc::new(TokioProcessRunner::new());
    let profile = coding_agent(temp.path())
        .patch_tool()
        .accepted_process_session(process_session(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
        ))
        .build()
        .expect("coding-agent profile should build");

    let tool_names = profile
        .tool_names()
        .into_iter()
        .map(ToolName::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        tool_names,
        vec![
            "run_process",
            "request_permissions",
            "workspace_read_file",
            "workspace_list_dir",
            "workspace_search_text",
            "workspace_patch",
        ]
    );
}

#[test]
fn coding_agent_profile_hash_is_deterministic_and_tracks_stable_material() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let first = coding_agent(temp.path())
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use the project rules.")
                .expect("project rules should be valid"),
        )
        .task_anchor(TaskAnchor::new("First task").expect("task anchor should be valid"))
        .build()
        .expect("coding-agent profile should build");
    let dynamic_change = coding_agent(temp.path())
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use the project rules.")
                .expect("project rules should be valid"),
        )
        .task_anchor(TaskAnchor::new("Different task").expect("task anchor should be valid"))
        .build()
        .expect("coding-agent profile should build");
    let stable_change = coding_agent(temp.path())
        .project_rules(
            ProjectRules::new("AGENTS.md", "Changed project rules.")
                .expect("project rules should be valid"),
        )
        .task_anchor(TaskAnchor::new("First task").expect("task anchor should be valid"))
        .build()
        .expect("coding-agent profile should build");
    let retry_change = coding_agent(temp.path())
        .retry_policy(ModelRetryPolicy::disabled())
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use the project rules.")
                .expect("project rules should be valid"),
        )
        .task_anchor(TaskAnchor::new("First task").expect("task anchor should be valid"))
        .build()
        .expect("coding-agent profile should build");

    assert_eq!(first.profile_hash(), dynamic_change.profile_hash());
    assert_ne!(first.profile_hash(), stable_change.profile_hash());
    assert_ne!(first.profile_hash(), retry_change.profile_hash());
    assert!(first.profile_hash().as_str().starts_with("fnv1a64:"));
}

#[test]
fn coding_agent_profile_hash_includes_advertised_tool_order() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let first = coding_agent(temp.path())
        .register_tools([bridge_tool("alpha"), bridge_tool("beta")])
        .build()
        .expect("coding-agent profile should build");
    let reordered = coding_agent(temp.path())
        .register_tools([bridge_tool("beta"), bridge_tool("alpha")])
        .build()
        .expect("coding-agent profile should build");

    assert_ne!(first.profile_hash(), reordered.profile_hash());
    assert_eq!(
        first
            .tool_names()
            .into_iter()
            .map(ToolName::as_str)
            .collect::<Vec<_>>(),
        vec![
            "workspace_read_file",
            "workspace_list_dir",
            "workspace_search_text",
            "alpha",
            "beta"
        ]
    );
}

#[test]
fn coding_agent_profile_hash_tracks_tool_schema_and_coding_run_policy() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let base = coding_agent(temp.path())
        .register_tool(bridge_tool("catalog_tool"))
        .build()
        .expect("base profile should build");
    let changed_schema = coding_agent(temp.path())
        .register_tool(bridge_tool_with_description(
            "catalog_tool",
            "Changed description",
        ))
        .build()
        .expect("changed schema profile should build");
    let changed_policy = coding_agent(temp.path())
        .run_policy(
            CodingAgentRunPolicy::new(16, CodingFinalReportPolicy::EvidenceBackedSummary)
                .expect("valid coding run policy"),
        )
        .build()
        .expect("changed policy profile should build");

    assert_ne!(base.profile_hash(), changed_schema.profile_hash());
    assert_ne!(base.profile_hash(), changed_policy.profile_hash());
    assert_eq!(base.run_policy().max_model_turns(), 1024);
    assert_eq!(
        changed_policy
            .loop_config()
            .expect("loop config")
            .max_model_turns(),
        16
    );
    assert_eq!(
        changed_policy.run_policy().final_report().as_str(),
        "evidence_backed_summary"
    );
}

#[test]
fn coding_agent_profile_owns_the_coding_prompt_and_hashes_its_exact_text() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let profile = coding_agent(temp.path())
        .build()
        .expect("coding-agent profile should build");
    let runtime_profile = profile.runtime_profile();
    let prompt = runtime_profile.prompt_profile();

    assert_eq!(prompt.stable_blocks().len(), 1);
    assert_eq!(prompt.stable_blocks()[0].tag(), "merry_coding_policy");
    assert_eq!(prompt.stable_blocks()[0].text(), CODING_AGENT_POLICY_PROMPT);
    assert!(
        prompt.stable_blocks()[0]
            .text()
            .contains("evidence-backed summary")
    );
}

#[test]
fn coding_agent_profile_hash_distinguishes_patch_scope_and_process_admission() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let runner: Arc<dyn merry_runtime::ProcessRunner> = Arc::new(TokioProcessRunner::new());
    let unrestricted = coding_agent(temp.path())
        .patch_tool()
        .build()
        .expect("unrestricted profile should build");
    let read_only = coding_agent(temp.path())
        .patch_tool()
        .read_only_patch_scope()
        .build()
        .expect("read-only profile should build");
    let isolated = coding_agent(temp.path())
        .accepted_process_session(process_session(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            Arc::clone(&runner),
        ))
        .build()
        .expect("isolated profile should build");
    let host = coding_agent(temp.path())
        .accepted_process_session(process_session(
            AcceptedLocalWorkspaceProcessAdmission::accept_host(),
            runner,
        ))
        .build()
        .expect("host profile should build");

    assert_ne!(unrestricted.profile_hash(), read_only.profile_hash());
    assert_ne!(isolated.profile_hash(), host.profile_hash());
}

#[test]
fn workspace_profile_can_enable_patch_tool() {
    let temp = tempfile::tempdir().expect("tempdir should be created");

    let profile = coding_agent(temp.path())
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
