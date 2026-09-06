use crate::support::{
    models::{
        ScriptedModelProvider, completed_text_event, completed_tool_call_event, model_name,
        model_tool_call,
    },
    runtime::{run_default_loop, session_id},
    tools::{ScriptedToolExecutor, tool_spec},
};
use merry_core::ToolCallResultStatus;
use merry_llm::{ModelCapabilities, ModelName};
use merry_runtime::{
    AgentLoopConfig, AgentLoopStatus, AutomaticCompactionConfig, CitationCompactionPolicy,
    ProjectRules, Runtime, RuntimeModelRole, StepContext, StepInput, TaskAnchor,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn provider_step_auto_compacts_before_hard_watermark_request() {
    let primary = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("old assistant sentinel"))],
        vec![Ok(completed_text_event(
            &"tail assistant sentinel ".repeat(350),
        ))],
        vec![Ok(completed_text_event("final after automatic compaction"))],
    ])
    .with_capabilities(
        ModelCapabilities::new(true, true, false, true, Some(8_000), Some(16))
            .expect("valid capabilities"),
    );
    let compactor = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [
            {
              "id": "c1",
              "text": "Old turn was compacted automatically.",
              "refs": ["h0", "h1"]
            }
          ],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }"#,
    ))]]);
    let runtime = Runtime::builder(session_id("agent-loop-auto-compaction-hard-watermark"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/compactor").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(
            CitationCompactionPolicy::new(None, None, 1).expect("valid policy"),
        ))
        .build()
        .expect("runtime should build");

    let first = run_default_loop(&runtime, &"old user sentinel ".repeat(800)).await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "tail user sentinel").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);
    let third = run_default_loop(&runtime, "current user sentinel").await;
    assert_eq!(third.status(), &AgentLoopStatus::Completed);

    assert_eq!(
        compactor.recorded_requests().len(),
        1,
        "runtime should compact before sending the hard-watermark request"
    );
    let compaction_request_text = compactor.recorded_requests()[0]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compaction_request_text.contains("old user sentinel"));
    assert!(!compaction_request_text.contains("tail user sentinel"));
    assert!(!compaction_request_text.contains("current user sentinel"));

    let primary_requests = primary.recorded_requests();
    assert_eq!(primary_requests.len(), 3);
    let final_request = primary_requests.last().expect("final request exists");
    let final_text = final_request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(final_text.contains("compacted-checkpoint:"));
    assert!(final_text.contains("Old turn was compacted automatically."));
    assert!(final_text.contains("tail user sentinel"));
    assert!(final_text.contains("tail assistant sentinel"));
    assert_eq!(
        final_text.matches("current user sentinel").count(),
        1,
        "current input must enter the final primary request exactly once"
    );
    assert!(
        !final_text.contains("old user sentinel"),
        "covered raw history should be replaced by checkpoint projection"
    );
    assert!(
        !final_text.contains("old assistant sentinel"),
        "covered assistant history should be replaced by checkpoint projection"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn auto_compaction_config_controls_retained_model_turns() {
    let primary = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("old assistant configurable tail"))],
        vec![Ok(completed_text_event("tail one assistant"))],
        vec![Ok(completed_text_event(&"tail two assistant ".repeat(300)))],
        vec![Ok(completed_text_event(
            "final after configurable automatic compaction",
        ))],
    ])
    .with_capabilities(
        ModelCapabilities::new(true, true, false, true, Some(8_000), Some(16))
            .expect("valid capabilities"),
    );
    let compactor = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [
            {
              "id": "c1",
              "text": "Only the old configurable-tail turn was compacted.",
              "refs": ["h0", "h1"]
            }
          ],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }"#,
    ))]]);
    let policy = CitationCompactionPolicy::new(Some(192), Some(8192), 2).expect("valid policy");
    let runtime = Runtime::builder(session_id("agent-loop-auto-compaction-config-tail"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/compactor").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(policy))
        .build()
        .expect("runtime should build");

    let first = run_default_loop(&runtime, &"old configurable tail user ".repeat(650)).await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "tail one user").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);
    let third = run_default_loop(&runtime, "tail two user").await;
    assert_eq!(third.status(), &AgentLoopStatus::Completed);
    let fourth = run_default_loop(&runtime, "current configurable tail user").await;
    assert_eq!(fourth.status(), &AgentLoopStatus::Completed);

    assert_eq!(compactor.recorded_requests().len(), 1);
    let compaction_request_text = compactor.recorded_requests()[0]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compaction_request_text.contains("old configurable tail user"));
    assert!(!compaction_request_text.contains("tail one user"));
    assert!(!compaction_request_text.contains("tail two user"));
    assert!(!compaction_request_text.contains("current configurable tail user"));

    let primary_requests = primary.recorded_requests();
    assert_eq!(primary_requests.len(), 4);
    let final_text = primary_requests
        .last()
        .expect("final request exists")
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(final_text.contains("Only the old configurable-tail turn was compacted."));
    assert!(final_text.contains("tail one user"));
    assert!(final_text.contains("tail one assistant"));
    assert!(final_text.contains("tail two user"));
    assert!(final_text.contains("tail two assistant"));
    assert!(final_text.contains("current configurable tail user"));
    assert!(!final_text.contains("old configurable tail user"));
    assert!(!final_text.contains("old assistant configurable tail"));
}

