use super::{
    controller::{ControllerEffect, handle_key_action, project_local_effect},
    keymap::{KeyAction, Keymap},
    projector::TuiProjector,
    render,
    state::{TimelineItem, TuiState},
    theme::TuiTheme,
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, InteractiveRunState, PendingToolCall,
    QueuedInputLane, QueuedInputView, RuntimeEvent, RuntimeEventSource, SessionId,
    TOOL_CANCELLED_BY_USER_CODE, ToolCallArguments, ToolCallId, ToolCallResult, ToolName,
    ToolOutput,
};

fn state() -> TuiState {
    TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
}

fn runtime_source() -> RuntimeEventSource {
    RuntimeEventSource::new(SessionId::new("slash-stop-test").expect("session id"), 1)
}

fn run_cancelled_event() -> RuntimeEvent {
    RuntimeEvent::RunCancelled {
        diagnostic: ErrorInfo::new("cancelled", "runtime step cancelled")
            .expect("valid diagnostic"),
        source: runtime_source(),
    }
}

fn pending_tool_call(id: &str) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(id).expect("tool call id"),
        ToolName::new("workspace_search_text").expect("tool name"),
        ToolCallArguments::new(Default::default()),
    )
}

fn tool_cancelled_event(id: &str) -> RuntimeEvent {
    let artifact_id = format!("{id}-cancelled");
    let diagnostic_message = format!("tool call {id} was cancelled by user interrupt");
    RuntimeEvent::ToolCallFinished {
        result: ToolCallResult::failed(
            ToolCallId::new(id).expect("tool call id"),
            ArtifactRef::new(
                ArtifactId::new(&artifact_id).expect("artifact id"),
                ArtifactKind::Text,
            ),
            ErrorInfo::new(TOOL_CANCELLED_BY_USER_CODE, &diagnostic_message)
                .expect("cancellation diagnostic"),
        ),
        output: Some(ToolOutput::Text {
            text: "Tool execution was cancelled by user interrupt.".to_owned(),
        }),
        source: runtime_source(),
    }
}

#[test]
fn tool_cancellation_finishes_stop_without_reporting_an_error() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    state.set_run_state(InteractiveRunState::RunningTool);
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_tool_call("call-cancelled"),
            source: runtime_source(),
        },
        &mut state,
    );

    state.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::Interrupt
    );
    projector.apply(tool_cancelled_event("call-cancelled"), &mut state);
    projector.apply(
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput,
        },
        &mut state,
    );

    assert!(matches!(
        state.timeline().first(),
        Some(TimelineItem::Muted { title, detail })
            if title.ends_with("-> cancelled") && detail == "cancelled by user"
    ));
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Run stopped"
    ));
    assert_eq!(
        state
            .timeline()
            .iter()
            .filter(|item| matches!(
                item,
                TimelineItem::LocalCommand { title, .. } if title == "Run stopped"
            ))
            .count(),
        1
    );
    assert!(
        state
            .timeline()
            .iter()
            .all(|item| !matches!(item, TimelineItem::Diagnostic { .. }))
    );
}

#[test]
fn batched_tool_cancellation_completes_stop_feedback_once() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    state.set_run_state(InteractiveRunState::RunningTool);
    for id in ["call-one", "call-two"] {
        projector.apply(
            RuntimeEvent::ToolCallStarted {
                call: pending_tool_call(id),
                source: runtime_source(),
            },
            &mut state,
        );
    }

    state.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::Interrupt
    );
    projector.apply(tool_cancelled_event("call-one"), &mut state);
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Stopping"
    ));
    assert!(state.timeline().iter().all(|item| !matches!(
        item,
        TimelineItem::LocalCommand { title, .. } if title == "Run stopped"
    )));
    projector.apply(tool_cancelled_event("call-two"), &mut state);
    assert!(state.timeline().iter().all(|item| !matches!(
        item,
        TimelineItem::LocalCommand { title, .. } if title == "Run stopped"
    )));
    projector.apply(
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput,
        },
        &mut state,
    );

    assert_eq!(
        state
            .timeline()
            .iter()
            .filter(|item| matches!(
                item,
                TimelineItem::Muted { title, .. } if title.ends_with("-> cancelled")
            ))
            .count(),
        2
    );
    assert_eq!(
        state
            .timeline()
            .iter()
            .filter(|item| matches!(
                item,
                TimelineItem::LocalCommand { title, .. } if title == "Run stopped"
            ))
            .count(),
        1
    );
}

