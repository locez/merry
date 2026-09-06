use crate::tui::{
    keymap::Keymap,
    render::{render_to_buffer, render_to_text},
    state::{PatchChangeView, PatchLineView, QueuePreview, TimelineItem, TuiState},
    tests::{find_cell_color, find_cell_style, find_text_position, rendered_buffer_text},
    theme::{SemanticColor, TuiTheme},
};
use merry_core::{QueuedInputLane, QueuedInputView};
use ratatui::style::{Color, Modifier};

#[test]
fn theme_has_required_semantic_color_slots() {
    let theme = TuiTheme::default();

    for slot in [
        SemanticColor::Status,
        SemanticColor::Muted,
        SemanticColor::Focus,
        SemanticColor::Assistant,
        SemanticColor::Selection,
        SemanticColor::ToolKeyword,
        SemanticColor::Command,
        SemanticColor::CodeBackground,
        SemanticColor::DiffAdd,
        SemanticColor::DiffDelete,
        SemanticColor::Warning,
        SemanticColor::Error,
        SemanticColor::Risk,
        SemanticColor::Success,
    ] {
        assert!(theme.color(slot).is_some());
    }
}

#[test]
fn renderer_makes_diagnostic_code_and_reason_visible_in_the_timeline() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Diagnostic {
        title: "auto_compaction".to_owned(),
        body: "compaction state error: compaction window is stale".to_owned(),
    });

    let rendered = render_to_text(&state, 120, 24);

    assert!(rendered.contains("! Error"));
    assert!(rendered.contains("auto_compaction"));
    assert!(rendered.contains("compaction window is stale"));
}

#[test]
fn renderer_applies_configured_semantic_theme_colors() {
    let theme = TuiTheme::from_config(&crate::config::TuiThemeToml {
        status: Some("red".to_owned()),
        muted: Some("blue".to_owned()),
        focus: Some("magenta".to_owned()),
        assistant: Some("white".to_owned()),
        tool_keyword: Some("cyan".to_owned()),
        command: Some("light_blue".to_owned()),
        warning: Some("yellow".to_owned()),
        success: Some("green".to_owned()),
        diff_add: Some("green".to_owned()),
        diff_delete: Some("yellow".to_owned()),
        ..crate::config::TuiThemeToml::default()
    })
    .expect("theme config should validate");
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        theme,
    );
    state.push_timeline_item(TimelineItem::Muted {
        title: "tool".to_owned(),
        detail: "read".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran cargo test --package 'hello world' && printf $HOME (.)".to_owned(),
        body: "ok".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Patch {
        changes: vec![PatchChangeView {
            path: "patch".to_owned(),
            added: 1,
            removed: 1,
            hunks: 1,
            bytes_before: Some(8),
            bytes_after: Some(6),
            lines: vec![
                PatchLineView::remove("removed", Some(1)),
                PatchLineView::add("added", Some(1)),
            ],
        }],
    });
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "queued".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![],
    });
    let buffer = render_to_buffer(&state, 180, 24);

    assert_eq!(find_cell_color(&buffer, "merry"), Some(Color::Red));
    assert_eq!(find_cell_color(&buffer, "/repo"), Some(Color::LightBlue));
    assert_eq!(find_cell_color(&buffer, "gpt-test"), Some(Color::Cyan));
    assert_eq!(find_cell_color(&buffer, "tool"), Some(Color::Blue));
    assert_eq!(find_cell_color(&buffer, "Ran"), Some(Color::Cyan));
    assert_eq!(find_cell_color(&buffer, "cargo"), Some(Color::LightBlue));
    assert_eq!(find_cell_color(&buffer, "--package"), Some(Color::Magenta));
    assert_eq!(
        find_cell_color(&buffer, "'hello world'"),
        Some(Color::LightBlue)
    );
    assert_eq!(find_cell_color(&buffer, "&&"), Some(Color::Cyan));
    assert_eq!(find_cell_color(&buffer, "$HOME"), Some(Color::Green));
    assert_eq!(find_cell_color(&buffer, "patch"), Some(Color::Magenta));
    assert_eq!(find_cell_color(&buffer, "Next"), Some(Color::Magenta));
    assert_eq!(find_cell_color(&buffer, "queued"), Some(Color::Blue));
}

