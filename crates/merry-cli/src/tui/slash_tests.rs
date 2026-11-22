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
    ErrorInfo, InteractiveRunState, QueuedInputView, RuntimeEvent, RuntimeEventSource, SessionId,
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

fn sample_plan_document() -> merry_core::PlanDocument {
    merry_core::PlanDocument {
        summary: merry_core::PlanDocumentSummary {
            id: merry_core::PlanId::new("plan-1").expect("plan id"),
            title: "Slash test plan".to_owned(),
            file_name: "001-slash-test-plan.md".to_owned(),
            status: merry_core::PlanDocumentStatus::AwaitingApproval,
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 2,
        },
        content: "# Slash test plan\n\n- Step 1\n".to_owned(),
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
    let values = all
        .items()
        .iter()
        .map(|item| item.value())
        .collect::<Vec<_>>();
    assert!(
        all.items()
            .iter()
            .all(|item| item.kind() == &CompletionKind::Slash)
    );
    for expected in [
        "/code-review",
        "/compact",
        "/exit",
        "/help",
        "/model",
        "/new",
        "/plan",
        "/plans",
        "/resume",
        "/save",
        "/security-review",
        "/settings",
        "/shortcuts",
        "/skills",
        "/status",
        "/stop",
        "/tasks",
    ] {
        assert!(
            values.contains(&expected),
            "{expected} missing from slash completions: {values:?}"
        );
    }

    let save = sources
        .menu_for_input("/sa", 3, None)
        .expect("save completion");
    assert_eq!(save.items()[0].value(), "/save");
    assert!(save.items()[0].detail().is_some());
    assert!(sources.menu_for_input("hello", 5, None).is_none());
    assert!(sources.menu_for_input("hello /sa", 9, None).is_none());
    assert!(sources.menu_for_input("/xyz", 4, None).is_none());

    // Alias and substring queries surface matching canonical slash commands.
    let clear = sources
        .menu_for_input("/clear", 6, None)
        .expect("clear alias completion");
    assert_eq!(clear.items()[0].value(), "/new");
    assert_eq!(clear.selected_index(), 0);

    let review = sources
        .menu_for_input("/rev", 4, None)
        .expect("review substring completion");
    assert_eq!(
        review
            .items()
            .iter()
            .map(|item| item.value())
            .collect::<Vec<_>>(),
        vec!["/code-review", "/security-review"]
    );

    let middle = sources
        .menu_for_input("/save", 3, None)
        .expect("mid-token slash completion");
    assert_eq!(middle.replacement_range(), 0..5);
    assert_eq!(middle.replacement_text(), Some("/save".to_owned()));

    let compact = sources
        .menu_for_input("/comp", 5, None)
        .expect("compact completion");
    assert_eq!(compact.replacement_text(), Some("/compact ".to_owned()));
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
fn local_slash_commands_do_not_enter_user_history_or_runtime_queue() {
    let mut help = state();
    help.insert_input_str("/help");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut help),
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
        handle_key_action(KeyAction::Submit, &mut status),
        ControllerEffect::None
    );
    assert!(matches!(
        status.timeline().last(),
        Some(TimelineItem::LocalCommand { title, body })
            if title == "Session status"
                && body.contains("Run: ready")
                && body.contains("Plan: inactive")
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
            handle_key_action(KeyAction::Submit, &mut help),
            ControllerEffect::None
        );
        let help_text = render::render_to_text(&help, width, height);
        for expected in [
            "Command help",
            "/new",
            "/clear",
            "/compact",
            "/code-review",
            "/security-review",
            "/model",
            "/resume",
            "/settings",
            "/help",
            "/save",
            "/status",
            "/stop",
            "Submit",
            "Commands",
            "Stop",
        ] {
            assert!(
                help_text.contains(expected),
                "{expected:?} should render at {width}x{height}:\n{help_text}"
            );
        }

        let mut status = state();
        status.insert_input_str("/status");
        assert_eq!(
            handle_key_action(KeyAction::Submit, &mut status),
            ControllerEffect::None
        );
        let status_text = render::render_to_text(&status, width, height);
        for expected in [
            "Session status",
            "Run: ready",
            "Model: gpt-test",
            "Usage:",
            "Plan: inactive",
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
        state.plan_mut().set_mode(merry_core::PlanModeState::Active);
        state.plan_mut().upsert_plan(sample_plan_document());
        state.set_run_state(run_state);

        assert_eq!(
            run_palette_command(PaletteCommand::ShowStatus, &mut state),
            ControllerEffect::None
        );
        assert!(matches!(
            state.timeline().last(),
            Some(TimelineItem::LocalCommand { body, .. })
                if body.contains(expected) && body.contains("Plan: active (read-only)")
        ));
    }
}

#[test]
fn palette_slash_commands_reveal_feedback_from_plan_overlay() {
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
                "⊘ Stopping…",
            ),
            (
                PaletteCommand::SaveSession,
                InteractiveRunState::RunningTool,
                ControllerEffect::None,
                "Stop the active run",
            ),
        ] {
            let mut state = state();
            state.plan_mut().upsert_plan(sample_plan_document());
            state.open_plan_view(None);
            state.set_run_state(run_state);

            assert!(state.overlay().is_some());
            assert_eq!(run_palette_command(command, &mut state), effect);
            assert!(state.overlay().is_none());

            let rendered = render::render_to_text(&state, width, height);
            assert!(
                rendered.contains(expected),
                "{expected:?} should render for {command:?} at {width}x{height}:\n{rendered}"
            );
        }
    }
}

