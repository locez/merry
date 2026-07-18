use super::{
    highlight::highlight_code_to_lines,
    layout::{BottomPaneHeights, cockpit_layout},
    markdown::markdown_lines,
    overlay_render,
    panels::{
        DirectoryEntryKind, DirectoryEntryView, FocusPanelBody, FocusPanelTone, FocusPanelView,
        focus_panel_view,
    },
    plan_render,
    state::{PatchChangeView, PatchLineView, TimelineItem, TuiState},
    theme::{SemanticColor, dim_color},
};
use merry_core::QueuedInputLane;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const QUEUE_PREVIEW_HEIGHT: u16 = 5;
const MAX_COMPLETION_PREVIEW_HEIGHT: u16 = 6;
// Keep prefix eviction below the viewport start when Paragraph scroll exceeds u16.
const MAX_TIMELINE_LOGICAL_LINE_GRAPHEMES: usize = 32_768;
const MAX_INPUT_VISIBLE_ROWS: usize = 5;
pub(crate) const STATUS_HEIGHT: u16 = 1;
const HEADER_HEIGHT: u16 = 1;
const MIN_TIMELINE_HEIGHT: u16 = 3;
const MIN_INPUT_HEIGHT: u16 = 3;

#[allow(dead_code)]
pub(crate) fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let pane_heights = pane_heights(state, frame.area().height);
    let rects = cockpit_layout(
        frame.area(),
        BottomPaneHeights {
            queue: pane_heights.queue,
            completion: pane_heights.completion,
            input: pane_heights.input,
            status: STATUS_HEIGHT,
        },
        state.is_artifact_reviewing(),
        state.plan().is_open(),
        state.plan().is_focused(),
    );

    render_header(frame, state, rects.header);
    if rects.timeline.width > 0 && rects.timeline.height > 0 {
        render_timeline_pane(frame, state, rects.timeline);
    }
    if let Some(region) = rects.detail {
        let view = focus_panel_view(state);
        render_focus_pane(frame, state, region, &view);
    }
    if let Some(region) = rects.plan {
        plan_render::render_plan(frame, state, region);
    }

    if let Some(queue_region) = rects.queue {
        frame.render_widget(
            Paragraph::new(queue_lines(state, queue_region)).block(
                Block::default()
                    .title("queue")
                    .border_style(semantic_style(state, SemanticColor::Muted)),
            ),
            queue_region,
        );
    }
    if pane_heights.completion > 0 {
        frame.render_widget(
            Paragraph::new(completion_lines(state, rects.completion)),
            rects.completion,
        );
    }
    render_input(frame, state, rects.input, pane_heights.input);
    render_status(frame, state, rects.status);
    overlay_render::render_overlay(frame, state);
}

pub(crate) fn pane_heights_for_area(state: &TuiState, area: Rect) -> PaneHeights {
    pane_heights(state, area.height)
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

fn render_header(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    let [workspace, model, usage] = state.header_status_parts(region.width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "merry",
                semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(workspace, semantic_style(state, SemanticColor::Command)),
            Span::styled("  ", Style::default()),
            Span::styled(
                model,
                semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(
                usage,
                semantic_style(state, SemanticColor::Assistant).add_modifier(Modifier::DIM),
            ),
        ]))
        .style(header_background_style(state)),
        region,
    );
}

fn header_background_style(state: &TuiState) -> Style {
    state
        .theme()
        .color(SemanticColor::Status)
        .map(dim_color)
        .map_or_else(Style::default, |color| Style::default().bg(color))
}

