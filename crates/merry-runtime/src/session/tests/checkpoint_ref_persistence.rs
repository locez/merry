use super::*;
use crate::{FileSessionStore, artifact::ArtifactContent};
use merry_core::{ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef};

fn citation_checkpoint_with_evidence(evidence: EvidenceRef) -> crate::CompactedCheckpoint {
    let checkpoint_id = CheckpointId::new("checkpoint-persistence").expect("valid checkpoint id");
    let manifest = CheckpointRefManifest::new(
        checkpoint_id.clone(),
        vec![CheckpointRef::new(
            CheckpointRefId::new("h42").expect("valid ref id"),
            CheckpointSourceKind::UserMessage,
            CheckpointSequenceRange::new(42, 42).expect("valid range"),
            evidence,
        )],
    )
    .expect("valid manifest");
    let candidate = CompactedCheckpointCandidate::from_json(
        r#"{
          "claims": [{
            "id": "c1",
            "kind": "constraint",
            "text": "The persisted checkpoint keeps exact source evidence.",
            "refs": ["h42"]
          }],
          "working_intent": null
        }"#,
    )
    .expect("candidate parses");
    let checkpoint = CitationBackedCheckpoint::from_candidate(
        checkpoint_id,
        candidate,
        manifest,
        CheckpointValidationPolicy::default(),
    )
    .expect("citation checkpoint builds");
    crate::CompactedCheckpoint::from_citation_backed(checkpoint)
        .expect("compacted checkpoint builds")
}

fn install_checkpoint_source(session: &mut SessionState, text: &str) {
    let artifact = ArtifactRef::new(
        artifact_id("checkpoint-persistence-source"),
        ArtifactKind::Text,
    );
    session
        .record_artifact_events(artifact.clone(), ArtifactContent::text(text))
        .expect("checkpoint source records");
    session.set_compacted_checkpoint(citation_checkpoint_with_evidence(EvidenceRef::new(
        artifact.id().clone(),
        EvidenceLocator::whole_artifact(),
    )));
}

#[tokio::test]
async fn citation_checkpoint_evidence_round_trips_through_session_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    install_checkpoint_source(&mut session, "exact persisted checkpoint source");

    session.save_to(&store).await.expect("session saves");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("session loads");
    let page = loaded
        .read_checkpoint_ref_page(&CheckpointRefId::new("h42").expect("valid ref id"), 0, 4096)
        .expect("persisted checkpoint ref reads");

    assert_eq!(page.content(), "exact persisted checkpoint source");
    assert_eq!(page.artifact_id().as_str(), "checkpoint-persistence-source");
}

#[test]
fn session_save_rejects_checkpoint_evidence_without_artifact() {
    let mut session = SessionState::new(session_id());
    session.set_compacted_checkpoint(citation_checkpoint_with_evidence(EvidenceRef::new(
        artifact_id("missing-checkpoint-source"),
        EvidenceLocator::whole_artifact(),
    )));

    let error = session
        .persistable_bundle()
        .expect_err("checkpoint evidence without an artifact must reject save");

    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidDocument { .. }
    ));
    assert!(error.to_string().contains("checkpoint evidence"));
}

#[test]
fn session_save_rejects_non_text_checkpoint_evidence() {
    let mut session = SessionState::new(session_id());
    let artifact = ArtifactRef::new(
        artifact_id("binary-checkpoint-source"),
        ArtifactKind::Binary,
    );
    session
        .record_artifact_events(artifact.clone(), ArtifactContent::binary([1, 2, 3]))
        .expect("binary artifact records");
    session.set_compacted_checkpoint(citation_checkpoint_with_evidence(EvidenceRef::new(
        artifact.id().clone(),
        EvidenceLocator::whole_artifact(),
    )));

    let error = session
        .persistable_bundle()
        .expect_err("non-text checkpoint evidence must reject save");

    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidDocument { .. }
    ));
    assert!(error.to_string().contains("checkpoint evidence"));
}

#[tokio::test]
async fn session_load_rejects_checkpoint_evidence_without_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    install_checkpoint_source(&mut session, "source removed after save");
    session.save_to(&store).await.expect("session saves");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is JSON");
    document["artifacts"] = serde_json::Value::Array(Vec::new());
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("corrupt state serializes"),
        )
        .await
        .expect("corrupt state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("missing checkpoint evidence must reject load");
    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidDocument { .. }
    ));
    assert!(error.to_string().contains("checkpoint evidence"));
}

#[tokio::test]
async fn session_load_rejects_non_text_checkpoint_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    install_checkpoint_source(&mut session, "source changed to binary after save");
    session.save_to(&store).await.expect("session saves");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is JSON");
    document["artifacts"] = serde_json::json!([{
        "artifact": ArtifactRef::new(
            artifact_id("checkpoint-persistence-source"),
            ArtifactKind::Binary,
        ),
        "content": ArtifactContent::binary([1, 2, 3]),
    }]);
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("corrupt state serializes"),
        )
        .await
        .expect("corrupt state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("non-text checkpoint evidence must reject load");
    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidDocument { .. }
    ));
    assert!(error.to_string().contains("checkpoint evidence"));
}

#[tokio::test]
async fn session_load_explicitly_rejects_legacy_excerpt_and_prior_claim_refs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    install_checkpoint_source(&mut session, "source replaced by legacy ref shape");
    session.save_to(&store).await.expect("session saves");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is JSON");
    document["compacted_checkpoint"]["citation_backed"]["refs"][0] = serde_json::json!({
        "id": "prior-c1",
        "source_kind": "prior_checkpoint_claim",
        "source_id": "checkpoint:old:claim:c1",
        "sequence_start": 0,
        "sequence_end": 0,
        "locator": "checkpoint_claim:c1",
        "excerpt": "old summary text is not original evidence"
    });
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("legacy state serializes"),
        )
        .await
        .expect("legacy state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("legacy excerpt refs must reject load");
    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidDocument { .. }
    ));
    assert!(error.to_string().contains("legacy checkpoint excerpt refs"));
}