#[test]
fn invalid_slash_feedback_is_visible_and_remains_correctable() {
    for (input, expected) in [
        ("/unknown", "Unknown command"),
        ("/help now", "Command not run"),
    ] {
        let mut state = state();
        state.plan_mut().set_mode(merry_core::PlanModeState::Active);
        state.insert_input_str(input);

        assert_eq!(
            handle_key_action(KeyAction::Submit, &mut state),
            ControllerEffect::None
        );
        assert_eq!(state.input_text(), input);
        assert!(render::render_to_text(&state, 80, 16).contains(expected));
    }
}

#[test]
fn help_remains_complete_at_80_by_16() {
    let mut state = state();

    assert_eq!(
        run_palette_command(PaletteCommand::ShowHelp, &mut state),
        ControllerEffect::None
    );

    let rendered = render::render_to_text(&state, 80, 16);
    for expected in [
        "Command help",
        "/new",
        "/clear",
        "/resume",
        "/compact",
        "/code-review",
        "/security-review",
        "/model",
        "/settings",
        "/help",
        "/plan",
        "/plans",
        "/save",
        "/status",
        "/stop",
        "/tasks",
        "Submit",
        "Commands",
        "Stop",
    ] {
        assert!(
            rendered.contains(expected),
            "{expected:?} should render at 80x16:\n{rendered}"
        );
    }
}

#[test]
fn local_slash_input_bypasses_timeline_review_confirmation() {
    let mut timeline_review = state();
    timeline_review.push_timeline_item(TimelineItem::User {
        text: "earlier request".to_owned(),
    });
    timeline_review.jump_to_previous_user_input();
    timeline_review.insert_input_str("/help");

    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut timeline_review),
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
        handle_key_action(KeyAction::Submit, &mut invalid_input),
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
        handle_key_action(KeyAction::Submit, &mut provider_input),
        ControllerEffect::Submit(text_submission("/tmp/file"))
    );
    assert_eq!(provider_input.input_text(), "");
}

#[test]
fn keyboard_stop_reveals_the_same_feedback_as_slash_stop() {
    let mut state = state();
    state.set_run_state(InteractiveRunState::RunningModel);

    let effect = handle_key_action(KeyAction::Interrupt, &mut state);
    assert_eq!(effect, ControllerEffect::Interrupt);
    project_local_effect(&effect, &mut state);

    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::Muted { title, .. }) if title == "⊘ Stopping…"
    ));

    assert_eq!(
        handle_key_action(KeyAction::Interrupt, &mut state),
        ControllerEffect::None
    );
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::Muted { title, detail })
            if title == "⊘ Stopping…" && detail == "waiting for current step"
    ));
}

