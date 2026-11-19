//! Geometry, split-pane layout, pointer hit-testing, and mouse-wheel navigation for command details.

use super::{
    CapturedOutput, CommandDetails, CommandNavigation, DetailsOutput, HistoryPlace, StreamPane,
    finished_command_indices,
    highlight::{append_highlighted_output, highlight_command_lines},
    inspect_command,
    output::CapturedStream,
    render::footer_hints,
    shown_pane, stream_has_body,
};
use crate::tui::{
    click_target::{OverlayPointer, chip_hit},
    controller::ControllerEffect,
    overlay::Overlay,
    preferences::CodeTheme,
    state::{CommandFailure, CommandView, TuiState},
    text_wrap::{StyledTextPart, semantic_style, wrap_styled_parts_preserving_leading_whitespace},
    theme::SemanticColor,
};
use ratatui::{
    layout::{Margin, Position, Rect},
    text::Line,
    widgets::Paragraph,
};

const MAX_DETAILS_WIDTH: u16 = 140;
const HISTORY_RAIL_MIN_INNER_WIDTH: u16 = 76;
const HISTORY_RAIL_COMPACT_WIDTH: u16 = 22;
const HISTORY_RAIL_WIDE_THRESHOLD: u16 = 92;
const HISTORY_RAIL_WIDE_WIDTH: u16 = 28;
const CHIPS_MIN_INNER_WIDTH: u16 = 52;
const WHEEL_SCROLL_STEP: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DetailsLayout {
    pub(super) width: u16,
    pub(super) code_theme: CodeTheme,
    pub(super) header_lines: Vec<Line<'static>>,
    pub(super) stdout_lines: Vec<Line<'static>>,
    pub(super) stderr_lines: Vec<Line<'static>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FooterChipAction {
    Previous,
    Next,
    CopyCommand,
    CopyOutput,
    Close,
}

impl FooterChipAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Previous => "◀ Prev",
            Self::Next => "Next ▶",
            Self::CopyCommand => "Copy cmd",
            Self::CopyOutput => "Copy out",
            Self::Close => "Close",
        }
    }

    pub(super) fn pointer(self) -> OverlayPointer {
        match self {
            Self::Previous => OverlayPointer::CommandPrevious,
            Self::Next => OverlayPointer::CommandNext,
            Self::CopyCommand => OverlayPointer::CommandCopyCommand,
            Self::CopyOutput => OverlayPointer::CommandCopyOutput,
            Self::Close => OverlayPointer::CommandClose,
        }
    }
}

