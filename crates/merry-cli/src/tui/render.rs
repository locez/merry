use super::{
    state::{PatchChangeView, PatchLineView, TimelineItem, TuiState},
    theme::SemanticColor,
};
use merry_core::QueuedInputLane;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const QUEUE_PREVIEW_HEIGHT: u16 = 5;
const MAX_COMPLETION_PREVIEW_HEIGHT: u16 = 6;
const MAX_INPUT_VISIBLE_ROWS: usize = 5;

#[allow(dead_code)]
pub(crate) fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let queue_height = queue_preview_height(state);
    let completion_height = completion_preview_height(state);
    let input_height = input_region_height(state);
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(queue_height),
            Constraint::Length(completion_height),
            Constraint::Length(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let input_inner = bordered_inner(root[4]);
    let input_viewport =
        state.input_viewport_rows(usize::from(input_inner.width), MAX_INPUT_VISIBLE_ROWS);

    frame.render_widget(
        Paragraph::new(timeline_lines(state, root[0]))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_type(BorderType::Plain)
                    .border_style(semantic_style(state, SemanticColor::Muted))
                    .title_style(semantic_style(state, SemanticColor::Muted)),
            ),
        root[0],
    );
    if queue_height > 0 {
        frame.render_widget(
            Paragraph::new(queue_lines(state, root[1])).block(
                Block::default()
                    .title("queue")
                    .border_style(semantic_style(state, SemanticColor::Muted)),
            ),
            root[1],
        );
    }
    if completion_height > 0 {
        frame.render_widget(Paragraph::new(completion_lines(state, root[2])), root[2]);
    }
    let interaction_style = if state.is_active_run() {
        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
    } else {
        semantic_style(state, SemanticColor::Status)
    };
    frame.render_widget(
        Paragraph::new(state.interaction_status_text()).style(interaction_style),
        root[3],
    );
    frame.render_widget(
        Paragraph::new(input_viewport.text)
            .style(semantic_style(state, SemanticColor::Focus))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .title("input")
                    .border_style(semantic_style(state, SemanticColor::Focus))
                    .title_style(semantic_style(state, SemanticColor::Focus)),
            ),
        root[4],
    );
    set_input_cursor(
        frame,
        input_inner,
        input_viewport.cursor_column,
        input_viewport.cursor_row,
    );
    frame.render_widget(
        Paragraph::new(state.status_text()).style(semantic_style(state, SemanticColor::Status)),
        root[5],
    );
}

#[cfg(test)]
pub(crate) fn render_to_text(state: &TuiState, width: u16, height: u16) -> String {
    let buffer = render_to_buffer(state, width, height);
    let area = buffer.area;
    let mut text = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

#[cfg(test)]
pub(crate) fn render_to_buffer(
    state: &TuiState,
    width: u16,
    height: u16,
) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal should build");
    terminal
        .draw(|frame| render(frame, state))
        .expect("test render should draw");

    terminal.backend().buffer().clone()
}

#[cfg(test)]
pub(crate) fn render_to_buffer_and_cursor(
    state: &TuiState,
    width: u16,
    height: u16,
) -> (ratatui::buffer::Buffer, Position) {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal should build");
    terminal
        .draw(|frame| render(frame, state))
        .expect("test render should draw");

    (
        terminal.backend().buffer().clone(),
        terminal.backend().cursor_position(),
    )
}

fn bordered_inner(region: Rect) -> Rect {
    Rect {
        x: region.x.saturating_add(1),
        y: region.y.saturating_add(1),
        width: region.width.saturating_sub(2),
        height: region.height.saturating_sub(2),
    }
}

fn set_input_cursor(frame: &mut Frame<'_>, region: Rect, cursor_column: usize, cursor_row: usize) {
    if region.width == 0 || region.height == 0 {
        return;
    }
    let cursor_column = u16::try_from(cursor_column).unwrap_or(u16::MAX);
    let cursor_row = u16::try_from(cursor_row).unwrap_or(u16::MAX);
    let max_x = region.x.saturating_add(region.width.saturating_sub(1));
    let max_y = region.y.saturating_add(region.height.saturating_sub(1));
    frame.set_cursor_position(Position {
        x: region.x.saturating_add(cursor_column).min(max_x),
        y: region.y.saturating_add(cursor_row).min(max_y),
    });
}

