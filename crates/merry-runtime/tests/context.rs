use merry_core::{ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, SessionId};
use merry_runtime::{
    ArtifactContent, ArtifactError, CompiledContextSection, ContextCompiler, ContextEntry,
    ContextError, ContextEvidence, ContextSummary, Runtime,
};

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

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

fn runtime(value: &str) -> Runtime {
    Runtime::builder(session_id(value))
        .build()
        .expect("runtime should build")
}

async fn record_text_artifact(runtime: &Runtime, id: &str, content: &str) {
    runtime
        .record_artifact(
            ArtifactRef::new(artifact_id(id), ArtifactKind::Text),
            ArtifactContent::text(content),
        )
        .await
        .expect("valid artifact record");
}

async fn record_text_artifacts(runtime: &Runtime, artifacts: &[(&str, &str)]) {
    for (id, content) in artifacts {
        record_text_artifact(runtime, id, content).await;
    }
}

async fn record_summary(runtime: &Runtime, summary: ContextEntry) {
    runtime.record_context_entry(summary).await;
}

#[tokio::test(flavor = "current_thread")]
async fn compiled_context_is_deterministic_from_session_snapshot() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-determinism");
    record_text_artifacts(
        &runtime,
        &[
            ("artifact-a", "a1\na2\na3\n"),
            ("artifact-b", "b1\nb2\nb3\nb4\nb5\nb6\n"),
            (
                "artifact-z",
                "z1\nz2\nz3\nz4\nz5\nz6\nz7\nz8\nz9\nz10\nz11\nz12\n",
            ),
        ],
    )
    .await;
    record_summary(
        &runtime,
        summary(
            "summary-z",
            "Later finding.",
            vec![evidence(
                "zeta artifact",
                line_evidence("artifact-z", 9, 12),
            )],
        ),
    )
    .await;
    record_summary(
        &runtime,
        summary(
            "summary-a",
            "Earlier finding.",
            vec![
                evidence("second artifact", line_evidence("artifact-b", 4, 6)),
                evidence("first artifact", line_evidence("artifact-a", 1, 3)),
            ],
        ),
    )
    .await;

    let first = compiler
        .compile(&runtime.context_snapshot().await)
        .expect("context compiles");
    let second = compiler
        .compile(&runtime.context_snapshot().await)
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