pub(super) struct DetailsRegions {
    pub(super) region: Rect,
    pub(super) history_rail: Option<Rect>,
    pub(super) rail_divider: Option<Rect>,
    pub(super) detail_area: Rect,
    pub(super) footer_divider: Option<Rect>,
    pub(super) chips_row: Option<Rect>,
    pub(super) hints_rect: Rect,
    pub(super) hints: Paragraph<'static>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PaneRects {
    pub(super) full: Rect,
    pub(super) header: Rect,
    pub(super) body: Rect,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum OutputPanes {
    Single {
        divider: Option<Rect>,
        body: Rect,
    },
    Process {
        stdout: PaneRects,
        stderr: PaneRects,
        stdout_has_body: bool,
        stderr_has_body: bool,
    },
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DetailSplit {
    pub(super) header: Rect,
    pub(super) output: OutputPanes,
}

pub(super) fn footer_actions(place: HistoryPlace) -> Vec<FooterChipAction> {
    if place.show_navigation() {
        vec![
            FooterChipAction::Previous,
            FooterChipAction::Next,
            FooterChipAction::CopyCommand,
            FooterChipAction::CopyOutput,
            FooterChipAction::Close,
        ]
    } else {
        vec![
            FooterChipAction::CopyCommand,
            FooterChipAction::CopyOutput,
            FooterChipAction::Close,
        ]
    }
}

pub(super) fn history_scroll_offset(current: usize, total: usize, visible: usize) -> usize {
    if total <= visible || visible == 0 {
        return 0;
    }
    current
        .saturating_sub(visible / 2)
        .min(total.saturating_sub(visible))
}

pub(super) fn regions(area: Rect, state: &TuiState, details: &CommandDetails) -> DetailsRegions {
    let width = area.width.saturating_sub(2).min(MAX_DETAILS_WIDTH);
    let height = area.height.saturating_sub(2);
    let region = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let inner = region.inner(Margin::new(1, 1));
    let hints = footer_hints(state, details, inner.width);
    let hints_height = u16::try_from(hints.line_count(inner.width))
        .unwrap_or(u16::MAX)
        .min(inner.height.saturating_sub(1));

    let show_chips =
        inner.width >= CHIPS_MIN_INNER_WIDTH && inner.height >= hints_height.saturating_add(4);
    let chips_height: u16 = if show_chips { 1 } else { 0 };
    let show_divider = inner.height >= hints_height.saturating_add(chips_height).saturating_add(4);
    let divider_height: u16 = if show_divider { 1 } else { 0 };
    let footer_total = hints_height
        .saturating_add(chips_height)
        .saturating_add(divider_height);

    let main_height = inner.height.saturating_sub(footer_total);
    let main_area = Rect::new(inner.x, inner.y, inner.width, main_height);

    let show_history = HistoryPlace::resolve(state, details.timeline_index).show_navigation();
    let (history_rail, rail_divider, detail_area) =
        if show_history && main_area.width >= HISTORY_RAIL_MIN_INNER_WIDTH {
            let rail_width = if main_area.width > HISTORY_RAIL_WIDE_THRESHOLD {
                HISTORY_RAIL_WIDE_WIDTH
            } else {
                HISTORY_RAIL_COMPACT_WIDTH
            };
            let rail = Rect::new(main_area.x, main_area.y, rail_width, main_area.height);
            let divider = Rect::new(rail.right(), main_area.y, 1, main_area.height);
            let body_x = divider.right().saturating_add(1);
            let body_w = main_area.right().saturating_sub(body_x);
            let detail = Rect::new(body_x, main_area.y, body_w, main_area.height);
            (Some(rail), Some(divider), detail)
        } else {
            (None, None, main_area)
        };

    let mut cursor_y = main_area.bottom();
    let footer_divider = show_divider.then(|| {
        let rect = Rect::new(inner.x, cursor_y, inner.width, 1);
        cursor_y = cursor_y.saturating_add(1);
        rect
    });
    let chips_row = show_chips.then(|| {
        let rect = Rect::new(inner.x, cursor_y, inner.width, 1);
        cursor_y = cursor_y.saturating_add(1);
        rect
    });
    let hints_rect = Rect::new(inner.x, cursor_y, inner.width, hints_height);

    DetailsRegions {
        region,
        history_rail,
        rail_divider,
        detail_area,
        footer_divider,
        chips_row,
        hints_rect,
        hints,
    }
}

pub(super) fn split_detail_area(
    detail_area: Rect,
    header_line_count: usize,
    output: &DetailsOutput,
) -> DetailSplit {
    let max_header = if detail_area.height >= 14 {
        5
    } else if detail_area.height >= 8 {
        4
    } else {
        detail_area.height.saturating_sub(2).min(3)
    };
    let desired_header = u16::try_from(header_line_count).unwrap_or(max_header);
    let header_height = desired_header.min(max_header).min(detail_area.height);
    let header = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        header_height,
    );
    let output_area = Rect::new(
        detail_area.x,
        header.bottom(),
        detail_area.width,
        detail_area.height.saturating_sub(header_height),
    );

    let output_panes = match output {
        DetailsOutput::Ready(CapturedOutput::Process { stdout, stderr }) => {
            let stdout_has_body = stream_has_body(stdout);
            let stderr_has_body = stream_has_body(stderr);
            let (stdout_h, stderr_h) =
                allocate_process_heights(output_area.height, stdout_has_body, stderr_has_body);
            let stdout_full = Rect::new(output_area.x, output_area.y, output_area.width, stdout_h);
            let stderr_full = Rect::new(
                output_area.x,
                stdout_full.bottom(),
                output_area.width,
                stderr_h,
            );
            OutputPanes::Process {
                stdout: pane_rects(stdout_full, stdout_has_body),
                stderr: pane_rects(stderr_full, stderr_has_body),
                stdout_has_body,
                stderr_has_body,
            }
        }
        _ => {
            if output_area.height >= 3 {
                let divider = Rect::new(output_area.x, output_area.y, output_area.width, 1);
                let body = Rect::new(
                    output_area.x,
                    divider.bottom(),
                    output_area.width,
                    output_area.height.saturating_sub(1),
                );
                OutputPanes::Single {
                    divider: Some(divider),
                    body,
                }
            } else {
                OutputPanes::Single {
                    divider: None,
                    body: output_area,
                }
            }
        }
    };

    DetailSplit {
        header,
        output: output_panes,
    }
}

fn allocate_process_heights(
    total: u16,
    stdout_has_body: bool,
    stderr_has_body: bool,
) -> (u16, u16) {
    if total == 0 {
        return (0, 0);
    }
    match (stdout_has_body, stderr_has_body) {
        (true, true) => {
            if total >= 4 {
                let stdout_h = ((usize::from(total) * 55) / 100)
                    .max(2)
                    .min(usize::from(total.saturating_sub(2)));
                let stdout_u16 = u16::try_from(stdout_h).unwrap_or(total / 2);
                (stdout_u16, total.saturating_sub(stdout_u16))
            } else {
                let half = total / 2;
                (total.saturating_sub(half), half)
            }
        }
        (true, false) => {
            if total >= 3 {
                (total.saturating_sub(1), 1)
            } else {
                (total, 0)
            }
        }
        (false, true) => {
            if total >= 3 {
                (1, total.saturating_sub(1))
            } else {
                (0, total)
            }
        }
        (false, false) => {
            if total >= 2 {
                (1, 1)
            } else {
                (total, 0)
            }
        }
    }
}

fn pane_rects(full: Rect, has_body: bool) -> PaneRects {
    if full.height == 0 {
        return PaneRects {
            full,
            header: full,
            body: full,
        };
    }
    let header = Rect::new(full.x, full.y, full.width, 1);
    let body = if has_body && full.height > 1 {
        Rect::new(
            full.x,
            header.bottom(),
            full.width,
            full.height.saturating_sub(1),
        )
    } else {
        Rect::new(full.x, header.bottom(), full.width, 0)
    };
    PaneRects { full, header, body }
}

pub(super) fn build_layout(
    state: &TuiState,
    details: &CommandDetails,
    width: u16,
) -> DetailsLayout {
    let code_theme = state.code_theme();
    let header_lines = build_header_lines(state, details, width);
    let active_command = command_text(&details.view);
    let (stdout_lines, stderr_lines) = match &details.output {
        DetailsOutput::Loading => {
            let mut lines = Vec::new();
            append_text(
                &mut lines,
                state,
                "Loading captured output…",
                width,
                SemanticColor::Muted,
            );
            (lines, Vec::new())
        }
        DetailsOutput::Failed(error) => {
            let mut lines = Vec::new();
            append_text(
                &mut lines,
                state,
                &format!("Could not load output: {error}"),
                width,
                SemanticColor::Error,
            );
            (lines, Vec::new())
        }
        DetailsOutput::Ready(CapturedOutput::Text(text)) => {
            let mut lines = Vec::new();
            append_highlighted_output(&mut lines, state, active_command, text, width);
            (lines, Vec::new())
        }
        DetailsOutput::Ready(CapturedOutput::Process { stdout, stderr }) => (
            build_stream_lines(state, active_command, stdout, width),
            build_stream_lines(state, active_command, stderr, width),
        ),
    };
    DetailsLayout {
        width,
        code_theme,
        header_lines,
        stdout_lines,
        stderr_lines,
    }
}

pub(crate) fn prepare_viewport(state: &mut TuiState, area: Rect) {
    let Some(Overlay::CommandDetails(details)) = state.overlay() else {
        return;
    };
    let layout_regions = regions(area, state, details);
    let code_theme = state.code_theme();
    let rebuilt = details
        .layout
        .as_ref()
        .is_none_or(|layout| {
            layout.width != layout_regions.detail_area.width || layout.code_theme != code_theme
        })
        .then(|| build_layout(state, details, layout_regions.detail_area.width));
    if let Some(Overlay::CommandDetails(details)) = state.overlay_mut() {
        if let Some(layout) = rebuilt {
            details.layout = Some(layout);
        }
        if let Some(layout) = &details.layout {
            let split = split_detail_area(
                layout_regions.detail_area,
                layout.header_lines.len(),
                &details.output,
            );
            let stdout_len = layout.stdout_lines.len();
            let stderr_len = layout.stderr_lines.len();
            clamp_scrolls(
                &mut details.stdout_scroll,
                &mut details.stderr_scroll,
                stdout_len,
                stderr_len,
                &split.output,
            );
        }
    }
}

fn clamp_scrolls(
    stdout_scroll: &mut usize,
    stderr_scroll: &mut usize,
    stdout_len: usize,
    stderr_len: usize,
    panes: &OutputPanes,
) {
    match panes {
        OutputPanes::Single { body, .. } => {
            let max = stdout_len.saturating_sub(usize::from(body.height));
            *stdout_scroll = (*stdout_scroll).min(max);
            *stderr_scroll = 0;
        }
        OutputPanes::Process { stdout, stderr, .. } => {
            let stdout_max = stdout_len.saturating_sub(usize::from(stdout.body.height));
            let stderr_max = stderr_len.saturating_sub(usize::from(stderr.body.height));
            *stdout_scroll = (*stdout_scroll).min(stdout_max);
            *stderr_scroll = (*stderr_scroll).min(stderr_max);
        }
    }
}

/// Handles mouse wheel scrolling inside the command details overlay:
/// - Hovering the left history rail switches previous/next commands.
/// - Hovering stdout/stderr/detail panes scrolls only that pane and stops cleanly at its bounds.
pub(crate) fn handle_mouse_scroll(
    state: &mut TuiState,
    area: Rect,
    position: Position,
    scroll_up: bool,
) -> ControllerEffect {
    let Some(Overlay::CommandDetails(details)) = state.overlay() else {
        return ControllerEffect::None;
    };
    let layout_regions = regions(area, state, details);
    if layout_regions
        .history_rail
        .is_some_and(|rail| rail.contains(position))
    {
        let direction = if scroll_up {
            CommandNavigation::Previous
        } else {
            CommandNavigation::Next
        };
        return inspect_command(state, direction);
    }

    prepare_viewport(state, area);

    let (target_pane, focus, current_scroll, max_scroll) = {
        let Some(Overlay::CommandDetails(details)) = state.overlay() else {
            return ControllerEffect::None;
        };
        let Some(layout) = &details.layout else {
            return ControllerEffect::None;
        };
        if !layout_regions.region.contains(position) {
            return ControllerEffect::None;
        }
        let split = split_detail_area(
            layout_regions.detail_area,
            layout.header_lines.len(),
            &details.output,
        );
        let (pane, focus) = match split.output {
            OutputPanes::Single { .. } => (StreamPane::Stdout, false),
            OutputPanes::Process {
                stdout,
                stderr,
                stdout_has_body,
                stderr_has_body,
            } => {
                if stderr_has_body && stderr.full.contains(position) {
                    (StreamPane::Stderr, true)
                } else if stdout_has_body && stdout.full.contains(position) {
                    (StreamPane::Stdout, true)
                } else {
                    (shown_pane(&details.output, details.active_pane), false)
                }
            }
        };
        let (len, height) = match (&split.output, pane) {
            (OutputPanes::Single { body, .. }, _) => (layout.stdout_lines.len(), body.height),
            (OutputPanes::Process { stdout, .. }, StreamPane::Stdout) => {
                (layout.stdout_lines.len(), stdout.body.height)
            }
            (OutputPanes::Process { stderr, .. }, StreamPane::Stderr) => {
                (layout.stderr_lines.len(), stderr.body.height)
            }
        };
        let max = len.saturating_sub(usize::from(height));
        let scroll = match pane {
            StreamPane::Stdout => details.stdout_scroll,
            StreamPane::Stderr => details.stderr_scroll,
        };
        (pane, focus, scroll.min(max), max)
    };

    if let Some(Overlay::CommandDetails(details)) = state.overlay_mut() {
        if focus {
            details.active_pane = target_pane;
        }
        let next = if scroll_up {
            current_scroll.saturating_sub(WHEEL_SCROLL_STEP)
        } else {
            current_scroll
                .saturating_add(WHEEL_SCROLL_STEP)
                .min(max_scroll)
        };
        match target_pane {
            StreamPane::Stdout => details.stdout_scroll = next,
            StreamPane::Stderr => details.stderr_scroll = next,
        }
    }
    ControllerEffect::None
}

/// Resolves pointer clicks on the history rail, stream panes, or footer action chips.
pub(crate) fn details_pointer(
    state: &TuiState,
    area: Rect,
    details: &CommandDetails,
    position: Position,
) -> Option<OverlayPointer> {
    let layout_regions = regions(area, state, details);
    if let Some(rail) = layout_regions.history_rail
        && rail.contains(position)
        && position.y > rail.y
    {
        let indices = finished_command_indices(state);
        let visible_rows = usize::from(rail.height.saturating_sub(1));
        if visible_rows == 0 || indices.is_empty() {
            return None;
        }
        let place = HistoryPlace::from_indices(&indices, details.timeline_index);
        let scroll = place.rail_index().map_or(0, |current| {
            history_scroll_offset(current, indices.len(), visible_rows)
        });
        let target = scroll.saturating_add(usize::from(position.y - rail.y - 1));
        return (target < indices.len()).then_some(OverlayPointer::CommandSelect(target));
    }

    if let Some(chips_rect) = layout_regions.chips_row
        && chips_rect.contains(position)
    {
        let actions = footer_actions(HistoryPlace::resolve(state, details.timeline_index));
        let labels = actions
            .iter()
            .map(|action| action.label())
            .collect::<Vec<_>>();
        return chip_hit(Position::new(chips_rect.x, chips_rect.y), &labels, position)
            .and_then(|index| actions.get(index).copied())
            .map(FooterChipAction::pointer);
    }

    if layout_regions.detail_area.contains(position) {
        let header_count = details.layout.as_ref().map_or_else(
            || build_header_lines(state, details, layout_regions.detail_area.width).len(),
            |layout| layout.header_lines.len(),
        );
        let split = split_detail_area(layout_regions.detail_area, header_count, &details.output);
        if let OutputPanes::Process {
            stdout,
            stderr,
            stdout_has_body: true,
            stderr_has_body: true,
        } = split.output
        {
            if stderr.full.contains(position) {
                return Some(OverlayPointer::CommandFocusPane(StreamPane::Stderr));
            }
            if stdout.full.contains(position) {
                return Some(OverlayPointer::CommandFocusPane(StreamPane::Stdout));
            }
        }
    }

    None
}

fn command_text(view: &CommandView) -> &str {
    match view {
        CommandView::Finished {
            detail, command, ..
        } => {
            if command.is_empty() {
                detail.as_str()
            } else {
                command.as_str()
            }
        }
        CommandView::Running { detail, .. } => detail.as_str(),
    }
}

fn build_header_lines(
    state: &TuiState,
    details: &CommandDetails,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let CommandView::Finished {
        detail,
        command,
        cwd,
        exit_code,
        failure,
        elapsed,
        ..
    } = &details.view
    {
        let active_command = if command.is_empty() {
            detail.as_str()
        } else {
            command.as_str()
        };
        lines.extend(highlight_command_lines(state, active_command, width));

        append_text(
            &mut lines,
            state,
            &format!("Directory: {cwd}"),
            width,
            SemanticColor::Muted,
        );
        let mut status = match failure {
            Some(CommandFailure::Cancelled) => "Cancelled".to_owned(),
            Some(CommandFailure::Failed) => "Failed".to_owned(),
            None => exit_code
                .map(|code| format!("Exit: {code}"))
                .unwrap_or_else(|| "Exit: unknown".into()),
        };
        if let Some(elapsed) = elapsed {
            status.push_str(&format!(
                " · Elapsed: {:.1}s (including waiting)",
                elapsed.as_secs_f64()
            ));
        }
        let color = if exit_code.is_some_and(|code| code != 0)
            || *failure == Some(CommandFailure::Failed)
        {
            SemanticColor::Error
        } else if *failure == Some(CommandFailure::Cancelled) {
            SemanticColor::Warning
        } else {
            SemanticColor::Success
        };
        append_text(&mut lines, state, &status, width, color);
    }
    lines
}

fn build_stream_lines(
    state: &TuiState,
    command: &str,
    stream: &CapturedStream,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !stream.text.is_empty() {
        append_highlighted_output(&mut lines, state, command, &stream.text, width);
    }
    if stream.truncated {
        append_text(
            &mut lines,
            state,
            "… capture truncated; only captured output is available",
            width,
            SemanticColor::Warning,
        );
    }
    if stream.utf8 == Some(false) {
        append_text(
            &mut lines,
            state,
            "Non-UTF-8 output is displayed as text; the artifact retains the original bytes.",
            width,
            SemanticColor::Warning,
        );
    }
    lines
}

fn append_text(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    text: &str,
    width: u16,
    color: SemanticColor,
) {
    for line in text.split('\n') {
        let clean = line
            .chars()
            .filter(|character| !character.is_control() || *character == '\t')
            .collect::<String>()
            .replace('\t', "    ");
        if clean.is_empty() {
            lines.push(Line::default());
        } else {
            lines.extend(wrap_styled_parts_preserving_leading_whitespace(
                vec![StyledTextPart {
                    text: clean,
                    style: semantic_style(state, color),
                    atomic: false,
                }],
                width,
            ));
        }
    }
}
