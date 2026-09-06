use crate::support::{
    compaction::{citation_checkpoint_for_provider_tests, seed_history_text_for_compaction},
    events::pending_tool_call,
    models::{
        ScriptedModelProvider, completed_event, completed_outputs_event, completed_text_event,
        model_name, model_tool_call,
    },
    runtime::{artifact_id, collect_step, runtime_with_provider, session_id},
    tools::{ScriptedToolExecutor, test_tool_spec},
};
use merry_core::{ArtifactKind, ArtifactRef};
use merry_llm::{FinishReason, ModelMessageRole, ModelOutput, testing::FakeModelProvider};
use merry_runtime::{
    ArtifactContent, CheckpointRefId, CitationCompactionPolicy, CompactedCheckpoint, ProjectRules,
    RegisteredTool, Runtime, SessionTranscriptItem, StepContext, TaskAnchor, ToolExecutionContext,
};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn empty_checkpoint_slot_renders_no_prompt_text() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = Runtime::builder(session_id("provider-empty-checkpoint-slot"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "No saved state yet.").await;

    let request = provider.recorded_requests()[0].clone();
    assert_eq!(request.messages().len(), 2);
    assert_eq!(request.stable_prefix_message_count(), 1);
    assert!(
        request
            .messages()
            .iter()
            .all(|message| !message.content().as_text().contains("checkpoint:")),
        "empty checkpoint segment must not render prompt text"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_reads_checkpoint_ref_by_checkpoint_and_ref_id() {
    let checkpoint = citation_checkpoint_for_provider_tests(
        "checkpoint-lookup",
        "bootstrap-ref",
        "user rejected resource timelines for this slice",
    );
    let runtime = Runtime::builder(session_id("checkpoint-ref-lookup"))
        .compacted_checkpoint(checkpoint)
        .compacted_checkpoint_evidence(
            ArtifactRef::new(
                artifact_id("provider-checkpoint-source-bootstrap-ref"),
                ArtifactKind::Text,
            ),
            ArtifactContent::text("user rejected resource timelines for this slice"),
        )
        .build()
        .expect("runtime should build");

    let page = runtime
        .read_checkpoint_ref_page(
            &CheckpointRefId::new("bootstrap-ref").expect("valid ref id"),
            0,
            4096,
        )
        .await
        .expect("ref should resolve");

    assert_eq!(
        page.content(),
        "user rejected resource timelines for this slice"
    );
    assert_eq!(
        page.artifact_id().as_str(),
        "provider-checkpoint-source-bootstrap-ref"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_compaction_input_excludes_retained_raw_tail() {
    let provider = FakeModelProvider::new(vec![
        Ok(completed_text_event("old assistant message to compact")),
        Ok(completed_text_event("retained raw tail assistant sentinel")),
    ]);
    let runtime = Runtime::builder(session_id("runtime-compaction-input-tail"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "old user message to compact").await;
    collect_step(&runtime, "retained raw tail user sentinel").await;

    let input = runtime
        .citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .await
        .expect("input builds")
        .expect("old prefix should be compressible");
    let payload = input.to_model_payload_json().expect("payload serializes");

    assert!(payload.contains("old user message to compact"));
    assert!(payload.contains("old assistant message to compact"));
    assert!(!payload.contains("retained raw tail user sentinel"));
    assert!(!payload.contains("retained raw tail assistant sentinel"));
}

#[tokio::test(flavor = "current_thread")]
async fn installed_checkpoint_replaces_old_body_but_keeps_raw_tail_in_next_request() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("old assistant"))],
        vec![Ok(completed_event())],
        vec![Ok(completed_event())],
    ]);
    let runtime = Runtime::builder(session_id("checkpoint-install-request"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "old user").await;
    collect_step(&runtime, "tail user").await;

    let input = runtime
        .citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .await
        .expect("input builds")
        .expect("input exists");

    runtime
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [
                {
                  "id": "c1",
                  "text": "The old request was compacted.",
                  "refs": ["h0", "h1"]
                }
              ],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .await
        .expect("install succeeds");

    assert_eq!(
        runtime
            .session_transcript()
            .await
            .expect("public transcript remains readable after compaction"),
        vec![
            SessionTranscriptItem::UserMessage {
                text: "old user".to_owned(),
                images: Vec::new(),
            },
            SessionTranscriptItem::AssistantText {
                text: "old assistant".to_owned(),
            },
            SessionTranscriptItem::UserMessage {
                text: "tail user".to_owned(),
                images: Vec::new(),
            },
            SessionTranscriptItem::AssistantText {
                text: "model result".to_owned(),
            },
        ]
    );

    collect_step(&runtime, "current user").await;
    let requests = provider.recorded_requests();
    let request = requests.last().expect("request exists");
    let text = request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("compacted-checkpoint:"));
    assert!(text.contains("The old request was compacted."));
    assert!(text.contains("tail user"));
    assert!(text.contains("current user"));
    assert!(!text.contains("\nold user\n"));
}