#[test]
fn renderer_uses_colored_header_bar_and_branded_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("draft");

    let buffer = render_to_buffer(&state, 80, 16);
    let text = rendered_buffer_text(&buffer);
    let path_style = find_cell_style(&buffer, "/repo").expect("workspace path should render");
    let model_style = find_cell_style(&buffer, "gpt-test").expect("model should render");
    let usage_style = find_cell_style(&buffer, "usage -").expect("usage should render");
    let input_brand_style = find_cell_style(&buffer, "M").expect("input brand should render");
    let input_style = find_cell_style(&buffer, "draft").expect("input text should render");

    assert!(!text.contains("input"));
    assert_eq!(path_style.fg, Some(Color::LightBlue));
    assert_eq!(model_style.fg, Some(Color::LightCyan));
    assert!(model_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(usage_style.fg, Some(Color::White));
    assert!(usage_style.add_modifier.contains(Modifier::DIM));
    assert_eq!(path_style.bg, Some(Color::Rgb(54, 26, 58)));
    assert_eq!(model_style.bg, Some(Color::Rgb(54, 26, 58)));
    assert_eq!(input_brand_style.fg, Some(Color::LightMagenta));
    assert!(input_brand_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(input_style.fg, Some(Color::White));
}

#[test]
fn renderer_highlights_inline_code_spans() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "call `build_message()` now".to_owned(),
    });

    let buffer = render_to_buffer(&state, 80, 16);
    let assistant_style =
        find_cell_style(&buffer, "call").expect("assistant text should be rendered");
    let code_style =
        find_cell_style(&buffer, "build_message()").expect("inline code text should be rendered");

    assert_eq!(assistant_style.fg, Some(Color::White));
    assert_eq!(code_style.fg, Some(Color::LightMagenta));
    assert_eq!(code_style.bg, Some(Color::Reset));
    assert!(code_style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn renderer_renders_assistant_markdown_strong_without_markers() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "**hello** world".to_owned(),
    });

    let text = render_to_text(&state, 80, 16);
    assert!(!text.contains("**hello**"));
    assert!(text.contains("hello"));

    let buffer = render_to_buffer(&state, 80, 16);
    let strong_style = find_cell_style(&buffer, "hello").expect("strong text should render");
    assert_eq!(strong_style.fg, Some(Color::LightMagenta));
    assert!(strong_style.add_modifier.contains(Modifier::BOLD));
    assert!(!strong_style.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn renderer_keeps_plain_assistant_markdown_text_white() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "plain text with **strong text**".to_owned(),
    });

    let buffer = render_to_buffer(&state, 80, 16);
    let plain_style = find_cell_style(&buffer, "plain text").expect("plain text should render");
    let trailing_style =
        find_cell_style(&buffer, "with").expect("trailing plain text should render");

    assert_eq!(plain_style.fg, Some(Color::White));
    assert_eq!(trailing_style.fg, Some(Color::White));
}

#[test]
fn renderer_renders_assistant_markdown_heading_as_title_block() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "# Result\nBody text".to_owned(),
    });

    let text = render_to_text(&state, 80, 16);
    assert!(!text.contains("# Result"));
    assert!(text.contains("Result"));
    assert!(text.contains("Body text"));

    let buffer = render_to_buffer(&state, 80, 16);
    let heading_style = find_cell_style(&buffer, "Result").expect("heading should render");
    let body_style = find_cell_style(&buffer, "Body text").expect("body should render");
    assert_eq!(heading_style.fg, Some(Color::LightMagenta));
    assert!(heading_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(body_style.fg, Some(Color::White));
}

#[test]
fn renderer_renders_assistant_markdown_table_with_header_and_cells() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "| Name | Status |\n| --- | --- |\n| API | **OK** |\n| UI | pending |".to_owned(),
    });

    let text = render_to_text(&state, 100, 18);
    assert!(text.contains("Name"));
    assert!(text.contains("Status"));
    assert!(text.contains("API"));
    assert!(text.contains("OK"));
    assert!(text.contains("UI"));
    assert!(text.contains("pending"));
    assert!(!text.contains("| --- |"));

    let buffer = render_to_buffer(&state, 100, 18);
    let header_style = find_cell_style(&buffer, "Status").expect("table header should render");
    let strong_style = find_cell_style(&buffer, "OK").expect("strong table cell should render");
    assert_eq!(header_style.fg, Some(Color::LightMagenta));
    assert!(header_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(strong_style.fg, Some(Color::LightMagenta));
    assert!(strong_style.add_modifier.contains(Modifier::BOLD));
    assert!(!strong_style.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn renderer_renders_assistant_markdown_strikethrough_as_muted_crossed_text() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "before ~~removed~~ after".to_owned(),
    });

    let text = render_to_text(&state, 80, 16);
    assert!(!text.contains("~~removed~~"));
    assert!(text.contains("removed"));

    let buffer = render_to_buffer(&state, 80, 16);
    let removed_style =
        find_cell_style(&buffer, "removed").expect("strikethrough text should render");
    assert_eq!(removed_style.fg, Some(Color::DarkGray));
    assert!(removed_style.add_modifier.contains(Modifier::CROSSED_OUT));
}

