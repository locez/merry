use super::{
    overlay_render::{centered_rect, inset, render_surface, selection_style, semantic_style},
    provider_overlay::{
        ModelPickerOverlay, ProviderFormField, ProviderFormOverlay, ProviderManagerOverlay,
    },
    reasoning_picker::ReasoningPickerOverlay,
    state::TuiState,
    theme::SemanticColor,
};
use crate::config::{ConfiguredProviderKind, ManagedProviderKind, ProviderConfigSource};
use merry_provider_openai::OpenAiProtocol;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position},
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};

const PROVIDER_OVERLAY_WIDTH: u16 = 82;

pub(super) fn render_provider_manager(
    frame: &mut Frame<'_>,
    state: &TuiState,
    manager: &ProviderManagerOverlay,
) {
    let height = (manager.items().len() as u16 + 7).clamp(12, 22);
    let region = centered_rect(frame.area(), PROVIDER_OVERLAY_WIDTH, height);
    render_surface(frame, state, region, " M  Providers ");
    let inner = inset(region, 2, 1);
    let mut lines = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{:<22}", "Provider"),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<19}", "Protocol"),
            semantic_style(state, SemanticColor::Muted),
        ),
        Span::styled(
            format!("{:<9}", "Source"),
            semantic_style(state, SemanticColor::Muted),
        ),
        Span::styled("Model", semantic_style(state, SemanticColor::Muted)),
    ])];
    let visible_height = usize::from(inner.height.saturating_sub(2)).max(1);
    let offset = manager
        .selected()
        .saturating_sub(visible_height.saturating_sub(1));
    for (index, item) in manager
        .items()
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_height)
    {
        let selected = index == manager.selected();
        let base = selection_style(state, selected);
        let active = manager.current_alias() == Some(item.alias());
        let kind = match item.kind() {
            ConfiguredProviderKind::OpenAiCompatible => match item.protocol() {
                Some(OpenAiProtocol::Responses) => "Responses",
                Some(OpenAiProtocol::ChatCompletions) => "Chat completions",
                None => "OpenAI-compatible",
            },
            ConfiguredProviderKind::Anthropic => "Anthropic",
        };
        let source = match item.source() {
            ProviderConfigSource::User => "config",
            ProviderConfigSource::Managed => "managed",
        };
        let model = match (active, item.model()) {
            (true, Some(model)) => format!("active · {model}"),
            (true, None) => "active".to_owned(),
            (false, Some(model)) => model.to_owned(),
            (false, None) => String::new(),
        };
        lines.push(Line::from(vec![
            Span::styled(if selected { "▌ " } else { "  " }, base),
            Span::styled(
                format!("{:<22}", item.display_name()),
                base.add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{kind:<19}"), base),
            Span::styled(
                format!("{source:<9}"),
                semantic_style(state, SemanticColor::ToolKeyword).patch(base),
            ),
            Span::styled(
                model,
                semantic_style(state, SemanticColor::Success).patch(base),
            ),
        ]));
    }
    if manager.items().is_empty() {
        lines.push(Line::from(Span::styled(
            "No providers",
            semantic_style(state, SemanticColor::Muted),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        manager.notice().unwrap_or_else(|| {
            if manager.selected_source() == Some(ProviderConfigSource::User) {
                "config.toml provider · connection settings are read-only"
            } else {
                ""
            }
        }),
        semantic_style(state, SemanticColor::Muted),
    )));
    let actions: &[(&str, &str)] =
        if manager.selected_source() == Some(ProviderConfigSource::Managed) {
            &[
                ("N", "Add"),
                ("Enter", "Switch"),
                ("M", "Models"),
                ("E", "Edit"),
                ("D", "Delete"),
                ("Esc", "Back"),
            ]
        } else {
            &[
                ("N", "Add"),
                ("Enter", "Switch"),
                ("M", "Models"),
                ("Esc", "Back"),
            ]
        };
    lines.push(action_hint_line(state, actions));
    frame.render_widget(Paragraph::new(lines), inner);
}

pub(super) fn render_provider_form(
    frame: &mut Frame<'_>,
    state: &TuiState,
    form: &ProviderFormOverlay,
) {
    let region = centered_rect(frame.area(), PROVIDER_OVERLAY_WIDTH, 20);
    render_surface(frame, state, region, form.title());
    let inner = inset(region, 2, 1);
    let value_width = inner.width.saturating_sub(24).max(1) as usize;
    let lines = ProviderFormField::ALL
        .iter()
        .map(|field| {
            let selected = form.selected_field() == *field;
            let base = selection_style(state, selected);
            if *field == ProviderFormField::Save {
                return Line::from(vec![
                    Span::styled(if selected { "▌ " } else { "  " }, base),
                    Span::styled(
                        "Save provider",
                        semantic_style(state, SemanticColor::Status).patch(base),
                    ),
                ]);
            }
            let label = form_field_label(*field);
            let value = match field {
                ProviderFormField::Kind => match form.kind() {
                    ManagedProviderKind::OpenAiCompatible => "OpenAI-compatible".to_owned(),
                    ManagedProviderKind::Anthropic => "Anthropic".to_owned(),
                },
                ProviderFormField::Protocol => match form.protocol() {
                    Some(OpenAiProtocol::Responses) => "Responses".to_owned(),
                    Some(OpenAiProtocol::ChatCompletions) => "Chat Completions".to_owned(),
                    None => "Messages".to_owned(),
                },
                ProviderFormField::ApiKey => form.masked_api_key(),
                ProviderFormField::ReasoningEffort => form
                    .field_viewport(*field, value_width)
                    .map(|viewport| viewport.text)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_default(),
                _ => form
                    .field_viewport(*field, value_width)
                    .map(|viewport| viewport.text)
                    .unwrap_or_default(),
            };
            Line::from(vec![
                Span::styled(if selected { "▌ " } else { "  " }, base),
                Span::styled(format!("{label:<20}"), base),
                Span::styled(
                    value,
                    if *field == ProviderFormField::Alias && form.is_editing() {
                        semantic_style(state, SemanticColor::Muted).patch(base)
                    } else {
                        semantic_style(state, SemanticColor::ToolKeyword).patch(base)
                    },
                ),
            ])
        })
        .chain([
            Line::default(),
            Line::from(Span::styled(
                form.notice().unwrap_or(""),
                semantic_style(state, SemanticColor::Muted),
            )),
            action_hint_line(
                state,
                match form.selected_field() {
                    ProviderFormField::Model => {
                        &[("Enter", "Models"), ("Ctrl+S", "Save"), ("Esc", "Back")]
                    }
                    ProviderFormField::ReasoningEffort => &[
                        ("Ctrl+Space", "Presets"),
                        ("Ctrl+S", "Save"),
                        ("Esc", "Back"),
                    ],
                    ProviderFormField::Save => &[("Enter", "Save"), ("Esc", "Back")],
                    _ => &[
                        ("Tab", "Next"),
                        ("Left/Right", "Change"),
                        ("Ctrl+S", "Save"),
                        ("Esc", "Back"),
                    ],
                },
            ),
        ])
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);

    let selected = form.selected_field();
    if selected != ProviderFormField::Kind
        && selected != ProviderFormField::Protocol
        && selected != ProviderFormField::ApiKey
        && !(selected == ProviderFormField::Alias && form.is_editing())
        && let Some(viewport) = form.field_viewport(selected, value_width)
    {
        frame.set_cursor_position(Position::new(
            inner
                .x
                .saturating_add(22)
                .saturating_add(viewport.cursor_column as u16)
                .min(inner.right().saturating_sub(1)),
            inner.y.saturating_add(form_field_index(selected) as u16),
        ));
    }
}