#[tokio::test(flavor = "current_thread")]
async fn auto_compaction_keeps_current_tool_turn_raw_during_continuation() {
    let original_task = format!(
        "original long task sentinel {}",
        "keep-this-exact-task ".repeat(80)
    );
    let primary = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-auto-compact-continuation",
            "search_notes",
        )))],
        vec![Ok(completed_text_event(
            "final after compacted continuation",
        ))],
    ])
    .with_capabilities(
        ModelCapabilities::new(true, true, false, true, Some(4_000), Some(16))
            .expect("valid capabilities"),
    );
    let compactor = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [
            {
              "id": "c1",
              "text": "The opening task turn was compacted before tool continuation.",
              "refs": ["h0"]
            }
          ],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }"#,
    ))]]);
    let policy = CitationCompactionPolicy::new(Some(192), Some(8192), 1).expect("valid policy");
    let runtime = Runtime::builder(session_id("agent-loop-auto-compaction-keeps-original-task"))
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(ScriptedToolExecutor::succeeding_text(
                "tool result sentinel\n",
            )),
        ))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/compactor").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(policy))
        .build()
        .expect("runtime should build");

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&original_task).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(2).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(
        compactor.recorded_requests().len(),
        0,
        "the current user/tool/result turn must stay whole instead of being partly compacted"
    );

    let primary_requests = primary.recorded_requests();
    assert_eq!(primary_requests.len(), 2);
    let continuation_request = &primary_requests[1];
    let continuation_request_text = continuation_request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert!(!continuation_request_text.contains("compacted-checkpoint:"));
    assert!(!continuation_request_text.contains("Continue after tool result."));
    assert!(!continuation_request_text.contains("Original task:"));
    assert!(
        continuation_request_text.contains(&original_task),
        "the user text must stay raw while its tool exchange remains in the retained turn"
    );
    assert_eq!(continuation_request.continuations().len(), 1);
    let continuation = &continuation_request.continuations()[0];
    assert_eq!(
        continuation.call().id().as_str(),
        "call-auto-compact-continuation"
    );
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(
        continuation.result().content().as_text(),
        Some("tool result sentinel\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn auto_compacted_agent_loop_continuation_keeps_checkpoint_refs_and_stable_prefix() {
    let original_task = format!(
        "long coding loop task sentinel {}",
        "inspect-read-patch-verify ".repeat(80)
    );
    let primary = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event(&format!(
            "prelude assistant sentinel {}",
            "prelude ballast ".repeat(700)
        )))],
        vec![Ok(completed_text_event(&format!(
            "retained prelude assistant {}",
            "retained prelude ballast ".repeat(450)
        )))],
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-covered-search",
            "search_notes",
        )))],
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-retained-search",
            "search_notes",
        )))],
        vec![Ok(completed_text_event(
            "final after checkpointed continuation",
        ))],
    ])
    .with_capabilities(
        ModelCapabilities::new(true, true, false, true, Some(8_000), Some(16))
            .expect("valid capabilities"),
    );
    let compactor = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event(
            r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [
            {
              "id": "c1",
              "text": "The prelude turn was checkpointed.",
              "refs": ["h0", "h1"]
            }
          ],
          "open_questions": [],
          "current_progress_and_next_steps": [
            {
              "id": "c2",
              "text": "Continue from the compacted prelude into the raw current task.",
              "refs": ["h0"]
            }
          ],
          "exact_details": [],
          "handoffs": []
        }"#,
        ))],
        vec![Ok(completed_text_event(
            r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [
            {
              "id": "c3",
              "text": "The prior checkpoint and first covered tool result were checkpointed.",
              "refs": ["h0", "h1", "h4", "h6"]
            }
          ],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": [
            {
              "action": "replace",
              "old_id": "c1",
              "new_ids": ["c3"],
              "reason": "The new conclusion combines the prior checkpoint with newly covered evidence."
            },
            {
              "action": "drop",
              "old_id": "c2",
              "reason": "The next step was completed by the newly covered tool exchange."
            }
          ]
        }"#,
        ))],
    ]);
    let policy = CitationCompactionPolicy::new(Some(192), Some(8192), 1).expect("valid policy");
    let runtime = Runtime::builder(session_id("agent-loop-auto-compaction-checkpoint-refs"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Stable prefix rules sentinel.")
                .expect("valid project rules"),
        )
        .task_anchor(
            TaskAnchor::new("Complete the disposable coding-loop fixture.")
                .expect("valid task anchor"),
        )
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(ScriptedToolExecutor::succeeding_texts(vec![
                format!(
                    "covered tool result sentinel {}\n",
                    "covered-result-evidence ".repeat(90)
                ),
                format!(
                    "retained tool result sentinel {}\n",
                    "retained-result-evidence ".repeat(350)
                ),
            ])),
        ))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/compactor").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(policy))
        .build()
        .expect("runtime should build");

    let prelude = run_default_loop(&runtime, "prelude user sentinel").await;
    assert_eq!(prelude.status(), &AgentLoopStatus::Completed);
    let retained_prelude = run_default_loop(&runtime, "retained prelude user sentinel").await;
    assert_eq!(retained_prelude.status(), &AgentLoopStatus::Completed);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&original_task).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(3).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 3);
    assert_eq!(compactor.recorded_requests().len(), 2);

    let compactor_requests = compactor.recorded_requests();
    let first_compaction_request_text = compactor_requests[0]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(first_compaction_request_text.contains("prelude user sentinel"));
    assert!(first_compaction_request_text.contains("prelude assistant sentinel"));
    assert!(!first_compaction_request_text.contains("long coding loop task sentinel"));
    assert!(!first_compaction_request_text.contains("covered tool result sentinel"));
    assert!(!first_compaction_request_text.contains("Continue after tool result."));

    let second_compaction_request_text = compactor_requests[1]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(second_compaction_request_text.contains("The prelude turn was checkpointed."));
    assert!(second_compaction_request_text.contains("long coding loop task sentinel"));
    assert!(second_compaction_request_text.contains("covered tool result sentinel"));
    assert!(!second_compaction_request_text.contains("retained tool result sentinel"));
    assert!(!second_compaction_request_text.contains("Continue after tool result."));

    let primary_requests = primary.recorded_requests();
    assert_eq!(primary_requests.len(), 5);
    let opening_request = &primary_requests[2];
    let first_continuation_request = &primary_requests[3];
    let final_continuation_request = &primary_requests[4];
    assert_eq!(
        opening_request.stable_prefix_hash(),
        final_continuation_request.stable_prefix_hash(),
        "auto-installed checkpoints and continuations must not move stable prefix"
    );
    assert!(
        first_continuation_request.dynamic_context_hash()
            != final_continuation_request.dynamic_context_hash(),
        "checkpoint projection and continuation should change dynamic context"
    );
    assert!(
        final_continuation_request
            .continuations()
            .iter()
            .all(|continuation| continuation.call().id().as_str() != "call-covered-search"),
        "covered tool continuation should be removed after successful auto compaction"
    );
    assert_eq!(
        final_continuation_request.continuations().len(),
        1,
        "latest retained tool continuation should remain raw after compaction"
    );
    assert_eq!(
        final_continuation_request.continuations()[0]
            .call()
            .id()
            .as_str(),
        "call-retained-search"
    );

    let stable_text = final_continuation_request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(stable_text.contains("Stable prefix rules sentinel."));
    assert!(!stable_text.contains("compacted-checkpoint:"));

    let dynamic_text = final_continuation_request
        .dynamic_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(dynamic_text.contains("task-anchor:"));
    assert!(dynamic_text.contains("compacted-checkpoint:"));
    assert!(
        dynamic_text
            .contains("The prior checkpoint and first covered tool result were checkpointed.")
    );
    assert!(!dynamic_text.contains("The prelude turn was checkpointed."));
    assert!(!dynamic_text.contains("Continue after tool result."));
    assert!(!dynamic_text.contains("Original task:"));
    assert!(
        !dynamic_text.contains(&original_task),
        "covered task text should stay behind checkpoint refs after rolling compaction"
    );
    assert!(!dynamic_text.contains("covered tool result sentinel"));

    let ref_page = runtime
        .read_checkpoint_ref_page(
            &merry_runtime::CheckpointRefId::new("h6").expect("valid ref id"),
            0,
            4096,
        )
        .await
        .expect("checkpoint ref resolves");
    assert!(ref_page.content().contains("covered tool result sentinel"));
}

#[tokio::test(flavor = "current_thread")]
async fn auto_compaction_config_can_disable_hard_watermark_compaction() {
    let primary = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("old assistant no auto compaction"))],
        vec![Ok(completed_text_event(
            "final without automatic compaction",
        ))],
    ])
    .with_capabilities(
        ModelCapabilities::new(true, true, false, true, Some(360), Some(16))
            .expect("valid capabilities"),
    );
    let compactor = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [
            {
              "id": "c1",
              "text": "This checkpoint should not be requested.",
              "refs": ["h0"]
            }
          ],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }"#,
    ))]]);
    let runtime = Runtime::builder(session_id("agent-loop-auto-compaction-disabled"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/compactor").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime should build");

    let first = run_default_loop(&runtime, &"old no auto compaction user ".repeat(70)).await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "current no auto compaction user").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    assert!(
        compactor.recorded_requests().is_empty(),
        "disabled automatic compaction must not call the compactor"
    );
    let primary_requests = primary.recorded_requests();
    assert_eq!(primary_requests.len(), 2);
    let final_text = primary_requests[1]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!final_text.contains("compacted-checkpoint:"));
    assert!(final_text.contains("old no auto compaction user"));
    assert!(final_text.contains("current no auto compaction user"));
}
