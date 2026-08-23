use super::{
    command::PaletteCommand,
    command_controller::run_palette_command,
    completion::{CompletionKind, CompletionSources},
    controller::{ControllerEffect, handle_key_action, handle_key_event, project_local_effect},
    input::{DraftImage, TuiSubmission},
    keymap::{KeyAction, Keymap},
    overlay::Overlay,
    projector::TuiProjector,
    render,
    state::{TimelineItem, TuiState},
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{
    ErrorInfo, InteractiveRunState, QueuedInputLane, QueuedInputView, RuntimeEvent,
    RuntimeEventSource, SessionId,
};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::Arc;

fn state() -> TuiState {
    TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
}

fn text_submission(text: &str) -> TuiSubmission {
    TuiSubmission {
        text: text.to_owned(),
        history_text: text.to_owned(),
        images: Vec::new(),
    }
}

fn draft_image() -> DraftImage {
    DraftImage::new(
        Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 7]),
        2,
        3,
    )
    .expect("valid draft image")
}

fn plan_snapshot() -> merry_core::PlanSnapshot {
    merry_core::PlanSnapshot {
        plan_id: merry_core::PlanId::new("slash-test-plan").expect("plan id"),
        revision: 1,
        phase: merry_core::PlanPhase::Executing,
        activation_source: merry_core::PlanActivationSource::User,
        root_node_id: None,
        coordinator_node_id: None,
        execution_contract_fingerprint: None,
        execution_authorization_refs: Vec::new(),
        authorized_capability_envelope: None,
        approval_requirements: Vec::new(),
        nodes: Vec::new(),
        attempts: Vec::new(),
        leases: Vec::new(),
        attempt_progress: Vec::new(),
        directives: Vec::new(),
        resource_policy_snapshot: merry_core::PlanResourcePolicySnapshot::default(),
        max_concurrency_hint: None,
        scheduler_status: merry_core::PlanSchedulerStatus::Active,
        revision_summaries: Vec::new(),
    }
}

fn assert_timeline_feedback(state: &TuiState, expected: &[&str]) {
    for (width, height) in [(50, 20), (80, 16), (120, 24)] {
        let rendered = render::render_to_text(state, width, height);
        for value in expected {
            assert!(
                rendered.contains(value),
                "{value:?} should render at {width}x{height}:\n{rendered}"
            );
        }
    }
}

fn run_cancelled_event() -> RuntimeEvent {
    RuntimeEvent::RunCancelled {
        diagnostic: ErrorInfo::new("cancelled", "runtime step cancelled")
            .expect("valid diagnostic"),
        source: runtime_source(),
    }
}

fn runtime_source() -> RuntimeEventSource {
    RuntimeEventSource::new(SessionId::new("slash-stop-test").expect("session id"), 1)
}

#[test]
fn slash_completion_uses_the_shared_registry_only_at_the_input_start() {
    let sources = CompletionSources::from_skill_names("/repo".into(), &[]);
    let all = sources
        .menu_for_input("/", 1, None)
        .expect("slash completion");
    assert_eq!(all.items().len(), 4);
    assert!(
        all.items()
            .iter()
            .all(|item| item.kind() == &CompletionKind::Slash)
    );
    assert_eq!(
        all.items()
            .iter()
            .map(|item| item.value())
            .collect::<Vec<_>>(),
        vec!["/help", "/save", "/status", "/stop"]
    );

    let save = sources
        .menu_for_input("/sa", 3, None)
        .expect("save completion");
    assert_eq!(save.items()[0].value(), "/save");
    assert!(save.items()[0].detail().is_some());
    assert!(sources.menu_for_input("hello", 5, None).is_none());
    assert!(sources.menu_for_input("hello /sa", 9, None).is_none());
    assert!(sources.menu_for_input("/xyz", 4, None).is_none());

    let middle = sources
        .menu_for_input("/save", 3, None)
        .expect("mid-token slash completion");
    assert_eq!(middle.replacement_range(), 0..5);
    assert_eq!(middle.replacement_text(), Some("/save".to_owned()));
}

#[test]
fn slash_completion_does_not_auto_submit_multiline_or_attached_input() {
    let mut multiline = state();
    multiline.insert_input_str("/sa\nexplain");
    multiline.handle_input_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    for _ in 0..3 {
        multiline.handle_input_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    }
    assert!(multiline.completion_menu().is_none());

    let mut attached = state();
    attached.insert_input_str("/sa ");
    attached
        .insert_input_image(draft_image())
        .expect("image attaches");
    attached.handle_input_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    for _ in 0..3 {
        attached.handle_input_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    }
    assert!(attached.completion_menu().is_none());
}

