use crate::{
    ArchiveConfig, CitationCompactionPolicy, FileSessionStore, HistoryRetention, RuntimeModelRole,
    SessionTranscriptItem, StepContext, UserImageInput, UserMessageInput,
    durable_memory::condense::{condense_turn_range_with_offset, count_external_turns},
    runtime::{
        Runtime,
        tests::support::{
            common::{completed_event_with, model_name, named_model, session_id},
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
        },
    },
    session::{TranscriptItemSnapshot, UserInputOrigin},
};
use merry_core::{
    PendingToolCall, ToolCallArguments, ToolCallId, ToolCallResult, ToolCallResultStatus, ToolName,
    ToolOutput,
};
use merry_llm::{FinishReason, ModelCapabilities, ModelOutput};
use std::{num::NonZeroUsize, path::Path, sync::Arc};

fn test_png(byte: u8) -> UserImageInput {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&[byte; 56]);
    UserImageInput::png("[Image #1]", bytes, 16, 16).expect("valid test png")
}

fn compactor_provider(summaries: Vec<&str>) -> RecordingModelProvider {
    let responses = summaries
        .into_iter()
        .map(|text| {
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                vec![ModelOutput::text(text)],
                FinishReason::Stop,
            ))])
        })
        .collect();
    RecordingModelProvider::with_script_and_capabilities(
        responses,
        ModelCapabilities::new(true, true, false, true, Some(256_000), None)
            .expect("valid compactor capabilities"),
    )
}

fn primary_provider() -> RecordingModelProvider {
    RecordingModelProvider::with_script_and_capabilities(
        Vec::new(),
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("valid primary capabilities"),
    )
}

fn compaction_policy() -> CitationCompactionPolicy {
    CitationCompactionPolicy::new(Some(512), Some(16_384), 1).expect("valid policy")
}

async fn count_png_files(images_dir: &Path) -> usize {
    let Ok(mut entries) = tokio::fs::read_dir(images_dir).await else {
        return 0;
    };
    let mut count = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".png"))
        {
            count += 1;
        }
    }
    count
}

#[tokio::test]
async fn context_only_in_memory_evicts_compacted_turns_without_disk_store() {
    let id = session_id("retention-context-only-mem");
    let runtime = Runtime::builder(id)
        .history_retention(HistoryRetention::context_only())
        .model_provider(Arc::new(primary_provider()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor_provider(vec![
                "Turn one discussed deployment architecture.",
            ])),
            named_model("fake/compactor"),
        )
        .build()
        .expect("runtime builds");

    {
        let mut session = runtime.inner.session.lock().await;
        for text in ["first turn to evict", "second turn stays hot"] {
            let turn_id = session.begin_model_turn().expect("turn begins");
            session
                .record_user_message_body(turn_id, text)
                .expect("user message records");
            session
                .close_model_response(turn_id, false)
                .expect("turn completes");
        }
    }

    let outcome = runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect("compaction succeeds")
        .expect("compaction installs");
    assert_eq!(outcome.covered_model_turn_count(), 1);

    let session = runtime.inner.session.lock().await;
    let items = session.full_transcript_snapshot();
    assert_eq!(items.len(), 1);
    assert!(matches!(
        &items[0],
        TranscriptItemSnapshot::UserMessage { text, .. } if text == "second turn stays hot"
    ));
    assert_eq!(session.pruned_external_turns(), 1);
    assert!(session.compacted_checkpoint_summary().is_some());
}