#[test]
fn stop_and_save_respect_the_current_run_state() {
    let mut idle_stop = state();
    idle_stop.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut idle_stop),
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
        handle_key_action(KeyAction::Submit, &mut running_stop),
        ControllerEffect::Interrupt
    );
    assert_timeline_feedback(&running_stop, &["⊘ Stopping…"]);

    let mut cancelled_stop = state();
    cancelled_stop.set_run_state(InteractiveRunState::RunningModel);
    cancelled_stop.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut cancelled_stop),
        ControllerEffect::Interrupt
    );
    TuiProjector::default().apply(run_cancelled_event(), &mut cancelled_stop);
    assert!(matches!(
        cancelled_stop.timeline().last(),
        Some(TimelineItem::Muted { title, .. }) if title == "⊘ Stopped"
    ));
    assert!(
        cancelled_stop
            .timeline()
            .iter()
            .all(|item| !matches!(item, TimelineItem::Diagnostic { .. }))
    );
    assert_timeline_feedback(&cancelled_stop, &["⊘ Stopped"]);

    let mut unexpected_cancel = state();
    TuiProjector::default().apply(run_cancelled_event(), &mut unexpected_cancel);
    assert!(matches!(
        unexpected_cancel.timeline().last(),
        Some(TimelineItem::Diagnostic { title, body })
            if title == "cancelled" && body == "runtime step cancelled"
    ));

    running_stop.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut running_stop),
        ControllerEffect::None
    );
    assert!(matches!(
        running_stop.timeline().last(),
        Some(TimelineItem::Muted { title, detail })
            if title == "⊘ Stopping…" && detail == "waiting for current step"
    ));
    assert_eq!(running_stop.timeline().len(), 1);
    assert_timeline_feedback(&running_stop, &["⊘ Stopping…", "waiting for current step"]);
    TuiProjector::default().apply(run_cancelled_event(), &mut running_stop);
    assert_eq!(running_stop.timeline().len(), 1);
    assert!(matches!(
        running_stop.timeline().last(),
        Some(TimelineItem::Muted { title, .. }) if title == "⊘ Stopped"
    ));

    let mut interrupting = state();
    interrupting.set_run_state(InteractiveRunState::Interrupting);
    interrupting.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut interrupting),
        ControllerEffect::None
    );

    let mut running_save = state();
    running_save.set_run_state(InteractiveRunState::RunningTool);
    running_save.insert_input_str("/save");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut running_save),
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
            "waiting for current step",
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
            });
        }
        state.set_run_state(run_state);
        state.insert_input_str(command);
        assert_eq!(
            handle_key_action(KeyAction::Submit, &mut state),
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
    let submit = handle_key_action(KeyAction::Submit, &mut state);
    assert!(matches!(submit, ControllerEffect::Submit(_)));
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
        handle_key_action(KeyAction::Submit, &mut state),
        ControllerEffect::None
    );
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Save unavailable"
    ));

    projector.apply(
        RuntimeEvent::QueuedInputAccepted {
            inputs: vec![QueuedInputView {
                id: 1,
                text: "start work".to_owned(),
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
fn submitting_while_running_defers_timeline_item_until_runtime_accepts_it() {
    let mut state = state();
    state.set_run_state(InteractiveRunState::RunningModel);
    state.insert_input_str("queued next");
    let submit = handle_key_action(KeyAction::Submit, &mut state);
    project_local_effect(&submit, &mut state);
    assert!(
        state.timeline().is_empty(),
        "running submit waits in pending queue instead of pushing a stale timeline echo"
    );

    state.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut state),
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
            inputs: vec![QueuedInputView {
                id: 1,
                text: "queued next".to_owned(),
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
                TimelineItem::User { text } if text == "queued next"
            ))
            .count(),
        1
    );
}

#[test]
fn unknown_commands_are_correctable_while_nested_paths_and_plain_text_still_submit() {
    let mut unknown = state();
    unknown.insert_input_str("/unknown");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut unknown),
        ControllerEffect::None
    );
    assert_eq!(unknown.input_text(), "/unknown");
    assert!(unknown.input_history_entries().is_empty());

    let mut path = state();
    path.insert_input_str("/tmp/file");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut path),
        ControllerEffect::Submit(text_submission("/tmp/file"))
    );

    let mut plain = state();
    plain.insert_input_str("save");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut plain),
        ControllerEffect::Submit(text_submission("save"))
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
        let total_items = state
            .completion_menu()
            .expect("slash completion menu")
            .items()
            .len();
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
        for command in [
            "/code-review",
            "/compact",
            "/exit",
            "/help",
            "/model",
            "/new",
        ] {
            assert!(
                text.contains(command),
                "{command} should render at {width}x{height}"
            );
        }
        // Navigating to the end reveals the remaining slash commands in the bounded popup.
        for _ in 0..total_items.saturating_sub(1) {
            state.select_next_completion();
        }
        terminal
            .draw(|frame| render::render(frame, &state))
            .expect("render scrolled slash completion");
        let scrolled = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for command in ["/stop", "/tasks"] {
            assert!(
                scrolled.contains(command),
                "{command} should render after scrolling at {width}x{height}"
            );
        }
    }
}

