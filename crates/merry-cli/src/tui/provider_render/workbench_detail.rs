//! Right-hand detail panes for the Provider Workbench (inline editor and empty state).

use super::{form_field_index, form_field_label};
use crate::{
    config::ManagedProviderKind,
    tui::{
        modal_frame::{modal_selection_style, modal_surface_style},
        overlay_render::semantic_style,
        provider_overlay::{ProviderFormField, ProviderFormOverlay},
        state::TuiState,
        theme::SemanticColor,
    },
};
use merry_provider_openai::OpenAiProtocol;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

#[allow(clippy::too_many_arguments)]
pub(super) fn render_form_detail_pane(
    frame: &mut Frame<'_>,
    state: &TuiState,
    form: &ProviderFormOverlay,
    focused: bool,
    show_workbench_chips: bool,
    is_active: bool,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let input_bg = surface;
    let label_col_width: usize = if area.width >= 48 { 16 } else { 14 };
    let value_width = usize::from(area.width)
        .saturating_sub(label_col_width + 3)
        .max(1);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(usize::from(area.height));

    let heading = if form.is_editing() {
        "Edit provider"
    } else {
        "Add provider"
    };
    let badge = if !form.is_editing() {
        " [draft]"
    } else if is_active {
        " [active · managed]"
    } else {
        " [managed]"
    };
    lines.push(Line::from(vec![
        Span::styled(
            heading,
            semantic_style(state, SemanticColor::Heading)
                .add_modifier(Modifier::BOLD)
                .patch(surface),
        ),
        Span::styled(
            badge,
            semantic_style(
                state,
                if is_active {
                    SemanticColor::Success
                } else {
                    SemanticColor::Muted
                },
            )
            .patch(surface),
        ),
    ]));

    let has_header_gap = area.height >= 13;
    if has_header_gap {
        lines.push(Line::styled("", surface));
    }

    let first_field_row = lines.len();

    for field in ProviderFormField::ALL {
        let selected = focused && form.selected_field() == field;
        let base = if selected {
            modal_selection_style(state, true)
        } else {
            surface
        };
        let marker = if selected { "▌ " } else { "  " };
        let marker_style = if selected {
            semantic_style(state, SemanticColor::Focus)
                .add_modifier(Modifier::BOLD)
                .patch(base)
        } else {
            base
        };

        if field == ProviderFormField::Save {
            if area.height >= 15 {
                lines.push(Line::styled("", surface));
            }
            lines.push(form_action_chips_line(
                state,
                selected,
                show_workbench_chips,
                is_active,
                surface,
            ));
            continue;
        }

        let label = form_field_label(field);
        let label_text = format!("{:<width$}", label, width = label_col_width);
        let label_style = if selected {
            semantic_style(state, SemanticColor::Assistant)
                .add_modifier(Modifier::BOLD)
                .patch(base)
        } else {
            semantic_style(state, SemanticColor::TextSecondary).patch(base)
        };

        let mut spans = vec![
            Span::styled(marker, marker_style),
            Span::styled(label_text, label_style),
        ];

        match field {
            ProviderFormField::Kind => {
                spans.extend(radio_pills(
                    state,
                    &[
                        (
                            "OpenAI-compatible",
                            form.kind() == ManagedProviderKind::OpenAiCompatible,
                        ),
                        ("Anthropic", form.kind() == ManagedProviderKind::Anthropic),
                    ],
                    base,
                ));
            }
            ProviderFormField::Protocol => match form.protocol() {
                Some(protocol) => {
                    spans.extend(radio_pills(
                        state,
                        &[
                            ("Responses", protocol == OpenAiProtocol::Responses),
                            (
                                "Chat Completions",
                                protocol == OpenAiProtocol::ChatCompletions,
                            ),
                        ],
                        base,
                    ));
                }
                None => {
                    spans.push(Span::styled(
                        "Messages",
                        semantic_style(state, SemanticColor::ToolKeyword).patch(base),
                    ));
                }
            },
            ProviderFormField::ApiKey => {
                let copy_label = " [Copy]";
                let show_copy = form.has_copyable_api_key()
                    && value_width >= UnicodeWidthStr::width(copy_label) + 4;
                let key_budget = if show_copy {
                    value_width
                        .saturating_sub(UnicodeWidthStr::width(copy_label))
                        .max(1)
                } else {
                    value_width
                };
                let masked = form.masked_api_key();
                let visible_masked =
                    crate::tui::click_target::truncate_columns(&masked, key_budget);
                let is_saved_placeholder =
                    form.field(ProviderFormField::ApiKey).is_empty() && form.is_editing();
                let val_style = if is_saved_placeholder {
                    semantic_style(state, SemanticColor::Muted).patch(if selected {
                        base
                    } else {
                        input_bg
                    })
                } else {
                    semantic_style(state, SemanticColor::ToolKeyword).patch(if selected {
                        base
                    } else {
                        input_bg
                    })
                };
                spans.push(Span::styled(visible_masked, val_style));
                if show_copy {
                    spans.push(Span::styled(" ", base));
                    let mut chip_style =
                        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
                    if let Some(bg) = state.theme().bg_color(SemanticColor::ChipBackground) {
                        chip_style = chip_style.bg(bg);
                    }
                    spans.push(Span::styled("[Copy]", chip_style));
                }
            }
            ProviderFormField::ServiceTier => {
                let label = form.service_tier_label();
                let is_muted = form.kind() != ManagedProviderKind::OpenAiCompatible
                    || form.service_tier().is_none();
                let is_warning = form.service_tier().is_some_and(|tier| {
                    form.protocol()
                        .is_some_and(|protocol| !protocol.supports_service_tier(tier))
                });
                let slot = if is_warning {
                    SemanticColor::Warning
                } else if is_muted {
                    SemanticColor::Muted
                } else {
                    SemanticColor::ToolKeyword
                };
                spans.push(Span::styled(label, semantic_style(state, slot).patch(base)));
            }
            ProviderFormField::Model => {
                let browse_label = " [Browse ▸]";
                let model_budget = value_width
                    .saturating_sub(UnicodeWidthStr::width(browse_label))
                    .max(1);
                let text = form
                    .field_viewport(field, model_budget)
                    .map(|vp| vp.text)
                    .unwrap_or_default();
                let field_style = semantic_style(state, SemanticColor::ToolKeyword)
                    .patch(if selected { base } else { input_bg });
                spans.push(Span::styled(text, field_style));
                if value_width >= UnicodeWidthStr::width(browse_label) + 4 {
                    spans.push(Span::styled(" ", base));
                    let mut chip_style =
                        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
                    if let Some(bg) = state.theme().bg_color(SemanticColor::ChipBackground) {
                        chip_style = chip_style.bg(bg);
                    }
                    spans.push(Span::styled("[Browse ▸]", chip_style));
                }
            }
            _ => {
                let text = form
                    .field_viewport(field, value_width)
                    .map(|vp| vp.text)
                    .unwrap_or_default();
                let val_style = if field == ProviderFormField::Alias && form.is_editing() {
                    semantic_style(state, SemanticColor::Muted).patch(base)
                } else {
                    semantic_style(state, SemanticColor::ToolKeyword).patch(if selected {
                        base
                    } else {
                        input_bg
                    })
                };
                spans.push(Span::styled(text, val_style));
            }
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines).style(surface), area);

    if focused {
        let selected = form.selected_field();
        if selected != ProviderFormField::Kind
            && selected != ProviderFormField::Protocol
            && selected != ProviderFormField::ApiKey
            && selected != ProviderFormField::ServiceTier
            && selected != ProviderFormField::Save
            && !(selected == ProviderFormField::Alias && form.is_editing())
        {
            let budget = if selected == ProviderFormField::Model {
                value_width.saturating_sub(11).max(1)
            } else {
                value_width
            };
            if let Some(viewport) = form.field_viewport(selected, budget) {
                let row_idx = first_field_row + form_field_index(selected);
                let cursor_y = area
                    .y
                    .saturating_add(u16::try_from(row_idx).unwrap_or(0))
                    .min(area.bottom().saturating_sub(1));
                let cursor_x = area
                    .x
                    .saturating_add(u16::try_from(2 + label_col_width).unwrap_or(18))
                    .saturating_add(u16::try_from(viewport.cursor_column).unwrap_or(0))
                    .min(area.right().saturating_sub(1));
                frame.set_cursor_position(Position::new(cursor_x, cursor_y));
            }
        }
    }
}

