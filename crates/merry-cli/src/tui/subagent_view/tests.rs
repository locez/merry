//! Unit tests for subagent timeline cards and fullscreen takeover drill-in.

use crate::tui::{
    controller::{
        ControllerEffect, handle_key_event, handle_mouse_input, handle_mouse_scroll_down,
    },
    keymap::Keymap,
    overlay::Overlay,
    projector::TuiProjector,
    render::{render_to_buffer, render_to_text},
    state::TuiState,
    text_interaction::MouseInput,
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{
    RuntimeEvent, RuntimeEventSource, SessionId, SubagentActivityPhase, SubagentActivitySnapshot,
    SubagentId, SubagentTaskId,
};
use ratatui::layout::{Position, Size};

fn source() -> RuntimeEventSource {
    RuntimeEventSource::new(SessionId::new("test-session").expect("session id"), 1)
}

fn rendered_buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in buffer.area.y..buffer.area.bottom() {
        for x in buffer.area.x..buffer.area.right() {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn find_text_position(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
    for y in buffer.area.y..buffer.area.bottom() {
        let mut row = String::new();
        let mut cols = Vec::new();
        for x in buffer.area.x..buffer.area.right() {
            let sym = buffer[(x, y)].symbol();
            let start = row.len();
            row.push_str(sym);
            cols.push((start, x));
        }
        if let Some(byte_idx) = row.find(needle)
            && let Some((_, col)) = cols.into_iter().find(|(start, _)| *start == byte_idx)
        {
            return Some((col, y));
        }
    }
    None
}

#[test]
fn subagent_card_and_status_chip_open_fullscreen_takeover_and_switch_agents() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();
    let agent_a = SubagentId::new("agent-alpha").expect("agent id");
    let task_a = SubagentTaskId::new("task-1").expect("task id");
    let agent_b = SubagentId::new("agent-beta").expect("agent id");
    let task_b = SubagentTaskId::new("task-2").expect("task id");

    projector.apply(
        RuntimeEvent::SubagentStarted {
            agent_id: agent_a.clone(),
            task_id: task_a.clone(),
            source: source(),
        },
        &mut state,
    );
    state.update_subagent_activity(&[SubagentActivitySnapshot {
        subagent_id: agent_a.clone(),
        task_id: task_a.clone(),
        phase: SubagentActivityPhase::Running,
        summary: "Inspecting layout engine".to_owned(),
        updated_at_ms: 100,
    }]);
    projector.apply(
        RuntimeEvent::SubagentCompleted {
            agent_id: agent_b.clone(),
            task_id: task_b.clone(),
            summary: "Audit finished".to_owned(),
            output_paths: vec!["report.md".to_owned()],
            changed_paths: vec!["src/lib.rs".to_owned()],
            usage: None,
            source: source(),
        },
        &mut state,
    );

    let size = Size::new(100, 26);
    let buffer = render_to_buffer(&state, size.width, size.height);
    let text = rendered_buffer_text(&buffer);
    assert!(text.contains("agent-alpha"), "{text}");
    assert!(text.contains("[Inspect ▸]"), "{text}");
    assert!(text.contains("[2 agents]"), "{text}");

    let (ax, ay) = find_text_position(&buffer, "Subagent agent-alpha").expect("subagent card");
    assert_eq!(
        handle_mouse_input(MouseInput::Down(Position::new(ax, ay)), size, &mut state),
        ControllerEffect::RefreshSubagents
    );
    assert!(matches!(state.overlay(), Some(Overlay::SubagentInspect(_))));

    let takeover = render_to_buffer(&state, size.width, size.height);
    let takeover_text = rendered_buffer_text(&takeover);
    assert!(
        takeover_text.contains("Subagent · agent-alpha"),
        "{takeover_text}"
    );
    assert!(
        takeover_text.contains("Inspecting layout engine"),
        "{takeover_text}"
    );
    assert!(takeover_text.contains("AGENTS"), "{takeover_text}");
    assert!(
        takeover_text.contains("Guide agent-alpha"),
        "{takeover_text}"
    );
    assert!(takeover_text.contains("[Stop agent]"), "{takeover_text}");

    let (stop_x, stop_y) = find_text_position(&takeover, "[Stop agent]").expect("stop button");
    assert_eq!(
        handle_mouse_input(
            MouseInput::Down(Position::new(stop_x + 1, stop_y)),
            size,
            &mut state
        ),
        ControllerEffect::CancelSubagent("agent-alpha".to_owned())
    );

    let (bx, by) = find_text_position(&takeover, "agent-beta").expect("agent-beta in rail");
    assert_eq!(
        handle_mouse_input(MouseInput::Down(Position::new(bx, by)), size, &mut state),
        ControllerEffect::RefreshSubagents
    );
    let beta_text = render_to_text(&state, size.width, size.height);
    assert!(beta_text.contains("Subagent · agent-beta"), "{beta_text}");
    assert!(beta_text.contains("src/lib.rs"), "{beta_text}");
    assert!(beta_text.contains("report.md"), "{beta_text}");
    assert!(
        beta_text.contains("Follow up with agent-beta"),
        "{beta_text}"
    );

    // Type a follow-up message directly inside the takeover composer and submit.
    for ch in "check tests too".chars() {
        assert_eq!(
            handle_key_event(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                &mut state
            ),
            ControllerEffect::None
        );
    }
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::SendSubagentMessage {
            agent_id: "agent-beta".to_owned(),
            message: "check tests too".to_owned(),
        }
    );
    let after_send = render_to_text(&state, size.width, size.height);
    assert!(after_send.contains("check tests too"), "{after_send}");

    assert_eq!(
        handle_mouse_scroll_down(Position::new(40, 10), size, &mut state),
        ControllerEffect::None
    );

    assert_eq!(
        handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state),
        ControllerEffect::None
    );
    assert!(state.overlay().is_none());

    let parent = render_to_buffer(&state, size.width, size.height);
    let (chip_x, chip_y) = find_text_position(&parent, "[2 agents]").expect("agents chip");
    assert_eq!(
        handle_mouse_input(
            MouseInput::Down(Position::new(chip_x + 1, chip_y)),
            size,
            &mut state,
        ),
        ControllerEffect::RefreshSubagents
    );
    assert!(matches!(state.overlay(), Some(Overlay::SubagentInspect(_))));
}
