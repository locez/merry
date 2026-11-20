//! Timeline content, wrapping, and bounded viewport projection.

use super::activity::paint_activity;
pub(super) use super::command_rows::{
    command_fold_hit, command_lines, expanded_tool_lines, read_tool_lines, tool_running_lines,
};
use super::command_rows::{command_fold_marks, command_title_line, paint_command_folds};
use super::comment_marks::{comment_overlay, paint_comment_overlay};
use super::sticky::{paint_sticky_header, split_timeline_content, timeline_scroll_split};
use super::task_dock::{adjust_body_scroll, render_task_dock, split_dock_region};
use crate::tui::{
    copy_controls::{CopyContent, CopyTarget, copy_header},
    markdown::{RenderedMarkdown, markdown_lines},
    motion::{self, MotionClock},
    state::{PatchChangeView, PatchLineKind, PatchLineView, PatchOperationView, TuiState},
    text_wrap::{
        StyledTextPart, inline_code_spans, semantic_style, truncate_chars, wrap_styled_parts,
    },
    theme::SemanticColor,
    transcript::{SelectionPolicy, TranscriptRow},
};
use merry_core::QueuedInputLane;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub(super) use super::timeline_layout::prepare_timeline_viewport;
use super::timeline_layout::{timeline_layout, timeline_padding, timeline_scroll_start};

pub(crate) fn render_timeline_pane(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    let block = Block::default()
        .borders(Borders::NONE)
        .padding(timeline_padding(region));
    let content_region = block.inner(region);
    frame.render_widget(block, region);
    if content_region.is_empty() {
        return;
    }
    let timeline = timeline_layout(state, content_region);
    let scroll = timeline_scroll_start(state, &timeline, content_region);
    let split = timeline_scroll_split(state, &timeline, content_region.height, scroll);
    let (header, after_header) = split_timeline_content(content_region, split.header_rows);
    paint_sticky_header(frame, header, &split);
    let (dock_rect, body) = split_dock_region(state, after_header);
    render_task_dock(frame, state, dock_rect);
    let body_scroll = adjust_body_scroll(
        split.body_scroll,
        after_header.height,
        body.height,
        timeline.total_rows(),
    );
    if body.is_empty() {
        let comments = comment_overlay(state, &timeline, content_region.width);
        paint_comment_overlay(frame, state, &comments, &split, header, body);
        return;
    }
    if let Some(selection) = state.text_selection()
        && selection.area() == body
    {
        selection.render(frame.buffer_mut());
        return;
    }
    let comments = comment_overlay(state, &timeline, content_region.width);
    let folds = command_fold_marks(state, &timeline, body, body_scroll);
    let viewport = timeline.viewport_slice(body_scroll, body.height);
    frame.render_widget(
        Paragraph::new(viewport.lines)
            .wrap(Wrap { trim: false })
            .scroll((viewport.scroll, 0)),
        body,
    );
    paint_activity(frame, state, body, &timeline, body_scroll);
    paint_command_folds(frame, state, body, &folds);
    paint_comment_overlay(frame, state, &comments, &split, header, body);
}

pub(super) fn assistant_lines(
    state: &TuiState,
    text: &str,
    item_index: usize,
    region_width: u16,
) -> RenderedMarkdown {
    let mut rendered = markdown_lines(state, text, region_width);
    if let Some((header, column, width)) = copy_header(state, region_width) {
        rendered
            .rows
            .insert(0, TranscriptRow::new(header, SelectionPolicy::Skip));
        for target in &mut rendered.copy_targets {
            target.line_index += 1;
        }
        rendered.copy_targets.insert(
            0,
            CopyTarget::new(0, column, width, CopyContent::AssistantMessage(item_index)),
        );
    }
    rendered
}

