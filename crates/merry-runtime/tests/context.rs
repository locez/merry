use merry_core::{ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef};
use merry_runtime::{
    ArtifactContent, ArtifactRegistry, CompiledContextSection, ContextCompiler, ContextEntry,
    ContextEvidence, ContextSummary,
};

fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("valid artifact id")
}

fn line_evidence(artifact: &str, start: u64, end: u64) -> EvidenceRef {
    EvidenceRef::new(
        artifact_id(artifact),
        EvidenceLocator::line_range(start, end).expect("valid line range"),
    )
}

fn summary(id: &str, text: &str, evidence: Vec<ContextEvidence>) -> ContextEntry {
    ContextEntry::summary(ContextSummary::new(id, text, evidence).expect("valid summary"))
}

fn evidence(label: &str, reference: EvidenceRef) -> ContextEvidence {
    ContextEvidence::new(label, reference).expect("valid context evidence")
}

fn registry_with_text_artifact(id: &str, content: &str) -> ArtifactRegistry {
    let mut registry = ArtifactRegistry::default();
    registry
        .record(
            ArtifactRef::new(artifact_id(id), ArtifactKind::Text),
            ArtifactContent::text(content),
        )
        .expect("valid artifact record");
    registry
}

fn registry_with_text_artifacts(artifacts: &[(&str, &str)]) -> ArtifactRegistry {
    let mut registry = ArtifactRegistry::default();
    for (id, content) in artifacts {
        registry
            .record(
                ArtifactRef::new(artifact_id(id), ArtifactKind::Text),
                ArtifactContent::text(*content),
            )
            .expect("valid artifact record");
    }
    registry
}

#[test]
fn compiled_context_is_deterministic_from_structured_state() {
    let compiler = ContextCompiler::new();
    let registry = registry_with_text_artifacts(&[
        ("artifact-a", "a1\na2\na3\n"),
        ("artifact-b", "b1\nb2\nb3\nb4\nb5\nb6\n"),
        (
            "artifact-z",
            "z1\nz2\nz3\nz4\nz5\nz6\nz7\nz8\nz9\nz10\nz11\nz12\n",
        ),
    ]);
    let input = vec![
        summary(
            "summary-z",
            "Later finding.",
            vec![evidence(
                "zeta artifact",
                line_evidence("artifact-z", 9, 12),
            )],
        ),
        summary(
            "summary-a",
            "Earlier finding.",
            vec![
                evidence("second artifact", line_evidence("artifact-b", 4, 6)),
                evidence("first artifact", line_evidence("artifact-a", 1, 3)),
            ],
        ),
    ];

    let first = compiler
        .compile(input.clone(), &registry)
        .expect("context compiles");
    let second = compiler
        .compile(input, &registry)
        .expect("context compiles again");

    assert_eq!(first, second);
    assert_eq!(
        first.sections(),
        &[
            CompiledContextSection::Summary {
                id: "summary-a".to_owned(),
                text: "Earlier finding.".to_owned(),
                evidence: vec![
                    evidence("first artifact", line_evidence("artifact-a", 1, 3),),
                    evidence("second artifact", line_evidence("artifact-b", 4, 6),)
                ],
            },
            CompiledContextSection::Summary {
                id: "summary-z".to_owned(),
                text: "Later finding.".to_owned(),
                evidence: vec![evidence(
                    "zeta artifact",
                    line_evidence("artifact-z", 9, 12)
                )],
            },
        ]
    );
}

#[test]
fn registry_compilation_rejects_missing_evidence_artifacts() {
    let compiler = ContextCompiler::new();
    let registry = ArtifactRegistry::default();
    let input = vec![summary(
        "summary-with-missing-artifact",
        "Navigation must not outrun exact evidence.",
        vec![evidence(
            "missing build output",
            line_evidence("artifact-missing", 1, 1),
        )],
    )];

    let error = compiler
        .compile(input, &registry)
        .expect_err("missing evidence artifact must be rejected");

    assert_eq!(
        error.to_string(),
        "context summary summary-with-missing-artifact references unreadable evidence artifact-missing: artifact id artifact-missing is not recorded"
    );
}

#[test]
fn summary_text_requires_linked_exact_evidence_metadata() {
    let compiler = ContextCompiler::new();
    let registry = ArtifactRegistry::default();
    let input = vec![summary(
        "summary-without-evidence",
        "Navigation only.",
        vec![],
    )];

    let error = compiler
        .compile(input, &registry)
        .expect_err("summary without evidence must be rejected");

    assert_eq!(
        error.to_string(),
        "context summary summary-without-evidence has no exact evidence references"
    );
}

#[test]
fn compiled_snapshot_includes_summary_and_exact_evidence_refs() {
    let compiler = ContextCompiler::new();
    let registry = registry_with_text_artifact(
        "artifact-build-log",
        "01\n02\n03\n04\n05\n06\n07\n08\n09\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\n25\n26\n27\n28\n29\n30\n31\n32\n33\n34\n35\n36\n37\n38\n39\n40\n41\n42\n43\n44\n45\n46\n47\n",
    );
    let context = compiler
        .compile(
            vec![summary(
                "summary-a",
                "Build errors point at context ownership.",
                vec![evidence(
                    "compiler output",
                    line_evidence("artifact-build-log", 42, 47),
                )],
            )],
            &registry,
        )
        .expect("context compiles");

    let snapshot = context.to_snapshot();

    assert_eq!(
        snapshot,
        [
            "summary:summary-a",
            "text:Build errors point at context ownership.",
            "evidence:compiler output:artifact-build-log:line:42-47",
        ]
        .join("\n")
    );
}