#[test]
fn new_resume_and_compact_slash_commands_respect_run_state_and_aliases() {
    for input in ["/new", "/clear"] {
        let mut idle = state();
        idle.insert_input_str(input);
        assert_eq!(
            handle_key_action(KeyAction::Submit, &mut idle),
            ControllerEffect::NewSession
        );
        assert_eq!(idle.input_text(), "");

        let mut running = state();
        running.set_run_state(InteractiveRunState::RunningModel);
        running.insert_input_str(input);
        assert_eq!(
            handle_key_action(KeyAction::Submit, &mut running),
            ControllerEffect::None
        );
        assert!(matches!(
            running.timeline().last(),
            Some(TimelineItem::LocalCommand { title, .. }) if title == "New session unavailable"
        ));
    }

    let mut resume_idle = state();
    resume_idle.insert_input_str("/resume");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut resume_idle),
        ControllerEffect::ResumeSession
    );

    let mut resume_running = state();
    resume_running.set_run_state(InteractiveRunState::RunningTool);
    resume_running.insert_input_str("/resume");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut resume_running),
        ControllerEffect::None
    );
    assert!(matches!(
        resume_running.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Resume unavailable"
    ));

    let mut compact_idle = state();
    compact_idle.insert_input_str("/compact keep auth invariants");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut compact_idle),
        ControllerEffect::CompactContext {
            focus: Some("keep auth invariants".to_owned()),
        }
    );

    let mut compact_running = state();
    compact_running.set_run_state(InteractiveRunState::RunningModel);
    compact_running.insert_input_str("/compact");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut compact_running),
        ControllerEffect::None
    );
    assert!(matches!(
        compact_running.timeline().last(),
        Some(TimelineItem::LocalCommand { title, .. }) if title == "Compaction unavailable"
    ));
}

#[test]
fn review_workflow_slash_commands_submit_structured_prompts_with_compact_history() {
    for (input, expected_scope, expected_phrase) in [
        ("/code-review", "git diff", "CONFIRMED"),
        ("/review main..HEAD", "main..HEAD", "role: \"review\""),
        ("/security-review", "git diff", "Hard Exclusions"),
        (
            "/sec-review crates/merry-tools/src/ssrf.rs",
            "crates/merry-tools/src/ssrf.rs",
            "security review",
        ),
    ] {
        let mut s = state();
        s.insert_input_str(input);
        let effect = handle_key_action(KeyAction::Submit, &mut s);
        let ControllerEffect::Submit(submission) = effect else {
            panic!("expected workflow submit for {input}, got {effect:?}");
        };
        assert_eq!(submission.history_text, input);
        assert!(
            submission.text.contains(expected_scope),
            "{input} prompt missing scope {expected_scope:?}"
        );
        assert!(
            submission.text.contains(expected_phrase),
            "{input} prompt missing phrase {expected_phrase:?}"
        );
        assert_eq!(s.input_text(), "");
    }
}

#[test]
fn quick_ui_and_quit_slash_commands_route_to_existing_overlays_and_effects() {
    for input in ["/model", "/provider", "/providers"] {
        let mut s = state();
        s.insert_input_str(input);
        assert_eq!(
            handle_key_action(KeyAction::Submit, &mut s),
            ControllerEffect::OpenProviderManager
        );
    }

    for input in ["/settings", "/config"] {
        let mut s = state();
        s.insert_input_str(input);
        assert_eq!(
            handle_key_action(KeyAction::Submit, &mut s),
            ControllerEffect::None
        );
        assert!(matches!(s.overlay(), Some(Overlay::Settings(_))));
    }

    for input in ["/shortcuts", "/keys"] {
        let mut s = state();
        s.insert_input_str(input);
        assert_eq!(
            handle_key_action(KeyAction::Submit, &mut s),
            ControllerEffect::None
        );
        assert!(matches!(s.overlay(), Some(Overlay::Shortcuts(_))));
    }

    for input in ["/exit", "/quit"] {
        let mut s = state();
        s.insert_input_str(input);
        assert_eq!(
            handle_key_action(KeyAction::Submit, &mut s),
            ControllerEffect::Quit
        );
    }
}