pub(super) fn render_model_picker(
    frame: &mut Frame<'_>,
    state: &TuiState,
    picker: &ModelPickerOverlay,
) {
    let region = centered_rect(frame.area(), PROVIDER_OVERLAY_WIDTH, 22);
    render_surface(frame, state, region, " M  Models ");
    let inner = inset(region, 2, 1);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                picker.display_name().to_owned(),
                semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", picker.alias()),
                semantic_style(state, SemanticColor::Muted),
            ),
        ])),
        rows[0],
    );
    let query = picker.query_viewport(rows[1].width.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("M ", semantic_style(state, SemanticColor::Focus)),
            Span::styled(
                if picker.query().is_empty() {
                    "Search models".to_owned()
                } else {
                    query.text.clone()
                },
                if picker.query().is_empty() {
                    semantic_style(state, SemanticColor::Muted)
                } else {
                    semantic_style(state, SemanticColor::Assistant)
                },
            ),
        ])),
        rows[1],
    );
    let visible = picker.visible_models();
    let list_height = usize::from(rows[2].height).max(1);
    let offset = picker
        .selected()
        .saturating_sub(list_height.saturating_sub(1));
    let mut lines = visible
        .iter()
        .enumerate()
        .skip(offset)
        .take(list_height)
        .map(|(index, model)| {
            let base = selection_style(state, index == picker.selected());
            Line::from(vec![
                Span::styled(
                    if index == picker.selected() {
                        "▌ "
                    } else {
                        "  "
                    },
                    base,
                ),
                Span::styled(
                    format!("{:<44}", model.id()),
                    base.add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    model.owner().unwrap_or("").to_owned(),
                    semantic_style(state, SemanticColor::ToolKeyword).patch(base),
                ),
            ])
        })
        .collect::<Vec<_>>();
    if lines.is_empty()
        && let Some(manual) = picker.manual_model()
    {
        lines.push(Line::from(vec![
            Span::styled("▌ ", selection_style(state, true)),
            Span::styled(
                manual.to_owned(),
                selection_style(state, true).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), rows[2]);
    let status = if picker.is_loading() {
        "Refreshing"
    } else {
        picker.error().unwrap_or("")
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                status,
                if picker.error().is_some() {
                    semantic_style(state, SemanticColor::Error)
                } else {
                    semantic_style(state, SemanticColor::Muted)
                },
            )),
            action_hint_line(
                state,
                &[("Enter", "Use"), ("F5", "Refresh"), ("Esc", "Back")],
            ),
        ]),
        rows[3],
    );
    frame.set_cursor_position(Position::new(
        rows[1]
            .x
            .saturating_add(2)
            .saturating_add(query.cursor_column as u16)
            .min(rows[1].right().saturating_sub(1)),
        rows[1].y,
    ));
}

