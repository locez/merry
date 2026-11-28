use crate::tui::{
    controller::{handle_key_action, handle_key_event},
    keymap::{KeyAction, Keymap},
    render::{prepare_viewport, render_to_text},
    state::{TimelineItem, TuiState},
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Size;

fn populated_state() -> TuiState {
    let mut state = TuiState::new(
        "/repo".into(),
        "test".into(),
        Keymap::default(),
        TuiTheme::default(),
    );
    for index in 0..40 {
        state.push_timeline_item(TimelineItem::Muted {
            title: format!("entry-{index:02}"),
            detail: "reading position".into(),
        });
    }
    state
}

fn draw(state: &mut TuiState, size: Size) -> String {
    prepare_viewport(state, size);
    render_to_text(state, size.width, size.height)
}

fn visible_entries(rendered: &str) -> Vec<&str> {
    rendered
        .lines()
        .filter(|line| line.contains("entry-"))
        .collect()
}

#[test]
fn incoming_text_and_item_replacement_preserve_the_reading_position() {
    let mut state = populated_state();
    let size = Size::new(80, 24);
    handle_key_action(KeyAction::ScrollUp, &mut state);
    let before = draw(&mut state, size);
    assert!(!visible_entries(&before).is_empty());
    let assistant = state.append_assistant_delta(None, "new streaming answer\n");
    state.append_assistant_delta(Some(assistant), &"more output\n".repeat(30));
    state.replace_timeline_item(
        0,
        TimelineItem::Assistant {
            text: "earlier output\n".repeat(20),
        },
    );
    let after = draw(&mut state, size);
    assert_eq!(visible_entries(&before), visible_entries(&after));
    assert!(after.contains("New content"));
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    let latest = draw(&mut state, size);
    assert!(latest.contains("more output"));
    assert!(!latest.contains("New content"));
    assert!(!state.is_timeline_detached());
}

#[test]
fn scrolling_down_to_the_tail_resumes_following_new_output() {
    let mut state = populated_state();
    let size = Size::new(80, 24);
    handle_key_action(KeyAction::ScrollUp, &mut state);
    draw(&mut state, size);
    state.append_assistant_delta(None, "first update");
    draw(&mut state, size);
    state.scroll_timeline_down_by(usize::MAX);
    state.append_assistant_delta(None, "latest update");
    let rendered = draw(&mut state, size);
    assert!(rendered.contains("latest update"));
    assert!(!rendered.contains("New content"));
}

#[test]
fn review_anchor_survives_narrow_resize_and_repeated_scrolls() {
    let mut state = populated_state();
    let wide = Size::new(100, 24);
    handle_key_action(KeyAction::ScrollUp, &mut state);
    let before = draw(&mut state, wide);
    let first = visible_entries(&before)[0].trim().to_owned();
    let narrow = draw(&mut state, Size::new(45, 24));
    assert_eq!(visible_entries(&narrow)[0].trim(), first);
    handle_key_action(KeyAction::ScrollUp, &mut state);
    let earlier = draw(&mut state, wide);
    assert_ne!(visible_entries(&earlier)[0].trim(), first);
    handle_key_action(KeyAction::ScrollDown, &mut state);
    let restored = draw(&mut state, wide);
    assert_eq!(visible_entries(&restored)[0].trim(), first);
}

#[tokio::test(start_paused = true)]
async fn mouse_move_scroll_and_keys_extend_running_review_timeout_before_auto_follow() {
    use crate::tui::{
        controller::{
            handle_mouse_input, handle_mouse_scroll_down_by, handle_mouse_scroll_up_by,
            handle_paste_event,
        },
        state::TIMELINE_REVIEW_IDLE_TIMEOUT,
        text_interaction::MouseInput,
    };
    use merry::events::InteractiveRunState;
    use ratatui::layout::Position;
    use std::time::Duration;

    let mut state = populated_state();
    let size = Size::new(80, 24);
    state.set_run_state(InteractiveRunState::RunningModel);
    handle_key_action(KeyAction::ScrollUp, &mut state);
    draw(&mut state, size);
    state.append_assistant_delta(None, "latest streamed response");
    let detached = draw(&mut state, size);
    assert!(detached.contains("New content"));
    assert!(detached.contains("Enter latest"));
    assert!(state.is_timeline_detached());

    let almost_expired = TIMELINE_REVIEW_IDLE_TIMEOUT - Duration::from_secs(5);

    // Moving the mouse pointer extends the review timeout.
    tokio::time::advance(almost_expired).await;
    handle_mouse_input(MouseInput::Move(Position::new(12, 6)), size, &mut state);
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(!state.expire_idle_timeline_review());
    assert!(state.is_timeline_detached());

    // Scrolling up or down extends the review timeout.
    tokio::time::advance(almost_expired - Duration::from_secs(10)).await;
    handle_mouse_scroll_up_by(Position::new(12, 6), size, 2, &mut state);
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(!state.expire_idle_timeline_review());
    assert!(state.is_timeline_detached());

    tokio::time::advance(almost_expired - Duration::from_secs(10)).await;
    handle_mouse_scroll_down_by(Position::new(12, 6), size, 1, &mut state);
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(!state.expire_idle_timeline_review());
    assert!(state.is_timeline_detached());

    // Keyboard and paste input extend the review timeout.
    tokio::time::advance(almost_expired - Duration::from_secs(10)).await;
    handle_key_event(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        &mut state,
    );
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(!state.expire_idle_timeline_review());
    assert!(state.is_timeline_detached());

    tokio::time::advance(almost_expired - Duration::from_secs(10)).await;
    handle_paste_event("bc", &mut state);
    tokio::time::advance(almost_expired).await;
    assert!(!state.expire_idle_timeline_review());
    assert!(state.is_timeline_detached());

    // Once idle past the timeout with no activity, review exits silently to latest.
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(state.expire_idle_timeline_review());
    assert!(!state.is_timeline_detached());
    let latest = draw(&mut state, size);
    assert!(latest.contains("latest streamed response"));
    assert!(!latest.contains("New content"));
}