pub(super) fn muted_lines(state: &TuiState, title: &str, detail: &str) -> Vec<Line<'static>> {
    if let Some(line) = tool_title_line_from_parts(state, title, detail) {
        return vec![line];
    }

    if let Some(label) = title.strip_prefix("⊘ ") {
        let mut spans = vec![
            Span::styled("⊘ ", semantic_style(state, SemanticColor::Muted)),
            Span::styled(
                label.to_owned(),
                semantic_style(state, SemanticColor::TextSecondary),
            ),
        ];
        if !detail.is_empty() {
            spans.push(Span::styled(
                format!(" · {detail}"),
                semantic_style(state, SemanticColor::Muted),
            ));
        }
        return vec![Line::from(spans)];
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

const MAX_EXPANDED_PATCH_LINES: usize = 60;

pub(super) fn patch_lines(
    state: &TuiState,
    changes: &[PatchChangeView],
    expanded: bool,
    region_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for change in changes {
        let title = patch_change_title_line(state, change);
        if !expanded {
            lines.push(title);
            continue;
        }
        lines.push(title);
        lines.push(Line::from(Span::styled(
            patch_change_detail(change),
            semantic_style(state, SemanticColor::Muted),
        )));
        lines.extend(patch_diff_lines(state, &change.lines, region_width));
    }
    lines
}

/// Titles a patch change with the verb that matches its file operation and
/// soft-luminance green/red line-count stats.
fn patch_change_title_line(state: &TuiState, change: &PatchChangeView) -> Line<'static> {
    let header_style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let muted_style = semantic_style(state, SemanticColor::Muted);
    let add_style = semantic_style(state, SemanticColor::DiffAdd).add_modifier(Modifier::BOLD);
    let del_style = semantic_style(state, SemanticColor::DiffDelete).add_modifier(Modifier::BOLD);

    let verb = match change.operation {
        PatchOperationView::Add => "Created",
        PatchOperationView::Update => "Edited",
        PatchOperationView::Delete => "Deleted",
    };
    let mut spans = vec![
        Span::styled(format!("{verb} {} ", change.path), header_style),
        Span::styled("(", muted_style),
    ];
    match change.operation {
        PatchOperationView::Add => {
            spans.push(Span::styled(format!("+{}", change.added), add_style));
        }
        PatchOperationView::Update => {
            spans.push(Span::styled(format!("+{}", change.added), add_style));
            spans.push(Span::styled(" ", muted_style));
            spans.push(Span::styled(format!("-{}", change.removed), del_style));
        }
        PatchOperationView::Delete => {
            spans.push(Span::styled(format!("-{}", change.removed), del_style));
        }
    }
    spans.push(Span::styled(")", muted_style));
    Line::from(spans)
}

fn patch_diff_lines(
    state: &TuiState,
    diff_lines: &[PatchLineView],
    region_width: u16,
) -> Vec<Line<'static>> {
    if diff_lines.is_empty() {
        return Vec::new();
    }
    let max_line_no = diff_lines
        .iter()
        .filter_map(|line| line.new_line.or(line.old_line))
        .max()
        .unwrap_or(0);
    let num_width = if max_line_no == 0 {
        0
    } else {
        max_line_no.to_string().len().max(2)
    };
    let rail_style = semantic_style(state, SemanticColor::Border);
    let muted_style = semantic_style(state, SemanticColor::Muted);
    let context_style = semantic_style(state, SemanticColor::TextSecondary);
    let add_style = semantic_style(state, SemanticColor::DiffAdd);
    let del_style = semantic_style(state, SemanticColor::DiffDelete);

    // Prefix columns: "  │ " (4) + optional "{num:>w} " + "+ " (2).
    let prefix_cols = 4 + if num_width > 0 { num_width + 1 } else { 0 } + 2;
    let code_width = usize::from(region_width).saturating_sub(prefix_cols).max(8);

    let shown_count = diff_lines.len().min(MAX_EXPANDED_PATCH_LINES);
    let mut out = Vec::with_capacity(shown_count.saturating_add(1));
    for entry in &diff_lines[..shown_count] {
        let mut spans = vec![Span::raw("  "), Span::styled("│ ", rail_style)];
        if num_width > 0 {
            let num_text = match entry.kind {
                PatchLineKind::Remove => entry.old_line,
                PatchLineKind::Add | PatchLineKind::Context => entry.new_line.or(entry.old_line),
            }
            .map_or_else(|| " ".repeat(num_width), |n| format!("{n:>num_width$}"));
            spans.push(Span::styled(format!("{num_text} "), muted_style));
        }
        let clean = crate::text::without_control_chars(&entry.text);
        let text = truncate_chars(&clean, code_width);
        match entry.kind {
            PatchLineKind::Add => {
                spans.push(Span::styled("+ ", add_style.add_modifier(Modifier::BOLD)));
                spans.push(Span::styled(text, add_style));
            }
            PatchLineKind::Remove => {
                spans.push(Span::styled("- ", del_style.add_modifier(Modifier::BOLD)));
                spans.push(Span::styled(text, del_style));
            }
            PatchLineKind::Context => {
                spans.push(Span::styled("  ", muted_style));
                spans.push(Span::styled(text, context_style));
            }
        }
        out.push(Line::from(spans));
    }
    if diff_lines.len() > shown_count {
        let omitted = diff_lines.len() - shown_count;
        out.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("│ ", rail_style),
            Span::styled(format!("… (+{omitted} more lines)"), muted_style),
        ]));
    }
    out
}

