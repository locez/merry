use crate::{
    CodingModelRoleConfig, CodingModelRoleConfigError, CodingPermissionPolicy,
    CodingRuntimeBuildError, CodingRuntimeBuilder, CodingRuntimeInput, CodingSubagentsConfig,
    DEFAULT_CODING_AGENT_MAX_MODEL_TURNS,
    runtime::CodingRuntimePolicy,
    tests::{completing_provider, process_backend},
};
use merry_core::{SessionId, ToolName};
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use merry_runtime::{RuntimeModelRole, StepContext, StepInput, SubagentConfig};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn parent_builder_composes_full_coding_runtime_and_loop_policy() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let provider = completing_provider();
    let provider_input: Arc<dyn ModelProvider> = provider.clone();
    let model = ModelName::new("debug-model").expect("model name should be valid");
    let subagents = CodingSubagentsConfig::enabled(
        SubagentConfig::new(2, 1)
            .expect("subagent limits should be valid")
            .with_model_turn_bounds(2048, 2048)
            .expect("coding subagent bounds should be valid"),
    );
    let input = CodingRuntimeInput::new(
        SessionId::new("coding-parent-builder").expect("session id should be valid"),
        temp.path(),
        provider_input.clone(),
        model.clone(),
        process_backend(),
    )
    .with_automatic_compaction(merry_runtime::AutomaticCompactionConfig::disabled())
    .with_retry_policy(ModelRetryPolicy::disabled())
    .with_model_role(
        CodingModelRoleConfig::new(RuntimeModelRole::ContextCompaction, provider_input, model)
            .expect("secondary model role should be valid"),
    )
    .with_subagents(subagents);

    let coding_runtime = CodingRuntimeBuilder::new(input)
        .build()
        .expect("full coding runtime should build");
    assert_eq!(
        coding_runtime.loop_config().max_model_turns(),
        DEFAULT_CODING_AGENT_MAX_MODEL_TURNS
    );
    let tool_names = coding_runtime
        .profile()
        .tool_names()
        .into_iter()
        .map(ToolName::as_str)
        .collect::<Vec<_>>();
    for tool in [
        "run_process",
        "request_permissions",
        "apply_patch",
        "spawn_subagents",
        "wait_subagents",
        "cancel_subagents",
    ] {
        assert!(tool_names.contains(&tool), "missing composed tool {tool}");
    }

    let loop_config = coding_runtime.loop_config();
    let result = coding_runtime
        .runtime()
        .run_agent_loop(
            StepInput::user_text("Inspect the workspace.").expect("input should be valid"),
            StepContext::default(),
            loop_config,
        )
        .await
        .expect("coding loop should complete");
    assert!(matches!(
        result.status(),
        merry_runtime::AgentLoopStatus::Completed
    ));
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn command_generation_builder_is_read_only_even_with_full_policy_inputs() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let provider = completing_provider();
    let model = ModelName::new("debug-model").expect("model name should be valid");
    let input = CodingRuntimeInput::new(
        SessionId::new("coding-command-builder").expect("session id should be valid"),
        temp.path(),
        provider.clone(),
        model,
        process_backend(),
    )
    .with_subagents(CodingSubagentsConfig::enabled(
        SubagentConfig::new(2, 1)
            .expect("subagent limits should be valid")
            .with_model_turn_bounds(2048, 2048)
            .expect("coding subagent bounds should be valid"),
    ));

    let coding_runtime = CodingRuntimeBuilder::for_command_generation(input)
        .build()
        .expect("command generation runtime should build");
    let tool_names = coding_runtime
        .profile()
        .tool_names()
        .into_iter()
        .map(ToolName::as_str)
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"read_text"));
    for tool in [
        "run_process",
        "request_permissions",
        "apply_patch",
        "spawn_subagents",
        "wait_subagents",
        "cancel_subagents",
    ] {
        assert!(
            !tool_names.contains(&tool),
            "read-only runtime exposed {tool}"
        );
    }

    let result = coding_runtime
        .runtime()
        .run_agent_loop(
            StepInput::user_text("Describe the workspace.").expect("input should be valid"),
            StepContext::default(),
            coding_runtime.loop_config(),
        )
        .await
        .expect("read-only loop should complete");
    assert!(matches!(
        result.status(),
        merry_runtime::AgentLoopStatus::Completed
    ));
}

#[test]
fn full_builder_rejects_missing_process_backend() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let provider = completing_provider();
    let input = CodingRuntimeInput::read_only(
        SessionId::new("coding-missing-process").expect("session id should be valid"),
        temp.path(),
        provider,
        ModelName::new("debug-model").expect("model name should be valid"),
    );

    let error = match CodingRuntimeBuilder::new(input).build() {
        Ok(_) => panic!("full coding runtime must require a process backend"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CodingRuntimeBuildError::MissingProcessBackend
    ));
}

#[test]
fn coding_runtime_policy_rejects_duplicate_model_roles_at_its_boundary() {
    let provider: Arc<dyn ModelProvider> = completing_provider();
    let model = ModelName::new("debug-model").expect("model name should be valid");
    let role = CodingModelRoleConfig::new(
        RuntimeModelRole::ContextCompaction,
        Arc::clone(&provider),
        model,
    )
    .expect("secondary model role should be valid");

    let error = match CodingRuntimePolicy::try_new(
        vec![role.clone(), role],
        CodingPermissionPolicy::default(),
    ) {
        Ok(_) => panic!("duplicate model roles must be rejected by the policy owner"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CodingRuntimeBuildError::DuplicateModelRole {
            role: RuntimeModelRole::ContextCompaction
        }
    ));
}

#[test]
fn parent_builder_rejects_ambiguous_model_roles() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let provider = completing_provider();
    let provider_input: Arc<dyn ModelProvider> = provider.clone();
    let model = ModelName::new("debug-model").expect("model name should be valid");
    let role = CodingModelRoleConfig::new(
        RuntimeModelRole::ContextCompaction,
        provider_input.clone(),
        model.clone(),
    )
    .expect("secondary model role should be valid");
    let duplicate_input = CodingRuntimeInput::read_only(
        SessionId::new("coding-duplicate-role").expect("session id should be valid"),
        temp.path(),
        provider_input.clone(),
        model.clone(),
    )
    .with_model_roles([role.clone(), role]);
    let duplicate_error =
        match CodingRuntimeBuilder::for_command_generation(duplicate_input).build() {
            Ok(_) => panic!("duplicate model roles must be rejected"),
            Err(error) => error,
        };
    assert!(matches!(
        duplicate_error,
        CodingRuntimeBuildError::DuplicateModelRole {
            role: RuntimeModelRole::ContextCompaction
        }
    ));

    let primary_error = match CodingModelRoleConfig::new(RuntimeModelRole::Primary, provider, model)
    {
        Ok(_) => panic!("primary role must be rejected at construction"),
        Err(error) => error,
    };
    assert!(matches!(
        primary_error,
        CodingModelRoleConfigError::PrimaryRole
    ));
}