#[test]
fn compaction_cancellation_finishes_both_progress_and_stop_feedback() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    state.set_run_state(InteractiveRunState::RunningModel);
    projector.apply(
        RuntimeEvent::CompactionStarted {
            source: runtime_source(),
        },
        &mut state,
    );

    state.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::Interrupt
    );
    projector.apply(run_cancelled_event(), &mut state);

    assert!(matches!(
        state.timeline().first(),
        Some(TimelineItem::Muted { title, .. }) if title == "Compaction cancelled"
    ));
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Run stopped"
    ));
    assert!(state.timeline().iter().all(|item| !matches!(
        item,
        TimelineItem::LocalCommand { title, .. }
            if matches!(title.as_str(), "Stopping" | "Stop already requested")
    )));
}

#[test]
fn interleaved_stop_feedback_stays_chronological_and_finishes_latest() {
    let mut state = state();
    state.set_run_state(InteractiveRunState::RunningModel);

    state.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::Interrupt
    );
    state.insert_input_str("/status");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::None
    );
    state.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::None
    );
    TuiProjector::default().apply(run_cancelled_event(), &mut state);

    assert!(matches!(
        state.timeline().first(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Stop requested"
    ));
    assert!(matches!(
        state.timeline().get(1),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Session status"
    ));
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Run stopped"
    ));
    assert!(state.timeline().iter().all(|item| !matches!(
        item,
        TimelineItem::LocalCommand { title, .. }
            if matches!(title.as_str(), "Stopping" | "Stop already requested")
    )));
}

#[test]
fn waiting_state_is_a_stop_completion_fallback_without_duplicates() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    state.set_run_state(InteractiveRunState::RunningModel);
    state.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::Interrupt
    );

    for _ in 0..2 {
        projector.apply(
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            },
            &mut state,
        );
    }

    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Run stopped"
    ));
    assert_eq!(
        state
            .timeline()
            .iter()
            .filter(|item| matches!(
                item,
                TimelineItem::LocalCommand { title, .. } if title == "Run stopped"
            ))
            .count(),
        1
    );
}

#[test]
fn slash_stop_feedback_is_idempotent_when_the_effect_is_locally_projected() {
    let mut state = state();
    state.set_run_state(InteractiveRunState::RunningModel);
    state.insert_input_str("/stop");

    let stop = handle_key_action(KeyAction::SubmitNext, &mut state);
    assert_eq!(stop, ControllerEffect::Interrupt);
    assert!(matches!(
        state.timeline(),
        [TimelineItem::LocalCommand { title, .. }] if title == "Stopping"
    ));

    project_local_effect(&stop, &mut state);

    assert!(matches!(
        state.timeline(),
        [TimelineItem::LocalCommand { title, .. }] if title == "Stopping"
    ));
}

#[test]
fn stale_waiting_does_not_complete_stop_for_a_pending_local_run() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    state.insert_input_str("start work");
    let submit = handle_key_action(KeyAction::SubmitNext, &mut state);
    assert!(matches!(submit, ControllerEffect::SubmitNext(_)));
    project_local_effect(&submit, &mut state);

    state.insert_input_str("/stop");
    let stop = handle_key_action(KeyAction::SubmitNext, &mut state);
    assert_eq!(stop, ControllerEffect::Interrupt);
    project_local_effect(&stop, &mut state);
    projector.apply(
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput,
        },
        &mut state,
    );

    assert!(state.is_interrupting());
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Stopping"
    ));

    projector.apply(
        RuntimeEvent::QueuedInputAccepted {
            lane: QueuedInputLane::Next,
            inputs: vec![QueuedInputView {
                text: "start work".to_owned(),
                lane: QueuedInputLane::Next,
                position: 0,
            }],
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::RunningModel,
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::Interrupting,
        },
        &mut state,
    );
    projector.apply(run_cancelled_event(), &mut state);

    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Run stopped"
    ));
    assert_eq!(
        state
            .timeline()
            .iter()
            .filter(|item| matches!(
                item,
                TimelineItem::LocalCommand { title, .. } if title == "Run stopped"
            ))
            .count(),
        1
    );
}

#[test]
fn timeline_beyond_u16_scroll_limit_still_shows_latest_command_feedback() {
    let mut state = state();
    for _ in 0..33_000 {
        state.push_timeline_item(TimelineItem::Muted {
            title: "Earlier event".to_owned(),
            detail: "completed".to_owned(),
        });
    }
    state.insert_input_str("/help");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::None
    );

    let rendered = render::render_to_text(&state, 80, 16);
    assert!(rendered.contains("Command help"));
    assert!(rendered.contains("/stop"));
}

#[test]
fn single_wrapped_line_beyond_u16_scroll_limit_keeps_its_tail_visible() {
    let mut state = state();
    let mut text = "x".repeat(1_400_000);
    text.push_str("TAIL_SENTINEL");
    state.push_timeline_item(TimelineItem::User {
        text,
        lane: QueuedInputLane::Next,
    });

    let rendered = render::render_to_text(&state, 20, 12);
    assert!(rendered.contains("TAIL_SENTINEL"), "{rendered}");
}