fn render_timeline_pane(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    let timeline = timeline_lines_compact(state, region);
    let viewport = timeline_viewport(state, timeline, region);
    frame.render_widget(
        Paragraph::new(viewport.lines)
            .wrap(Wrap { trim: false })
            .scroll((viewport.scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_type(BorderType::Plain)
                    .border_style(semantic_style(state, SemanticColor::Muted))
                    .title_style(semantic_style(state, SemanticColor::Muted)),
            ),
        region,
    );
}

fn render_status(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    let interaction_style = if state.is_active_run() {
        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
    } else {
        semantic_style(state, SemanticColor::Muted)
    };
    frame.render_widget(
        Paragraph::new(state.interaction_status_text()).style(interaction_style),
        region,
    );
}

fn render_input(frame: &mut Frame<'_>, state: &TuiState, region: Rect, input_height: u16) {
    let input_inner = bordered_inner(region);
    let max_input_rows = usize::from(input_height.saturating_sub(2)).max(1);
    let input_viewport = state.input_viewport_rows(usize::from(input_inner.width), max_input_rows);
    let input_lines = styled_input_lines(
        state,
        &input_viewport.text,
        &input_viewport.image_placeholders,
    );

    frame.render_widget(
        Paragraph::new(input_lines)
            .style(semantic_style(state, SemanticColor::Assistant))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .title(Line::from(Span::styled(
                        " M ",
                        semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD),
                    )))
                    .border_style(semantic_style(state, SemanticColor::Focus)),
            ),
        region,
    );
    set_input_cursor(
        frame,
        input_inner,
        input_viewport.cursor_column,
        input_viewport.cursor_row,
    );
}

fn styled_input_lines(
    state: &TuiState,
    text: &str,
    image_placeholders: &[String],
) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| styled_input_line(state, line, image_placeholders))
        .collect()
}

fn styled_input_line(state: &TuiState, line: &str, image_placeholders: &[String]) -> Line<'static> {
    let normal = semantic_style(state, SemanticColor::Assistant);
    let image = semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    let mut cursor = 0;

    while cursor < line.len() {
        let Some((start, placeholder)) = image_placeholders
            .iter()
            .filter_map(|placeholder| {
                line[cursor..]
                    .find(placeholder)
                    .map(|offset| (cursor + offset, placeholder.as_str()))
            })
            .min_by_key(|(start, _)| *start)
        else {
            spans.push(Span::styled(line[cursor..].to_owned(), normal));
            break;
        };

        if start > cursor {
            spans.push(Span::styled(line[cursor..start].to_owned(), normal));
        }
        spans.push(Span::styled(placeholder.to_owned(), image));
        cursor = start + placeholder.len();
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), normal));
    }
    Line::from(spans)
}

fn render_focus_pane(frame: &mut Frame<'_>, state: &TuiState, region: Rect, view: &FocusPanelView) {
    let inner = bordered_inner(region);
    let lines = focus_lines(state, view, inner);
    let color = match view.tone {
        FocusPanelTone::Default => SemanticColor::ToolKeyword,
        FocusPanelTone::Error => SemanticColor::Error,
    };
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .title(view.title.strip_prefix("FOCUS ").unwrap_or(&view.title))
                .border_style(semantic_style(state, color))
                .title_style(semantic_style(state, color).add_modifier(Modifier::BOLD)),
        ),
        region,
    );
}

fn focus_lines(state: &TuiState, view: &FocusPanelView, region: Rect) -> Vec<Line<'static>> {
    let mut lines = match &view.body {
        FocusPanelBody::Empty => vec![Line::from(Span::styled(
            "No focus item",
            semantic_style(state, SemanticColor::Muted),
        ))],
        FocusPanelBody::Patch { changes } => patch_lines(state, changes),
        FocusPanelBody::Source { path, content } => {
            if let Some(lang) = source_lang_from_path(path) {
                highlight_code_to_lines(content, lang, state.code_theme())
            } else {
                content
                    .lines()
                    .flat_map(|line| focus_text_wrapped_lines(state, line, region.width))
                    .collect()
            }
        }
        FocusPanelBody::DirectoryListing { entries } => {
            directory_listing_lines(state, entries, region.width)
        }
        FocusPanelBody::CommandOutput { lines } => lines
            .iter()
            .flat_map(|line| focus_text_wrapped_lines(state, line, region.width))
            .collect(),
        FocusPanelBody::Text { lines } => {
            if let Some(lang) = source_lang_from_focus_title(&view.title) {
                highlight_code_to_lines(&lines.join("\n"), lang, state.code_theme())
            } else {
                lines
                    .iter()
                    .flat_map(|line| focus_text_wrapped_lines(state, line, region.width))
                    .collect()
            }
        }
    };

    let max_lines = usize::from(region.height).max(1);
    if lines.len() > max_lines {
        let offset = state
            .focus_scroll_offset()
            .min(lines.len().saturating_sub(max_lines));
        if offset > 0 {
            lines = lines.into_iter().skip(offset).collect();
        }
        lines.truncate(max_lines.saturating_sub(1));
        lines.push(Line::from(Span::styled(
            "...",
            semantic_style(state, SemanticColor::Muted),
        )));
    }
    lines
}

