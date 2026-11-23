use crate::tui::{
    keymap::Keymap,
    render::{render_to_buffer, render_to_text},
    state::{PatchChangeView, PatchLineView, PatchOperationView, TimelineItem, TuiState},
    tests::{find_cell_color, find_cell_style, find_text_position, rendered_buffer_text},
    theme::{SemanticColor, TuiTheme},
};
use merry_core::{QueuedInputView, QueuedInputsView};
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
        expanded: true,
    });
    state.push_timeline_item(TimelineItem::Patch {
        changes: vec![PatchChangeView {
            path: "patch".to_owned(),
            operation: PatchOperationView::Update,
            added: 1,
            removed: 1,
            hunks: 1,
            lines_before: Some(3),
            lines_after: Some(3),
            bytes_before: 8,
            bytes_after: 6,
            lines: vec![
                PatchLineView::remove("removed", Some(1)),
                PatchLineView::add("added", Some(1)),
            ],
            omitted_lines: 0,
        }],
        expanded: false,
    });
    state.update_queue_preview(QueuedInputsView {
        items: vec![QueuedInputView {
            id: 1,
            text: "queued".to_owned(),
            position: 0,
        }],
        paused: false,
    });
    let buffer = render_to_buffer(&state, 180, 24);

    assert_eq!(
        find_cell_color(&buffer, crate::tui::glyphs::BRAND_PAW),
        Some(Color::Red)
    );
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
    assert_eq!(find_cell_color(&buffer, "Pending"), Some(Color::Magenta));
    assert!(find_cell_color(&buffer, "queued").is_some());
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
    let input_brand_style = find_cell_style(&buffer, crate::tui::glyphs::BRAND_RIBBON)
        .expect("input brand should render");
    let input_style = find_cell_style(&buffer, "draft").expect("input text should render");

    let theme = TuiTheme::default();
    assert!(!text.contains("input"));
    assert_eq!(path_style.fg, theme.color(SemanticColor::Command));
    assert_eq!(model_style.fg, theme.color(SemanticColor::ToolKeyword));
    assert!(model_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(usage_style.fg, theme.color(SemanticColor::TextSecondary));
    assert!(usage_style.add_modifier.contains(Modifier::DIM));
    assert_eq!(
        path_style.bg,
        theme.bg_color(SemanticColor::HeaderBackground)
    );
    assert_eq!(
        model_style.bg,
        theme.bg_color(SemanticColor::HeaderBackground)
    );
    assert_eq!(input_brand_style.fg, theme.color(SemanticColor::Focus));
    assert!(input_brand_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(input_style.fg, theme.color(SemanticColor::Assistant));

    let snowflake_state = state.with_brand_mark(crate::config::BrandMarkMode::Snowflake);
    let snowflake_buffer = render_to_buffer(&snowflake_state, 80, 16);
    let snowflake_text = rendered_buffer_text(&snowflake_buffer);
    assert!(snowflake_text.contains(crate::tui::glyphs::BRAND_SNOWFLAKE));
    assert!(snowflake_text.contains(crate::tui::glyphs::BRAND_PAW));
    assert!(!snowflake_text.contains(crate::tui::glyphs::BRAND_RIBBON));
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
    let theme = TuiTheme::default();

    assert_eq!(assistant_style.fg, theme.color(SemanticColor::Assistant));
    assert_eq!(code_style.fg, theme.color(SemanticColor::Focus));
    assert_eq!(code_style.bg, theme.color(SemanticColor::Background));
    assert!(code_style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn renderer_preserves_historical_emphasis_across_user_and_assistant_text() {
    let default_focus = TuiTheme::default()
        .color(SemanticColor::Focus)
        .expect("default focus color");
    for (focus, expected_color) in [(None, default_focus), (Some("red"), Color::Red)] {
        let theme = TuiTheme::from_config(&crate::config::TuiThemeToml {
            focus: focus.map(str::to_owned),
            ..crate::config::TuiThemeToml::default()
        })
        .expect("theme config should validate");
        let mut state = TuiState::new(
            "/repo".into(),
            "gpt-test".to_owned(),
            Keymap::default(),
            theme,
        );
        state.push_timeline_item(TimelineItem::User {
            text: "inspect `user_keyword`".to_owned(),
        });
        state.push_timeline_item(TimelineItem::Assistant {
            text: "**important** and `assistant_keyword`".to_owned(),
        });

        let buffer = render_to_buffer(&state, 120, 24);
        let user_style =
            find_cell_style(&buffer, "user_keyword").expect("user keyword should render");
        assert_eq!(user_style.fg, Some(expected_color));
        assert_eq!(
            user_style.bg,
            state.theme().bg_color(SemanticColor::UserBackground)
        );
        assert!(user_style.add_modifier.contains(Modifier::BOLD));
        assert!(!user_style.add_modifier.contains(Modifier::DIM));

        let bold_style = find_cell_style(&buffer, "important").expect("bold text should render");
        assert_eq!(bold_style.fg, Some(expected_color));
        assert_eq!(
            bold_style.bg,
            state.theme().color(SemanticColor::Background)
        );
        assert!(bold_style.add_modifier.contains(Modifier::BOLD));
        assert!(!bold_style.add_modifier.contains(Modifier::DIM));

        let code_style =
            find_cell_style(&buffer, "assistant_keyword").expect("code text should render");
        assert_eq!(code_style.fg, Some(expected_color));
        assert_eq!(
            code_style.bg,
            state.theme().color(SemanticColor::Background)
        );
        assert!(code_style.add_modifier.contains(Modifier::BOLD));
        assert!(!code_style.add_modifier.contains(Modifier::DIM));
    }
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
    assert_eq!(
        strong_style.fg,
        TuiTheme::default().color(SemanticColor::Focus)
    );
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
    let expected = TuiTheme::default().color(SemanticColor::Assistant);

    assert_eq!(plain_style.fg, expected);
    assert_eq!(trailing_style.fg, expected);
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
    let theme = TuiTheme::default();
    assert_eq!(heading_style.fg, theme.color(SemanticColor::Heading));
    assert!(heading_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(body_style.fg, theme.color(SemanticColor::Assistant));
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
    let theme = TuiTheme::default();
    assert_eq!(header_style.fg, theme.color(SemanticColor::Focus));
    assert!(header_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(strong_style.fg, theme.color(SemanticColor::Focus));
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
    assert_eq!(
        removed_style.fg,
        TuiTheme::default().color(SemanticColor::Muted)
    );
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
    assert_eq!(
        quote_style.fg,
        TuiTheme::default().color(SemanticColor::Focus)
    );
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
    assert!(text.contains("- changed hello_world.py"));
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
    let theme = TuiTheme::default();
    let code_bg = theme.bg_color(SemanticColor::CodeBackground);
    assert_eq!(rail_style.fg, theme.color(SemanticColor::Focus));
    assert_eq!(rail_style.bg, code_bg);
    assert_eq!(code_style.bg, code_bg);
    assert_eq!(buffer[(40, code_row)].style().bg, code_bg);
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
    assert!(text.contains("hello world"));
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
fn renderer_uses_user_turn_card_instead_of_dashed_assistant_separator() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "first question".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "done".to_owned(),
    });

    let text = render_to_text(&state, 40, 14);
    assert!(
        !text.lines().any(|line| line.contains("--------")),
        "assistant replies should not emit dashed hyphen separators:\n{text}"
    );
    assert!(text.contains(" ❯ first question"), "{text}");
    assert!(!text.contains('▌'), "{text}");
    assert!(
        !text.contains('▔') && !text.contains('▁'),
        "compact user band should not add floating cage hairlines:\n{text}"
    );
}

#[test]
fn markdown_list_continuation_lines_keep_hanging_indent_and_multiline_gap() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
    .with_max_reading_width(36);
    state.push_timeline_item(TimelineItem::Assistant {
        text: "- first bullet item with enough words to wrap onto a second line cleanly\n- second bullet item".to_owned(),
    });

    let rendered = render_to_text(&state, 60, 18);
    let list_lines: Vec<&str> = rendered
        .lines()
        .skip_while(|line| !line.contains("- first bullet"))
        .take(5)
        .collect();
    assert!(list_lines.len() >= 4, "{rendered}");
    let bullet_col = list_lines[0].find("- ").expect("bullet marker");
    let wrap_col = list_lines[1]
        .find(|ch: char| !ch.is_whitespace())
        .expect("wrapped text");
    assert_eq!(
        wrap_col,
        bullet_col + 2,
        "wrapped bullet continuation should align past `- `:\n{rendered}"
    );
    let second_index = list_lines
        .iter()
        .position(|line| line.contains("- second bullet"))
        .expect("second bullet should render");
    assert!(
        second_index >= 3 && list_lines[second_index - 1].trim().is_empty(),
        "multi-line bullet should breathe before next bullet:\n{rendered}"
    );
}

#[test]
fn composer_renders_horizontal_padding_surface_and_prompt() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    // Empty composer shows a subtle placeholder without a redundant left rail.
    let empty_text = render_to_text(&state, 80, 24);
    assert!(empty_text.contains("Message Merry…"), "{empty_text}");

    state.insert_input_str("hello world");

    let buffer = render_to_buffer(&state, 80, 24);
    let (text_x, text_y) =
        find_text_position(&buffer, "hello world").expect("composer text should render");
    // Insets: cols 0..2 indent, cols 2..4 "❯ ", col 4 text (aligned with 🎀).
    assert_eq!(text_x, 4);
    assert_eq!(buffer[(2, text_y)].symbol(), "❯");
    assert_ne!(buffer[(1, text_y)].symbol(), "┃");
    assert_eq!(
        buffer[(text_x, text_y)].style().bg,
        TuiTheme::default().bg_color(SemanticColor::SurfaceSubtle)
    );
}

#[test]
fn header_respects_configured_items_including_cache_and_rate() {
    use crate::config::HeaderItemKind;
    use merry_core::{ContextWindowSource, ModelUsage, SessionUsage, UsageContextWindow};

    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
    .with_header_items(vec![
        HeaderItemKind::Model,
        HeaderItemKind::Context,
        HeaderItemKind::Cache,
        HeaderItemKind::Rate,
    ]);
    let last = ModelUsage::with_details(10_000, Some(7_500), 400, None, 10_400);
    state.set_usage(SessionUsage {
        last,
        total: last,
        context: Some(UsageContextWindow {
            effective_window_tokens: 100_000,
            resolved_model_window_tokens: 128_000,
            source: ContextWindowSource::BundledCatalog,
        }),
        compaction: None,
        subagent_total: None,
    });

    let text = render_to_text(&state, 100, 16);
    let header_line = text.lines().next().expect("header line should exist");
    assert!(
        !header_line.contains("/repo"),
        "workspace excluded: {header_line}"
    );
    assert!(header_line.contains("gpt-test"), "{header_line}");
    assert!(header_line.contains("ctx 10k/100k"), "{header_line}");
    assert!(header_line.contains("cache 75%"), "{header_line}");
    assert!(header_line.contains("tok/s"), "{header_line}");
}

#[test]
fn markdown_renders_block_gaps_heading_hierarchy_and_code_card_header() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
    .with_max_reading_width(50);
    state.push_timeline_item(TimelineItem::Assistant {
        text: "# Primary Title\n\nParagraph one with enough words to verify max reading width clamping across wide terminals.\n\n```rust\nfn main() {}\n```".to_owned(),
    });

    let buffer = render_to_buffer(&state, 120, 24);
    let text = rendered_buffer_text(&buffer);
    assert!(text.contains("Primary Title"), "{text}");
    assert!(
        text.contains('━'),
        "H1 should render subtle accent underline:\n{text}"
    );
    assert!(
        text.contains("rust"),
        "code block card header should show language:\n{text}"
    );
    assert!(
        text.contains("⧉ Copy"),
        "code block card header should show copy chip:\n{text}"
    );
    let heading_style = find_cell_style(&buffer, "Primary").expect("heading should render");
    assert_eq!(
        heading_style.fg,
        TuiTheme::default().color(SemanticColor::Heading)
    );
    assert!(heading_style.add_modifier.contains(Modifier::BOLD));
    // Prose line should wrap within max_reading_width (50 cols) even on a 120-col terminal.
    let prose_line = text
        .lines()
        .find(|line| line.contains("Paragraph one"))
        .expect("prose line should render");
    assert!(
        prose_line.trim().len() <= 50,
        "prose should wrap within max_reading_width: {prose_line:?}"
    );
}