#[test]
fn enter_executes_a_slash_completion_once_while_tab_only_completes() {
    let mut enter_state = state();
    enter_state.insert_input_str("/sa");
    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut enter_state,
    );
    assert_eq!(effect, ControllerEffect::SaveSession);
    assert_eq!(enter_state.input_text(), "");
    assert!(enter_state.input_history_entries().is_empty());

    let mut tab_state = state();
    tab_state.insert_input_str("/sa");
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut tab_state,
        ),
        ControllerEffect::None
    );
    assert_eq!(tab_state.input_text(), "/save");
    assert!(tab_state.completion_menu().is_none());
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut tab_state,
        ),
        ControllerEffect::SaveSession
    );

    let mut mid_token = state();
    mid_token.set_run_state(InteractiveRunState::RunningModel);
    mid_token.insert_input_str("/stop");
    for _ in 0..3 {
        mid_token.handle_input_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    }
    assert_eq!(
        mid_token
            .completion_menu()
            .and_then(|menu| menu.replacement_text()),
        Some("/stop".to_owned())
    );
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut mid_token,
        ),
        ControllerEffect::Interrupt
    );
}

#[test]
fn local_slash_commands_do_not_enter_user_history_or_either_runtime_lane() {
    let mut help = state();
    help.insert_input_str("/help");
    assert_eq!(
        handle_key_action(KeyAction::SubmitBacklog, &mut help),
        ControllerEffect::None
    );
    assert_eq!(help.input_text(), "");
    assert!(help.input_history_entries().is_empty());
    assert!(matches!(
        help.timeline().last(),
        Some(TimelineItem::LocalCommand { title, body })
            if title == "Command help" && body.contains("/status")
    ));

    let mut status = state();
    status.insert_input_str("/status");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut status),
        ControllerEffect::None
    );
    assert!(matches!(
        status.timeline().last(),
        Some(TimelineItem::LocalCommand { title, body })
            if title == "Session status"
                && body.contains("Run: ready")
                && body.contains("Plan: none")
                && body.contains("Workspace: /repo")
    ));
    assert!(
        status
            .timeline()
            .iter()
            .all(|item| !matches!(item, TimelineItem::User { .. }))
    );
}

#[test]
fn help_and_status_render_complete_bodies_at_supported_terminal_sizes() {
    for (width, height) in [(50, 20), (80, 16), (80, 24), (120, 24), (120, 36)] {
        let mut help = state();
        help.insert_input_str("/help");
        assert_eq!(
            handle_key_action(KeyAction::SubmitNext, &mut help),
            ControllerEffect::None
        );
        let help_text = render::render_to_text(&help, width, height);
        for expected in [
            "Command help",
            "/help",
            "/save",
            "/status",
            "/stop",
            "List slash commands",
            "Submit",
            "Backlog",
            "Commands",
        ] {
            assert!(
                help_text.contains(expected),
                "{expected:?} should render at {width}x{height}:\n{help_text}"
            );
        }

        let mut status = state();
        status.insert_input_str("/status");
        assert_eq!(
            handle_key_action(KeyAction::SubmitNext, &mut status),
            ControllerEffect::None
        );
        let status_text = render::render_to_text(&status, width, height);
        for expected in [
            "Session status",
            "Run: ready",
            "Model: gpt-test",
            "Usage:",
            "Plan: none",
            "Workspace: /repo",
        ] {
            assert!(
                status_text.contains(expected),
                "{expected:?} should render at {width}x{height}:\n{status_text}"
            );
        }
    }
}

#[test]
fn status_reports_every_runtime_state_and_the_current_plan_phase() {
    for (run_state, expected) in [
        (InteractiveRunState::WaitingForInput, "Run: ready"),
        (InteractiveRunState::RunningModel, "Run: running model"),
        (InteractiveRunState::RunningTool, "Run: running tool"),
        (InteractiveRunState::Interrupting, "Run: interrupting"),
        (InteractiveRunState::Closed, "Run: closed"),
    ] {
        let mut state = state();
        state.plan_mut().update_snapshot(plan_snapshot());
        state.set_run_state(run_state);

        assert_eq!(
            run_palette_command(PaletteCommand::ShowStatus, &mut state),
            ControllerEffect::None
        );
        assert!(matches!(
            state.timeline().last(),
            Some(TimelineItem::LocalCommand { body, .. })
                if body.contains(expected) && body.contains("Plan: executing")
        ));
    }
}