fn source_lang_from_focus_title(title: &str) -> Option<&str> {
    let path = title.strip_prefix("FOCUS Read ")?;
    source_lang_from_path(path)
}

fn source_lang_from_path(path: &str) -> Option<&str> {
    path.rsplit_once('.').map(|(_, extension)| extension)
}

fn directory_listing_lines(
    state: &TuiState,
    entries: &[DirectoryEntryView],
    region_width: u16,
) -> Vec<Line<'static>> {
    entries
        .iter()
        .flat_map(|entry| directory_entry_lines(state, entry, region_width))
        .collect()
}

fn directory_entry_lines(
    state: &TuiState,
    entry: &DirectoryEntryView,
    region_width: u16,
) -> Vec<Line<'static>> {
    let style = directory_entry_style(state, entry);
    wrap_styled_parts(
        vec![StyledTextPart {
            text: entry.path.clone(),
            style,
            atomic: false,
        }],
        region_width,
    )
}

fn directory_entry_style(state: &TuiState, entry: &DirectoryEntryView) -> Style {
    if entry.path.starts_with('.') {
        return semantic_style(state, SemanticColor::Muted);
    }
    match entry.kind {
        DirectoryEntryKind::Directory => {
            semantic_style(state, SemanticColor::Command).add_modifier(Modifier::BOLD)
        }
        DirectoryEntryKind::File => semantic_style(state, SemanticColor::Assistant),
    }
}

fn focus_text_wrapped_lines(state: &TuiState, line: &str, region_width: u16) -> Vec<Line<'static>> {
    let base_style = semantic_style(state, SemanticColor::Assistant);
    let Some((label, value)) = line.split_once(": ") else {
        return inline_code_wrapped_lines(state, line, base_style, region_width);
    };
    if !matches!(label.trim(), "stdout" | "stderr") {
        return inline_code_wrapped_lines(state, line, base_style, region_width);
    }

    let label_text = format!("{label}: ");
    let label_width = UnicodeWidthStr::width(label_text.as_str());
    let content_width = usize::from(region_width).saturating_sub(label_width).max(1);
    let mut content_lines = inline_code_wrapped_lines(
        state,
        value,
        semantic_style(state, SemanticColor::Muted),
        u16::try_from(content_width).unwrap_or(u16::MAX),
    );
    if content_lines.is_empty() {
        content_lines.push(Line::from(""));
    }

    let mut result = Vec::with_capacity(content_lines.len());
    for (index, content_line) in content_lines.into_iter().enumerate() {
        let prefix = if index == 0 {
            label_text.clone()
        } else {
            " ".repeat(label_width)
        };
        let mut spans = vec![Span::styled(
            prefix,
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        )];
        spans.extend(content_line.spans);
        result.push(Line::from(spans));
    }
    result
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneHeights {
    pub(crate) queue: u16,
    pub(crate) completion: u16,
    pub(crate) input: u16,
}

fn pane_heights(state: &TuiState, total_height: u16) -> PaneHeights {
    let desired_queue = desired_queue_preview_height(state);
    let desired_completion = desired_completion_preview_height(state);
    let desired_input = desired_input_region_height(state, MAX_INPUT_VISIBLE_ROWS);
    let desired_bottom = desired_queue
        .saturating_add(desired_completion)
        .saturating_add(desired_input)
        .saturating_add(STATUS_HEIGHT)
        .saturating_add(HEADER_HEIGHT);

    if total_height >= desired_bottom.saturating_add(MIN_TIMELINE_HEIGHT) {
        return PaneHeights {
            queue: desired_queue,
            completion: desired_completion,
            input: desired_input,
        };
    }

    let reserved = HEADER_HEIGHT
        .saturating_add(STATUS_HEIGHT)
        .saturating_add(MIN_TIMELINE_HEIGHT);
    let mut remaining = total_height.saturating_sub(reserved);
    let input = desired_input
        .min(remaining)
        .max(MIN_INPUT_HEIGHT.min(remaining));
    remaining = remaining.saturating_sub(input);

    let completion = desired_completion.min(remaining);
    remaining = remaining.saturating_sub(completion);

    let queue = desired_queue.min(remaining);
    PaneHeights {
        queue,
        completion,
        input,
    }
}

