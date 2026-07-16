use super::{
    command::PaletteCommand,
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
use merry_core::{InteractiveRunState, QueuedInputLane, QueuedInputView, RuntimeEvent};
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
        Some(TimelineItem::Expanded { title, body })
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
        Some(TimelineItem::Expanded { title, body })
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
fn stop_and_save_respect_the_current_run_state() {
    let mut idle_stop = state();
    idle_stop.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut idle_stop),
        ControllerEffect::None
    );
    assert!(matches!(
        idle_stop.timeline().last(),
        Some(TimelineItem::Muted { title, .. }) if title == "Nothing to stop"
    ));

    let mut running_stop = state();
    running_stop.set_run_state(InteractiveRunState::RunningModel);
    running_stop.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut running_stop),
        ControllerEffect::Interrupt
    );
    running_stop.insert_input_str("/stop");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut running_stop),
        ControllerEffect::None
    );
    assert!(matches!(
        running_stop.timeline().last(),
        Some(TimelineItem::Muted { title, .. }) if title == "Stop already requested"
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
        Some(TimelineItem::Muted { title, .. }) if title == "Save unavailable"
    ));
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
        Some(TimelineItem::Muted { title, .. }) if title == "Save unavailable"
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
