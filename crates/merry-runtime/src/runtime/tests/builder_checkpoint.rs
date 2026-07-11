use super::*;
use crate::CheckpointError;

fn citation_checkpoint_with_evidence(
    checkpoint_id: &str,
    ref_id: &str,
    artifact_id: &str,
    locator: EvidenceLocator,
) -> CompactedCheckpoint {
    let checkpoint_id = CheckpointId::new(checkpoint_id).expect("valid checkpoint id");
    let manifest = CheckpointRefManifest::new(
        checkpoint_id.clone(),
        vec![CheckpointRef::new(
            CheckpointRefId::new(ref_id).expect("valid ref id"),
            CheckpointSourceKind::UserMessage,
            CheckpointSequenceRange::new(1, 1).expect("valid range"),
            EvidenceRef::new(
                ArtifactId::new(artifact_id).expect("valid artifact id"),
                locator,
            ),
        )],
    )
    .expect("valid manifest");
    let candidate = CompactedCheckpointCandidate::from_json(&format!(
        r#"{{
          "claims": [{{
            "id": "c1",
            "kind": "current_state",
            "text": "The checkpoint keeps an exact source.",
            "refs": [{ref_id}]
          }}],
          "working_intent": null
        }}"#,
        ref_id = serde_json::to_string(ref_id).expect("ref id serializes"),
    ))
    .expect("valid candidate");
    let citation = CitationBackedCheckpoint::from_candidate(
        checkpoint_id,
        candidate,
        manifest,
        Default::default(),
    )
    .expect("valid citation checkpoint");
    CompactedCheckpoint::from_citation_backed(citation).expect("valid compacted checkpoint")
}

