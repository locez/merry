use crate::support::{
    events::event_kind_names,
    models::{
        BlockingModelProvider, ScriptedModelProvider, completed_text_event,
        completed_tool_call_event, model_name, model_tool_call,
    },
    runtime::{artifact_id, runtime_with_tool, session_id, tool_call_id},
    tools::BlockingToolExecutor,
};
use merry_core::{
    ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, PendingToolCall, ToolCallResult,
    ToolName,
};
use merry_runtime::{
    AgentLoopConfig, AgentLoopStatus, ContextEvidence, ContextSummary, Runtime, RuntimeError,
    StepContext, StepInput,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_rejects_concurrent_pending_consumption_during_tool_execution() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-inter-operation",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let executor = BlockingToolExecutor::new(started_tx, release_rx);
    let runtime = runtime_with_tool(
        "agent-loop-inter-operation-admission",
        provider,
        executor.clone(),
    );
    let loop_runtime = runtime.clone();
    let loop_handle = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop(
                StepInput::user_text("Search notes.").expect("valid step input"),
                StepContext::new(CancellationToken::new()),
                AgentLoopConfig::default(),
            )
            .await
            .expect("agent loop should complete")
    });

    started_rx
        .await
        .expect("executor should signal after tool execution starts");

    assert_eq!(
        runtime.pending_tool_calls().await,
        vec![PendingToolCall::new(
            merry_core::ToolCallId::new("call-inter-operation").expect("valid tool call id"),
            ToolName::new("search_notes").expect("valid tool name"),
            merry_core::ToolCallArguments::try_from(json!({"query": "test query"}))
                .expect("valid tool arguments"),
        )]
    );
    assert_eq!(executor.calls().len(), 1);

    let shadow_result = ToolCallResult::succeeded(
        tool_call_id("call-inter-operation"),
        ArtifactRef::new(
            artifact_id("agent-loop-inter-operation-shadow-result"),
            ArtifactKind::Text,
        ),
    );
    let err = runtime
        .submit_tool_result(
            shadow_result,
            merry_runtime::ArtifactContent::text("should not consume pending\n"),
        )
        .await
        .expect_err("loop-level guard should reject concurrent pending consumption");
    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id("agent-loop-inter-operation-admission")
    ));

    release_tx
        .send(())
        .expect("blocking executor release receiver should still be active");
    let result = loop_handle
        .await
        .expect("agent loop task should not panic after rejected mutation");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(executor.calls().len(), 1);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_running_step_rejects_concurrent_context_summary_write() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingModelProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("agent-loop-context-provider-admission"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    let loop_runtime = runtime.clone();
    let loop_handle = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop(
                StepInput::user_text("Block inside provider.").expect("valid step input"),
                StepContext::new(CancellationToken::new()),
                AgentLoopConfig::default(),
            )
            .await
            .expect("agent loop should complete after provider release")
    });

    started_rx
        .await
        .expect("provider should signal after the loop step starts");

    let err = runtime
        .record_context_summary(
            ContextSummary::new(
                "blocked-summary",
                "Raw context write should wait.",
                Vec::new(),
            )
            .expect("summary construction allows compiler validation"),
        )
        .await
        .expect_err("active provider step should reject direct context summary writes");
    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id("agent-loop-context-provider-admission")
    ));

    release_tx
        .send(())
        .expect("blocking provider release receiver should still be active");
    let result = loop_handle
        .await
        .expect("agent loop task should not panic after release");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    let artifact = ArtifactRef::new(artifact_id("post-run-context-source"), ArtifactKind::Text);
    runtime
        .record_artifact(
            artifact.clone(),
            merry_runtime::ArtifactContent::text("post-run exact evidence\n"),
        )
        .await
        .expect("runtime should accept artifact after the loop completes");
    let evidence = EvidenceRef::new(artifact.id().clone(), EvidenceLocator::whole_artifact());
    runtime
        .record_context_summary(
            ContextSummary::new(
                "post-run-summary",
                "Raw context write may resume.",
                vec![
                    ContextEvidence::new("post-run source", evidence)
                        .expect("context evidence builds"),
                ],
            )
            .expect("summary construction allows compiler validation"),
        )
        .await
        .expect("runtime should accept context summary after the loop completes");
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_tool_execution_rejects_concurrent_context_entry_write() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-context-entry-blocked",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let executor = BlockingToolExecutor::new(started_tx, release_rx);
    let runtime = runtime_with_tool("agent-loop-context-tool-admission", provider, executor);
    let loop_runtime = runtime.clone();
    let loop_handle = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop(
                StepInput::user_text("Search notes.").expect("valid step input"),
                StepContext::new(CancellationToken::new()),
                AgentLoopConfig::default(),
            )
            .await
            .expect("agent loop should complete after tool release")
    });

    started_rx
        .await
        .expect("executor should signal after tool execution starts");

    let err = runtime
        .record_context_entry(merry_runtime::ContextEntry::summary(
            ContextSummary::new("blocked-entry", "Entry write should wait.", Vec::new())
                .expect("summary construction allows compiler validation"),
        ))
        .await
        .expect_err("active tool execution should reject direct context entry writes");
    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id("agent-loop-context-tool-admission")
    ));

    release_tx
        .send(())
        .expect("blocking executor release receiver should still be active");
    let result = loop_handle
        .await
        .expect("agent loop task should not panic after release");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_running_step_rejects_concurrent_runtime_mutation() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingModelProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("agent-loop-active-step-admission"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    let loop_runtime = runtime.clone();
    let loop_handle = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop(
                StepInput::user_text("Block inside provider.").expect("valid step input"),
                StepContext::new(CancellationToken::new()),
                AgentLoopConfig::default(),
            )
            .await
            .expect("agent loop should complete after provider release")
    });

    started_rx
        .await
        .expect("provider should signal after the loop step starts");

    let err = runtime
        .record_artifact(
            ArtifactRef::new(
                artifact_id("agent-loop-concurrent-artifact"),
                ArtifactKind::Text,
            ),
            merry_runtime::ArtifactContent::text("should not record\n"),
        )
        .await
        .expect_err("active loop step should reject concurrent mutation");
    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id("agent-loop-active-step-admission")
    ));

    release_tx
        .send(())
        .expect("blocking provider release receiver should still be active");
    let result = loop_handle
        .await
        .expect("agent loop task should not panic after release");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    let events = runtime
        .record_artifact(
            ArtifactRef::new(
                artifact_id("agent-loop-post-run-artifact"),
                ArtifactKind::Text,
            ),
            merry_runtime::ArtifactContent::text("runtime usable after loop\n"),
        )
        .await
        .expect("runtime should accept mutation after the loop step completes");
    assert_eq!(event_kind_names(&events), ["ArtifactRecorded"]);
}