/// Summarizes a patch change by line counts first, then byte counts.
///
/// Reviewers reason about a change in lines, while byte counts describe the
/// file-size effect that a line count cannot express, so lines lead and both
/// stay visible. The hunk count only describes an update's hunks.
fn patch_change_detail(change: &PatchChangeView) -> String {
    let mut segments = Vec::new();
    if change.operation == PatchOperationView::Update {
        segments.push(format!("{} hunk(s)", change.hunks));
    }
    segments.push(format!(
        "{} -> {} lines",
        optional_size(change.lines_before),
        optional_size(change.lines_after)
    ));
    segments.push(format!(
        "{} -> {} bytes",
        change.bytes_before, change.bytes_after
    ));
    format!("  {}", segments.join(", "))
}

/// Renders a stored size that a replayed envelope may not carry.
fn optional_size(value: Option<usize>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

pub(super) fn expanded_title_line(state: &TuiState, title: &str) -> Line<'static> {
    if let Some(line) = tool_title_line(state, title) {
        return line;
    }

    Line::from(Span::styled(
        title.to_owned(),
        semantic_style(state, SemanticColor::Focus),
    ))
}

pub(super) fn local_command_lines(
    state: &TuiState,
    title: &str,
    body: &str,
    region_width: u16,
) -> RenderedMarkdown {
    let mut rows = vec![TranscriptRow::new(
        Line::from(Span::styled(
            title.to_owned(),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        )),
        SelectionPolicy::Skip,
    )];
    let body_width = region_width.saturating_sub(1).max(1);
    let mut rendered = markdown_lines(state, body, body_width);
    rows.extend(rendered.rows.into_iter().map(|row| {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(row.display.spans);
        TranscriptRow::new(Line::from(spans), row.selection.with_prefix(1))
    }));
    rendered.rows = rows;
    for target in &mut rendered.copy_targets {
        target.line_index += 1;
        target.column += 1;
    }
    rendered
}

pub(super) fn diagnostic_lines(
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

pub(super) fn tool_title_line_from_parts(
    state: &TuiState,
    title: &str,
    detail: &str,
) -> Option<Line<'static>> {
    if detail.is_empty() {
        return tool_title_line(state, title);
    }

    if title == "Ran" {
        return Some(command_title_line(state, "Ran", detail));
    }
    tool_title_keyword(title).map(|keyword| tool_keyword_title_line(state, keyword, detail))
}

pub(super) fn tool_title_line(state: &TuiState, title: &str) -> Option<Line<'static>> {
    if let Some(command) = title
        .strip_prefix("Ran ")
        .or_else(|| title.strip_prefix("Ran: "))
    {
        return Some(command_title_line(state, "Ran", command));
    }

    for keyword in TOOL_TITLE_KEYWORDS {
        if let Some(detail) = strip_tool_title_detail(title, keyword) {
            return Some(tool_keyword_title_line(state, keyword, detail));
        }
    }

    None
}

pub(super) fn strip_tool_title_detail<'a>(title: &'a str, keyword: &str) -> Option<&'a str> {
    if title == keyword {
        return Some("");
    }
    title.strip_prefix(keyword)?.strip_prefix(' ')
}

pub(super) const TOOL_TITLE_KEYWORDS: &[&str] = &[
    "Read",
    "Searched",
    "MCP",
    "Permission",
    "Patch",
    "Delegated",
    "Waited",
    "Cancelled",
    "Process",
    "Stopped",
    "Tool",
];

pub(super) fn tool_title_keyword(title: &str) -> Option<&'static str> {
    TOOL_TITLE_KEYWORDS
        .iter()
        .copied()
        .find(|keyword| title == *keyword)
}

pub(super) fn tool_keyword_title_line(
    state: &TuiState,
    keyword: &str,
    detail: &str,
) -> Line<'static> {
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

const USER_LEFT_PAD: &str = " ";
const USER_PROMPT_MARK: &str = "\u{276F} ";
const USER_PROMPT_INDENT: &str = "  ";