#[tokio::test]
async fn context_only_persisted_rotates_journal_deletes_orphan_images_and_resumes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let id = session_id("retention-context-only-disk");

    let runtime = Runtime::builder(id.clone())
        .session_store(store.clone())
        .history_retention(HistoryRetention::context_only())
        .model_provider(Arc::new(primary_provider()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor_provider(vec![
                "Turn one inspected screenshot A and concluded the layout bug.",
            ])),
            named_model("fake/compactor"),
        )
        .build()
        .expect("runtime builds");

    {
        let mut session = runtime.inner.session.lock().await;
        let t1 = session.begin_model_turn().expect("turn 1 begins");
        let msg1 = UserMessageInput::new("old turn with image A [Image #1]", vec![test_png(0xAA)])
            .expect("valid message 1");
        session
            .record_user_message(t1, &msg1)
            .expect("turn 1 user records");
        session
            .close_model_response(t1, false)
            .expect("turn 1 completes");

        let t2 = session.begin_model_turn().expect("turn 2 begins");
        let msg2 =
            UserMessageInput::new("active tail with image B [Image #1]", vec![test_png(0xBB)])
                .expect("valid message 2");
        session
            .record_user_message(t2, &msg2)
            .expect("turn 2 user records");
        session
            .close_model_response(t2, false)
            .expect("turn 2 completes");
    }

    runtime.save_session().await.expect("initial save succeeds");
    let images_dir = store.session_dir(&id).join("images");
    assert_eq!(count_png_files(&images_dir).await, 2);

    runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect("compaction succeeds")
        .expect("compaction installs");

    // Orphan image A must be deleted while hot image B remains.
    assert_eq!(count_png_files(&images_dir).await, 1);
    assert!(
        !store.session_dir(&id).join("archive").exists(),
        "context_only must not create an archive directory"
    );
    assert!(
        !store.session_dir(&id).join("compaction").exists(),
        "context_only must not create a legacy compaction directory"
    );

    let journal_text = String::from_utf8(
        store
            .read_journal_bytes(&id)
            .await
            .expect("rotated journal reads"),
    )
    .expect("journal is UTF-8");
    assert!(!journal_text.contains("old turn with image A"));
    assert!(journal_text.contains("active tail with image B"));

    // Resume from the rotated compact journal and verify invariants hold.
    let resumed = Runtime::builder(id.clone())
        .history_retention(HistoryRetention::context_only())
        .model_provider(Arc::new(primary_provider()), model_name())
        .resume_from_store(store)
        .await
        .expect("resumed runtime loads rotated journal");
    let resumed_session = resumed.inner.session.lock().await;
    assert_eq!(resumed_session.full_transcript_snapshot().len(), 1);
    assert_eq!(resumed_session.pruned_external_turns(), 1);
    assert!(resumed_session.compacted_checkpoint_summary().is_some());
}