fn desired_queue_preview_height(state: &TuiState) -> u16 {
    if state.has_queue_preview_items() {
        QUEUE_PREVIEW_HEIGHT
    } else {
        0
    }
}

fn desired_completion_preview_height(state: &TuiState) -> u16 {
    state
        .completion_menu()
        .map(|menu| {
            u16::try_from(menu.items().len())
                .unwrap_or(MAX_COMPLETION_PREVIEW_HEIGHT)
                .min(MAX_COMPLETION_PREVIEW_HEIGHT)
        })
        .unwrap_or(0)
}

fn desired_input_region_height(state: &TuiState, max_rows: usize) -> u16 {
    let visible_rows = state.input_visible_rows(max_rows);
    u16::try_from(visible_rows)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
}

struct TimelineLines {
    lines: Vec<Line<'static>>,
    review_logical_start: Option<usize>,
}

struct TimelineViewport {
    lines: Vec<Line<'static>>,
    scroll: u16,
}

fn timeline_lines_compact(state: &TuiState, region: Rect) -> TimelineLines {
    let mut lines = Vec::new();
    let mut review_logical_start = None;
    for (index, item) in state.timeline().iter().enumerate() {
        if state.timeline_review_user_index() == Some(index) {
            review_logical_start = Some(lines.len());
        }
        let item_lines = match item {
            TimelineItem::User { text, lane } => user_lines(state, text, *lane),
            TimelineItem::Assistant { text } => assistant_lines(state, text, region.width),
            TimelineItem::Muted { title, detail } => muted_lines(state, title, detail),
            TimelineItem::LocalCommand { title, body } => {
                local_command_lines(state, title, body, region.width)
            }
            TimelineItem::Expanded { title, body }
            | TimelineItem::ExpandedDetail { title, body, .. } => {
                expanded_timeline_lines(state, title, body, region.width)
            }
            TimelineItem::Diagnostic { title, body } => {
                diagnostic_lines(state, title, body, region.width)
            }
            TimelineItem::Patch { changes } => compact_patch_lines(state, changes),
        };
        let item_lines = item_lines
            .into_iter()
            .flat_map(split_oversized_timeline_line)
            .collect();
        lines.extend(spaced_timeline_item(
            item_lines,
            index + 1 < state.timeline().len(),
        ));
    }
    TimelineLines {
        lines,
        review_logical_start,
    }
}

fn timeline_viewport(state: &TuiState, timeline: TimelineLines, region: Rect) -> TimelineViewport {
    let mut scroll = timeline_scroll_start(state, &timeline, region);
    let mut lines = timeline.lines;
    let max_scroll = usize::from(u16::MAX);
    if scroll > max_scroll {
        // Paragraph scroll is u16; remove complete prefix lines in one linear pass.
        let rows_to_drop = scroll - max_scroll;
        let mut dropped_lines = 0;
        let mut dropped_rows = 0;
        for line in &lines {
            if dropped_rows >= rows_to_drop {
                break;
            }
            dropped_rows += wrapped_line_count(std::slice::from_ref(line), region.width).max(1);
            dropped_lines += 1;
        }
        if dropped_lines > 0 {
            lines.drain(..dropped_lines);
            scroll = scroll.saturating_sub(dropped_rows);
        }
    }

    TimelineViewport {
        lines,
        scroll: u16::try_from(scroll).unwrap_or(u16::MAX),
    }
}

fn timeline_scroll_start(state: &TuiState, timeline: &TimelineLines, region: Rect) -> usize {
    if let Some(logical_start) = timeline.review_logical_start {
        wrapped_line_count(&timeline.lines[..logical_start], region.width)
    } else {
        let total = wrapped_line_count(&timeline.lines, region.width);
        let visible = usize::from(region.height.saturating_sub(1));
        total
            .saturating_sub(visible)
            .saturating_sub(state.timeline_scroll_offset())
    }
}