#[test]
fn palette_slash_commands_reveal_feedback_from_plan_focus() {
    for (width, height) in [(50, 20), (80, 16), (120, 24)] {
        for (command, run_state, effect, expected) in [
            (
                PaletteCommand::ShowHelp,
                InteractiveRunState::WaitingForInput,
                ControllerEffect::None,
                "Commands",
            ),
            (
                PaletteCommand::ShowStatus,
                InteractiveRunState::WaitingForInput,
                ControllerEffect::None,
                "Workspace: /repo",
            ),
            (
                PaletteCommand::Interrupt,
                InteractiveRunState::RunningModel,
                ControllerEffect::Interrupt,
                "Interrupt requested",
            ),
            (
                PaletteCommand::SaveSession,
                InteractiveRunState::RunningTool,
                ControllerEffect::None,
                "Stop the active run",
            ),
        ] {
            let mut state = state();
            state.plan_mut().update_snapshot(plan_snapshot());
            assert!(state.plan_mut().open_and_focus());
            state.set_run_state(run_state);

            assert!(state.plan().is_focused());
            assert_eq!(run_palette_command(command, &mut state), effect);
            assert!(state.plan().is_open());
            assert!(!state.plan().is_focused());

            let rendered = render::render_to_text(&state, width, height);
            assert!(
                rendered.contains(expected),
                "{expected:?} should render for {command:?} at {width}x{height}:\n{rendered}"
            );
        }
    }
}

#[test]
fn invalid_slash_feedback_is_visible_from_plan_focus_and_remains_correctable() {
    for (input, expected) in [
        ("/unknown", "Unknown command"),
        ("/help now", "Command not run"),
    ] {
        let mut state = state();
        state.plan_mut().update_snapshot(plan_snapshot());
        assert!(state.plan_mut().open_and_focus());
        state.insert_input_str(input);

        assert_eq!(
            handle_key_action(KeyAction::SubmitNext, &mut state),
            ControllerEffect::None
        );
        assert!(state.plan().is_open());
        assert!(!state.plan().is_focused());
        assert_eq!(state.input_text(), input);
        assert!(render::render_to_text(&state, 80, 16).contains(expected));
    }
}

#[test]
fn help_remains_complete_with_the_plan_open_at_80_by_16() {
    let mut state = state();
    state.plan_mut().update_snapshot(plan_snapshot());
    assert!(state.plan_mut().open_and_focus());

    assert_eq!(
        run_palette_command(PaletteCommand::ShowHelp, &mut state),
        ControllerEffect::None
    );

    let rendered = render::render_to_text(&state, 80, 16);
    for expected in [
        "Command help",
        "/help",
        "/save",
        "/status",
        "/stop",
        "Submit",
        "Backlog",
        "Commands",
        "Stop",
    ] {
        assert!(
            rendered.contains(expected),
            "{expected:?} should render with Plan open at 80x16:\n{rendered}"
        );
    }
    assert!(state.plan().is_open());
    assert!(!state.plan().is_focused());
}

#[test]
fn local_slash_input_bypasses_timeline_review_confirmation() {
    let mut timeline_review = state();
    timeline_review.push_timeline_item(TimelineItem::User {
        text: "earlier request".to_owned(),
        lane: QueuedInputLane::Next,
    });
    timeline_review.jump_to_previous_user_input();
    timeline_review.insert_input_str("/help");

    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut timeline_review),
        ControllerEffect::None
    );
    assert!(!timeline_review.is_timeline_reviewing());
    assert_eq!(timeline_review.input_text(), "");
    assert!(matches!(
        timeline_review.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Command help"
    ));

    let mut invalid_input = state();
    invalid_input.insert_input_str("/unknown");

    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut invalid_input),
        ControllerEffect::None
    );
    assert_eq!(invalid_input.input_text(), "/unknown");
    assert!(matches!(
        invalid_input.timeline().last(),
        Some(TimelineItem::Muted { title, .. }) if title == "Unknown command"
    ));

    let mut provider_input = state();
    provider_input.insert_input_str("/tmp/file");

    assert_eq!(
        handle_key_action(KeyAction::SubmitBacklog, &mut provider_input),
        ControllerEffect::SubmitBacklog(text_submission("/tmp/file"))
    );
    assert_eq!(provider_input.input_text(), "");
}

