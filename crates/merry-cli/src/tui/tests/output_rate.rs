use super::{pending_call, source};
use crate::tui::{
    keymap::Keymap, output_rate::OutputRate, projector::TuiProjector, render::render_to_text,
    state::TuiState, theme::TuiTheme,
};
use merry_core::{
    ContextWindowSource, ErrorInfo, InteractiveRunState, ModelOutputRate, ModelUsage, RuntimeEvent,
    SessionUsage, UsageContextWindow,
};
use std::time::Duration;

fn rate_event(tokens: u64, seconds: u64, estimated: bool) -> RuntimeEvent {
    RuntimeEvent::ModelOutputRateUpdated {
        rate: Some(ModelOutputRate::new(
            tokens,
            Duration::from_secs(seconds),
            if estimated {
                merry_core::OutputTokenSource::Estimated
            } else {
                merry_core::OutputTokenSource::ProviderUsage
            },
        )),
        source: source(),
    }
}

#[test]
fn runtime_sample_drives_estimate_without_a_ui_stopwatch() {
    let mut rate = OutputRate::default();
    rate.observe(&rate_event(400, 8, true));
    assert_eq!(rate.label(), "≈50.0 tok/s");
    rate.observe(&rate_event(600, 10, true));
    assert_eq!(rate.label(), "≈60.0 tok/s");
}

#[test]
fn actual_usage_corrects_tokens_without_adding_delivery_or_completion_latency() {
    let mut rate = OutputRate::default();
    rate.observe(&rate_event(400, 8, true));
    rate.observe(&rate_event(2_400, 8, false));
    assert_eq!(rate.label(), "300.0 tok/s");
    rate.observe(&RuntimeEvent::AssistantMessageDelta {
        delta: "late text".to_owned(),
        source: source(),
    });
    assert_eq!(rate.label(), "300.0 tok/s");
}

#[test]
fn unavailable_provider_timing_does_not_fall_back_to_text_or_running_time() {
    let mut rate = OutputRate::default();
    rate.observe(&RuntimeEvent::StepStarted { source: source() });
    rate.observe(&RuntimeEvent::AssistantMessageDelta {
        delta: "hello".to_owned(),
        source: source(),
    });
    assert_eq!(rate.label(), "- tok/s");
}

#[test]
fn restored_usage_does_not_create_a_rate_without_receive_timing() {
    let mut rate = OutputRate::default();
    rate.observe(&RuntimeEvent::UsageUpdated {
        usage: SessionUsage {
            last: ModelUsage::new(100, 100),
            total: ModelUsage::new(100, 100),
            context: None,
            compaction: None,
        },
        source: source(),
    });
    assert_eq!(rate.label(), "- tok/s");
}

#[test]
fn single_receive_instant_is_unavailable_but_zero_output_with_elapsed_time_is_valid() {
    let mut rate = OutputRate::default();
    rate.observe(&rate_event(100, 0, false));
    assert_eq!(rate.label(), "- tok/s");
    rate.observe(&rate_event(0, 2, false));
    assert_eq!(rate.label(), "0.0 tok/s");
}

#[test]
fn lifecycle_events_do_not_override_runtime_owned_samples() {
    for event in [
        RuntimeEvent::StepStarted { source: source() },
        RuntimeEvent::SessionStarted { source: source() },
        RuntimeEvent::ModelRetryAttemptStarted {
            attempt: 2,
            max_attempts: 2,
            source: source(),
        },
        RuntimeEvent::CompactionStarted { source: source() },
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::RunningModel,
        },
    ] {
        let mut rate = OutputRate::default();
        rate.observe(&rate_event(100, 2, false));
        rate.observe(&event);
        assert_eq!(rate.label(), "50.0 tok/s");
    }
}

#[test]
fn tools_idle_cancellation_and_failure_do_not_extend_provider_receive_time() {
    let mut rate = OutputRate::default();
    rate.observe(&rate_event(100, 2, true));
    for event in [
        RuntimeEvent::ToolCallStarted {
            call: pending_call("read", "read_text"),
            source: source(),
        },
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput,
        },
        RuntimeEvent::RunCancelled {
            diagnostic: ErrorInfo::new("cancelled", "cancelled").unwrap(),
            source: source(),
        },
        RuntimeEvent::RunFailed {
            diagnostic: ErrorInfo::new("failed", "failed").unwrap(),
            source: source(),
        },
        RuntimeEvent::Closed,
    ] {
        rate.observe(&event);
        assert_eq!(rate.label(), "≈50.0 tok/s");
    }
}

