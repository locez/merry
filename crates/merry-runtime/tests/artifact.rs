use merry_core::{ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef};
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

fn registry_with_text(value: &str) -> (ArtifactRegistry, EvidenceRef) {
    let mut registry = ArtifactRegistry::default();
    let artifact = registry
        .record(
            artifact_ref("paged-text", ArtifactKind::Text),
            ArtifactContent::text(value),
        )
        .expect("artifact should record");
    let evidence = EvidenceRef::new(artifact.id().clone(), EvidenceLocator::whole_artifact());
    (registry, evidence)
}

#[test]
fn text_evidence_pages_continue_from_original_content() {
    let content = format!("{}SENTINEL_AFTER_1200_BYTES", "x".repeat(1200));
    let (registry, evidence) = registry_with_text(&content);

    let first = registry
        .read_text_evidence_page(&evidence, 0, 1024)
        .expect("first page should read");
    let second = registry
        .read_text_evidence_page(
            &evidence,
            first.next_offset().expect("first page should continue"),
            1024,
        )
        .expect("second page should read");

    assert_eq!(first.content().len(), 1024);
    assert!(second.content().contains("SENTINEL_AFTER_1200_BYTES"));
    assert_eq!(second.artifact_id(), first.artifact_id());
    assert_eq!(second.total_bytes(), content.len());
    assert_eq!(second.next_offset(), None);
}

#[test]
fn evidence_page_offset_is_relative_to_selected_range() {
    let mut registry = ArtifactRegistry::default();
    let artifact = registry
        .record(
            artifact_ref("selected-range", ArtifactKind::Text),
            ArtifactContent::text("ignored\nselected-value\nignored-again\n"),
        )
        .expect("artifact should record");
    let evidence = EvidenceRef::new(
        artifact.id().clone(),
        EvidenceLocator::line_range(2, 2).expect("valid line range"),
    );

    let page = registry
        .read_text_evidence_page(&evidence, 9, 5)
        .expect("selected evidence page should read");

    assert_eq!(page.offset(), 9);
    assert_eq!(page.content(), "value");
    assert_eq!(page.total_bytes(), "selected-value".len());
    assert_eq!(page.next_offset(), None);
}

#[test]
fn evidence_page_rejects_non_utf8_boundary() {
    let (registry, evidence) = registry_with_text("甲乙丙");

    let error = registry
        .read_text_evidence_page(&evidence, 1, 4)
        .expect_err("offset inside a UTF-8 character must reject");

    assert!(matches!(error, ArtifactError::InvalidEvidencePage { .. }));
}

#[test]
fn evidence_page_rejects_max_bytes_too_small_to_make_progress() {
    let (registry, evidence) = registry_with_text("甲乙丙");

    let error = registry
        .read_text_evidence_page(&evidence, 0, 1)
        .expect_err("a page that cannot hold one character must reject");

    assert!(matches!(error, ArtifactError::InvalidEvidencePage { .. }));
}

#[test]
fn evidence_page_rejects_zero_and_out_of_range_offsets() {
    let (registry, evidence) = registry_with_text("short text");

    let zero_page = registry
        .read_text_evidence_page(&evidence, 0, 0)
        .expect_err("zero-sized pages must reject");
    let past_end = registry
        .read_text_evidence_page(&evidence, "short text".len() + 1, 4)
        .expect_err("offsets after the evidence range must reject");

    assert!(matches!(
        zero_page,
        ArtifactError::InvalidEvidencePage { .. }
    ));
    assert!(matches!(
        past_end,
        ArtifactError::InvalidEvidencePage { .. }
    ));
}

#[test]
fn evidence_page_at_end_is_complete() {
    let (registry, evidence) = registry_with_text("short text");

    let page = registry
        .read_text_evidence_page(&evidence, "short text".len(), 4)
        .expect("offset at the end should return a complete empty page");

    assert_eq!(page.content(), "");
    assert_eq!(page.next_offset(), None);
}

#[test]
fn evidence_page_rejects_non_text_and_missing_artifacts() {
    let mut registry = ArtifactRegistry::default();
    let binary = registry
        .record(
            artifact_ref("binary-evidence", ArtifactKind::Binary),
            ArtifactContent::binary([1, 2, 3]),
        )
        .expect("binary artifact should record");
    let binary_evidence = EvidenceRef::new(binary.id().clone(), EvidenceLocator::whole_artifact());
    let missing_evidence = EvidenceRef::new(
        artifact_id("missing-evidence"),
        EvidenceLocator::whole_artifact(),
    );

    let non_text = registry
        .read_text_evidence_page(&binary_evidence, 0, 4)
        .expect_err("binary evidence must reject text paging");
    let missing = registry
        .read_text_evidence_page(&missing_evidence, 0, 4)
        .expect_err("missing evidence must reject paging");

    assert!(matches!(
        non_text,
        ArtifactError::NonTextEvidencePage { ref id } if id == binary.id()
    ));
    assert!(matches!(
        missing,
        ArtifactError::MissingArtifact { ref id } if id == &missing_evidence.artifact_id
    ));
}

#[test]
fn text_evidence_validation_rejects_non_text_whole_and_byte_ranges() {
    let mut registry = ArtifactRegistry::default();
    let text = registry
        .record(
            artifact_ref("validated-text-evidence", ArtifactKind::Text),
            ArtifactContent::text("first line\nsecond line\n"),
        )
        .expect("text artifact should record");
    let binary = registry
        .record(
            artifact_ref("validated-binary-evidence", ArtifactKind::Binary),
            ArtifactContent::binary([1, 2, 3, 4]),
        )
        .expect("binary artifact should record");
    let text_evidence = EvidenceRef::new(
        text.id().clone(),
        EvidenceLocator::line_range(2, 2).expect("valid line range"),
    );
    let binary_whole = EvidenceRef::new(binary.id().clone(), EvidenceLocator::whole_artifact());
    let binary_range = EvidenceRef::new(
        binary.id().clone(),
        EvidenceLocator::byte_range(0, 2).expect("valid byte range"),
    );

    registry
        .validate_text_evidence(&text_evidence)
        .expect("text evidence readable by paging should validate");
    for evidence in [binary_whole, binary_range] {
        let error = registry
            .validate_text_evidence(&evidence)
            .expect_err("non-text evidence must reject checkpoint text paging");
        assert!(matches!(
            error,
            ArtifactError::NonTextEvidencePage { ref id } if id == binary.id()
        ));
    }
}