#[test]
fn keyboard_stop_reveals_the_same_feedback_as_slash_stop() {
    let mut state = state();
    state.plan_mut().update_snapshot(plan_snapshot());
    assert!(state.plan_mut().open_and_focus());
    state.set_run_state(InteractiveRunState::RunningModel);

    let effect = handle_key_action(KeyAction::Interrupt, &mut state);
    assert_eq!(effect, ControllerEffect::Interrupt);
    project_local_effect(&effect, &mut state);

    assert!(state.plan().is_open());
    assert!(!state.plan().is_focused());
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, body })
            if title == "Stopping" && body.contains("Interrupt requested")
    ));

    assert_eq!(
        handle_key_action(KeyAction::Interrupt, &mut state),
        ControllerEffect::None
    );
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, body })
            if title == "Stop already requested" && body.contains("cancellation boundary")
    ));
}

#[test]
fn stop_and_save_respect_the_current_run_state() {
    let mut idle_stop = state();
    idle_stop.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut idle_stop),
        ControllerEffect::None
    );
    assert!(matches!(
        idle_stop.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Nothing to stop"
    ));
    assert_timeline_feedback(&idle_stop, &["Nothing to stop", "No model or tool run"]);

    let mut running_stop = state();
    running_stop.set_run_state(InteractiveRunState::RunningModel);
    running_stop.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut running_stop),
        ControllerEffect::Interrupt
    );
    assert_timeline_feedback(
        &running_stop,
        &["Stopping", "Interrupt requested for the active run"],
    );

    let mut cancelled_stop = state();
    cancelled_stop.set_run_state(InteractiveRunState::RunningModel);
    cancelled_stop.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut cancelled_stop),
        ControllerEffect::Interrupt
    );
    TuiProjector::default().apply(run_cancelled_event(), &mut cancelled_stop);
    assert!(matches!(
        cancelled_stop.timeline().last(),
        Some(TimelineItem::LocalCommand { title, body })
            if title == "Run stopped" && body.contains("cancellation boundary")
    ));
    assert!(
        cancelled_stop
            .timeline()
            .iter()
            .all(|item| !matches!(item, TimelineItem::Diagnostic { .. }))
    );
    assert_timeline_feedback(
        &cancelled_stop,
        &["Run stopped", "The active run reached", "boundary."],
    );

    let mut unexpected_cancel = state();
    TuiProjector::default().apply(run_cancelled_event(), &mut unexpected_cancel);
    assert!(matches!(
        unexpected_cancel.timeline().last(),
        Some(TimelineItem::Diagnostic { title, body })
            if title == "cancelled" && body == "runtime step cancelled"
    ));

    running_stop.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut running_stop),
        ControllerEffect::None
    );
    assert!(matches!(
        running_stop.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Stop already requested"
    ));
    assert_eq!(running_stop.timeline().len(), 1);
    assert_timeline_feedback(
        &running_stop,
        &[
            "Stop already requested",
            "Waiting for the active run",
            "boundary.",
        ],
    );
    TuiProjector::default().apply(run_cancelled_event(), &mut running_stop);
    assert_eq!(running_stop.timeline().len(), 1);
    assert!(matches!(
        running_stop.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Run stopped"
    ));

    let mut interrupting = state();
    interrupting.set_run_state(InteractiveRunState::Interrupting);
    interrupting.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut interrupting),
        ControllerEffect::None
    );

    let mut running_save = state();
    running_save.set_run_state(InteractiveRunState::RunningTool);
    running_save.insert_input_str("/save");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut running_save),
        ControllerEffect::None
    );
    assert!(matches!(
        running_save.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Save unavailable"
    ));
    assert_timeline_feedback(
        &running_save,
        &["Save unavailable", "Stop the active run", "before saving"],
    );
}