fn text_artifact(id: &str) -> ArtifactRef {
    ArtifactRef::new(
        ArtifactId::new(id).expect("valid artifact id"),
        ArtifactKind::Text,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_builder_atomically_seeds_checkpoint_backing_evidence() {
    let runtime = Runtime::builder(session_id("builder-checkpoint-evidence"))
        .compacted_checkpoint(citation_checkpoint_with_evidence(
            "checkpoint-builder-seeded",
            "bootstrap-ref",
            "checkpoint-builder-source",
            EvidenceLocator::whole_artifact(),
        ))
        .compacted_checkpoint_evidence(
            text_artifact("checkpoint-builder-source"),
            ArtifactContent::text("exact checkpoint source"),
        )
        .build()
        .expect("checkpoint and its backing evidence should build together");

    let page = runtime
        .read_checkpoint_ref_page(
            &CheckpointRefId::new("bootstrap-ref").expect("valid ref id"),
            0,
            4096,
        )
        .await
        .expect("seeded checkpoint evidence should be readable");
    assert_eq!(page.content(), "exact checkpoint source");
}

#[test]
fn runtime_builder_rejects_citation_checkpoint_without_backing_evidence() {
    let result = Runtime::builder(session_id("builder-checkpoint-missing-evidence"))
        .compacted_checkpoint(citation_checkpoint_with_evidence(
            "checkpoint-builder-missing",
            "bootstrap-ref",
            "checkpoint-builder-missing-source",
            EvidenceLocator::whole_artifact(),
        ))
        .build();
    let Err(error) = result else {
        panic!("citation checkpoint without backing evidence must fail");
    };

    assert!(matches!(
        error,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id.as_str() == "checkpoint-builder-missing-source"
    ));
}

#[test]
fn runtime_builder_validates_checkpoint_evidence_locator_after_seeding() {
    let result = Runtime::builder(session_id("builder-checkpoint-invalid-locator"))
        .compacted_checkpoint(citation_checkpoint_with_evidence(
            "checkpoint-builder-invalid-locator",
            "bootstrap-ref",
            "checkpoint-builder-short-source",
            EvidenceLocator::line_range(2, 2).expect("valid locator shape"),
        ))
        .compacted_checkpoint_evidence(
            text_artifact("checkpoint-builder-short-source"),
            ArtifactContent::text("only one line\n"),
        )
        .build();
    let Err(error) = result else {
        panic!("unreadable checkpoint evidence range must fail");
    };

    assert!(matches!(
        error,
        RuntimeError::Artifact {
            source: ArtifactError::InvalidEvidenceLocator { id, .. }
        } if id.as_str() == "checkpoint-builder-short-source"
    ));
}

#[test]
fn runtime_builder_rejects_reserved_checkpoint_evidence_seed() {
    let result = Runtime::builder(session_id("builder-checkpoint-reserved-evidence"))
        .compacted_checkpoint(citation_checkpoint_with_evidence(
            "checkpoint-builder-reserved",
            "bootstrap-ref",
            "user-message-1",
            EvidenceLocator::whole_artifact(),
        ))
        .compacted_checkpoint_evidence(
            text_artifact("user-message-1"),
            ArtifactContent::text("runtime-owned transcript source"),
        )
        .build();
    let Err(error) = result else {
        panic!("manual checkpoint evidence must not claim runtime-owned artifact ids");
    };

    assert!(matches!(
        error,
        RuntimeError::ReservedArtifactId { artifact_id }
            if artifact_id.as_str() == "user-message-1"
    ));
}

#[test]
fn runtime_builder_rejects_non_text_checkpoint_evidence_seed() {
    let result = Runtime::builder(session_id("builder-checkpoint-binary-evidence"))
        .compacted_checkpoint(citation_checkpoint_with_evidence(
            "checkpoint-builder-binary",
            "bootstrap-binary-ref",
            "checkpoint-builder-binary-source",
            EvidenceLocator::whole_artifact(),
        ))
        .compacted_checkpoint_evidence(
            ArtifactRef::new(
                ArtifactId::new("checkpoint-builder-binary-source").expect("valid artifact id"),
                ArtifactKind::Binary,
            ),
            ArtifactContent::binary([1, 2, 3]),
        )
        .build();
    let Err(error) = result else {
        panic!("binary checkpoint evidence must reject build");
    };

    assert!(matches!(
        error,
        RuntimeError::Artifact {
            source: ArtifactError::NonTextEvidencePage { id }
        } if id.as_str() == "checkpoint-builder-binary-source"
    ));
}

#[test]
fn runtime_builder_rejects_manual_runtime_history_ref_namespace() {
    let result = Runtime::builder(session_id("builder-checkpoint-history-ref"))
        .compacted_checkpoint(citation_checkpoint_with_evidence(
            "checkpoint-builder-history-ref",
            "h1",
            "checkpoint-builder-history-source",
            EvidenceLocator::whole_artifact(),
        ))
        .compacted_checkpoint_evidence(
            text_artifact("checkpoint-builder-history-source"),
            ArtifactContent::text("manual bootstrap source"),
        )
        .build();
    let Err(error) = result else {
        panic!("manual checkpoints must not claim runtime history ref ids");
    };

    assert!(matches!(
        error,
        RuntimeError::Checkpoint {
            source: CheckpointError::ManualCheckpointHistoryRefReserved { ref_id }
        } if ref_id == "h1"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn manual_bootstrap_ref_can_roll_with_new_runtime_history_refs() {
    let runtime = Runtime::builder(session_id("builder-checkpoint-bootstrap-roll"))
        .compacted_checkpoint(citation_checkpoint_with_evidence(
            "checkpoint-builder-bootstrap-roll",
            "bootstrap-ref",
            "checkpoint-builder-bootstrap-source",
            EvidenceLocator::whole_artifact(),
        ))
        .compacted_checkpoint_evidence(
            text_artifact("checkpoint-builder-bootstrap-source"),
            ArtifactContent::text("manual bootstrap source"),
        )
        .build()
        .expect("bootstrap checkpoint should build");
    let mut session = runtime.inner.session.lock().await;
    session
        .record_test_user_message_body("new covered history")
        .expect("covered history records");
    session
        .record_test_user_message_body("new retained history")
        .expect("retained history records");

    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("rolling input builds")
        .expect("new history is compressible");
    let ref_ids = input
        .manifest()
        .refs()
        .iter()
        .map(|reference| reference.id().as_str())
        .collect::<Vec<_>>();

    assert_eq!(ref_ids, ["bootstrap-ref", "h0"]);
}

#[test]
fn runtime_builder_plain_checkpoint_needs_no_backing_evidence() {
    Runtime::builder(session_id("builder-plain-checkpoint"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("plain checkpoint remains supported")
                .expect("valid plain checkpoint"),
        )
        .build()
        .expect("plain checkpoint should not require citation evidence");
}