fn queue_preview_height(state: &TuiState) -> u16 {
    if state.has_queue_preview_items() {
        QUEUE_PREVIEW_HEIGHT
    } else {
        0
    }
}

fn completion_preview_height(state: &TuiState) -> u16 {
    state
        .completion_menu()
        .map(|menu| {
            u16::try_from(menu.items().len())
                .unwrap_or(MAX_COMPLETION_PREVIEW_HEIGHT)
                .min(MAX_COMPLETION_PREVIEW_HEIGHT)
        })
        .unwrap_or(0)
}

fn input_region_height(state: &TuiState) -> u16 {
    let visible_rows = state.input_visible_rows(MAX_INPUT_VISIBLE_ROWS);
    u16::try_from(visible_rows)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
}

fn timeline_lines(state: &TuiState, region: Rect) -> Vec<Line<'static>> {
    let mut user_line_indexes = Vec::new();
    let mut lines = Vec::new();
    for (index, item) in state.timeline().iter().enumerate() {
        if matches!(item, TimelineItem::User { .. }) {
            user_line_indexes.push((index, lines.len()));
        }
        let item_lines = match item {
            TimelineItem::User { text, lane } => user_lines(state, text, *lane),
            TimelineItem::Assistant { text } => assistant_lines(state, text, region.width),
            TimelineItem::Muted { title, detail } => muted_lines(state, title, detail),
            TimelineItem::Expanded { title, body } | TimelineItem::Diagnostic { title, body } => {
                expanded_lines(state, item, title, body)
            }
            TimelineItem::Patch { changes } => patch_lines(state, changes),
        };
        lines.extend(spaced_timeline_item(
            item_lines,
            index + 1 < state.timeline().len(),
        ));
    }
    let visible_height = usize::from(region.height).saturating_sub(1).max(1);
    let (start, take) = timeline_viewport(state, &lines, &user_line_indexes, visible_height);
    lines.into_iter().skip(start).take(take).collect()
}

fn timeline_viewport(
    state: &TuiState,
    lines: &[Line<'static>],
    user_line_indexes: &[(usize, usize)],
    visible_height: usize,
) -> (usize, usize) {
    if let Some(target_index) = state.timeline_review_user_index()
        && let Some((_, start)) = user_line_indexes
            .iter()
            .find(|(item_index, _)| *item_index == target_index)
    {
        let start = (*start).min(lines.len());
        let end = start.saturating_add(visible_height).min(lines.len());
        return (start, end.saturating_sub(start));
    }

    let end = lines
        .len()
        .saturating_sub(state.timeline_scroll_offset())
        .max(visible_height)
        .min(lines.len());
    let start = end.saturating_sub(visible_height);
    (start, end.saturating_sub(start))
}

fn spaced_timeline_item(mut lines: Vec<Line<'static>>, has_next_item: bool) -> Vec<Line<'static>> {
    if has_next_item && !lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn assistant_lines(state: &TuiState, text: &str, region_width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_fence = false;
    for line in text.split('\n') {
        if is_code_fence_line(line) {
            in_code_fence = !in_code_fence;
            continue;
        }

        if in_code_fence {
            lines.extend(code_block_lines(state, line, region_width));
        } else {
            lines.extend(inline_code_wrapped_lines(
                state,
                line,
                semantic_style(state, SemanticColor::Assistant),
                region_width,
            ));
        }
    }
    lines.push(assistant_separator_line(state, region_width));
    lines
}

fn is_code_fence_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

fn code_block_lines(state: &TuiState, text: &str, region_width: u16) -> Vec<Line<'static>> {
    let style = inline_code_style(state, semantic_style(state, SemanticColor::Assistant));
    wrap_styled_parts_preserving_leading_whitespace(
        vec![StyledTextPart {
            text: format!("  {text}"),
            style,
            atomic: false,
        }],
        region_width,
    )
}

fn assistant_separator_line(state: &TuiState, region_width: u16) -> Line<'static> {
    let width = usize::from(region_width).max(1);
    Line::from(Span::styled(
        "-".repeat(width),
        semantic_style(state, SemanticColor::Muted),
    ))
}

fn muted_lines(state: &TuiState, title: &str, detail: &str) -> Vec<Line<'static>> {
    let mut spans = vec![Span::styled(
        title.to_owned(),
        semantic_style(state, SemanticColor::Muted),
    )];
    if detail.is_empty() {
        return vec![Line::from(spans)];
    }
    spans.push(Span::styled(
        " ",
        semantic_style(state, SemanticColor::Muted),
    ));
    spans.extend(inline_code_spans(
        state,
        detail,
        semantic_style(state, SemanticColor::Muted),
    ));
    vec![Line::from(spans)]
}

