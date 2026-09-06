use crate::support::{
    models::{ScriptedModelProvider, model_name},
    tools::{ScriptedToolExecutor, test_tool_spec},
};
use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, RuntimeJournalEvent, SessionId,
};
use merry_llm::testing::FakeModelProvider;
use merry_runtime::{
    ArtifactContent, ContextCompiler, ContextEvidence, ContextSummary, RegisteredTool, Runtime,
    StepContext, StepInput, ToolActionKind,
};
use std::{num::NonZeroUsize, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(crate) fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

pub(crate) fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("valid artifact id")
}

pub(crate) fn runtime_with_provider(session: &str, provider: FakeModelProvider) -> Runtime {
    Runtime::builder(session_id(session))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

pub(crate) fn runtime_with_provider_event_buffer(
    session: &str,
    provider: FakeModelProvider,
    event_buffer_size: usize,
) -> Runtime {
    Runtime::builder(session_id(session))
        .event_buffer_size(NonZeroUsize::new(event_buffer_size).expect("non-zero buffer"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

pub(crate) fn runtime_with_scripted_provider(
    session: &str,
    provider: ScriptedModelProvider,
) -> Runtime {
    Runtime::builder(session_id(session))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

pub(crate) async fn collect_step(runtime: &Runtime, text: &str) -> Vec<RuntimeJournalEvent> {
    collect_step_with_context(runtime, text, StepContext::new(CancellationToken::new())).await
}

pub(crate) async fn collect_step_with_context(
    runtime: &Runtime,
    text: &str,
    context: StepContext,
) -> Vec<RuntimeJournalEvent> {
    runtime
        .step(
            StepInput::user_text(text).expect("valid step input"),
            context,
        )
        .expect("step should start")
        .collect()
        .await
}

pub(crate) async fn record_valid_context(runtime: &Runtime) -> String {
    let artifact = ArtifactRef::new(
        artifact_id("provider-boundary-context-artifact"),
        ArtifactKind::Text,
    );
    runtime
        .record_artifact(
            artifact.clone(),
            ArtifactContent::text("alpha\nbeta\ngamma\n"),
        )
        .await
        .expect("artifact should record through eventful path");
    let evidence = runtime
        .evidence_ref(
            artifact.id(),
            EvidenceLocator::line_range(2, 3).expect("valid line range"),
        )
        .await
        .expect("evidence should resolve");

    runtime
        .record_context_summary(
            ContextSummary::new(
                "provider-boundary-summary",
                "Provider boundary context is compiled.",
                vec![
                    ContextEvidence::new("selected lines", evidence)
                        .expect("valid context evidence"),
                ],
            )
            .expect("valid context summary"),
        )
        .await
        .expect("context summary should record");

    ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("context should compile")
        .to_snapshot()
}

pub(crate) fn runtime_with_registered_tool(
    session: &str,
    provider: ScriptedModelProvider,
    executor: ScriptedToolExecutor,
) -> Runtime {
    Runtime::builder(session_id(session))
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("search_notes"),
            Arc::new(executor),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

pub(crate) fn runtime_with_registered_tool_action(
    session: &str,
    provider: ScriptedModelProvider,
    executor: ScriptedToolExecutor,
    action_kind: ToolActionKind,
) -> Runtime {
    Runtime::builder(session_id(session))
        .register_tool(RegisteredTool::new(
            test_tool_spec("search_notes"),
            Arc::new(executor),
            action_kind,
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}