#[test]
fn wrapped_command_feedback_stays_visible_at_the_bottom_of_a_full_timeline() {
    for (command, run_state, expected) in [
        (
            "/stop",
            InteractiveRunState::Interrupting,
            "cancellation boundary.",
        ),
        (
            "/save",
            InteractiveRunState::RunningTool,
            "before saving the session.",
        ),
    ] {
        let mut state = state();
        for index in 0..12 {
            state.push_timeline_item(TimelineItem::User {
                text: format!(
                    "Earlier user message {index}: {}",
                    "long history content ".repeat(14)
                ),
                lane: QueuedInputLane::Next,
            });
        }
        state.set_run_state(run_state);
        state.insert_input_str(command);
        assert_eq!(
            handle_key_action(KeyAction::SubmitNext, &mut state),
            ControllerEffect::None
        );

        for (width, height) in [(50, 20), (80, 16), (120, 24)] {
            let rendered = render::render_to_text(&state, width, height);
            assert!(
                rendered.contains(expected),
                "{expected:?} should remain visible at {width}x{height}:\n{rendered}"
            );
        }
    }
}

#[test]
fn locally_accepted_input_closes_the_idle_save_window() {
    let mut state = state();
    state.insert_input_str("start work");
    let submit = handle_key_action(KeyAction::SubmitNext, &mut state);
    assert!(matches!(submit, ControllerEffect::SubmitNext(_)));
    project_local_effect(&submit, &mut state);
    assert!(state.is_active_run());

    let mut projector = TuiProjector::default();
    projector.apply(
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput,
        },
        &mut state,
    );
    assert!(state.is_active_run());

    state.insert_input_str("/save");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::None
    );
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Save unavailable"
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
            state: InteractiveRunState::WaitingForInput,
        },
        &mut state,
    );
    assert!(!state.is_active_run());
}

#[test]
fn interrupting_queued_next_input_accepts_real_waiting_and_relabels_the_echo() {
    let mut state = state();
    state.set_run_state(InteractiveRunState::RunningModel);
    state.insert_input_str("queued next");
    let submit = handle_key_action(KeyAction::SubmitNext, &mut state);
    project_local_effect(&submit, &mut state);

    state.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::Interrupt
    );
    let mut projector = TuiProjector::default();
    projector.apply(
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput,
        },
        &mut state,
    );
    assert!(!state.is_active_run());

    projector.apply(
        RuntimeEvent::QueuedInputAccepted {
            lane: QueuedInputLane::Suspended,
            inputs: vec![QueuedInputView {
                text: "queued next".to_owned(),
                lane: QueuedInputLane::Suspended,
                position: 0,
            }],
        },
        &mut state,
    );
    assert_eq!(
        state
            .timeline()
            .iter()
            .filter(|item| matches!(
                item,
                TimelineItem::User { text, .. } if text == "queued next"
            ))
            .count(),
        1
    );
    assert!(matches!(
        state.timeline().first(),
        Some(TimelineItem::User {
            text,
            lane: QueuedInputLane::Suspended,
        }) if text == "queued next"
    ));
}

#[test]
fn unknown_commands_are_correctable_while_nested_paths_and_plain_text_still_submit() {
    let mut unknown = state();
    unknown.insert_input_str("/unknown");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut unknown),
        ControllerEffect::None
    );
    assert_eq!(unknown.input_text(), "/unknown");
    assert!(unknown.input_history_entries().is_empty());

    let mut path = state();
    path.insert_input_str("/tmp/file");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut path),
        ControllerEffect::SubmitNext(text_submission("/tmp/file"))
    );

    let mut plain = state();
    plain.insert_input_str("save");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut plain),
        ControllerEffect::SubmitNext(text_submission("save"))
    );
}

#[test]
fn command_palette_searches_the_same_slash_aliases() {
    for query in ["save", "/save"] {
        let mut state = state();
        state.open_command_palette();
        assert!(state.insert_overlay_paste(query));
        let Overlay::CommandPalette(palette) = state.overlay().expect("command palette") else {
            panic!("expected command palette");
        };
        let commands = palette.visible_commands();
        assert!(commands.iter().any(|command| {
            command.command == PaletteCommand::SaveSession && command.slash_name() == Some("save")
        }));
    }
}

#[test]
fn slash_completion_renders_all_commands_at_compact_and_standard_sizes() {
    for (width, height) in [(80, 16), (120, 24)] {
        let mut state = state();
        state.insert_input_str("/");
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render::render(frame, &state))
            .expect("render slash completion");
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for command in ["/help", "/save", "/status", "/stop"] {
            assert!(
                text.contains(command),
                "{command} should render at {width}x{height}"
            );
        }
    }
}