fn expanded_lines(
    state: &TuiState,
    item: &TimelineItem,
    title: &str,
    body: &str,
) -> Vec<Line<'static>> {
    let mut lines = vec![expanded_title_line(state, item, title)];
    lines.extend(
        body.lines()
            .map(|line| inline_code_line(state, line, timeline_body_style(state, item, line))),
    );
    lines
}

fn expanded_title_line(state: &TuiState, item: &TimelineItem, title: &str) -> Line<'static> {
    if matches!(item, TimelineItem::Diagnostic { .. }) {
        return Line::from(Span::styled(
            title.to_owned(),
            semantic_style(state, SemanticColor::Error),
        ));
    }

    if let Some(command) = title
        .strip_prefix("Ran ")
        .or_else(|| title.strip_prefix("Ran: "))
    {
        return ran_title_line(state, command);
    }

    Line::from(Span::styled(
        title.to_owned(),
        semantic_style(state, SemanticColor::Focus),
    ))
}

fn ran_title_line(state: &TuiState, detail: &str) -> Line<'static> {
    let (command, suffix) = split_command_suffix(detail);
    let mut spans = vec![
        Span::styled(
            "Ran".to_owned(),
            semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ".to_owned(), semantic_style(state, SemanticColor::Muted)),
    ];
    spans.extend(command_spans(state, command));
    if !suffix.is_empty() {
        spans.push(Span::styled(
            suffix.to_owned(),
            semantic_style(state, SemanticColor::Muted),
        ));
    }
    Line::from(spans)
}

fn split_command_suffix(detail: &str) -> (&str, &str) {
    let Some((command, _)) = detail.rsplit_once(" (cwd: ") else {
        return (detail, "");
    };
    (command, &detail[command.len()..])
}

fn command_spans(state: &TuiState, command: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, word) in command.split_whitespace().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" ".to_owned()));
        }
        let style = if index == 0 {
            semantic_style(state, SemanticColor::Command).add_modifier(Modifier::BOLD)
        } else {
            semantic_style(state, SemanticColor::Assistant)
        };
        spans.push(Span::styled(word.to_owned(), style));
    }
    if spans.is_empty() {
        spans.push(Span::styled(
            command.to_owned(),
            semantic_style(state, SemanticColor::Command),
        ));
    }
    spans
}

fn user_lines(state: &TuiState, text: &str, lane: QueuedInputLane) -> Vec<Line<'static>> {
    let label = match lane {
        QueuedInputLane::Next => "user",
        QueuedInputLane::Suspended => "user suspended",
        QueuedInputLane::Backlog => "user backlog",
    };
    let mut spans = vec![
        Span::styled(
            label.to_owned(),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        ),
        Span::styled(": ", semantic_style(state, SemanticColor::Muted)),
    ];
    spans.extend(inline_code_spans(
        state,
        text,
        semantic_style(state, SemanticColor::Focus),
    ));
    vec![Line::from(spans)]
}

fn patch_lines(state: &TuiState, changes: &[PatchChangeView]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for change in changes {
        lines.push(Line::from(Span::styled(
            format!(
                "Edited {} (+{} -{})",
                change.path, change.added, change.removed
            ),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "  {} hunk(s), {} -> {} bytes",
                change.hunks,
                change
                    .bytes_before
                    .map_or_else(|| "-".to_owned(), |bytes| bytes.to_string()),
                change
                    .bytes_after
                    .map_or_else(|| "-".to_owned(), |bytes| bytes.to_string())
            ),
            semantic_style(state, SemanticColor::Muted),
        )));
        for line in &change.lines {
            lines.push(patch_line(state, line));
        }
    }
    lines
}