#[test]
fn estimates_remain_available_when_a_provider_does_not_report_usage() {
    let mut rate = OutputRate::default();
    rate.observe(&rate_event(100, 2, true));
    rate.observe(&RuntimeEvent::StepCompleted { source: source() });
    assert_eq!(rate.label(), "≈50.0 tok/s");
}

#[test]
fn header_shows_tool_only_response_rate_and_preserves_context_on_narrow_screens() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();
    projector.apply(RuntimeEvent::StepStarted { source: source() }, &mut state);
    projector.apply(rate_event(100, 2, false), &mut state);
    projector.apply(
        RuntimeEvent::UsageUpdated {
            usage: SessionUsage {
                total: ModelUsage::new(20_800, 100),
                last: ModelUsage::new(20_800, 100),
                context: Some(UsageContextWindow {
                    resolved_model_window_tokens: 272_000,
                    effective_window_tokens: 258_400,
                    source: ContextWindowSource::Fallback,
                }),
                compaction: None,
            },
            source: source(),
        },
        &mut state,
    );
    for width in [160, 72] {
        let rendered = render_to_text(&state, width, 16);
        assert!(rendered.contains("50.0 tok/s"));
        assert!(rendered.contains("ctx 20.8k/258.4k"));
    }
    let narrow = render_to_text(&state, 48, 16);
    assert!(narrow.contains("ctx 20.8k/258.4k"));
    assert!(!narrow.contains("50.0 tok/s"));
    assert!(
        state
            .status_text()
            .contains("last in 20.8k out 100 | total 20.9k tok")
    );
}

#[test]
fn runtime_reset_does_not_hide_the_last_measurable_sample() {
    let mut rate = OutputRate::default();
    rate.observe(&rate_event(100, 2, false));
    rate.observe(&RuntimeEvent::ModelOutputRateUpdated {
        rate: None,
        source: source(),
    });
    assert_eq!(rate.label(), "50.0 tok/s");
}

#[test]
fn backpressure_keeps_updating_the_rate_until_the_runtime_resets_it() {
    let mut rate = OutputRate::default();
    rate.observe(&rate_event(100, 2, false));
    assert_eq!(rate.label(), "50.0 tok/s");

    for (tokens, token_source, label) in [
        (
            400,
            merry_core::OutputTokenSource::Estimated,
            "≈100.0 tok/s",
        ),
        (
            800,
            merry_core::OutputTokenSource::ProviderUsage,
            "≈200.0 tok/s",
        ),
    ] {
        rate.observe(&RuntimeEvent::ModelOutputRateUpdated {
            rate: Some(
                ModelOutputRate::new(tokens, Duration::from_secs(4), token_source)
                    .with_timing_quality(merry_core::OutputTimingQuality::ConsumerLimited),
            ),
            source: source(),
        });
        assert_eq!(rate.label(), label);
    }
    rate.observe(&RuntimeEvent::StepCompleted { source: source() });
    assert_eq!(rate.label(), "≈200.0 tok/s");

    rate.observe(&RuntimeEvent::ModelOutputRateUpdated {
        rate: None,
        source: source(),
    });
    assert_eq!(rate.label(), "≈200.0 tok/s");
    rate.observe(&rate_event(50, 0, true));
    assert_eq!(rate.label(), "≈200.0 tok/s");
    rate.observe(&rate_event(150, 1, false));
    assert_eq!(rate.label(), "150.0 tok/s");
}

#[test]
fn an_unmeasurable_new_sample_does_not_hide_the_last_measurable_sample() {
    let mut rate = OutputRate::default();
    rate.observe(&rate_event(100, 2, false));
    rate.observe(&rate_event(2_400, 0, false));
    assert_eq!(rate.label(), "50.0 tok/s");
}

#[test]
fn timing_limitations_are_presented_as_estimates_not_missing_samples() {
    let mut rate = OutputRate::default();
    for quality in [
        merry_core::OutputTimingQuality::PartialOutput,
        merry_core::OutputTimingQuality::ConsumerLimited,
    ] {
        rate.observe(&RuntimeEvent::ModelOutputRateUpdated {
            rate: Some(
                ModelOutputRate::new(
                    100,
                    Duration::from_secs(2),
                    merry_core::OutputTokenSource::ProviderUsage,
                )
                .with_timing_quality(quality),
            ),
            source: source(),
        });
        assert_eq!(rate.label(), "≈50.0 tok/s");
    }
}