#[test]
fn tab_on_argument_slash_command_appends_space_while_enter_executes_immediately() {
    let mut tab_state = state();
    tab_state.insert_input_str("/comp");
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut tab_state,
        ),
        ControllerEffect::None
    );
    assert_eq!(tab_state.input_text(), "/compact ");

    let mut enter_state = state();
    enter_state.insert_input_str("/comp");
    let enter_state_effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut enter_state,
    );
    assert_eq!(
        enter_state_effect,
        ControllerEffect::CompactContext { focus: None }
    );
}

#[test]
fn slash_fuzzy_acronym_completion_matches_review_commands() {
    let sources = CompletionSources::from_skill_names("/repo".into(), &[]);
    let cr = sources
        .menu_for_input("/cr", 3, None)
        .expect("fuzzy acronym match for /code-review");
    assert_eq!(cr.items()[0].value(), "/code-review");
    assert!(!cr.items()[0].match_positions().is_empty());

    let sr = sources
        .menu_for_input("/sr", 3, None)
        .expect("fuzzy acronym match for /security-review");
    assert_eq!(sr.items()[0].value(), "/security-review");
}

#[test]
fn skills_slash_command_opens_fuzzy_picker_and_inserts_skill_token() {
    let mut empty = state();
    empty.insert_input_str("/skills");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut empty),
        ControllerEffect::None
    );
    assert!(empty.overlay().is_none());
    assert!(matches!(
        empty.timeline().last(),
        Some(TimelineItem::LocalCommand { title, body })
            if title == "Skills" && body.contains("No skills loaded")
    ));

    let skills = vec![
        merry_runtime::SkillMetadata::new(
            "brainstorming",
            "Explore intent and architecture collaboratively.",
            std::path::PathBuf::from("skills/brainstorming/SKILL.md"),
            std::path::PathBuf::from("/skills"),
        )
        .expect("valid skill"),
        merry_runtime::SkillMetadata::new(
            "frontend-design",
            "Create distinctive production UI.",
            std::path::PathBuf::from("skills/frontend-design/SKILL.md"),
            std::path::PathBuf::from("/skills"),
        )
        .expect("valid skill"),
    ];
    let mut populated = state();
    populated.set_completion_skills(skills);
    populated.insert_input_str("/skill");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut populated),
        ControllerEffect::None
    );
    assert!(matches!(populated.overlay(), Some(Overlay::Skills(_))));

    let rendered = render::render_to_text(&populated, 100, 24);
    assert!(rendered.contains("Skills"));
    assert!(rendered.contains("$brainstorming"));
    assert!(rendered.contains("$frontend-design"));

    // Filter live by acronym `fd` and insert `$frontend-design ` into composer.
    for ch in ['f', 'd'] {
        assert_eq!(
            handle_key_event(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                &mut populated
            ),
            ControllerEffect::None
        );
    }
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut populated
        ),
        ControllerEffect::None
    );
    assert!(populated.overlay().is_none());
    assert_eq!(populated.input_text(), "$frontend-design ");
    assert!(populated.completion_menu().is_none());
}

#[test]
fn slash_completion_aligns_description_column_across_commands() {
    let mut s = state();
    s.insert_input_str("/");
    let rendered = render::render_to_text(&s, 110, 20);
    let col_of = |needle: &str| {
        rendered
            .lines()
            .find_map(|line| line.find(needle))
            .unwrap_or_else(|| panic!("{needle:?} not found in rendered completion:\n{rendered}"))
    };
    let review_col = col_of("Review recent changes");
    let compact_col = col_of("Compact conversation history");
    let quit_col = col_of("Quit Merry");
    let help_col = col_of("List slash commands");
    let model_col = col_of("Switch provider or model");
    let new_col = col_of("Start a fresh session");

    assert_eq!(compact_col, review_col);
    assert_eq!(quit_col, review_col);
    assert_eq!(help_col, review_col);
    assert_eq!(model_col, review_col);
    assert_eq!(new_col, review_col);
}