fn patch_line(state: &TuiState, line: &PatchLineView) -> Line<'static> {
    let (marker, style) = match line.kind {
        super::state::PatchLineKind::Context => (
            " ",
            semantic_style(state, SemanticColor::Focus).bg(Color::Reset),
        ),
        super::state::PatchLineKind::Add => ("+", diff_line_style(state, SemanticColor::DiffAdd)),
        super::state::PatchLineKind::Remove => {
            ("-", diff_line_style(state, SemanticColor::DiffDelete))
        }
    };

    Line::from(vec![
        Span::styled(
            format_line_number(patch_display_line(line)),
            patch_gutter_style(state),
        ),
        Span::styled(" ", patch_gutter_style(state)),
        Span::styled(marker.to_owned(), style),
        Span::styled(line.text.clone(), style),
    ])
}

fn patch_display_line(line: &PatchLineView) -> Option<usize> {
    match line.kind {
        super::state::PatchLineKind::Context | super::state::PatchLineKind::Add => line.new_line,
        super::state::PatchLineKind::Remove => line.old_line,
    }
}

fn format_line_number(line: Option<usize>) -> String {
    line.map_or_else(|| "    ".to_owned(), |line| format!("{line:>4}"))
}

fn patch_gutter_style(state: &TuiState) -> Style {
    semantic_style(state, SemanticColor::Muted)
}

fn diff_line_style(state: &TuiState, slot: SemanticColor) -> Style {
    let foreground = state.theme().color(slot);
    let background = state.theme().color(slot).map(dim_color);
    match (foreground, background) {
        (Some(foreground), Some(background)) => Style::default().fg(foreground).bg(background),
        (Some(foreground), None) => Style::default().fg(foreground),
        (None, Some(background)) => Style::default().bg(background),
        (None, None) => Style::default(),
    }
}

fn dim_color(color: Color) -> Color {
    match color {
        Color::Black => Color::Black,
        Color::Red => Color::Rgb(45, 16, 24),
        Color::Green => Color::Rgb(18, 42, 28),
        Color::Yellow => Color::Rgb(48, 40, 18),
        Color::Blue => Color::Rgb(18, 30, 48),
        Color::Magenta => Color::Rgb(42, 20, 44),
        Color::Cyan => Color::Rgb(16, 42, 45),
        Color::LightRed => Color::Rgb(58, 20, 28),
        Color::LightGreen => Color::Rgb(22, 54, 34),
        Color::LightYellow => Color::Rgb(62, 52, 22),
        Color::LightBlue => Color::Rgb(22, 38, 62),
        Color::LightMagenta => Color::Rgb(54, 26, 58),
        Color::LightCyan => Color::Rgb(20, 54, 58),
        Color::Gray => Color::DarkGray,
        Color::DarkGray => Color::Black,
        Color::White => Color::DarkGray,
        Color::Rgb(red, green, blue) => Color::Rgb(red / 4, green / 4, blue / 4),
        Color::Indexed(index) => Color::Indexed(index),
        Color::Reset => Color::Reset,
    }
}

fn queue_lines(state: &TuiState, region: Rect) -> Vec<Line<'static>> {
    let queue = state.queue_preview();
    vec![
        queue_lane(state, "Next", &queue.next, region.width),
        queue_lane(state, "Suspended", &queue.suspended, region.width),
        queue_lane(state, "Backlog", &queue.backlog, region.width),
    ]
}

fn completion_lines(state: &TuiState, region: Rect) -> Vec<Line<'static>> {
    let Some(menu) = state.completion_menu() else {
        return Vec::new();
    };
    menu.items()
        .iter()
        .take(usize::from(region.height))
        .enumerate()
        .map(|(index, item)| {
            let selected = index == menu.selected_index();
            let base_style = if selected {
                semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
            } else {
                semantic_style(state, SemanticColor::Muted)
            };
            let marker = if selected { ">" } else { " " };
            let label_text = format!("{marker} ");
            let content_width =
                usize::from(region.width).saturating_sub(label_text.chars().count());
            let detail_width = if item.detail().is_some_and(|detail| !detail.is_empty()) {
                content_width / 2
            } else {
                0
            };
            let detail_text = item
                .detail()
                .filter(|detail| !detail.is_empty() && detail_width > 2)
                .map(|detail| format!("  {}", truncate_chars(detail, detail_width - 2)))
                .unwrap_or_default();
            let value_width = content_width.saturating_sub(detail_text.chars().count());
            Line::from(vec![
                Span::styled(label_text, base_style),
                Span::styled(truncate_chars(item.value(), value_width), base_style),
                Span::styled(detail_text, semantic_style(state, SemanticColor::Muted)),
            ])
        })
        .collect()
}

