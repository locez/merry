use super::find_text_position;
use crate::tui::{
    keymap::Keymap,
    render::{render_to_buffer, render_to_text},
    state::{TimelineItem, TuiState},
    theme::TuiTheme,
};

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
    assert!(
        user_row >= 2,
        "pinned user band should leave breathing space below the header bar (row {user_row})"
    );
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

#[test]
fn sending_a_new_user_message_immediately_anchors_it_at_the_top_above_the_task_dock() {
    let mut state = state();
    state.push_timeline_item(user("OLD_TURN"));
    state.push_timeline_item(TimelineItem::Assistant {
        text: reply("old-line", 30),
    });
    state.push_timeline_item(user("NEW_TURN"));
    state.push_timeline_item(TimelineItem::Assistant {
        text: "fresh short reply".to_owned(),
    });
    state.sync_background_processes(vec![
        merry::process::BackgroundProcessSummary::new(
            merry::process::BackgroundProcessId::new("bg-1").expect("id"),
            "sleep 60",
            None,
            merry::process::BackgroundProcessState::Running { pid: 42 },
        )
        .with_display_name(Some("api server".to_owned())),
    ]);

    let buffer = render_to_buffer(&state, 90, 24);
    let text = render_to_text(&state, 90, 24);
    assert_eq!(text.matches("NEW_TURN").count(), 1, "{text}");
    assert_eq!(
        text.matches("OLD_TURN").count(),
        0,
        "previous user turn must not stay pinned once a new turn is sent:\n{text}"
    );
    let (_, new_user_row) = find_text_position(&buffer, "NEW_TURN").expect("new user at top");
    let (_, dock_row) = find_text_position(&buffer, "api server").expect("task dock");
    let (_, reply_row) = find_text_position(&buffer, "fresh short reply").expect("new reply");
    assert!(
        new_user_row < dock_row && dock_row < reply_row,
        "expected NEW_TURN ({new_user_row}) < dock ({dock_row}) < reply ({reply_row}):\n{text}"
    );

    // Scrolling up smoothly reveals the previous turn's reply above NEW_TURN.
    state.scroll_timeline_up_by(8);
    let scrolled = render_to_text(&state, 90, 24);
    assert!(scrolled.contains("old-line-29"), "{scrolled}");
}

#[test]
fn scrolling_a_long_lazy_timeline_to_the_top_never_duplicates_the_earliest_user_message() {
    let mut state = state();
    let earliest = "开一个子agent，让他用浏览器，访问 locez.com，报告主页是什么？";
    state.push_timeline_item(user(earliest));
    state.push_timeline_item(TimelineItem::Muted {
        title: "Tool read_text".to_owned(),
        detail: "path=/tmp/example".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Muted {
        title: "Delegated spawn_subagents".to_owned(),
        detail: "foreground_budget_secs=240".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: format!(
            "子 agent 已用浏览器完成访问并关闭会话，报告如下：\n{}",
            reply("first-turn", 24)
        ),
    });
    for turn in 1..100 {
        state.push_timeline_item(user(&format!("follow-up-turn-{turn}")));
        state.push_timeline_item(TimelineItem::Assistant {
            text: reply(&format!("turn-{turn}-line"), 18),
        });
    }

    let size = ratatui::layout::Size::new(100, 24);
    crate::tui::render::prepare_viewport(&mut state, size);
    let _ = render_to_text(&state, size.width, size.height);

    // Scroll all the way up in wheel-sized steps (plus extra presses at the top)
    // across >EVICT_KEEP_MARGIN_ITEMS cold items and verify the earliest user
    // message is never duplicated on any frame.
    // Match the ASCII marker because TestBackend inserts a trailing space cell
    // after each 2-column CJK glyph.
    let marker = "locez.com";
    for step in 0..950 {
        state.scroll_timeline_up_by(3);
        crate::tui::render::prepare_viewport(&mut state, size);
        let text = render_to_text(&state, size.width, size.height);
        assert!(
            text.matches(marker).count() <= 1,
            "earliest user prompt duplicated at scroll step {step}:\n{text}"
        );
    }

    let top = render_to_text(&state, size.width, size.height);
    assert_eq!(top.matches(marker).count(), 1, "{top}");
    assert!(top.contains("read_text"), "{top}");

    // Stepping down 1 row at a time from the very top must also never duplicate it.
    for step in 0..10 {
        state.scroll_timeline_down_by(1);
        crate::tui::render::prepare_viewport(&mut state, size);
        let text = render_to_text(&state, size.width, size.height);
        assert!(
            text.matches(marker).count() <= 1,
            "earliest user prompt duplicated when stepping down {step} from top:\n{text}"
        );
    }
}
