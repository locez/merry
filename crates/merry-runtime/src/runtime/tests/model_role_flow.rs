use crate::runtime::{Runtime, tests::support::common::collect_step};
use merry_core::RuntimeJournalPayload;

// Keep tight-window fixtures on compaction boundaries instead of the conservative no-cap fallback.
const TIGHT_WINDOW_OUTPUT_CAP_TOKENS: u64 = 512;

#[path = "model_role_flow/compaction_generation.rs"]
mod compaction_generation;

async fn seed_two_history_items_for_compaction(runtime: &Runtime) {
    let events = collect_step(
        runtime,
        "old user message for compaction",
        crate::StepContext::default(),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "seed step should complete"
    );
    let events = collect_step(
        runtime,
        "retained tail user message",
        crate::StepContext::default(),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "tail seed step should complete"
    );
}

mod automatic_compaction;

mod budget;

mod manual_compaction;

mod routing;
