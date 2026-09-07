use crate::tui::{
    keymap::Keymap, projector::TuiProjector, render::render_to_text, state::TuiState,
    tests::source, theme::TuiTheme,
};
use merry_core::{
    CompactionUsageWindow, ContextWindowSource, InteractiveRunState, ModelUsage, RuntimeEvent,
    SessionUsage, UsageContextWindow,
};
use std::time::{Duration, Instant};

#[test]
fn projector_updates_queue_preview_and_usage_without_timeline_noise() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::QueuedInputsChanged {
            inputs: merry_core::QueuedInputsView {
                next: vec![merry_core::QueuedInputView {
                    text: "urgent".to_owned(),
                    lane: merry_core::QueuedInputLane::Next,
                    position: 0,
                }],
                suspended: vec![],
                backlog: vec![],
            },
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::UsageUpdated {
            usage: SessionUsage {
                total: ModelUsage::with_details(20_000, Some(18_000), 1_000, None, 21_000),
                last: ModelUsage::with_details(20_000, Some(18_000), 1_000, None, 21_000),
                context: Some(UsageContextWindow {
                    resolved_model_window_tokens: 64_000,
                    effective_window_tokens: 60_800,
                    source: ContextWindowSource::Fallback,
                }),
                compaction: Some(CompactionUsageWindow {
                    auto_compaction_enabled: true,
                    dynamic_body_estimated_tokens: Some(20_200),
                    body_budget_tokens: 56_792,
                    soft_water_tokens: 46_792,
                    hard_water_tokens: 54_792,
                }),
            },
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.queue_preview().next[0].text, "urgent");
    assert!(state.timeline().is_empty());
    assert!(state.status_text().contains("ctx 20.2k/54.8k"));
    assert!(state.status_text().contains("win 64k fallback"));
    assert!(state.status_text().contains("cache 90%"));
    assert!(
        state
            .status_text()
            .contains("last in 20k out 1k | total 21k tok")
    );
}

#[test]
fn narrow_header_preserves_context_pressure_before_secondary_usage() {
    let mut state = TuiState::new(
        "/home/alice/source/rust/merry".into(),
        "gpt-5.6-sol xhigh".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.set_usage(SessionUsage {
        total: ModelUsage::with_details(2_627_620, Some(2_045_312), 31_541, None, 2_659_161),
        last: ModelUsage::with_details(29_021, Some(28_160), 1_301, None, 30_322),
        context: Some(UsageContextWindow {
            resolved_model_window_tokens: 64_000,
            effective_window_tokens: 60_800,
            source: ContextWindowSource::Fallback,
        }),
        compaction: Some(CompactionUsageWindow {
            auto_compaction_enabled: true,
            dynamic_body_estimated_tokens: Some(20_200),
            body_budget_tokens: 56_792,
            soft_water_tokens: 46_792,
            hard_water_tokens: 54_792,
        }),
    });

    let rendered = render_to_text(&state, 72, 16);

    assert!(rendered.contains("ctx 20.2k/54.8k"));
    assert!(!rendered.contains("total 2659.1k"));
}

#[test]
fn narrow_header_counts_wide_characters_when_preserving_context() {
    let mut state = TuiState::new(
        "/home/用户/项目/非常长的中文目录/merry".into(),
        "模型-gpt-5.6-超高".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.set_usage(SessionUsage {
        total: ModelUsage::with_details(20_000, Some(18_000), 1_000, None, 21_000),
        last: ModelUsage::with_details(20_000, Some(18_000), 1_000, None, 21_000),
        context: Some(UsageContextWindow {
            resolved_model_window_tokens: 64_000,
            effective_window_tokens: 60_800,
            source: ContextWindowSource::Fallback,
        }),
        compaction: Some(CompactionUsageWindow {
            auto_compaction_enabled: true,
            dynamic_body_estimated_tokens: Some(20_200),
            body_budget_tokens: 56_792,
            soft_water_tokens: 46_792,
            hard_water_tokens: 54_792,
        }),
    });

    let rendered = render_to_text(&state, 48, 16);

    assert!(rendered.contains("ctx 20.2k/54.8k"));
}

#[test]
fn status_text_compacts_large_usage_counts_but_keeps_last_in_out_visible() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    state.set_usage(SessionUsage {
        total: ModelUsage::new(12_000, 1_248),
        last: ModelUsage::new(11_000, 1_000),
        context: None,
        compaction: None,
    });

    assert!(
        state
            .status_text()
            .contains("last in 11k out 1k | total 13.2k tok")
    );
}

#[test]
fn status_text_shows_model_reasoning_effort() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-5.5".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.set_reasoning_effort_label(Some("medium".to_owned()));

    assert!(state.status_text().contains("gpt-5.5 medium"));
}

#[test]
fn status_text_shows_compact_merry_shuttle_and_elapsed_while_running() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let now = Instant::now();

    state.set_run_state_at(InteractiveRunState::RunningModel, now);
    let frames = [
        (0, "[M··]"),
        (100, "[·M·]"),
        (200, "[··M]"),
        (300, "[·M·]"),
        (400, "[M··]"),
    ];
    for (elapsed_ms, expected) in frames {
        let status = state.interaction_status_text_at(now + Duration::from_millis(elapsed_ms));
        assert_eq!(status.split_whitespace().next(), Some(expected));
    }

    assert!(
        state
            .interaction_status_text_at(now + Duration::from_secs(37))
            .contains("Running model (37s)")
    );
}

#[test]
fn status_text_uses_quiet_ready_label_when_waiting() {
    let state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    let status = state.interaction_status_text();
    assert_eq!(status, "Ready");
    assert!(!status.contains("Running"));
    assert!(!status.contains("[M"));
}

#[test]
fn interaction_status_keeps_completed_run_elapsed_after_returning_ready() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let started = Instant::now();

    state.set_run_state_at(InteractiveRunState::RunningModel, started);
    state.set_run_state_at(
        InteractiveRunState::WaitingForInput,
        started + Duration::from_secs(42),
    );

    assert_eq!(state.interaction_status_text(), "Ready  last run 42s");
}
