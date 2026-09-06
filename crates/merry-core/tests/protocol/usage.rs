use crate::assert_json_round_trip;
use merry_core::{
    CompactionUsageWindow, ContextWindowSource, ModelUsage, RuntimeEvent, RuntimeEventSource,
    RuntimeJournalPayload, SessionId, SessionUsage, UsageContextWindow,
};
use serde_json::json;

#[test]
fn usage_protocol_round_trips_and_preserves_unknown_subcounts() {
    let last = ModelUsage::with_details(2400, Some(2200), 260, None, 2660);
    let total = ModelUsage::with_details(12000, Some(8000), 1400, None, 13400);
    let usage = SessionUsage {
        total,
        last,
        context: Some(UsageContextWindow {
            resolved_model_window_tokens: 128000,
            effective_window_tokens: 121600,
            source: ContextWindowSource::ProviderCapabilities,
        }),
        compaction: Some(CompactionUsageWindow {
            auto_compaction_enabled: true,
            dynamic_body_estimated_tokens: Some(64000),
            body_budget_tokens: 90000,
            soft_water_tokens: 70000,
            hard_water_tokens: 82000,
        }),
    };

    assert_json_round_trip(&last);
    assert_json_round_trip(&usage);
    assert_eq!(last.cached_input_tokens, Some(2200));
    assert_eq!(last.reasoning_output_tokens, None);

    assert_eq!(
        serde_json::to_value(&usage).expect("usage serializes"),
        json!({
            "total": {
                "input_tokens": 12000,
                "cached_input_tokens": 8000,
                "output_tokens": 1400,
                "reasoning_output_tokens": null,
                "total_tokens": 13400
            },
            "last": {
                "input_tokens": 2400,
                "cached_input_tokens": 2200,
                "output_tokens": 260,
                "reasoning_output_tokens": null,
                "total_tokens": 2660
            },
            "context": {
                "resolved_model_window_tokens": 128000,
                "effective_window_tokens": 121600,
                "source": "provider_capabilities"
            },
            "compaction": {
                "auto_compaction_enabled": true,
                "dynamic_body_estimated_tokens": 64000,
                "body_budget_tokens": 90000,
                "soft_water_tokens": 70000,
                "hard_water_tokens": 82000
            }
        })
    );
}

#[test]
fn compaction_usage_accepts_sessions_without_dynamic_body_estimate() {
    let usage: CompactionUsageWindow = serde_json::from_value(json!({
        "auto_compaction_enabled": true,
        "body_budget_tokens": 90000,
        "soft_water_tokens": 70000,
        "hard_water_tokens": 82000
    }))
    .expect("older compaction usage should remain readable");

    assert_eq!(usage.dynamic_body_estimated_tokens, None);
}

#[test]
fn usage_updated_events_round_trip_as_full_snapshots() {
    let usage = SessionUsage {
        total: ModelUsage::new(10, 4),
        last: ModelUsage::new(10, 4),
        context: None,
        compaction: None,
    };
    let source = RuntimeEventSource::new(
        SessionId::new("usage-event-session").expect("valid session id"),
        7,
    );

    assert_json_round_trip(&RuntimeJournalPayload::SessionUsageUpdated {
        usage: usage.clone(),
    });
    assert_json_round_trip(&RuntimeEvent::UsageUpdated {
        usage: usage.clone(),
        source,
    });
}

#[test]
fn compaction_lifecycle_events_round_trip_as_low_noise_public_events() {
    let source = RuntimeEventSource::new(
        SessionId::new("compaction-event-session").expect("valid session id"),
        8,
    );

    assert_json_round_trip(&RuntimeJournalPayload::CompactionStarted);
    assert_json_round_trip(&RuntimeJournalPayload::CompactionCompleted {
        checkpoint_id: "checkpoint-session-8".to_owned(),
        covered_history_item_count: 6,
    });

    assert_eq!(
        serde_json::to_value(&RuntimeEvent::CompactionStarted {
            source: source.clone(),
        })
        .expect("event serializes"),
        json!({
            "type": "compaction_started",
            "source": {
                "session_id": "compaction-event-session",
                "sequence": 8
            }
        })
    );
    assert_eq!(
        serde_json::to_value(&RuntimeEvent::CompactionCompleted {
            checkpoint_id: "checkpoint-session-8".to_owned(),
            covered_history_item_count: 6,
            source,
        })
        .expect("event serializes"),
        json!({
            "type": "compaction_completed",
            "checkpoint_id": "checkpoint-session-8",
            "covered_history_item_count": 6,
            "source": {
                "session_id": "compaction-event-session",
                "sequence": 8
            }
        })
    );
}
