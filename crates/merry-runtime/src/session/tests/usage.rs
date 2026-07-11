use crate::session::SessionState;
use merry_core::{
    CompactionUsageWindow, ContextWindowSource, ModelUsage, RuntimeJournalPayload, SessionId,
    UsageContextWindow,
};

fn session_id() -> SessionId {
    SessionId::new("usage-session").expect("valid session id")
}

#[test]
fn record_model_usage_accumulates_total_and_replaces_last() {
    let mut session = SessionState::new(session_id());
    let context = Some(UsageContextWindow {
        resolved_model_window_tokens: 128000,
        effective_window_tokens: 121600,
        source: ContextWindowSource::ProviderCapabilities,
    });
    let compaction = Some(CompactionUsageWindow {
        auto_compaction_enabled: true,
        dynamic_body_estimated_tokens: Some(64000),
        body_budget_tokens: 90000,
        soft_water_tokens: 70000,
        hard_water_tokens: 82000,
    });

    let first = session
        .record_model_usage(
            ModelUsage::with_details(10, Some(6), 4, None, 14),
            context,
            compaction,
        )
        .expect("usage should record");
    let second = session
        .record_model_usage(ModelUsage::with_details(3, None, 2, Some(1), 5), None, None)
        .expect("usage should record");

    let RuntimeJournalPayload::SessionUsageUpdated { usage } = second.payload else {
        panic!("expected usage update");
    };
    assert_eq!(first.sequence, 0);
    assert_eq!(second.sequence, 1);
    assert_eq!(usage.total.input_tokens, 13);
    assert_eq!(usage.total.cached_input_tokens, None);
    assert_eq!(usage.total.output_tokens, 6);
    assert_eq!(usage.total.reasoning_output_tokens, None);
    assert_eq!(usage.total.total_tokens, 19);
    assert_eq!(usage.last, ModelUsage::with_details(3, None, 2, Some(1), 5));
    assert_eq!(usage.context, None);
    assert_eq!(usage.compaction, None);
    assert_eq!(session.usage(), Some(usage));
}
