use crate::support::{
    models::{
        ScriptedModelProvider, completed_text_event, completed_tool_call_event, model_name,
        model_tool_call,
    },
    runtime::{run_default_loop, runtime_with_tool, session_id},
    tools::ScriptedToolExecutor,
};
use futures_util::StreamExt;
use merry_core::RuntimeJournalEvent;
use merry_llm::ModelMessageRole;
use merry_runtime::{
    AgentLoopConfig, AgentLoopStatus, CitationCompactionPolicy, ProjectRules, Runtime, StepContext,
    StepInput, TaskAnchor,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_preserves_transcript_tool_exchanges_until_compaction() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-first",
            "search_notes",
        )))],
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-second",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final after two tools"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool(
        "agent-loop-tool-exchange-continuity",
        provider.clone(),
        executor,
    );

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Search twice, then answer.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(4).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 3);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].continuations().is_empty());

    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-first"
    );

    assert_eq!(requests[2].continuations().len(), 2);
    assert_eq!(
        requests[2]
            .continuations()
            .iter()
            .map(|continuation| continuation.call().id().as_str())
            .collect::<Vec<_>>(),
        ["call-first", "call-second"]
    );
    assert!(
        requests[1].dynamic_context_hash() != requests[2].dynamic_context_hash(),
        "adding the second tool exchange should change only dynamic request context"
    );
    assert_eq!(
        requests[1].stable_prefix_hash(),
        requests[2].stable_prefix_hash(),
        "tool exchange growth must not move the cacheable stable prefix"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_keeps_tool_exchanges_after_final_answer_until_compaction() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-first",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("first final"))],
        vec![Ok(completed_text_event("second final"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool(
        "agent-loop-tool-exchange-continuity-final",
        provider.clone(),
        executor,
    );

    let first = run_default_loop(&runtime, "Search once.").await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);

    let second = run_default_loop(&runtime, "Answer without compaction.").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[2].continuations().len(),
        1,
        "terminal assistant completion is not compaction; old tool exchanges remain raw"
    );
    assert_eq!(
        requests[2].continuations()[0].call().id().as_str(),
        "call-first"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_removes_only_covered_tool_exchanges_after_successful_install() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-old",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("tail assistant"))],
        vec![Ok(completed_text_event(
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [
                {
                  "id": "c1",
                  "text": "The old tool result was compacted.",
                  "refs": ["h0", "h2"]
                }
              ],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        ))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let runtime = runtime_with_tool(
        "agent-loop-compaction-removes-continuations",
        provider.clone(),
        ScriptedToolExecutor::succeeding_text("old search result"),
    );

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Use the tool once.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(2).expect("valid config"),
        )
        .await
        .expect("loop runs");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    runtime
        .compact_context_once(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("compaction succeeds")
        .expect("compaction runs");

    let stream = runtime
        .step(
            StepInput::user_text("Continue after compaction.").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts");
    let _events: Vec<RuntimeJournalEvent> = stream.collect().await;

    let requests = provider.recorded_requests();
    let final_request = requests.last().expect("final request exists");
    assert!(
        final_request.continuations().is_empty(),
        "covered tool exchanges should be removed only after successful compaction"
    );
    let final_text = final_request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(final_text.contains("The old tool result was compacted."));
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_user_and_assistant_messages_remain_ordered_without_task_anchor() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("first final answer"))],
        vec![Ok(completed_text_event("second final answer"))],
    ]);
    let runtime = Runtime::builder(session_id("agent-loop-transcript-body"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let first = run_default_loop(&runtime, "First user task.").await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "Second user task.").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].stable_prefix_message_count(), 1);
    assert_eq!(requests[1].stable_prefix_message_count(), 1);
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash(),
        "transcript body growth must not move the stable prefix"
    );
    assert_ne!(
        requests[0].dynamic_context_hash(),
        requests[1].dynamic_context_hash(),
        "transcript growth should change only dynamic request context"
    );

    let dynamic = requests[1].dynamic_messages();
    assert_eq!(
        dynamic
            .iter()
            .map(|message| message.role())
            .collect::<Vec<_>>(),
        [
            ModelMessageRole::User,
            ModelMessageRole::Assistant,
            ModelMessageRole::User
        ]
    );
    assert_eq!(dynamic[0].content().as_text(), "First user task.");
    assert_eq!(dynamic[1].content().as_text(), "first final answer");
    assert_eq!(dynamic[2].content().as_text(), "Second user task.");
}

#[tokio::test(flavor = "current_thread")]
async fn task_anchor_is_dynamic_control_segment_before_transcript_body() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("first final answer"))],
        vec![Ok(completed_text_event("second final answer"))],
    ]);
    let runtime = Runtime::builder(session_id("agent-loop-task-anchor"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .task_anchor(TaskAnchor::new("Fix the status text fixture.").expect("valid task anchor"))
        .build()
        .expect("runtime should build");

    let first = run_default_loop(&runtime, "Start work.").await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "Continue.").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].stable_prefix_message_count(),
        1,
        "task anchor is dynamic control context, not stable prefix"
    );
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash(),
        "transcript body growth must not move the stable prefix when task anchor is set"
    );

    let dynamic = requests[1].dynamic_messages();
    assert_eq!(
        dynamic
            .iter()
            .map(|message| message.role())
            .collect::<Vec<_>>(),
        [
            ModelMessageRole::System,
            ModelMessageRole::User,
            ModelMessageRole::Assistant,
            ModelMessageRole::User
        ]
    );
    assert_eq!(
        dynamic[0].content().as_text(),
        "<merry_task_anchor>\ntask-anchor:\nFix the status text fixture.\n</merry_task_anchor>"
    );
    assert_eq!(dynamic[1].content().as_text(), "Start work.");
    assert_eq!(dynamic[2].content().as_text(), "first final answer");
    assert_eq!(dynamic[3].content().as_text(), "Continue.");
}

#[tokio::test(flavor = "current_thread")]
async fn task_anchor_does_not_join_project_rules_stable_prefix() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event("final answer"))]]);
    let runtime = Runtime::builder(session_id("agent-loop-task-anchor-project-rules"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .project_rules(ProjectRules::new("AGENTS.md", "Use project rules.").expect("valid rules"))
        .task_anchor(TaskAnchor::new("Keep this task pinned.").expect("valid task anchor"))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "Work on the pinned task.").await;
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.stable_prefix_message_count(),
        2,
        "only base instructions and project rules belong to the stable prefix"
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("project-rules-source:AGENTS.md")
    );

    let dynamic = request.dynamic_messages();
    assert_eq!(dynamic[0].role(), ModelMessageRole::System);
    assert_eq!(
        dynamic[0].content().as_text(),
        "<merry_task_anchor>\ntask-anchor:\nKeep this task pinned.\n</merry_task_anchor>"
    );
    assert_eq!(dynamic[1].role(), ModelMessageRole::User);
    assert_eq!(dynamic[1].content().as_text(), "Work on the pinned task.");
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_control_prompt_is_not_recorded_as_user_history() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-success",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("first final answer"))],
        vec![Ok(completed_text_event("second final answer"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool(
        "agent-loop-control-prompt-not-history",
        provider.clone(),
        executor,
    );

    let first = run_default_loop(&runtime, "Search once.").await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "Second user task.").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    let final_request_text = requests[2]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert!(
        !final_request_text.contains("Continue after tool result."),
        "agent-loop continuation control prompt must not be recorded as user history"
    );
    assert!(final_request_text.contains("Search once."));
    assert!(final_request_text.contains("first final answer"));
    assert!(final_request_text.contains("Second user task."));
}