fn wrapped_line_count(lines: &[Line<'static>], width: u16) -> usize {
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width)
}

fn split_oversized_timeline_line(line: Line<'static>) -> Vec<Line<'static>> {
    let byte_len = line.spans.iter().fold(0_usize, |total, span| {
        total.saturating_add(span.content.len())
    });
    if byte_len <= MAX_TIMELINE_LOGICAL_LINE_GRAPHEMES {
        return vec![line];
    }

    let Line {
        style,
        alignment,
        spans,
    } = line;
    let mut lines = Vec::new();
    let mut current_spans = Vec::new();
    let mut current_graphemes = 0;

    for span in spans {
        let mut chunk = String::new();
        for grapheme in span.styled_graphemes(Style::default()) {
            if current_graphemes == MAX_TIMELINE_LOGICAL_LINE_GRAPHEMES {
                if !chunk.is_empty() {
                    current_spans.push(Span::styled(std::mem::take(&mut chunk), span.style));
                }
                lines.push(Line {
                    style,
                    alignment,
                    spans: std::mem::take(&mut current_spans),
                });
                current_graphemes = 0;
            }
            chunk.push_str(grapheme.symbol);
            current_graphemes += 1;
        }
        if !chunk.is_empty() {
            current_spans.push(Span::styled(chunk, span.style));
        }
    }

    if !current_spans.is_empty() || lines.is_empty() {
        lines.push(Line {
            style,
            alignment,
            spans: current_spans,
        });
    }
    lines
}

fn spaced_timeline_item(mut lines: Vec<Line<'static>>, has_next_item: bool) -> Vec<Line<'static>> {
    if has_next_item && !lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn assistant_lines(state: &TuiState, text: &str, region_width: u16) -> Vec<Line<'static>> {
    let mut lines = markdown_lines(state, text, region_width);
    lines.push(assistant_separator_line(state, region_width));
    lines
}

fn assistant_separator_line(state: &TuiState, region_width: u16) -> Line<'static> {
    let width = usize::from(region_width).max(1);
    Line::from(Span::styled(
        "-".repeat(width),
        semantic_style(state, SemanticColor::Muted),
    ))
}

fn muted_lines(state: &TuiState, title: &str, detail: &str) -> Vec<Line<'static>> {
    if let Some(line) = tool_title_line_from_parts(state, title, detail) {
        return vec![line];
    }

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

fn compact_patch_lines(state: &TuiState, changes: &[PatchChangeView]) -> Vec<Line<'static>> {
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
    }
    lines
}

fn expanded_title_line(state: &TuiState, title: &str) -> Line<'static> {
    if let Some(line) = tool_title_line(state, title) {
        return line;
    }

    Line::from(Span::styled(
        title.to_owned(),
        semantic_style(state, SemanticColor::Focus),
    ))
}

fn local_command_lines(
    state: &TuiState,
    title: &str,
    body: &str,
    region_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        title.to_owned(),
        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
    ))];
    let body_width = region_width.saturating_sub(2).max(1);
    lines.extend(
        markdown_lines(state, body, body_width)
            .into_iter()
            .map(|line| {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(line.spans);
                Line::from(spans)
            }),
    );
    lines
}

fn expanded_timeline_lines(
    state: &TuiState,
    title: &str,
    body: &str,
    region_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![expanded_title_line(state, title)];
    if state.is_artifact_reviewing() || tool_title_line(state, title).is_none() {
        return lines;
    }

    let body_width = usize::from(region_width).saturating_sub(2).max(4);
    for line in body.lines().filter(|line| !line.trim().is_empty()).take(2) {
        let clean = line
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        let clean = clean.trim();
        if clean.is_empty() {
            continue;
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", truncate_chars(clean, body_width)),
            semantic_style(state, SemanticColor::Muted),
        )));
    }
    lines
}