fn form_action_chips_line(
    state: &TuiState,
    save_selected: bool,
    show_workbench_chips: bool,
    is_active: bool,
    surface: Style,
) -> Line<'static> {
    let chip_bg = state.theme().bg_color(SemanticColor::ChipBackground);
    let sel_bg = state.theme().bg_color(SemanticColor::SelectionBackground);
    let save_bg = if save_selected {
        sel_bg.or(chip_bg)
    } else {
        chip_bg
    };
    let mut save_style = semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD);
    if let Some(bg) = save_bg {
        save_style = save_style.bg(bg);
    }
    let mut spans = vec![
        Span::styled(
            if save_selected { "▌ " } else { "  " },
            semantic_style(state, SemanticColor::Focus)
                .add_modifier(Modifier::BOLD)
                .patch(surface),
        ),
        Span::styled("[Save provider]", save_style),
    ];
    if show_workbench_chips {
        let mut active_style = semantic_style(
            state,
            if is_active {
                SemanticColor::Success
            } else {
                SemanticColor::Focus
            },
        )
        .add_modifier(Modifier::BOLD);
        if let Some(bg) = chip_bg {
            active_style = active_style.bg(bg);
        }
        spans.push(Span::styled("  ", surface));
        spans.push(Span::styled(
            if is_active {
                "[Active]"
            } else {
                "[Set active]"
            },
            active_style,
        ));
        let mut del_style = semantic_style(
            state,
            if is_active {
                SemanticColor::Muted
            } else {
                SemanticColor::Error
            },
        )
        .add_modifier(Modifier::BOLD);
        if let Some(bg) = chip_bg {
            del_style = del_style.bg(bg);
        }
        spans.push(Span::styled("  ", surface));
        spans.push(Span::styled("[Delete]", del_style));
    }
    Line::from(spans)
}

pub(super) fn render_empty_detail_pane(frame: &mut Frame<'_>, state: &TuiState, area: Rect) {
    let surface = modal_surface_style(state);
    let lines = vec![
        Line::from(Span::styled(
            "No providers configured yet",
            semantic_style(state, SemanticColor::Heading)
                .add_modifier(Modifier::BOLD)
                .patch(surface),
        )),
        Line::styled("", surface),
        Line::from(Span::styled(
            "Press N or click + Add provider to configure your first model provider.",
            semantic_style(state, SemanticColor::TextSecondary).patch(surface),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

fn radio_pills(state: &TuiState, options: &[(&str, bool)], base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, (label, active)) in options.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("   ", base));
        }
        if *active {
            spans.push(Span::styled(
                "(●) ",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(base),
            ));
            spans.push(Span::styled(
                (*label).to_owned(),
                semantic_style(state, SemanticColor::ToolKeyword)
                    .add_modifier(Modifier::BOLD)
                    .patch(base),
            ));
        } else {
            spans.push(Span::styled(
                format!("( ) {label}"),
                semantic_style(state, SemanticColor::Muted).patch(base),
            ));
        }
    }
    spans
}
