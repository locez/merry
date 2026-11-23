use crate::tui::{
    controller::{ControllerEffect, handle_mouse_input},
    keymap::Keymap,
    render::{render_to_buffer, render_to_text},
    state::{TimelineItem, TuiState},
    tests::{find_cell_style, find_text_position, rendered_buffer_text},
    text_interaction::MouseInput,
    theme::{SemanticColor, TuiTheme},
};
use ratatui::{
    layout::{Position, Size},
    style::Modifier,
};

#[test]
fn renderer_hides_inline_markdown_url_and_reveals_destination_on_hover_and_click() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "See [OpenAI](https://openai.com/pricing), [https://trusted.example](https://evil.example/phish), and https://bare.example/guide."
            .to_owned(),
    });

    let size = Size::new(100, 18);
    let buffer = render_to_buffer(&state, size.width, size.height);
    let text = rendered_buffer_text(&buffer);
    assert!(text.contains("OpenAI"), "{text}");
    assert!(
        !text.contains("https://openai.com/pricing"),
        "masked link destination should not clutter inline text until hovered:\n{text}"
    );
    assert!(
        !text.contains("https://evil.example/phish"),
        "masked link destination should stay in hover tooltip:\n{text}"
    );
    assert!(
        text.contains("https://bare.example/guide"),
        "bare URL should stay visible:\n{text}"
    );

    let label_style = find_cell_style(&buffer, "OpenAI").expect("link label should render");
    let bare_style =
        find_cell_style(&buffer, "https://bare.example/guide").expect("bare url should render");
    let link_color = TuiTheme::default().color(SemanticColor::Link);
    assert_eq!(label_style.fg, link_color);
    assert!(label_style.add_modifier.contains(Modifier::UNDERLINED));
    assert_eq!(bare_style.fg, link_color);
    assert!(bare_style.add_modifier.contains(Modifier::UNDERLINED));

    // Hovering a deceptive link reveals its true destination URL in a floating tooltip.
    let (phish_x, phish_y) =
        find_text_position(&buffer, "https://trusted.example").expect("masked label");
    assert_eq!(
        handle_mouse_input(
            MouseInput::Move(Position::new(phish_x + 2, phish_y)),
            size,
            &mut state
        ),
        ControllerEffect::None
    );
    let hovered_text = render_to_text(&state, size.width, size.height);
    assert!(
        hovered_text.contains("https://evil.example/phish"),
        "hover tooltip must expose the real link target:\n{hovered_text}"
    );
    assert!(
        hovered_text.contains('↗'),
        "tooltip should show external link cue:\n{hovered_text}"
    );

    // Moving the pointer off the link clears the tooltip.
    handle_mouse_input(MouseInput::Move(Position::new(0, 0)), size, &mut state);
    assert!(state.hovered_link().is_none());
    let unhovered_text = render_to_text(&state, size.width, size.height);
    assert!(!unhovered_text.contains("https://evil.example/phish"));

    // Plain left-click (no Shift) directly opens the Markdown link target.
    let (link_x, link_y) = find_text_position(&buffer, "OpenAI").expect("OpenAI link");
    let link_pos = Position::new(link_x + 1, link_y);
    assert_eq!(
        handle_mouse_input(MouseInput::Down(link_pos), size, &mut state),
        ControllerEffect::None
    );
    assert_eq!(
        handle_mouse_input(MouseInput::Up(link_pos), size, &mut state),
        ControllerEffect::OpenUrl("https://openai.com/pricing".to_owned())
    );

    // Plain left-click on a bare URL also opens it directly.
    let (bare_x, bare_y) =
        find_text_position(&buffer, "https://bare.example/guide").expect("bare link");
    let bare_pos = Position::new(bare_x + 4, bare_y);
    handle_mouse_input(MouseInput::Down(bare_pos), size, &mut state);
    assert_eq!(
        handle_mouse_input(MouseInput::Up(bare_pos), size, &mut state),
        ControllerEffect::OpenUrl("https://bare.example/guide".to_owned())
    );

    // Drag-selecting across link text copies it instead of opening the browser.
    let drag_start = Position::new(link_x, link_y);
    let drag_end = Position::new(link_x + 5, link_y);
    handle_mouse_input(MouseInput::Down(drag_start), size, &mut state);
    handle_mouse_input(MouseInput::Drag(drag_end), size, &mut state);
    assert_eq!(
        handle_mouse_input(MouseInput::Up(drag_end), size, &mut state),
        ControllerEffect::CopyText("OpenAI".to_owned())
    );
}