#[tokio::test]
async fn archive_rotates_exports_markdown_with_time_range_pages_transcript_and_enforces_limits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let id = session_id("retention-archive");
    let retention = HistoryRetention::archive_with(
        ArchiveConfig::new()
            .with_max_segments(NonZeroUsize::new(1))
            .with_export_markdown(true),
    );

    let runtime = Runtime::builder(id.clone())
        .session_store(store.clone())
        .history_retention(retention)
        .model_provider(Arc::new(primary_provider()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor_provider(vec![
                "Investigated Cargo workspace layout and read diagnostics.",
                "Confirmed patch compilation and verified unit tests.",
            ])),
            named_model("fake/compactor"),
        )
        .build()
        .expect("runtime builds");

    let old_call_id = ToolCallId::new("call-archived-1").expect("valid call id");
    let large_output_text = format!("DIAGNOSTIC_HEADER\n{}", "x".repeat(3000));

    {
        let mut session = runtime.inner.session.lock().await;
        let t1 = session.begin_model_turn().expect("turn 1 begins");
        let msg1 = UserMessageInput::new(
            "Check the Cargo workspace diagnostics [Image #1]",
            vec![test_png(0x11)],
        )
        .expect("valid message 1");
        session
            .record_user_message(t1, &msg1)
            .expect("turn 1 user records");
        let args = serde_json::json!({"path": "Cargo.toml"})
            .as_object()
            .cloned()
            .expect("json object");
        let call = PendingToolCall::new(
            old_call_id.clone(),
            ToolName::new("read_diagnostics").expect("valid tool name"),
            ToolCallArguments::new(args),
        );
        session
            .record_model_tool_call_response(t1, None, vec![call])
            .expect("tool call records and awaits tool results");
        let result =
            ToolCallResult::new(old_call_id.clone(), ToolCallResultStatus::Succeeded, None)
                .expect("valid tool result");
        session
            .submit_tool_result(result, ToolOutput::text(large_output_text.clone()))
            .expect("tool result resolves turn 1");

        let t2 = session.begin_model_turn().expect("turn 2 begins");
        session
            .record_user_message_body(t2, "Now verify the patch build")
            .expect("turn 2 user records");
        session
            .close_model_response(t2, false)
            .expect("turn 2 completes");
    }

    runtime.save_session().await.expect("initial save succeeds");
    runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect("first compaction succeeds")
        .expect("first compaction installs");

    let archive_dir = store.session_dir(&id).join("archive");
    let seg1_jsonl = archive_dir.join("seg_0001.jsonl");
    let seg1_md = archive_dir.join("seg_0001.md");
    let index_md = archive_dir.join("INDEX.md");
    assert!(seg1_jsonl.exists(), "seg_0001.jsonl must be written");
    assert!(seg1_md.exists(), "seg_0001.md must be written");
    assert!(index_md.exists(), "INDEX.md must be written");
    assert!(
        !store.session_dir(&id).join("compaction").exists(),
        "archive mode must write a single unified Markdown archive instead of duplicate compaction/ files"
    );

    let seg1_md_text = tokio::fs::read_to_string(&seg1_md)
        .await
        .expect("seg_0001.md reads");
    assert!(seg1_md_text.contains("# Session Archive — seg_0001"));
    assert!(seg1_md_text.contains("- **Time (UTC)**:"));
    assert!(seg1_md_text.contains("Investigated Cargo workspace layout"));
    assert!(seg1_md_text.contains("read_diagnostics"));
    assert!(seg1_md_text.contains("Output truncated"));

    let index_text = tokio::fs::read_to_string(&index_md)
        .await
        .expect("INDEX.md reads");
    assert!(
        index_text.contains("| Segment | Time (UTC) | Model Turns | Summary | Keywords | Files |")
    );
    assert!(index_text.contains("`seg_0001`"));

    let snapshot = runtime.context_snapshot().await;
    let checkpoint = snapshot
        .compacted_checkpoint_for_tests()
        .expect("checkpoint exists");
    assert!(
        checkpoint.text().contains("archive/INDEX.md"),
        "checkpoint summary should point agents to unified archive/INDEX.md: {}",
        checkpoint.text()
    );

    assert_eq!(
        runtime
            .archived_segment_numbers()
            .await
            .expect("archived segment numbers"),
        vec![1]
    );
    let paged_items = runtime
        .read_archived_segment_transcript(1)
        .await
        .expect("archived segment transcript reads");
    assert!(paged_items.iter().any(|item| matches!(
        item,
        SessionTranscriptItem::UserMessage { text, .. }
            if text.contains("Check the Cargo workspace diagnostics")
    )));

    // Image from turn 1 is still referenced by seg_0001.jsonl, so GC keeps it.
    let images_dir = store.session_dir(&id).join("images");
    assert_eq!(count_png_files(&images_dir).await, 1);

    // Hot transcript pruned turn 1, yet read_tool_output falls back to archive!
    let full_output = runtime
        .read_tool_output(&old_call_id)
        .await
        .expect("archive lookup succeeds")
        .expect("archived tool output found");
    assert_eq!(full_output.as_str(), large_output_text);

    let preview = runtime
        .read_tool_output_preview(&old_call_id, 32)
        .await
        .expect("archive preview lookup succeeds")
        .expect("archived preview found");
    assert!(preview.content().starts_with("DIAGNOSTIC_HEADER"));
    assert!(preview.truncated());

    // Resume from disk and ensure the compact journal + archive fallback both work.
    let resumed = Runtime::builder(id.clone())
        .history_retention(retention)
        .model_provider(Arc::new(primary_provider()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor_provider(vec![
                "Confirmed patch compilation and verified unit tests.",
            ])),
            named_model("fake/compactor"),
        )
        .resume_from_store(store.clone())
        .await
        .expect("resumed runtime builds");
    assert_eq!(
        resumed
            .read_tool_output(&old_call_id)
            .await
            .expect("resumed archive lookup succeeds")
            .expect("output still found after resume")
            .as_str(),
        large_output_text
    );

    // Add turn 3 and trigger a second compaction; max_segments=1 must evict seg_0001 + its image.
    {
        let mut session = resumed.inner.session.lock().await;
        let t3 = session.begin_model_turn().expect("turn 3 begins");
        session
            .record_user_message_body(t3, "Third turn stays active")
            .expect("turn 3 user records");
        session
            .close_model_response(t3, false)
            .expect("turn 3 completes");
    }
    resumed
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect("second compaction succeeds")
        .expect("second compaction installs");

    assert!(
        !seg1_jsonl.exists(),
        "seg_0001.jsonl must be evicted by max_segments=1"
    );
    assert!(
        !seg1_md.exists(),
        "seg_0001.md must be evicted by max_segments=1"
    );
    assert!(archive_dir.join("seg_0002.jsonl").exists());
    assert!(archive_dir.join("seg_0002.md").exists());

    let updated_index = tokio::fs::read_to_string(&index_md)
        .await
        .expect("updated INDEX.md reads");
    assert!(!updated_index.contains("`seg_0001`"));
    assert!(updated_index.contains("`seg_0002`"));
    assert_eq!(
        count_png_files(&images_dir).await,
        0,
        "orphan image from evicted seg_0001 must be garbage-collected"
    );
    assert!(
        resumed
            .read_tool_output(&old_call_id)
            .await
            .expect("lookup after eviction succeeds")
            .is_none()
    );
}

