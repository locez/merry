use merry_core::{ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator};
use merry_runtime::{ArtifactContent, ArtifactError, ArtifactRegistry};

fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("valid artifact id")
}

fn artifact_ref(value: &str, kind: ArtifactKind) -> ArtifactRef {
    ArtifactRef::new(artifact_id(value), kind)
}

#[test]
fn records_and_reads_artifact_ref_and_content() {
    let mut registry = ArtifactRegistry::default();
    assert!(registry.is_empty());

    let artifact = artifact_ref("tool-output", ArtifactKind::Text)
        .with_label("Tool output")
        .expect("valid artifact label");
    let recorded = registry
        .record(
            artifact.clone(),
            ArtifactContent::text("first line\nsecond line\n"),
        )
        .expect("artifact should record");

    assert_eq!(recorded, artifact);
    assert!(!registry.is_empty());
    assert_eq!(
        registry
            .read_ref(artifact.id())
            .expect("artifact ref should be readable"),
        &artifact
    );
    assert_eq!(
        registry
            .read_content(artifact.id())
            .expect("artifact content should be readable")
            .as_text(),
        Some("first line\nsecond line\n")
    );
}

#[test]
fn duplicate_artifact_ids_are_rejected_without_overwriting_existing_content() {
    let mut registry = ArtifactRegistry::default();
    let artifact = artifact_ref("duplicate-artifact", ArtifactKind::Text);

    registry
        .record(
            artifact.clone(),
            ArtifactContent::text("original exact text"),
        )
        .expect("first record should succeed");
    let error = registry
        .record(artifact.clone(), ArtifactContent::text("replacement text"))
        .expect_err("duplicate id should reject");

    assert!(matches!(
        error,
        ArtifactError::DuplicateId { ref id } if id == artifact.id()
    ));
    assert_eq!(
        registry
            .read_content(artifact.id())
            .expect("original content should remain readable")
            .as_text(),
        Some("original exact text")
    );
}

#[test]
fn missing_reads_are_typed_errors() {
    let registry = ArtifactRegistry::default();
    let missing = artifact_id("missing-artifact");

    let content_error = registry
        .read_content(&missing)
        .expect_err("missing content should reject");
    assert!(matches!(
        content_error,
        ArtifactError::MissingArtifact { ref id } if id == &missing
    ));

    let evidence_error = registry
        .evidence_ref(&missing, EvidenceLocator::whole_artifact())
        .expect_err("missing evidence ref should reject");
    assert!(matches!(
        evidence_error,
        ArtifactError::MissingArtifact { ref id } if id == &missing
    ));
}

#[test]
fn incompatible_metadata_and_content_are_rejected() {
    let mut registry = ArtifactRegistry::default();
    let artifact = artifact_ref("binary-as-text", ArtifactKind::Binary);

    let error = registry
        .record(artifact.clone(), ArtifactContent::text("not binary bytes"))
        .expect_err("binary artifact should require byte content");

    assert!(matches!(
        error,
        ArtifactError::IncompatibleContent {
            ref id,
            artifact_kind: ArtifactKind::Binary,
            ..
        } if id == artifact.id()
    ));
    assert!(registry.is_empty());
}

#[test]
fn evidence_refs_point_to_exact_recorded_artifact_locations() {
    let mut registry = ArtifactRegistry::default();
    let artifact = artifact_ref("exact-evidence", ArtifactKind::Text)
        .with_label("Summary only")
        .expect("valid artifact label");
    let artifact = registry
        .record(
            artifact,
            ArtifactContent::text(
                "summary: source output follows\nexact evidence alpha\nexact evidence beta\n",
            ),
        )
        .expect("artifact should record");

    let evidence = registry
        .evidence_ref(
            artifact.id(),
            EvidenceLocator::line_range(2, 3).expect("valid line range"),
        )
        .expect("recorded artifact should produce evidence ref");
    let exact = registry
        .read_evidence(&evidence)
        .expect("line evidence should read back");

    assert_eq!(evidence.artifact_id, *artifact.id());
    assert_eq!(evidence.locator.as_line_range(), Some((2, 3)));
    assert_eq!(
        exact.as_text(),
        Some("exact evidence alpha\nexact evidence beta")
    );
}