fn queue_lane(
    state: &TuiState,
    label: &'static str,
    items: &[super::state::QueuePreviewItem],
    region_width: u16,
) -> Line<'static> {
    let label_text = format!("{label:<10} ");
    let content_width = usize::from(region_width).saturating_sub(label_text.chars().count());
    let content = if items.is_empty() {
        "--".to_owned()
    } else {
        items
            .iter()
            .enumerate()
            .map(|(index, item)| format!("{}. {}", index + 1, item.text))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    Line::from(vec![
        Span::styled(
            label_text,
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate_chars(&content, content_width),
            semantic_style(state, SemanticColor::Muted),
        ),
    ])
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars().take(max_chars - 3).collect::<String>() + "..."
}

fn inline_code_line(state: &TuiState, text: &str, base_style: Style) -> Line<'static> {
    Line::from(inline_code_spans(state, text, base_style))
}

fn inline_code_wrapped_lines(
    state: &TuiState,
    text: &str,
    base_style: Style,
    region_width: u16,
) -> Vec<Line<'static>> {
    wrap_styled_parts(inline_code_parts(state, text, base_style), region_width)
}

fn inline_code_spans(state: &TuiState, text: &str, base_style: Style) -> Vec<Span<'static>> {
    inline_code_parts(state, text, base_style)
        .into_iter()
        .map(|part| Span::styled(part.text, part.style))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StyledTextPart {
    text: String,
    style: Style,
    atomic: bool,
}

fn inline_code_parts(state: &TuiState, text: &str, base_style: Style) -> Vec<StyledTextPart> {
    let mut spans = Vec::new();
    let mut remainder = text;
    loop {
        let Some(start) = remainder.find('`') else {
            if !remainder.is_empty() {
                spans.push(StyledTextPart {
                    text: remainder.to_owned(),
                    style: base_style,
                    atomic: false,
                });
            }
            return spans;
        };
        let before = &remainder[..start];
        if !before.is_empty() {
            spans.push(StyledTextPart {
                text: before.to_owned(),
                style: base_style,
                atomic: false,
            });
        }
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('`') else {
            spans.push(StyledTextPart {
                text: remainder[start..].to_owned(),
                style: base_style,
                atomic: false,
            });
            return spans;
        };
        let code = &after_start[..end];
        spans.push(StyledTextPart {
            text: format!(" {code} "),
            style: inline_code_style(state, base_style),
            atomic: true,
        });
        remainder = &after_start[end + 1..];
    }
}

fn wrap_styled_parts(parts: Vec<StyledTextPart>, region_width: u16) -> Vec<Line<'static>> {
    wrap_styled_parts_with_policy(parts, region_width, false)
}

fn wrap_styled_parts_preserving_leading_whitespace(
    parts: Vec<StyledTextPart>,
    region_width: u16,
) -> Vec<Line<'static>> {
    wrap_styled_parts_with_policy(parts, region_width, true)
}

fn wrap_styled_parts_with_policy(
    parts: Vec<StyledTextPart>,
    region_width: u16,
    preserve_leading_whitespace: bool,
) -> Vec<Line<'static>> {
    let max_width = usize::from(region_width).max(1);
    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut current_width = 0;

    for part in parts {
        if part.atomic {
            push_atomic_part(
                part,
                max_width,
                &mut current,
                &mut current_width,
                &mut lines,
            );
        } else {
            push_wrappable_part(
                part,
                max_width,
                &mut current,
                &mut current_width,
                &mut lines,
                preserve_leading_whitespace,
            );
        }
    }

    lines.push(Line::from(current));
    lines
}

fn push_atomic_part(
    part: StyledTextPart,
    max_width: usize,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    lines: &mut Vec<Line<'static>>,
) {
    let width = UnicodeWidthStr::width(part.text.as_str());
    if *current_width > 0 && *current_width + width > max_width {
        lines.push(Line::from(std::mem::take(current)));
        *current_width = 0;
    }
    *current_width += width;
    current.push(Span::styled(part.text, part.style));
}