#[test]
fn dream_condense_with_pruned_offset_keeps_monotonic_external_turn_numbers() {
    let surviving = vec![
        TranscriptItemSnapshot::UserMessage {
            text: "turn three prompt".to_owned(),
            images: Vec::new(),
            origin: UserInputOrigin::ExternalUser,
        },
        TranscriptItemSnapshot::AssistantText {
            text: "turn three reply".to_owned(),
        },
        TranscriptItemSnapshot::UserMessage {
            text: "turn four prompt".to_owned(),
            images: Vec::new(),
            origin: UserInputOrigin::ExternalUser,
        },
        TranscriptItemSnapshot::AssistantText {
            text: "turn four reply".to_owned(),
        },
    ];
    let pruned_external_turns = 2;
    let total_turns = pruned_external_turns + count_external_turns(&surviving);
    assert_eq!(total_turns, 4);

    // Capturing turns 3..=4 picks both surviving turns.
    let condensed = condense_turn_range_with_offset(&surviving, pruned_external_turns, 3, 4)
        .expect("turn range 3..=4 condenses");
    assert!(condensed.json.contains("turn three prompt"));
    assert!(condensed.json.contains("turn four prompt"));

    // Capturing turns 2..=3 when turn 2 was already pruned clamps cleanly to turn 3.
    let clamped = condense_turn_range_with_offset(&surviving, pruned_external_turns, 2, 3)
        .expect("clamped range condenses surviving turn 3");
    assert!(clamped.json.contains("turn three prompt"));
    assert!(!clamped.json.contains("turn four prompt"));

    // Capturing already-pruned turns 1..=2 returns empty range error cleanly.
    assert!(condense_turn_range_with_offset(&surviving, pruned_external_turns, 1, 2).is_err());
}
