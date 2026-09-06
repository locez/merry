use merry_core::{ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{FinishReason, ModelEvent, ModelOutput, ModelResponse, testing::FakeModelProvider};
use merry_process::{LocalProcessBackend, ProcessSession, TokioProcessRunner};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, PermissionedProcessRunnerFactory, ProcessRunner,
    RegisteredTool, StaticPermissionedProcessRunnerFactory,
};
use schemars::Schema;
use serde_json::json;
use std::sync::Arc;

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

fn completing_provider() -> Arc<FakeModelProvider> {
    Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]))
}

fn process_backend() -> Arc<dyn merry_process::ProcessBackend> {
    let runner: Arc<dyn ProcessRunner> = Arc::new(TokioProcessRunner::new());
    let session = process_session(
        AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
        runner,
    );
    Arc::new(LocalProcessBackend::from_session(session))
}

mod composition;

mod process_policy;

mod profile_contract;
