use crate::support::runtime::{artifact_id, collect_step};
use merry_core::{EvidenceLocator, EvidenceRef, RuntimeJournalPayload};
use merry_runtime::{
    CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest, CheckpointSequenceRange,
    CheckpointSourceKind, CheckpointValidationPolicy, CitationBackedCheckpoint,
    CompactedCheckpoint, CompactedCheckpointCandidate, Runtime,
};

pub(crate) fn citation_checkpoint_for_provider_tests(
    checkpoint_id: &str,
    ref_id: &str,
    excerpt: &str,
) -> CompactedCheckpoint {
    let manifest = CheckpointRefManifest::new(
        CheckpointId::new(checkpoint_id).expect("valid checkpoint id"),
        vec![CheckpointRef::new(
            CheckpointRefId::new(ref_id).expect("valid ref id"),
            CheckpointSourceKind::UserMessage,
            CheckpointSequenceRange::new(1, 1).expect("valid range"),
            EvidenceRef::new(
                artifact_id(&format!("provider-checkpoint-source-{ref_id}")),
                EvidenceLocator::whole_artifact(),
            ),
        )],
    )
    .expect("valid manifest");
    let candidate = CompactedCheckpointCandidate::from_json(&format!(
        r#"{{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [
            {{
              "id": "c1",
              "text": {excerpt_json},
              "refs": [{ref_json}]
            }}
          ],
          "corrected_misunderstandings": [],
          "durable_conclusions": [],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }}"#,
        excerpt_json = serde_json::to_string(excerpt).expect("excerpt serializes"),
        ref_json = serde_json::to_string(ref_id).expect("ref id serializes"),
    ))
    .expect("candidate parses");
    let citation = CitationBackedCheckpoint::from_candidate(
        CheckpointId::new(checkpoint_id).expect("valid checkpoint id"),
        candidate,
        manifest,
        CheckpointValidationPolicy::default(),
    )
    .expect("citation checkpoint builds");

    CompactedCheckpoint::from_citation_backed(citation).expect("checkpoint renders")
}

pub(crate) async fn seed_history_text_for_compaction(
    runtime: &Runtime,
    old_user: &str,
    retained_tail: &str,
) {
    let first_events = collect_step(runtime, old_user).await;
    assert!(
        first_events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "first seed step should complete"
    );
    let second_events = collect_step(runtime, retained_tail).await;
    assert!(
        second_events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "tail seed step should complete"
    );
}

pub(crate) async fn read_full_checkpoint_ref(
    runtime: &Runtime,
    ref_id: &CheckpointRefId,
) -> String {
    let mut content = String::new();
    let mut offset = 0usize;
    loop {
        let page = runtime
            .read_checkpoint_ref_page(ref_id, offset, 4096)
            .await
            .expect("checkpoint ref page reads");
        assert_eq!(page.offset(), offset);
        content.push_str(page.content());
        match page.next_offset() {
            Some(next) => offset = next,
            None => {
                assert_eq!(content.len(), page.total_bytes());
                return content;
            }
        }
    }
}
