use crate::support::{
    models::{ScriptedModelProvider, model_name},
    tools::tool_spec,
};
use merry_core::{ArtifactId, SessionId, ToolCallId};
use merry_runtime::{
    AgentLoopConfig, Runtime, StepContext, StepInput, ToolActionKind, ToolExecutor,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

pub(crate) fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("valid artifact id")
}

pub(crate) fn tool_call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("valid tool call id")
}

pub(crate) fn runtime_with_provider(session: &str, provider: ScriptedModelProvider) -> Runtime {
    Runtime::builder(session_id(session))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

pub(crate) fn runtime_with_tool(
    session: &str,
    provider: ScriptedModelProvider,
    executor: impl ToolExecutor + 'static,
) -> Runtime {
    Runtime::builder(session_id(session))
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(executor),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

pub(crate) fn runtime_with_tool_action(
    session: &str,
    provider: ScriptedModelProvider,
    executor: impl ToolExecutor + 'static,
    action_kind: ToolActionKind,
) -> Runtime {
    Runtime::builder(session_id(session))
        .register_tool(merry_runtime::RegisteredTool::new(
            tool_spec("search_notes"),
            Arc::new(executor),
            action_kind,
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

pub(crate) fn runtime_with_bridge_tool(session: &str, provider: ScriptedModelProvider) -> Runtime {
    Runtime::builder(session_id(session))
        .allow_bridge_tools()
        .register_tool(merry_runtime::RegisteredTool::bridge(tool_spec(
            "search_notes",
        )))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

pub(crate) async fn run_default_loop(
    runtime: &Runtime,
    text: &str,
) -> merry_runtime::AgentLoopResult {
    runtime
        .run_agent_loop(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .await
        .expect("agent loop should run")
}
