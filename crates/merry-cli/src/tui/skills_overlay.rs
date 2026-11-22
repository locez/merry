//! Interactive `/skills` picker modal with live fuzzy search.
//!
//! Selecting a skill inserts `$<skill-name> ` into the composer without
//! expanding `SKILL.md` into the prompt, preserving Merry's stable prefix and
//! lazy disk-read skill contract.

use super::{
    fuzzy,
    input::{TextInput, TextInputViewport},
    modal_frame::{modal_rects, modal_selection_style, render_divider, render_modal_frame},
    state::TuiState,
    text_wrap::{semantic_style, truncate_chars},
    theme::SemanticColor,
};
use crossterm::event::{KeyCode, KeyEvent};
use merry_runtime::SkillMetadata;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

const MAX_SKILLS_OVERLAY_WIDTH: u16 = 84;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillPickerItem {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) skill_md_path: String,
}

impl From<&SkillMetadata> for SkillPickerItem {
    fn from(skill: &SkillMetadata) -> Self {
        Self {
            name: skill.name().to_owned(),
            description: skill.description().to_owned(),
            skill_md_path: skill.skill_md_path().to_string_lossy().replace('\\', "/"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchedSkill<'a> {
    pub(crate) item: &'a SkillPickerItem,
    pub(crate) name_positions: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SkillsOverlayAction {
    Consumed,
    Close,
    Select(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillsOverlay {
    query: TextInput,
    skills: Vec<SkillPickerItem>,
    selected: usize,
}

impl SkillsOverlay {
    pub(crate) fn new(skills: Vec<SkillMetadata>) -> Self {
        let mut items = skills.iter().map(SkillPickerItem::from).collect::<Vec<_>>();
        items.sort_by(|left, right| left.name.cmp(&right.name));
        Self {
            query: TextInput::default(),
            skills: items,
            selected: 0,
        }
    }

    pub(crate) fn query(&self) -> &str {
        self.query.text()
    }

    pub(crate) fn query_viewport(&self, width: usize) -> TextInputViewport {
        self.query.viewport(width)
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn select(&mut self, index: usize) {
        let count = self.visible_skills().len();
        if count > 0 {
            self.selected = index.min(count - 1);
        }
    }

    pub(crate) fn insert_paste(&mut self, text: &str) {
        self.query.insert_str(text);
        self.selected = 0;
    }

    pub(crate) fn visible_skills(&self) -> Vec<MatchedSkill<'_>> {
        let raw_query = self.query.text().trim();
        let query = raw_query.strip_prefix('$').unwrap_or(raw_query);
        if query.is_empty() {
            return self
                .skills
                .iter()
                .map(|item| MatchedSkill {
                    item,
                    name_positions: Vec::new(),
                })
                .collect();
        }

        let mut scored = self
            .skills
            .iter()
            .filter_map(|item| {
                if let Some(name_match) = fuzzy::fuzzy_match(&item.name, query) {
                    return Some((
                        0_u8,
                        name_match.score,
                        MatchedSkill {
                            item,
                            name_positions: name_match.positions,
                        },
                    ));
                }
                let desc_match = fuzzy::fuzzy_match(&item.description, query)?;
                Some((
                    1_u8,
                    desc_match.score,
                    MatchedSkill {
                        item,
                        name_positions: Vec::new(),
                    },
                ))
            })
            .collect::<Vec<_>>();

        scored.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.item.name.cmp(&right.2.item.name))
        });

        scored.into_iter().map(|(_, _, matched)| matched).collect()
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> SkillsOverlayAction {
        match key.code {
            KeyCode::Esc => SkillsOverlayAction::Close,
            KeyCode::Enter => self
                .visible_skills()
                .get(self.selected)
                .map_or(SkillsOverlayAction::Consumed, |matched| {
                    SkillsOverlayAction::Select(matched.item.name.clone())
                }),
            KeyCode::Down => {
                let count = self.visible_skills().len();
                if count > 0 {
                    self.selected = (self.selected + 1).min(count - 1);
                }
                SkillsOverlayAction::Consumed
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                SkillsOverlayAction::Consumed
            }
            _ => {
                self.query.handle_key(key);
                let count = self.visible_skills().len();
                self.selected = self.selected.min(count.saturating_sub(1));
                SkillsOverlayAction::Consumed
            }
        }
    }
}

pub(crate) enum SkillsPointer {
    Close,
    Row(usize),
}

pub(crate) fn render_skills_overlay(
    frame: &mut Frame<'_>,
    state: &TuiState,
    picker: &SkillsOverlay,
) {
    let visible = picker.visible_skills();
    let height = (visible.len() as u16 + 8).clamp(10, 22);
    let modal = modal_rects(frame.area(), MAX_SKILLS_OVERLAY_WIDTH, height);
    render_modal_frame(
        frame,
        state,
        modal,
        "Skills",
        SemanticColor::BorderFocus,
        true,
    );
    let inner = modal.inner;
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let footer_height = u16::from(inner.height >= 6) * 2;
    let divider_height = u16::from(footer_height > 0);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(divider_height),
            Constraint::Length(footer_height),
        ])
        .split(inner);

    let query = picker.query_viewport(rows[0].width as usize);
    let query_span = if picker.query().is_empty() {
        Span::styled(
            "Search skills (inserts $skill-name)",
            semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM),
        )
    } else {
        Span::styled(query.text, semantic_style(state, SemanticColor::Assistant))
    };
    frame.render_widget(Paragraph::new(Line::from(query_span)), rows[0]);

    let list_width = usize::from(rows[1].width);
    let visible_height = usize::from(rows[1].height).max(1);
    let scroll_offset = picker
        .selected()
        .saturating_sub(visible_height.saturating_sub(1))
        .min(visible.len().saturating_sub(visible_height));

    let lines = if visible.is_empty() {
        vec![Line::from(Span::styled(
            "  No matching skills",
            semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM),
        ))]
    } else {
        let name_col = visible
            .iter()
            .map(|matched| UnicodeWidthStr::width(matched.item.name.as_str()) + 1)
            .max()
            .unwrap_or(12)
            .min(list_width / 2);

        visible
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(visible_height)
            .map(|(index, matched)| {
                let selected = index == picker.selected();
                let base_style = if selected {
                    modal_selection_style(state, true)
                } else {
                    semantic_style(state, SemanticColor::TextSecondary)
                };
                let highlight_style = if selected {
                    base_style
                        .patch(semantic_style(state, SemanticColor::Focus))
                        .add_modifier(Modifier::BOLD)
                } else {
                    semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
                };
                let desc_style = if selected {
                    base_style
                } else {
                    semantic_style(state, SemanticColor::Muted)
                };

                let marker = if selected { "> " } else { "  " };
                let token = format!("${}", matched.item.name);
                let token_shown = truncate_chars(&token, name_col);
                let token_width = UnicodeWidthStr::width(token_shown.as_str());
                let pad_after_name = name_col.saturating_sub(token_width) + 2;
                let desc_budget = list_width.saturating_sub(2 + token_width + pad_after_name);
                let desc_shown = if desc_budget > 0 {
                    truncate_chars(&matched.item.description, desc_budget)
                } else {
                    String::new()
                };
                let used = 2 + token_width + pad_after_name + UnicodeWidthStr::width(desc_shown.as_str());
                let trailing = " ".repeat(list_width.saturating_sub(used));

                let shifted_positions = matched
                    .name_positions
                    .iter()
                    .map(|pos| pos + 1)
                    .collect::<Vec<_>>();
                let mut spans = vec![Span::styled(marker, base_style)];
                push_position_highlighted_spans(
                    &mut spans,
                    &token_shown,
                    &shifted_positions,
                    base_style,
                    highlight_style,
                );
                spans.push(Span::styled(" ".repeat(pad_after_name), desc_style));
                spans.push(Span::styled(desc_shown, desc_style));
                spans.push(Span::styled(trailing, desc_style));
                Line::from(spans)
            })
            .collect::<Vec<_>>()
    };
    frame.render_widget(Paragraph::new(lines), rows[1]);

