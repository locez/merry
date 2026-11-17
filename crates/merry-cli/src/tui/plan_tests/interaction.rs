use crate::tui::{
    controller::{ControllerEffect, handle_key_event, handle_mouse_input},
    plan_tests::{add_live_attempt, key, key_alt, node, node_id, snapshot, tui_state},
    render::{prepare_viewport, render_to_buffer, render_to_text},
    state::{TimelineItem, TuiState},
    text_interaction::MouseInput,
};
use crossterm::event::KeyCode;
use merry_core::PlanNodeStatus;
use ratatui::{buffer::Buffer, layout::Position, layout::Size};

#[test]
fn wide_tui_renders_timeline_and_recursive_plan_without_overlap() {
    let mut state = tui_state();
    state.push_timeline_item(TimelineItem::Assistant {
        text: "TIMELINE_SENTINEL".to_owned(),
    });
    let mut plan = snapshot(2, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    plan.nodes.push(node(
        "ready",
        Some("root"),
        2,
        PlanNodeStatus::Pending,
        Vec::new(),
    ));
    plan.nodes.push(node(
        "blocked",
        Some("root"),
        3,
        PlanNodeStatus::Blocked,
        Vec::new(),
    ));
    state.plan_mut().update_snapshot(plan);

    let rendered = render_to_text(&state, 140, 40);

    assert!(rendered.contains("TIMELINE_SENTINEL"));
    assert!(rendered.contains("Objective active-parent"));
    assert!(rendered.contains("Objective active-leaf"));
    assert!(rendered.contains("live 1"));
    assert!(rendered.contains("ready 1"));
    assert!(rendered.contains("blocked 1"));
}

#[test]
fn narrow_tui_renders_full_screen_plan_overlay_when_focused() {
    let mut state = tui_state();
    state.push_timeline_item(TimelineItem::Assistant {
        text: "TIMELINE_SENTINEL".to_owned(),
    });
    state
        .plan_mut()
        .update_snapshot(snapshot(2, PlanNodeStatus::InProgress));
    state.plan_mut().open_and_focus();

    let rendered = render_to_text(&state, 50, 20);

    assert!(!rendered.contains("TIMELINE_SENTINEL"));
    assert!(rendered.contains("Objective active-leaf"));
    assert!(rendered.contains("Ready"));
    assert_eq!(rendered.lines().count(), 20);
}

#[test]
fn standard_tui_renders_timeline_and_plan_side_by_side() {
    let mut state = tui_state();
    state.push_timeline_item(TimelineItem::Assistant {
        text: "TIMELINE_SENTINEL".to_owned(),
    });
    state
        .plan_mut()
        .update_snapshot(snapshot(2, PlanNodeStatus::InProgress));

    let rendered = render_to_text(&state, 80, 24);

    assert!(rendered.contains("TIMELINE_SENTINEL"));
    assert!(rendered.contains("Objective active-leaf"));
    assert_eq!(rendered.lines().count(), 24);
}

#[test]
fn plan_node_inspector_renders_bounded_content() {
    let mut state = tui_state();
    state
        .plan_mut()
        .update_snapshot(snapshot(2, PlanNodeStatus::InProgress));
    state.plan_mut().open_and_focus();
    state.plan_mut().select_node(node_id("active-leaf"));
    state.plan_mut().open_inspector();

    let rendered = render_to_text(&state, 80, 24);

    assert!(rendered.contains("OBJECTIVE"));
    assert!(rendered.contains("ACCEPTANCE"));
    assert!(rendered.contains("Objective active-leaf"));
    assert_eq!(rendered.lines().count(), 24);
}

#[test]
fn focused_plan_navigation_does_not_edit_chat_input() {
    let mut state = tui_state();
    state
        .plan_mut()
        .update_snapshot(snapshot(2, PlanNodeStatus::InProgress));
    state.plan_mut().open_and_focus();

    handle_key_event(key(KeyCode::Down), &mut state);
    assert_eq!(
        state.plan().selected_node_id(),
        Some(&node_id("active-parent"))
    );
    handle_key_event(key(KeyCode::Right), &mut state);
    assert_eq!(
        state.plan().selected_node_id(),
        Some(&node_id("active-leaf"))
    );
    handle_key_event(key(KeyCode::Enter), &mut state);
    assert!(state.plan().is_inspector_open());
    handle_key_event(key(KeyCode::Down), &mut state);
    assert_eq!(state.plan().inspector_scroll_offset(), 1);
    handle_key_event(key(KeyCode::Char('x')), &mut state);
    assert!(state.input_text().is_empty());

    handle_key_event(key(KeyCode::Esc), &mut state);
    assert!(!state.plan().is_inspector_open());
    assert!(state.plan().is_focused());
    handle_key_event(key(KeyCode::Esc), &mut state);
    assert!(!state.plan().is_focused());
    assert!(state.plan().is_open());
}

#[test]
fn alt_p_toggles_plan_visibility_and_focus() {
    let mut state = tui_state();
    state
        .plan_mut()
        .update_snapshot(snapshot(1, PlanNodeStatus::InProgress));
    assert!(state.plan().is_open());
    assert!(!state.plan().is_focused());

    handle_key_event(key_alt('p'), &mut state);
    assert!(!state.plan().is_open());

    handle_key_event(key_alt('p'), &mut state);
    assert!(state.plan().is_open());
    assert!(state.plan().is_focused());

    handle_key_event(key_alt('p'), &mut state);
    assert!(!state.plan().is_open());
}

#[test]
fn plan_header_shows_toggle_shortcut() {
    let mut state = tui_state();
    state
        .plan_mut()
        .update_snapshot(snapshot(1, PlanNodeStatus::InProgress));

    assert!(render_to_text(&state, 140, 40).contains(&crate::tui::keymap::alt_chord("P")));
}

#[test]
fn plan_rows_fold_and_status_chip_are_clickable() {
    let mut state = tui_state();
    state
        .plan_mut()
        .update_snapshot(snapshot(2, PlanNodeStatus::InProgress));
    let size = Size::new(140, 40);
    let buffer = draw(&mut state, size);

    assert_eq!(
        click(&mut state, size, point(&buffer, "Objective active-leaf")),
        ControllerEffect::None
    );
    assert_eq!(
        state.plan().selected_node_id(),
        Some(&node_id("active-leaf"))
    );
    assert!(state.plan().is_focused());
    assert!(!state.plan().is_inspector_open());

    let buffer = draw(&mut state, size);
    click(&mut state, size, point(&buffer, "Objective active-leaf"));
    assert!(state.plan().is_inspector_open());

    state.plan_mut().close_inspector();
    let buffer = draw(&mut state, size);
    click(&mut state, size, point(&buffer, "▾"));
    assert!(state.plan().is_collapsed(&node_id("root")));
    assert!(!render_to_text(&state, size.width, size.height).contains("Objective active-leaf"));

    let buffer = draw(&mut state, size);
    click(&mut state, size, point(&buffer, "[plan]"));
    assert!(!state.plan().is_open());
}

#[test]
fn clicking_the_composer_returns_focus_from_the_plan() {
    let mut state = tui_state();
    state
        .plan_mut()
        .update_snapshot(snapshot(2, PlanNodeStatus::Pending));
    state.plan_mut().open_and_focus();
    let size = Size::new(140, 40);

    assert_eq!(
        click(&mut state, size, Position::new(4, size.height - 2)),
        ControllerEffect::None
    );
    assert!(!state.plan().is_focused());
    assert!(state.plan().is_open());
}

fn draw(state: &mut TuiState, size: Size) -> Buffer {
    prepare_viewport(state, size);
    render_to_buffer(state, size.width, size.height)
}

fn point(buffer: &Buffer, text: &str) -> Position {
    let area = buffer.area;
    for y in area.y..area.bottom() {
        let mut row = String::new();
        for x in area.x..area.right() {
            row.push_str(buffer[(x, y)].symbol());
        }
        if let Some(byte_index) = row.find(text) {
            let x = row[..byte_index].chars().count();
            return Position::new(u16::try_from(x).unwrap(), y);
        }
    }
    panic!("missing {text}:\n{}", buffer_text(buffer));
}

fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut text = String::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn click(state: &mut TuiState, size: Size, position: Position) -> ControllerEffect {
    let effect = handle_mouse_input(MouseInput::Down(position), size, state);
    assert_eq!(
        handle_mouse_input(MouseInput::Up(position), size, state),
        ControllerEffect::None
    );
    effect
}