pub(super) fn render_reasoning_picker(
    frame: &mut Frame<'_>,
    state: &TuiState,
    picker: &ReasoningPickerOverlay,
) {
    let region = centered_rect(frame.area(), PROVIDER_OVERLAY_WIDTH, 20);
    render_surface(frame, state, region, " M  Thinking mode ");
    let inner = inset(region, 2, 1);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    picker.model().to_owned(),
                    semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", picker.alias()),
                    semantic_style(state, SemanticColor::Muted),
                ),
            ]),
            Line::from(Span::styled(
                "Choose a thinking mode for this model",
                semantic_style(state, SemanticColor::Muted),
            )),
        ]),
        rows[0],
    );

    if let Some(editor) = picker.custom_editor() {
        let viewport = editor.viewport(rows[1].width.saturating_sub(10) as usize);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Custom ", semantic_style(state, SemanticColor::Focus)),
                Span::styled(
                    viewport.text.clone(),
                    semantic_style(state, SemanticColor::Assistant),
                ),
            ])),
            rows[1],
        );
        let status = picker
            .error()
            .unwrap_or("Enter a provider-supported reasoning identifier");
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    status,
                    if picker.error().is_some() {
                        semantic_style(state, SemanticColor::Error)
                    } else {
                        semantic_style(state, SemanticColor::Muted)
                    },
                )),
                action_hint_line(state, &[("Enter", "Use"), ("Esc", "Back")]),
            ]),
            rows[2],
        );
        frame.set_cursor_position(Position::new(
            rows[1]
                .x
                .saturating_add(7)
                .saturating_add(viewport.cursor_column as u16)
                .min(rows[1].right().saturating_sub(1)),
            rows[1].y,
        ));
        return;
    }

    let lines = (0..picker.option_count())
        .map(|index| {
            let selected = index == picker.selected();
            let base = selection_style(state, selected);
            Line::from(vec![
                Span::styled(if selected { "▌ " } else { "  " }, base),
                Span::styled(
                    picker.option_label(index),
                    base.add_modifier(Modifier::BOLD),
                ),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), rows[1]);
    frame.render_widget(
        Paragraph::new(action_hint_line(
            state,
            &[("Enter", "Use"), ("Esc", "Back")],
        )),
        rows[2],
    );
}

fn form_field_label(field: ProviderFormField) -> &'static str {
    match field {
        ProviderFormField::DisplayName => "Provider name",
        ProviderFormField::Alias => "Config alias",
        ProviderFormField::Kind => "API type",
        ProviderFormField::Protocol => "API protocol",
        ProviderFormField::BaseUrl => "Base URL",
        ProviderFormField::ApiKey => "API key",
        ProviderFormField::Model => "Initial model",
        ProviderFormField::ReasoningEffort => "Thinking mode",
        ProviderFormField::Save => "Save provider",
    }
}

fn action_hint_line<'a>(state: &TuiState, actions: &[(&str, &str)]) -> Line<'a> {
    let mut spans = Vec::new();
    for (index, (key, label)) in actions.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                "   ",
                semantic_style(state, SemanticColor::Muted),
            ));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            semantic_style(state, SemanticColor::Muted),
        ));
    }
    Line::from(spans)
}

fn form_field_index(field: ProviderFormField) -> usize {
    ProviderFormField::ALL
        .iter()
        .position(|candidate| *candidate == field)
        .unwrap_or(0)
}