#[tokio::test(flavor = "current_thread")]
async fn registry_mismatch_is_not_expressible_through_public_context_compiler_api() {
    let compiler = ContextCompiler::new();
    let evidence_ref = line_evidence("artifact-same-id", 3, 3);

    let matching_runtime = runtime("context-matching-registry");
    record_text_artifact(&matching_runtime, "artifact-same-id", "m1\nm2\nm3\n").await;
    record_summary(
        &matching_runtime,
        summary(
            "summary-matching",
            "Summary and registry came from the same session.",
            vec![evidence("matching evidence", evidence_ref.clone())],
        ),
    )
    .await;

    let mismatched_runtime = runtime("context-wrong-registry");
    record_text_artifact(
        &mismatched_runtime,
        "artifact-same-id",
        "wrong-only-one-line\n",
    )
    .await;

    let compiled = compiler
        .compile(&matching_runtime.context_snapshot().await)
        .expect("matching snapshot compiles");

    assert_eq!(
        compiled.to_snapshot(),
        [
            "summary:summary-matching",
            "text:Summary and registry came from the same session.",
            "evidence:matching evidence:artifact-same-id:line:3-3",
        ]
        .join("\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn direct_record_context_summary_accepts_missing_evidence_until_compile() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-missing-evidence");
    let _: () = runtime
        .record_context_summary(
            ContextSummary::new(
                "summary-with-missing-artifact",
                "Navigation must not outrun exact evidence.",
                vec![evidence(
                    "missing build output",
                    line_evidence("artifact-missing", 1, 1),
                )],
            )
            .expect("valid summary"),
        )
        .await;

    let error = compiler
        .compile(&runtime.context_snapshot().await)
        .expect_err("missing evidence artifact must be rejected");

    assert_eq!(
        error.to_string(),
        "context summary summary-with-missing-artifact references unreadable evidence artifact-missing: artifact id artifact-missing is not recorded"
    );

    record_text_artifact(&runtime, "artifact-missing", "available after raw write\n").await;

    let current = compiler
        .compile(&runtime.context_snapshot().await)
        .expect("direct raw write remains in session once evidence is readable");
    assert_eq!(
        current.to_snapshot(),
        [
            "summary:summary-with-missing-artifact",
            "text:Navigation must not outrun exact evidence.",
            "evidence:missing build output:artifact-missing:line:1-1",
        ]
        .join("\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn session_context_snapshot_is_independent_from_later_session_mutation() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-snapshot-isolation");
    record_summary(
        &runtime,
        summary(
            "summary-before-artifact",
            "Snapshot evidence must come from the captured artifact state.",
            vec![evidence(
                "future artifact",
                line_evidence("artifact-recorded-later", 1, 1),
            )],
        ),
    )
    .await;

    let snapshot = runtime.context_snapshot().await;

    record_text_artifact(&runtime, "artifact-recorded-later", "available later\n").await;

    let stale_error = compiler
        .compile(&snapshot)
        .expect_err("snapshot must not observe artifacts recorded later");
    assert_eq!(
        stale_error,
        ContextError::UnreadableEvidence {
            summary_id: "summary-before-artifact".to_owned(),
            artifact_id: artifact_id("artifact-recorded-later"),
            source: ArtifactError::MissingArtifact {
                id: artifact_id("artifact-recorded-later"),
            },
        }
    );

    let current = compiler
        .compile(&runtime.context_snapshot().await)
        .expect("current snapshot sees the later artifact");
    assert_eq!(
        current.to_snapshot(),
        [
            "summary:summary-before-artifact",
            "text:Snapshot evidence must come from the captured artifact state.",
            "evidence:future artifact:artifact-recorded-later:line:1-1",
        ]
        .join("\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn summary_text_requires_linked_exact_evidence_metadata() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-summary-without-evidence");
    record_summary(
        &runtime,
        summary("summary-without-evidence", "Navigation only.", vec![]),
    )
    .await;

    let error = compiler
        .compile(&runtime.context_snapshot().await)
        .expect_err("summary without evidence must be rejected");

    assert_eq!(
        error.to_string(),
        "context summary summary-without-evidence has no exact evidence references"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn session_snapshot_compilation_rejects_unreadable_evidence_locators() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-unreadable-evidence");
    record_text_artifact(&runtime, "artifact-short-log", "01\n02\n").await;
    let _: () = runtime
        .record_context_summary(
            ContextSummary::new(
                "summary-with-unreadable-evidence",
                "Navigation must point at readable content.",
                vec![evidence(
                    "short compiler output",
                    line_evidence("artifact-short-log", 42, 47),
                )],
            )
            .expect("valid summary"),
        )
        .await;

    let error = compiler
        .compile(&runtime.context_snapshot().await)
        .expect_err("unreadable evidence locator must be rejected");

    assert_eq!(
        error.to_string(),
        "context summary summary-with-unreadable-evidence references unreadable evidence artifact-short-log: artifact id artifact-short-log has invalid evidence locator: line range is outside artifact content"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn direct_duplicate_summary_ids_compile_as_duplicate_sections_when_evidence_is_readable() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-duplicate-direct-summary-id");
    record_text_artifacts(
        &runtime,
        &[
            ("artifact-first", "first evidence\n"),
            ("artifact-second", "second evidence\n"),
        ],
    )
    .await;

    let _: () = runtime
        .record_context_summary(
            ContextSummary::new(
                "summary-duplicate",
                "First direct summary.",
                vec![evidence(
                    "first readable evidence",
                    line_evidence("artifact-first", 1, 1),
                )],
            )
            .expect("valid summary"),
        )
        .await;
    let _: () = runtime
        .record_context_summary(
            ContextSummary::new(
                "summary-duplicate",
                "Second direct summary.",
                vec![evidence(
                    "second readable evidence",
                    line_evidence("artifact-second", 1, 1),
                )],
            )
            .expect("valid summary"),
        )
        .await;

    let compiled = compiler
        .compile(&runtime.context_snapshot().await)
        .expect("direct duplicate summaries compile when evidence is readable");

    assert_eq!(
        compiled.sections(),
        &[
            CompiledContextSection::Summary {
                id: "summary-duplicate".to_owned(),
                text: "First direct summary.".to_owned(),
                evidence: vec![evidence(
                    "first readable evidence",
                    line_evidence("artifact-first", 1, 1),
                )],
            },
            CompiledContextSection::Summary {
                id: "summary-duplicate".to_owned(),
                text: "Second direct summary.".to_owned(),
                evidence: vec![evidence(
                    "second readable evidence",
                    line_evidence("artifact-second", 1, 1),
                )],
            },
        ]
    );
    assert_eq!(
        compiled.to_snapshot(),
        [
            "summary:summary-duplicate",
            "text:First direct summary.",
            "evidence:first readable evidence:artifact-first:line:1-1",
            "summary:summary-duplicate",
            "text:Second direct summary.",
            "evidence:second readable evidence:artifact-second:line:1-1",
        ]
        .join("\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compiled_snapshot_includes_summary_and_exact_evidence_refs() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-snapshot-text");
    record_text_artifact(
        &runtime,
        "artifact-build-log",
        "01\n02\n03\n04\n05\n06\n07\n08\n09\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\n25\n26\n27\n28\n29\n30\n31\n32\n33\n34\n35\n36\n37\n38\n39\n40\n41\n42\n43\n44\n45\n46\n47\n",
    )
    .await;
    record_summary(
        &runtime,
        summary(
            "summary-a",
            "Build errors point at context ownership.",
            vec![evidence(
                "compiler output",
                line_evidence("artifact-build-log", 42, 47),
            )],
        ),
    )
    .await;

    let context = compiler
        .compile(&runtime.context_snapshot().await)
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
