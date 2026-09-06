use crate::{
    coding::{
        ActionProcessBackend, CodingSubagentsConfig, HeadlessCodingRuntimeInput,
        fixed_process_backend,
    },
    testing::{FakeProcessRunner, model_name},
};
use merry_core::{RuntimeJournalEvent, ToolCallResult};
use merry_llm::ModelProvider;
use merry_process::ProcessSession;
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, PermissionedProcessRunnerFactory, ProcessRunner,
};
use std::{path::Path, sync::Arc};

fn headless_input<'a>(
    session_id: &'a str,
    root: &'a Path,
    provider: Arc<dyn ModelProvider>,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
) -> HeadlessCodingRuntimeInput<'a> {
    HeadlessCodingRuntimeInput {
        session_id,
        root,
        provider,
        model: model_name(),
        process_backend: fixed_process_backend(ProcessSession::from_parts(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
            permissioned_process_runner_factory,
        )),
        extra_tools: Vec::new(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
        workspace_tool_limits: None,
    }
}

fn test_process_backend() -> ActionProcessBackend {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
    let factory: Arc<dyn PermissionedProcessRunnerFactory> = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );
    fixed_process_backend(ProcessSession::from_parts(
        AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
        runner,
        factory,
    ))
}

mod composition;

mod project_rules;

mod skills;

mod subagents;

fn resolved_tool_result(events: &[RuntimeJournalEvent]) -> &ToolCallResult {
    events
        .iter()
        .find_map(|event| match &event.payload {
            merry_core::RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("events should include a resolved tool result")
}
