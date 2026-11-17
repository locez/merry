use super::find_text_position;
use crate::tui::{
    keymap::Keymap,
    render::{render_to_buffer, render_to_text},
    state::{TimelineItem, TuiState},
    theme::TuiTheme,
};
use merry_core::QueuedInputLane;

fn state() -> TuiState {
    TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
}

fn user(text: &str) -> TimelineItem {
    TimelineItem::User {
        text: text.to_owned(),
        lane: QueuedInputLane::Next,
    }
}

fn reply(prefix: &str, count: usize) -> String {
    (0..count)
        .map(|index| format!("{prefix}-{index:02}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_latest_user_message_stays_at_the_top_while_the_reply_follows() {
    let mut state = state();
    state.push_timeline_item(user("PIN_THIS"));
    state.push_timeline_item(TimelineItem::Assistant {
        text: reply("reply-line", 80),
    });

    let buffer = render_to_buffer(&state, 40, 20);
    let text = render_to_text(&state, 40, 20);
    assert_eq!(text.matches("PIN_THIS").count(), 1, "{text}");
    assert!(text.contains("reply-line-79"), "{text}");
    let (_, user_row) = find_text_position(&buffer, "PIN_THIS").expect("pinned user");
    let (_, tail_row) = find_text_position(&buffer, "reply-line-79").expect("reply tail");
    assert!(user_row < tail_row, "user {user_row}, tail {tail_row}");

    state.scroll_timeline_up_by(usize::MAX);
    let top = render_to_text(&state, 40, 20);
    assert_eq!(top.matches("PIN_THIS").count(), 1, "{top}");
    assert!(top.contains("reply-line-00"), "{top}");
    assert!(!top.contains("reply-line-79"), "{top}");
}

#[test]
fn an_older_user_message_yields_the_pin_to_the_current_turn() {
    let mut state = state();
    state.push_timeline_item(user("FIRST_TURN"));
    state.push_timeline_item(TimelineItem::Assistant {
        text: reply("early", 6),
    });
    state.push_timeline_item(user("SECOND_TURN"));
    state.push_timeline_item(TimelineItem::Assistant {
        text: reply("later", 80),
    });

    let text = render_to_text(&state, 40, 20);
    assert_eq!(text.matches("SECOND_TURN").count(), 1, "{text}");
    assert_eq!(text.matches("FIRST_TURN").count(), 0, "{text}");
    assert!(text.contains("later-79"), "{text}");
}

#[test]
fn a_user_message_that_still_fits_is_drawn_once() {
    let mut state = state();
    state.push_timeline_item(user("ONCE"));
    state.push_timeline_item(TimelineItem::Assistant {
        text: "short reply".to_owned(),
    });

    let text = render_to_text(&state, 40, 20);
    assert_eq!(text.matches("ONCE").count(), 1, "{text}");
    assert!(text.contains("short reply"), "{text}");
}