/// User-turn anchor band framed with two subtle gray hairlines (matching the
/// composer without the `🎀` badge), soft `UserBackground`, and clean `❯`
/// prompt mark.
pub(super) fn user_lines(
    state: &TuiState,
    text: &str,
    lane: QueuedInputLane,
    width: u16,
) -> Vec<TranscriptRow> {
    let lane_label = match lane {
        QueuedInputLane::Next => None,
        QueuedInputLane::Suspended => Some(("suspended", SemanticColor::Warning)),
        QueuedInputLane::Backlog => Some(("backlog", SemanticColor::Muted)),
    };
    let border_style = semantic_style(state, SemanticColor::Border);
    let hairline = Line::from(Span::styled("─".repeat(usize::from(width)), border_style));
    let band = state.theme().bg_color(SemanticColor::UserBackground);
    let bg_style = band.map_or_else(Style::default, |color| Style::default().bg(color));
    let mark_style = semantic_style(state, SemanticColor::Focus)
        .add_modifier(Modifier::BOLD)
        .patch(bg_style);
    let body_style = semantic_style(state, SemanticColor::Assistant)
        .add_modifier(Modifier::BOLD)
        .patch(bg_style);

    let segments: Vec<&str> = if text.is_empty() {
        vec![""]
    } else {
        text.split('\n').collect()
    };

    let mut rows = Vec::with_capacity(segments.len() + 2);
    if width > 0 {
        rows.push(TranscriptRow::new(hairline.clone(), SelectionPolicy::Skip));
    }
    for (index, segment) in segments.into_iter().enumerate() {
        let mut spans = vec![
            Span::styled(USER_LEFT_PAD.to_owned(), bg_style),
            Span::styled(
                if index == 0 {
                    USER_PROMPT_MARK
                } else {
                    USER_PROMPT_INDENT
                }
                .to_owned(),
                mark_style,
            ),
        ];
        if index == 0
            && let Some((label, color)) = lane_label
        {
            spans.push(Span::styled(
                format!("{label}  "),
                semantic_style(state, color)
                    .add_modifier(Modifier::BOLD)
                    .patch(bg_style),
            ));
        }
        spans.extend(inline_code_spans(state, segment, body_style));
        let mut line = Line::from(spans);
        if let Some(color) = band {
            line.style.bg = Some(color);
        }
        pad_user_line(&mut line, width, bg_style);
        rows.push(TranscriptRow::new(line, SelectionPolicy::StripPrefix(3)));
    }
    if width > 0 {
        rows.push(TranscriptRow::new(hairline, SelectionPolicy::Skip));
    }
    rows
}

fn pad_user_line(line: &mut Line<'static>, width: u16, bg_style: Style) {
    let columns = usize::from(width);
    let current = line.width();
    if columns > current {
        line.push_span(Span::styled(" ".repeat(columns - current), bg_style));
    }
}

/// Renders a clickable subagent summary row inside the parent timeline.
///
/// Active subagents use horizontal shimmer across the status line (no vertical
/// rail) and expose a right-aligned `[Inspect ▸]` chip.
pub(super) fn subagent_lines(
    state: &TuiState,
    agent_id: &str,
    task_id: &str,
    region_width: u16,
) -> Vec<Line<'static>> {
    let clock = MotionClock::now();
    let entry = state.subagent_by_id(agent_id);
    let phase = entry.map_or(crate::tui::subagent_view::SubagentPhase::Starting, |e| {
        e.phase
    });
    let summary = entry
        .map(|e| e.summary.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(task_id);
    let glyph = phase.glyph(clock);
    let inspect_label = "[Inspect ▸]";
    let inspect_width = unicode_width::UnicodeWidthStr::width(inspect_label);

    let prefix_text = format!("{glyph} 🐾 Subagent ");
    let mut left_spans = vec![
        Span::styled(
            prefix_text,
            semantic_style(state, phase.color()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            agent_id.to_owned(),
            semantic_style(state, SemanticColor::Heading).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", semantic_style(state, SemanticColor::Muted)),
    ];
    let left_prefix_w: usize = left_spans.iter().map(Span::width).sum();
    let summary_budget = usize::from(region_width)
        .saturating_sub(left_prefix_w)
        .saturating_sub(
            if usize::from(region_width) >= left_prefix_w + inspect_width + 6 {
                inspect_width + 2
            } else {
                0
            },
        )
        .max(4);
    left_spans.push(Span::styled(
        truncate_chars(summary, summary_budget),
        semantic_style(state, SemanticColor::TextSecondary),
    ));

    if phase.is_active() {
        left_spans = motion::flow_gradient_spans(
            left_spans,
            motion::RUNNING_FLOW_STOPS,
            motion::RUNNING_FLOW_WAVELENGTH,
            motion::RUNNING_FLOW_CHARS_PER_SEC,
            0,
            state.theme().color(SemanticColor::Muted),
            clock,
            state.animations_enabled(),
        );
    }

    let left_w: usize = left_spans.iter().map(Span::width).sum();
    if usize::from(region_width) >= left_w + inspect_width + 2 {
        let gap = usize::from(region_width).saturating_sub(left_w + inspect_width);
        left_spans.push(Span::raw(" ".repeat(gap.max(2))));
        let mut chip_style =
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
        if let Some(bg) = state.theme().bg_color(SemanticColor::ChipBackground) {
            chip_style = chip_style.bg(bg);
        }
        left_spans.push(Span::styled(inspect_label.to_owned(), chip_style));
    }

    vec![Line::from(left_spans)]
}