    if divider_height > 0 {
        render_divider(frame, state, rows[2]);
    }
    if footer_height > 0 {
        let path_line = visible
            .get(picker.selected())
            .map(|matched| {
                truncate_chars(
                    &format!("Source: {}", matched.item.skill_md_path),
                    usize::from(rows[3].width),
                )
            })
            .unwrap_or_default();
        let hint_line = truncate_chars(
            "Enter Insert $skill   ↑/↓ Select   Esc Close",
            usize::from(rows[3].width),
        );
        let footer = vec![
            Line::from(Span::styled(
                path_line,
                semantic_style(state, SemanticColor::Muted),
            )),
            Line::from(Span::styled(
                hint_line,
                semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM),
            )),
        ];
        frame.render_widget(Paragraph::new(footer), rows[3]);
    }

    let cursor_x = rows[0]
        .x
        .saturating_add(query.cursor_column as u16)
        .min(rows[0].right().saturating_sub(1));
    frame.set_cursor_position(Position::new(cursor_x, rows[0].y));
}

pub(crate) fn skills_overlay_pointer(
    area: Rect,
    picker: &SkillsOverlay,
    position: Position,
) -> Option<SkillsPointer> {
    let visible = picker.visible_skills();
    let height = (visible.len() as u16 + 8).clamp(10, 22);
    let modal = modal_rects(area, MAX_SKILLS_OVERLAY_WIDTH, height);
    if modal.close_hit(position) {
        return Some(SkillsPointer::Close);
    }
    let inner = modal.inner;
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let footer_height = u16::from(inner.height >= 6) * 2;
    let divider_height = u16::from(footer_height > 0);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(divider_height),
            Constraint::Length(footer_height),
        ])
        .split(inner);
    if !rows[1].contains(position) || visible.is_empty() {
        return None;
    }
    let visible_height = usize::from(rows[1].height).max(1);
    let scroll_offset = picker
        .selected()
        .saturating_sub(visible_height.saturating_sub(1))
        .min(visible.len().saturating_sub(visible_height));
    let index = scroll_offset + usize::from(position.y.saturating_sub(rows[1].y));
    (index < visible.len()).then_some(SkillsPointer::Row(index))
}

/// Renders `text` as spans where character indices listed in `positions`
/// receive `highlight_style` and remaining characters receive `base_style`.
pub(crate) fn push_position_highlighted_spans(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    positions: &[usize],
    base_style: Style,
    highlight_style: Style,
) {
    if positions.is_empty() {
        spans.push(Span::styled(text.to_owned(), base_style));
        return;
    }
    let mut current = String::new();
    let mut current_highlighted = false;
    let mut pos_idx = 0;

    for (char_idx, ch) in text.chars().enumerate() {
        while pos_idx < positions.len() && positions[pos_idx] < char_idx {
            pos_idx += 1;
        }
        let highlighted = pos_idx < positions.len() && positions[pos_idx] == char_idx;
        if !current.is_empty() && highlighted != current_highlighted {
            let style = if current_highlighted {
                highlight_style
            } else {
                base_style
            };
            spans.push(Span::styled(std::mem::take(&mut current), style));
        }
        current_highlighted = highlighted;
        current.push(ch);
    }

    if !current.is_empty() {
        let style = if current_highlighted {
            highlight_style
        } else {
            base_style
        };
        spans.push(Span::styled(current, style));
    }
}