fn diagnostic_lines(
    state: &TuiState,
    title: &str,
    body: &str,
    region_width: u16,
) -> Vec<Line<'static>> {
    let reason = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("run failed");
    wrap_styled_parts(
        vec![
            StyledTextPart {
                text: "! Error  ".to_owned(),
                style: semantic_style(state, SemanticColor::Error).add_modifier(Modifier::BOLD),
                atomic: true,
            },
            StyledTextPart {
                text: title.to_owned(),
                style: semantic_style(state, SemanticColor::Error).add_modifier(Modifier::BOLD),
                atomic: true,
            },
            StyledTextPart {
                text: format!(": {reason}"),
                style: semantic_style(state, SemanticColor::Assistant),
                atomic: false,
            },
        ],
        region_width,
    )
}

fn tool_title_line_from_parts(
    state: &TuiState,
    title: &str,
    detail: &str,
) -> Option<Line<'static>> {
    if detail.is_empty() {
        return tool_title_line(state, title);
    }

    if title == "Ran" {
        return Some(ran_title_line(state, detail));
    }
    tool_title_keyword(title).map(|keyword| tool_keyword_title_line(state, keyword, detail))
}

fn tool_title_line(state: &TuiState, title: &str) -> Option<Line<'static>> {
    if let Some(command) = title
        .strip_prefix("Ran ")
        .or_else(|| title.strip_prefix("Ran: "))
    {
        return Some(ran_title_line(state, command));
    }

    for keyword in TOOL_TITLE_KEYWORDS {
        if let Some(detail) = strip_tool_title_detail(title, keyword) {
            return Some(tool_keyword_title_line(state, keyword, detail));
        }
    }

    None
}

fn strip_tool_title_detail<'a>(title: &'a str, keyword: &str) -> Option<&'a str> {
    if title == keyword {
        return Some("");
    }
    title.strip_prefix(keyword)?.strip_prefix(' ')
}

const TOOL_TITLE_KEYWORDS: &[&str] = &[
    "Read",
    "Listed",
    "Searched",
    "MCP",
    "Permission",
    "Patch",
    "Tool",
];

fn tool_title_keyword(title: &str) -> Option<&'static str> {
    TOOL_TITLE_KEYWORDS
        .iter()
        .copied()
        .find(|keyword| title == *keyword)
}

fn tool_keyword_title_line(state: &TuiState, keyword: &str, detail: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        keyword.to_owned(),
        semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD),
    )];
    if !detail.is_empty() {
        spans.push(Span::styled(
            " ".to_owned(),
            semantic_style(state, SemanticColor::Muted),
        ));
        spans.extend(inline_code_spans(
            state,
            detail,
            semantic_style(state, SemanticColor::Assistant),
        ));
    }
    Line::from(spans)
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
    let lane_label = match lane {
        QueuedInputLane::Next => None,
        QueuedInputLane::Suspended => Some(("suspended", SemanticColor::Warning)),
        QueuedInputLane::Backlog => Some(("backlog", SemanticColor::Muted)),
    };
    let mut lines = Vec::new();
    for (index, segment) in text.split('\n').enumerate() {
        let mut spans = vec![Span::styled(
            "▌ ",
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        )];
        if index == 0
            && let Some((label, color)) = lane_label
        {
            spans.push(Span::styled(
                format!("{label}  "),
                semantic_style(state, color).add_modifier(Modifier::BOLD),
            ));
        }
        spans.extend(inline_code_spans(
            state,
            segment,
            semantic_style(state, SemanticColor::Assistant),
        ));
        lines.push(Line::from(spans));
    }
    lines
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
pub(crate) struct StyledTextPart {
    pub(crate) text: String,
    pub(crate) style: Style,
    pub(crate) atomic: bool,
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

pub(crate) fn wrap_styled_parts(
    parts: Vec<StyledTextPart>,
    region_width: u16,
) -> Vec<Line<'static>> {
    wrap_styled_parts_with_policy(parts, region_width, false)
}

pub(crate) fn wrap_styled_parts_preserving_leading_whitespace(
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

fn semantic_style(state: &TuiState, slot: SemanticColor) -> Style {
    state
        .theme()
        .color(slot)
        .map_or_else(Style::default, |color| Style::default().fg(color))
}