#[test]
fn renderer_wraps_long_table_cells_without_truncating_and_keeps_table_links_clickable() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "| Module | Notes |\n| --- | --- |\n| Table | This description is intentionally very long so it wraps across multiple lines without losing the TAIL_MARKER_ALPHA token at the end. |\n| 表格链接 | 支持长中文内容自动换行不会截断最后的中文结束标记，并且包含 [文档链接](https://docs.example.test/table) 可以点击。 |".to_owned(),
    });

    let size = Size::new(68, 24);
    let buffer = render_to_buffer(&state, size.width, size.height);
    // Ratatui stores a blank continuation cell after each 2-column CJK glyph;
    // collapse continuation cells using display widths just like copy extraction.
    let compact_text = {
        let area = buffer.area;
        let mut out = String::new();
        for y in area.y..area.bottom() {
            let mut x = area.x;
            while x < area.right() {
                let symbol = buffer[(x, y)].symbol();
                out.push_str(symbol);
                let w = u16::try_from(unicode_width::UnicodeWidthStr::width(symbol))
                    .unwrap_or(1)
                    .max(1);
                x = x.saturating_add(w);
            }
            out.push('\n');
        }
        out
    };

    assert!(
        compact_text.contains("TAIL_MARKER_ALPHA"),
        "long English table cell must wrap instead of truncating:\n{compact_text}"
    );
    assert!(
        compact_text.contains("中文结束标记"),
        "long CJK table cell must wrap instead of truncating:\n{compact_text}"
    );
    assert!(
        compact_text.contains("文档链接"),
        "table link label should render:\n{compact_text}"
    );
    assert!(
        !compact_text.contains("https://docs.example.test/table"),
        "table link URL should not be duplicated inline:\n{compact_text}"
    );

    // Hovering and clicking a link inside a wrapped table cell works accurately.
    let (link_x, link_y) = find_text_position(&buffer, "档").expect("table link");
    let link_pos = Position::new(link_x, link_y);
    handle_mouse_input(MouseInput::Move(link_pos), size, &mut state);
    let hovered_text = render_to_text(&state, size.width, size.height);
    assert!(
        hovered_text.contains("https://docs.example.test/table"),
        "hovering a table link must display its destination URL tooltip:\n{hovered_text}"
    );

    handle_mouse_input(MouseInput::Down(link_pos), size, &mut state);
    assert_eq!(
        handle_mouse_input(MouseInput::Up(link_pos), size, &mut state),
        ControllerEffect::OpenUrl("https://docs.example.test/table".to_owned())
    );
}

#[test]
fn renderer_falls_back_to_stacked_cards_for_many_columns_on_narrow_terminals() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "| Service | Region | Endpoint | Owner | Description |\n| --- | --- | --- | --- | --- |\n| auth-api | us-east-1 | https://auth.example.test | platform | Primary authentication service with FINAL_CARD_TOKEN |".to_owned(),
    });

    // 5 wide columns cannot fit tabularly in 42 columns, so it renders stacked cards with zero loss.
    let text = render_to_text(&state, 42, 24);
    assert!(text.contains("Service:"), "{text}");
    assert!(text.contains("auth-api"), "{text}");
    assert!(text.contains("Description:"), "{text}");
    assert!(
        text.contains("FINAL_CARD_TOKEN"),
        "stacked card fallback must preserve all cell text:\n{text}"
    );
}

#[test]
fn renderer_makes_plain_urls_clickable_across_punctuation_inline_code_code_blocks_and_user_turns() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "Check https://user.example.test/my_repo?a=1&b=2 please".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "Plain: https://github.com/my_org/my-repo?a=1&b=2#sec~1\n\nCode: `https://inline.example.test/v1_2?k=v&z=9`\n\n```text\nhttp://127.0.0.1:8080/health_check?ok=1&v=2\n```"
            .to_owned(),
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Tool output".to_owned(),
        body: "Listening at http://localhost:5173/app_home".to_owned(),
        expanded: true,
    });

    let size = Size::new(100, 26);
    let buffer = render_to_buffer(&state, size.width, size.height);

    for (needle, expected_url) in [
        (
            "https://user.example.test/my_repo?a=1&b=2",
            "https://user.example.test/my_repo?a=1&b=2",
        ),
        (
            "https://github.com/my_org/my-repo?a=1&b=2#sec~1",
            "https://github.com/my_org/my-repo?a=1&b=2#sec~1",
        ),
        (
            "https://inline.example.test/v1_2?k=v&z=9",
            "https://inline.example.test/v1_2?k=v&z=9",
        ),
        (
            "http://127.0.0.1:8080/health_check?ok=1&v=2",
            "http://127.0.0.1:8080/health_check?ok=1&v=2",
        ),
        (
            "http://localhost:5173/app_home",
            "http://localhost:5173/app_home",
        ),
    ] {
        let (x, y) = find_text_position(&buffer, needle)
            .unwrap_or_else(|| panic!("expected URL {needle} in rendered buffer"));
        // Click near the tail of the URL (past `_` and `&`) to confirm it was not split.
        let click_x = x + u16::try_from(needle.len().saturating_sub(3)).unwrap_or(0);
        let pos = Position::new(click_x, y);
        handle_mouse_input(MouseInput::Move(pos), size, &mut state);
        assert_eq!(
            state.hovered_link().map(|link| link.url.as_str()),
            Some(expected_url),
            "hovering tail of {needle} should resolve full URL"
        );
        handle_mouse_input(MouseInput::Down(pos), size, &mut state);
        assert_eq!(
            handle_mouse_input(MouseInput::Up(pos), size, &mut state),
            ControllerEffect::OpenUrl(expected_url.to_owned()),
            "clicking tail of {needle} should open full URL"
        );
    }
}

#[test]
fn renderer_shows_warning_tooltip_for_non_http_links() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "Do not click [Suspicious](javascript:alert(1))".to_owned(),
    });

    let size = Size::new(80, 16);
    let buffer = render_to_buffer(&state, size.width, size.height);
    let (x, y) = find_text_position(&buffer, "Suspicious").expect("link label");
    handle_mouse_input(MouseInput::Move(Position::new(x + 1, y)), size, &mut state);
    let hovered = render_to_text(&state, size.width, size.height);
    assert!(
        hovered.contains("javascript:alert(1)") && hovered.contains("not http/https"),
        "hover tooltip should warn about non-http(s) link schemes:\n{hovered}"
    );
}