fn push_wrappable_part(
    part: StyledTextPart,
    max_width: usize,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    lines: &mut Vec<Line<'static>>,
    preserve_leading_whitespace: bool,
) {
    for token in wrap_tokens(&part.text) {
        push_wrappable_token(
            token,
            part.style,
            max_width,
            current,
            current_width,
            lines,
            preserve_leading_whitespace,
        );
    }
}

fn wrap_tokens(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut tokens = Vec::new();
    let mut token_start = 0;
    let mut previous_was_whitespace: Option<bool> = None;
    for (index, character) in text.char_indices() {
        let is_whitespace = character.is_whitespace();
        if let Some(previous) = previous_was_whitespace
            && previous != is_whitespace
        {
            tokens.push(&text[token_start..index]);
            token_start = index;
        }
        previous_was_whitespace = Some(is_whitespace);
    }
    tokens.push(&text[token_start..]);
    tokens
}

fn push_wrappable_token(
    token: &str,
    style: Style,
    max_width: usize,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    lines: &mut Vec<Line<'static>>,
    preserve_leading_whitespace: bool,
) {
    if token.chars().all(char::is_whitespace) {
        push_whitespace_token(
            token,
            style,
            max_width,
            current,
            current_width,
            lines,
            preserve_leading_whitespace,
        );
        return;
    }

    let token_width = UnicodeWidthStr::width(token);
    if token_width <= max_width {
        if *current_width > 0 && *current_width + token_width > max_width {
            lines.push(Line::from(std::mem::take(current)));
            *current_width = 0;
        }
        current.push(Span::styled(token.to_owned(), style));
        *current_width += token_width;
        return;
    }

    push_long_token_by_char(token, style, max_width, current, current_width, lines);
}

fn push_whitespace_token(
    token: &str,
    style: Style,
    max_width: usize,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    lines: &mut Vec<Line<'static>>,
    preserve_leading_whitespace: bool,
) {
    if *current_width == 0 && !preserve_leading_whitespace {
        return;
    }

    let token_width = UnicodeWidthStr::width(token);
    if token_width > max_width {
        push_long_token_by_char(token, style, max_width, current, current_width, lines);
        return;
    }

    if *current_width > 0 && *current_width + token_width > max_width {
        lines.push(Line::from(std::mem::take(current)));
        *current_width = 0;
        return;
    }

    current.push(Span::styled(token.to_owned(), style));
    *current_width += token_width;
}

fn push_long_token_by_char(
    token: &str,
    style: Style,
    max_width: usize,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    lines: &mut Vec<Line<'static>>,
) {
    let mut chunk = String::new();
    for character in token.chars() {
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if *current_width > 0 && *current_width + width > max_width {
            if !chunk.is_empty() {
                current.push(Span::styled(std::mem::take(&mut chunk), style));
            }
            lines.push(Line::from(std::mem::take(current)));
            *current_width = 0;
        }
        chunk.push(character);
        *current_width += width;
    }
    if !chunk.is_empty() {
        current.push(Span::styled(chunk, style));
    }
}

fn inline_code_style(state: &TuiState, base_style: Style) -> Style {
    let foreground = state.theme().color(SemanticColor::Focus);
    let mut style = base_style.add_modifier(Modifier::BOLD);
    if let Some(foreground) = foreground {
        style = style.fg(foreground);
    }
    style
}

fn timeline_body_style(state: &TuiState, item: &TimelineItem, line: &str) -> Style {
    if matches!(item, TimelineItem::Diagnostic { .. }) {
        return semantic_style(state, SemanticColor::Error);
    }
    if line.starts_with('+') {
        return semantic_style(state, SemanticColor::DiffAdd);
    }
    if line.starts_with('-') {
        return semantic_style(state, SemanticColor::DiffDelete);
    }
    if line.starts_with("  stdout:") || line.starts_with("    ") {
        return semantic_style(state, SemanticColor::Muted);
    }
    if line.starts_with("  stderr:") || line.starts_with("  exit ") {
        return semantic_style(state, SemanticColor::Error);
    }
    semantic_style(state, SemanticColor::Assistant)
}

fn semantic_style(state: &TuiState, slot: SemanticColor) -> Style {
    state
        .theme()
        .color(slot)
        .map_or_else(Style::default, |color| Style::default().fg(color))
}