#[test]
fn renderer_prefixes_wrapped_assistant_markdown_block_quotes() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "> quoted text wraps onto another visual line".to_owned(),
    });

    let text = render_to_text(&state, 34, 16);
    let quote_lines = text
        .lines()
        .filter(|line| line.trim_start().starts_with(">"))
        .collect::<Vec<_>>();
    assert!(
        quote_lines.len() >= 2,
        "wrapped quote should keep quote prefix on each visual line:\n{text}"
    );

    let buffer = render_to_buffer(&state, 34, 16);
    let quote_style = find_cell_style(&buffer, ">").expect("quote marker should render");
    assert_eq!(quote_style.fg, Some(Color::LightMagenta));
}

#[test]
fn renderer_keeps_assistant_markdown_link_url_visible() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "[OpenAI](https://openai.com) and [https://example.com](https://example.com)"
            .to_owned(),
    });

    let text = render_to_text(&state, 100, 16);
    assert!(text.contains("OpenAI"));
    assert!(text.contains("https://openai.com"));
    assert!(text.contains("https://example.com"));
    assert_eq!(text.matches("https://example.com").count(), 1);

    let buffer = render_to_buffer(&state, 100, 16);
    let label_style = find_cell_style(&buffer, "OpenAI").expect("link label should render");
    let url_style = find_cell_style(&buffer, "https://openai.com").expect("link url should render");
    assert_eq!(label_style.fg, Some(Color::LightBlue));
    assert_eq!(url_style.fg, Some(Color::LightBlue));
    assert!(url_style.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn renderer_preserves_assistant_message_newlines() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "Done:\n- changed `hello_world.py`\n- verified output".to_owned(),
    });

    let text = render_to_text(&state, 80, 18);
    assert!(text.contains("Done:"));
    assert!(text.contains("- changed  hello_world.py"));
    assert!(text.contains("- verified output"));
    assert!(!text.contains("Done:- changed"));
}

#[test]
fn renderer_renders_assistant_fenced_code_blocks_without_fence_markers() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "Output:\n```text\nhello world\n```\nDone".to_owned(),
    });

    let buffer = render_to_buffer(&state, 80, 18);
    let text = rendered_buffer_text(&buffer);
    assert!(text.contains("Output:"));
    assert!(text.contains("▎ hello world"));
    assert!(text.contains("Done"));
    assert!(!text.contains("```"));

    let rail_style = find_cell_style(&buffer, "▎").expect("code rail should render");
    let code_style = find_cell_style(&buffer, "hello world").expect("code should render");
    let (_, code_row) = find_text_position(&buffer, "hello world").expect("code should render");
    assert_eq!(rail_style.fg, Some(Color::LightMagenta));
    assert_eq!(rail_style.bg, Some(Color::Rgb(40, 36, 42)));
    assert_eq!(code_style.bg, Some(Color::Rgb(40, 36, 42)));
    assert_eq!(
        buffer[(40, code_row)].style().bg,
        Some(Color::Rgb(40, 36, 42))
    );
}

#[test]
fn renderer_repeats_code_rail_on_wrapped_visual_lines() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "```text\nalpha beta gamma delta epsilon\n```".to_owned(),
    });

    let text = render_to_text(&state, 20, 18);
    let code_lines = text.lines().filter(|line| line.contains('▎')).count();

    assert!(
        code_lines >= 2,
        "wrapped code should repeat the rail:\n{text}"
    );
}

#[test]
fn renderer_keeps_inline_code_atomic_when_wrapping_assistant_text() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "prefix text `hello world` suffix".to_owned(),
    });

    let text = render_to_text(&state, 24, 18);
    assert!(text.contains(" hello world "));
    assert!(!text.contains("hello \nworld"));
}

#[test]
fn renderer_wraps_assistant_text_on_word_boundaries() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "alpha beta gamma delta".to_owned(),
    });

    let text = render_to_text(&state, 18, 18);
    assert!(text.contains("alpha beta"));
    assert!(!text.contains("bet\na"));
    assert!(!text.contains("gamm\na"));
    assert!(!text.contains("delt\na"));
}

#[test]
fn renderer_draws_assistant_separator_across_timeline_width() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "done".to_owned(),
    });

    let text = render_to_text(&state, 40, 12);
    assert!(text.lines().any(|line| line == "-".repeat(40)));
}
