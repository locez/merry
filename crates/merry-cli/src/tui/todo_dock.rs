//! Compact collapsible inline Todo checklist strip, matching Merry's `TaskDock`
//! visual language (zero height when empty, 1-row collapsed, bounded 5-row
//! scrollable drawer when expanded).

use crate::tui::{
    state::TuiState,
    text_wrap::{semantic_style, truncate_chars},
    theme::SemanticColor,
};
use merry::events::{TodoItem, TodoStatus};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

pub(crate) const MAX_TODO_VISIBLE_ROWS: usize = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TodoDockState {
    items: Vec<TodoItem>,
    expanded: bool,
    scroll: usize,
}

impl TodoDockState {
    pub(crate) fn items(&self) -> &[TodoItem] {
        &self.items
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn set_items(&mut self, items: Vec<TodoItem>) {
        self.items = items;
        let max_scroll = self.items.len().saturating_sub(MAX_TODO_VISIBLE_ROWS);
        self.scroll = self.scroll.min(max_scroll);
        if self.items.is_empty() {
            self.expanded = false;
            self.scroll = 0;
        }
    }

    pub(crate) fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub(crate) fn toggle_expanded(&mut self) {
        self.expanded = !self.expanded;
        if !self.expanded {
            self.scroll = 0;
        }
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    pub(crate) fn scroll_by(&mut self, delta: isize, visible_rows: usize) {
        let max_scroll = self.items.len().saturating_sub(visible_rows);
        if delta < 0 {
            self.scroll = self.scroll.saturating_sub(delta.unsigned_abs());
        } else {
            self.scroll = self
                .scroll
                .saturating_add(delta.unsigned_abs())
                .min(max_scroll);
        }
    }

    fn completed_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == TodoStatus::Completed)
            .count()
    }

