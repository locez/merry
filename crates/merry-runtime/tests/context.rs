use merry_core::SessionId;
use merry_runtime::{
    CompactedCheckpoint, CompiledContextSection, ContextCompiler, ContextEntry, ContextSummary,
    Runtime,
};

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

fn summary(id: &str, text: &str) -> ContextEntry {
    ContextEntry::summary(ContextSummary::new(id, text).expect("valid summary"))
}

fn runtime(value: &str) -> Runtime {
    Runtime::builder(session_id(value))
        .build()
        .expect("runtime should build")
}

async fn record_summary(runtime: &Runtime, summary: ContextEntry) {
    runtime
        .record_context_entry(summary)
        .await
        .expect("context entry should record");
}

#[tokio::test(flavor = "current_thread")]
async fn compiled_context_is_deterministic_from_session_snapshot() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-determinism");
    record_summary(&runtime, summary("summary-z", "Later finding.")).await;
    record_summary(&runtime, summary("summary-a", "Earlier finding.")).await;

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
            },
            CompiledContextSection::Summary {
                id: "summary-z".to_owned(),
                text: "Later finding.".to_owned(),
            },
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn prose_checkpoint_renders_before_summary_and_memory() {
    let runtime = Runtime::builder(session_id("prose-context-render-order"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("Prose checkpointing is the current direction.")
                .expect("valid checkpoint"),
        )
        .initial_context_summary(
            "summary-a",
            "Summary should render after the compacted checkpoint.",
        )
        .build()
        .expect("runtime should build");

    let snapshot = ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("context compiles")
        .to_snapshot();

    assert!(snapshot.starts_with(
        "compacted-checkpoint:\nguidance:This summary is the record of the turns that were removed."
    ));
    assert!(snapshot.contains("\ntext:Prose checkpointing is the current direction."));
    assert!(snapshot.contains("\nsummary:summary-a"));
    let checkpoint_at = snapshot
        .find("text:Prose checkpointing is the current direction.")
        .expect("checkpoint text renders");
    let summary_at = snapshot
        .find("\nsummary:summary-a")
        .expect("summary renders");
    assert!(checkpoint_at < summary_at);
}

#[tokio::test(flavor = "current_thread")]
async fn session_context_snapshot_is_independent_from_later_session_mutation() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-snapshot-isolation");
    record_summary(
        &runtime,
        summary(
            "summary-before-snapshot",
            "Snapshot must reflect captured state.",
        ),
    )
    .await;

    let snapshot = runtime.context_snapshot().await;

    record_summary(
        &runtime,
        summary(
            "summary-after-snapshot",
            "Later summary must not enter the captured snapshot.",
        ),
    )
    .await;

    let stale = compiler
        .compile(&snapshot)
        .expect("captured snapshot remains compilable");
    assert_eq!(
        stale.to_snapshot(),
        [
            "summary:summary-before-snapshot",
            "text:Snapshot must reflect captured state.",
        ]
        .join("\n")
    );

    let current = compiler
        .compile(&runtime.context_snapshot().await)
        .expect("current snapshot sees the later summary");
    assert!(
        current
            .to_snapshot()
            .contains("summary:summary-before-snapshot")
    );
    assert!(
        current
            .to_snapshot()
            .contains("summary:summary-after-snapshot")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn direct_duplicate_summary_ids_compile_in_deterministic_order() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-duplicate-direct-summary-id");

    runtime
        .record_context_summary(
            ContextSummary::new("summary-duplicate", "First direct summary.")
                .expect("valid summary"),
        )
        .await
        .expect("direct context summary should record");
    runtime
        .record_context_summary(
            ContextSummary::new("summary-duplicate", "Second direct summary.")
                .expect("valid summary"),
        )
        .await
        .expect("direct context summary should record");

    let compiled = compiler
        .compile(&runtime.context_snapshot().await)
        .expect("direct duplicate summaries compile");

    assert_eq!(
        compiled.sections(),
        &[
            CompiledContextSection::Summary {
                id: "summary-duplicate".to_owned(),
                text: "First direct summary.".to_owned(),
            },
            CompiledContextSection::Summary {
                id: "summary-duplicate".to_owned(),
                text: "Second direct summary.".to_owned(),
            },
        ]
    );
    assert_eq!(
        compiled.to_snapshot(),
        [
            "summary:summary-duplicate",
            "text:First direct summary.",
            "summary:summary-duplicate",
            "text:Second direct summary.",
        ]
        .join("\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compiled_snapshot_includes_summary_text() {
    let compiler = ContextCompiler::new();
    let runtime = runtime("context-snapshot-text");
    record_summary(
        &runtime,
        summary("summary-a", "Build errors point at context ownership."),
    )
    .await;

    let context = compiler
        .compile(&runtime.context_snapshot().await)
        .expect("context compiles");

    assert_eq!(
        context.to_snapshot(),
        [
            "summary:summary-a",
            "text:Build errors point at context ownership.",
        ]
        .join("\n")
    );
}
