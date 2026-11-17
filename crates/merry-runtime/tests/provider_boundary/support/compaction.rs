use crate::support::runtime::collect_step;
use merry_core::RuntimeJournalPayload;
use merry_runtime::Runtime;

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