    fn in_progress_item(&self) -> Option<&TodoItem> {
        self.items
            .iter()
            .find(|item| item.status == TodoStatus::InProgress)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoDockPointer {
    ToggleExpanded,
}

pub(crate) fn split_todo_dock_region(state: &TuiState, available: Rect) -> (Rect, Rect) {
    let height = todo_dock_height(state, available.height);
    if height == 0 || available.width == 0 {
        return (Rect::default(), available);
    }
    let gap = u16::from(available.height > height + 1);
    let dock = Rect {
        x: available.x,
        y: available.y,
        width: available.width,
        height,
    };
    let reserved = height.saturating_add(gap).min(available.height);
    let body = Rect {
        x: available.x,
        y: available.y.saturating_add(reserved),
        width: available.width,
        height: available.height.saturating_sub(reserved),
    };
    (dock, body)
}

fn todo_dock_visible_rows(state: &TuiState, available_height: u16) -> usize {
    if !state.todo_dock().is_expanded() || available_height <= 3 {
        return 0;
    }
    let budget = usize::from(available_height.saturating_sub(4)).clamp(1, MAX_TODO_VISIBLE_ROWS);
    state.todo_dock().items().len().max(1).min(budget)
}

fn todo_dock_height(state: &TuiState, available_height: u16) -> u16 {
    if state.todo_dock().is_empty() || available_height < 2 {
        return 0;
    }
    if !state.todo_dock().is_expanded() {
        return 1;
    }
    let rows = todo_dock_visible_rows(state, available_height);
    u16::try_from(rows.saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(available_height.saturating_sub(1).max(1))
}

pub(crate) fn render_todo_dock(frame: &mut Frame<'_>, state: &TuiState, area: Rect) {
    if area.is_empty() || state.todo_dock().is_empty() {
        return;
    }
    let header_area = Rect::new(area.x, area.y, area.width, 1);
    let dock = state.todo_dock();
    let total = dock.items().len();
    let done = dock.completed_count();

    if !dock.is_expanded() {
        let line = collapsed_todo_line(state, done, total, area.width);
        frame.render_widget(Paragraph::new(line), header_area);
        return;
    }

    let list_rows = usize::from(area.height.saturating_sub(2)).min(MAX_TODO_VISIBLE_ROWS);
    let scroll = dock.scroll().min(total.saturating_sub(list_rows));
    let header_line = expanded_todo_header_line(state, done, total, scroll, list_rows, area.width);
    frame.render_widget(Paragraph::new(header_line), header_area);

    let list_y = area.y.saturating_add(1);
    for (offset, item) in dock.items().iter().skip(scroll).take(list_rows).enumerate() {
        let row_y = list_y.saturating_add(u16::try_from(offset).unwrap_or(0));
        if row_y >= area.bottom().saturating_sub(1) {
            break;
        }
        let line = todo_item_line(state, item, area.width);
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(area.x, row_y, area.width, 1),
        );
    }

    if area.height >= 2 {
        let divider_y = area.bottom().saturating_sub(1);
        let divider = Line::from(Span::styled(
            "─".repeat(usize::from(area.width)),
            semantic_style(state, SemanticColor::Border),
        ));
        frame.render_widget(
            Paragraph::new(divider),
            Rect::new(area.x, divider_y, area.width, 1),
        );
    }
}

pub(crate) fn todo_dock_mouse_down(
    state: &TuiState,
    area: Rect,
    position: Position,
) -> Option<TodoDockPointer> {
    if area.is_empty() || !area.contains(position) || state.todo_dock().is_empty() {
        return None;
    }
    if !state.todo_dock().is_expanded() || position.y == area.y {
        return Some(TodoDockPointer::ToggleExpanded);
    }
    None
}

fn collapsed_todo_line(state: &TuiState, done: usize, total: usize, width: u16) -> Line<'static> {
    let max_width = usize::from(width);
    let focus = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let secondary = semantic_style(state, SemanticColor::TextSecondary);
    let badge = format!("[Todos {done}/{total} ▸]");
    let badge_w = UnicodeWidthStr::width(badge.as_str());

    let mut spans = if let Some(active) = state.todo_dock().in_progress_item() {
        let budget = max_width.saturating_sub(badge_w + 8).max(8);
        vec![
            Span::styled("▸ ", focus),
            Span::styled("◆ ", focus),
            Span::styled(truncate_chars(&active.content, budget), secondary),
        ]
    } else if done == total {
        vec![
            Span::styled("▸ ", focus),
            Span::styled(
                "✓ ",
                semantic_style(state, SemanticColor::Success).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("All {total} todos done"), secondary),
        ]
    } else {
        vec![
            Span::styled("▸ ", focus),
            Span::styled("o ", semantic_style(state, SemanticColor::Muted)),
            Span::styled(format!("{done}/{total} todos complete"), secondary),
        ]
    };

    let used: usize = spans.iter().map(Span::width).sum();
    if max_width >= used + badge_w + 2 {
        spans.push(Span::raw(
            " ".repeat(max_width.saturating_sub(used + badge_w)),
        ));
        spans.push(Span::styled(badge, focus));
    }
    Line::from(spans)
}

fn expanded_todo_header_line(
    state: &TuiState,
    done: usize,
    total: usize,
    scroll: usize,
    list_rows: usize,
    width: u16,
) -> Line<'static> {
    let max_width = usize::from(width);
    let focus = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let muted = semantic_style(state, SemanticColor::Muted);
    let mut spans = vec![
        Span::styled("▾ ", focus),
        Span::styled(format!("Todos ({done}/{total})"), focus),
    ];
    let right = if total > list_rows && list_rows > 0 {
        let start = scroll + 1;
        let end = (scroll + list_rows).min(total);
        format!("{start}–{end}/{total}  [Collapse]")
    } else {
        "[Collapse]".to_owned()
    };
    let right_w = UnicodeWidthStr::width(right.as_str());
    let used: usize = spans.iter().map(Span::width).sum();
    if max_width > used + right_w + 1 {
        spans.push(Span::raw(
            " ".repeat(max_width.saturating_sub(used + right_w)),
        ));
        spans.push(Span::styled(right, muted));
    }
    Line::from(spans)
}

fn todo_item_line(state: &TuiState, item: &TodoItem, width: u16) -> Line<'static> {
    let max_width = usize::from(width);
    let (glyph, glyph_color, text_style) = match item.status {
        TodoStatus::Completed => (
            "✓",
            SemanticColor::Success,
            semantic_style(state, SemanticColor::Muted),
        ),
        TodoStatus::InProgress => (
            "◆",
            SemanticColor::Focus,
            semantic_style(state, SemanticColor::Heading).add_modifier(Modifier::BOLD),
        ),
        TodoStatus::Pending => (
            "o",
            SemanticColor::Muted,
            semantic_style(state, SemanticColor::TextSecondary),
        ),
        TodoStatus::Cancelled => (
            "⊘",
            SemanticColor::Muted,
            semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM),
        ),
    };
    let budget = max_width.saturating_sub(5).max(8);
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{glyph} "),
            semantic_style(state, glyph_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(truncate_chars(&item.content, budget), text_style),
    ])
}