#[tokio::test(flavor = "current_thread")]
async fn dynamic_context_projection_keeps_checkpoint_tail_and_current_input_outside_stable_prefix()
{
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("covered assistant sentinel"))],
        vec![Ok(completed_text_event("tail assistant one sentinel"))],
        vec![Ok(completed_text_event("tail assistant two sentinel"))],
        vec![Ok(completed_event())],
    ]);
    let runtime = Runtime::builder(session_id("checkpoint-dynamic-projection"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Stable project rules sentinel.")
                .expect("valid project rules"),
        )
        .task_anchor(
            TaskAnchor::new("Keep implementing dynamic context projection.")
                .expect("valid task anchor"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(
        &runtime,
        "covered user sentinel should only remain reachable through checkpoint refs",
    )
    .await;
    collect_step(&runtime, "tail user one sentinel").await;
    collect_step(&runtime, "tail user two sentinel").await;

    let input = runtime
        .citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 2).expect("valid policy"),
        )
        .await
        .expect("input builds")
        .expect("input exists");

    let _outcome = runtime
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [
                {
                  "id": "c1",
                  "text": "The covered request was compacted.",
                  "refs": ["h0", "h1"]
                }
              ],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .await
        .expect("install succeeds");

    collect_step(&runtime, "current user sentinel").await;
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 4);

    let before_compaction = &requests[2];
    let after_compaction = requests.last().expect("request exists");
    assert_eq!(before_compaction.stable_prefix_message_count(), 2);
    assert_eq!(after_compaction.stable_prefix_message_count(), 2);
    assert_eq!(
        before_compaction.stable_prefix_hash(),
        after_compaction.stable_prefix_hash(),
        "checkpoint, raw tail, and current input are dynamic context, not stable prefix"
    );
    assert_ne!(
        before_compaction.dynamic_context_hash(),
        after_compaction.dynamic_context_hash(),
        "installing a checkpoint and adding current input should change dynamic context"
    );

    let stable_text = after_compaction
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(stable_text.contains("Stable project rules sentinel."));
    assert!(!stable_text.contains("compacted-checkpoint:"));
    assert!(!stable_text.contains("tail user one sentinel"));
    assert!(!stable_text.contains("current user sentinel"));

    let dynamic = after_compaction.dynamic_messages();
    assert_eq!(
        dynamic
            .iter()
            .map(|message| message.role())
            .collect::<Vec<_>>(),
        [
            ModelMessageRole::System,
            ModelMessageRole::System,
            ModelMessageRole::User,
            ModelMessageRole::Assistant,
            ModelMessageRole::User,
            ModelMessageRole::Assistant,
            ModelMessageRole::User,
        ]
    );

    let dynamic_text = dynamic
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>();
    assert!(dynamic_text[0].contains("compacted-checkpoint:"));
    assert!(dynamic_text[0].contains("The covered request was compacted."));
    assert!(dynamic_text[0].contains("[h0,h1]"));
    assert!(dynamic_text[1].contains("task-anchor:"));
    assert_eq!(dynamic_text[2], "tail user one sentinel");
    assert_eq!(dynamic_text[3], "tail assistant one sentinel");
    assert_eq!(dynamic_text[4], "tail user two sentinel");
    assert_eq!(dynamic_text[5], "tail assistant two sentinel");
    assert_eq!(dynamic_text[6], "current user sentinel");

    let request_text = dynamic_text.join("\n");
    assert!(!request_text.contains("covered user sentinel"));
    assert!(!request_text.contains("covered assistant sentinel"));
    assert!(after_compaction.continuations().is_empty());

    let ref_page = runtime
        .read_checkpoint_ref_page(&CheckpointRefId::new("h0").expect("valid ref id"), 0, 4096)
        .await
        .expect("checkpoint ref resolves");
    assert!(
        ref_page
            .content()
            .contains("covered user sentinel should only remain reachable through checkpoint refs")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_model_request_excludes_retained_tail_and_tools() {
    let compactor = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("old compacted assistant sentinel"))],
        vec![Ok(completed_text_event("tail assistant sentinel"))],
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::text(
                r#"{
                  "confirmed_decisions": [],
                  "rejected_approaches": [],
                  "constraints_preferences_boundaries": [],
                  "corrected_misunderstandings": [],
                  "durable_conclusions": [
                    {
                      "id": "c1",
                      "text": "Old history was compacted.",
                      "refs": ["h0", "h1"]
                    }
                  ],
                  "open_questions": [],
                  "current_progress_and_next_steps": [],
                  "exact_details": [],
                  "handoffs": []
                }"#,
            )],
            FinishReason::Stop,
        ))],
    ]);
    let runtime = Runtime::builder(session_id("compaction-request-tail"))
        .model_provider(Arc::new(compactor.clone()), model_name())
        .build()
        .expect("runtime builds");

    seed_history_text_for_compaction(
        &runtime,
        "old compacted user sentinel",
        "retained raw tail sentinel",
    )
    .await;

    runtime
        .compact_context_once(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("compaction succeeds");

    let requests = compactor.recorded_requests();
    let request_text = requests[2]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(request_text.contains("old compacted user sentinel"));
    assert!(request_text.contains("old compacted assistant sentinel"));
    assert!(!request_text.contains("retained raw tail sentinel"));
    assert!(requests[2].tools().is_empty());
    assert!(requests[2].continuations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn compacted_checkpoint_renders_before_task_anchor_and_transcript_body() {
    let provider = FakeModelProvider::new(vec![
        Ok(completed_text_event("transcript assistant sentinel")),
        Ok(completed_event()),
    ]);
    let runtime = Runtime::builder(session_id("provider-compacted-checkpoint-order"))
        .task_anchor(TaskAnchor::new("task anchor sentinel").expect("valid task anchor"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("compacted checkpoint sentinel")
                .expect("valid compacted checkpoint"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "transcript user sentinel").await;
    collect_step(&runtime, "current user sentinel").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>();

    let task_anchor_index = messages
        .iter()
        .position(|text| text.contains("task anchor sentinel"))
        .expect("task anchor should render");
    let checkpoint_index = messages
        .iter()
        .position(|text| text.contains("compacted checkpoint sentinel"))
        .expect("compacted checkpoint should render");
    let append_user_index = messages
        .iter()
        .position(|text| text.contains("transcript user sentinel"))
        .expect("transcript user body should render");
    let append_assistant_index = messages
        .iter()
        .position(|text| text.contains("transcript assistant sentinel"))
        .expect("transcript assistant body should render");
    let current_user_index = messages
        .iter()
        .position(|text| text.contains("current user sentinel"))
        .expect("current user input should render");

    assert_eq!(requests[1].stable_prefix_message_count(), 1);
    assert!(checkpoint_index < task_anchor_index);
    assert!(checkpoint_index < append_user_index);
    assert!(append_user_index < append_assistant_index);
    assert!(append_assistant_index < current_user_index);
    assert!(
        messages[checkpoint_index].contains("compacted-checkpoint:"),
        "checkpoint should be marked as compacted checkpoint context"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compacted_checkpoint_does_not_project_unrelated_artifact_payloads() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = Runtime::builder(session_id("provider-compacted-checkpoint-boundary"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("compacted checkpoint payload")
                .expect("valid compacted checkpoint"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    let payload = "unrelated artifact payload sentinel must stay out";
    runtime
        .record_artifact(
            ArtifactRef::new(artifact_id("unrelated-artifact"), ArtifactKind::Text),
            ArtifactContent::text(payload),
        )
        .await
        .expect("artifact should record");

    collect_step(&runtime, "Answer with compacted checkpoint only.").await;

    let request = provider.recorded_requests()[0].clone();
    assert!(request.messages().iter().any(|message| {
        message
            .content()
            .as_text()
            .contains("compacted checkpoint payload")
    }));
    assert!(
        request
            .messages()
            .iter()
            .all(|message| !message.content().as_text().contains(payload)),
        "compacted checkpoint must not sweep unrelated artifact payloads into prompt"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ledger_observations_do_not_enter_prompt_context_by_default() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_event())],
    ]);
    let runtime = Runtime::builder(session_id("provider-ledger-not-projected"))
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("search_notes"),
            Arc::new(ScriptedToolExecutor::succeeding_text(
                "ledger projection sentinel must stay out of prompt\n",
            )),
        ))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Request tool result.").await;
    let pending = pending_tool_call(&pending_events).clone();
    runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");
    collect_step(&runtime, "Use the resolved tool result.").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].stable_prefix_message_count(), 1);
    assert!(
        requests[1].messages().iter().all(|message| {
            let text = message.content().as_text();
            !text.contains("ledger projection sentinel")
                && !text.contains("tool_result_observation")
                && !text.contains("Ledger")
        }),
        "ledger/tool-result observations must not be rendered into prompt messages by default"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn artifact_payloads_do_not_enter_prompt_context_by_default() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-artifact-not-projected", provider.clone());
    let payload = "artifact payload sentinel must stay out of prompt";
    runtime
        .record_artifact(
            ArtifactRef::new(artifact_id("artifact-not-projected"), ArtifactKind::Text),
            ArtifactContent::text(payload),
        )
        .await
        .expect("artifact should record");

    collect_step(&runtime, "Answer without compacted checkpoint context.").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].stable_prefix_message_count(), 1);
    assert!(
        requests[0]
            .messages()
            .iter()
            .all(|message| !message.content().as_text().contains(payload)),
        "recorded artifact payload must not be rendered into prompt messages by default"
    );
}
