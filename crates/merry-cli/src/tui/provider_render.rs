//! Two-column Master-Detail Provider Workbench plus unified ModalFrame
//! renderers and hit-testers for Model Picker and Reasoning Picker.

mod pickers;
mod workbench;
mod workbench_detail;
mod workbench_pointer;

pub(crate) use pickers::{model_picker_pointer, reasoning_pointer};
pub(super) use pickers::{render_model_picker, render_reasoning_picker};
pub(super) use workbench::{render_provider_form, render_provider_manager};
pub(crate) use workbench_pointer::{provider_form_pointer, provider_manager_pointer};

use super::{
    modal_frame::{ModalRects, modal_rects},
    overlay_render::semantic_style,
    provider_overlay::ProviderFormField,
    state::TuiState,
    theme::SemanticColor,
};
use crate::config::ConfiguredProviderKind;
use merry::providers::OpenAiProtocol;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
};

pub(super) const WORKBENCH_WIDTH: u16 = 104;
pub(super) const WORKBENCH_HEIGHT: u16 = 24;
pub(super) const PICKER_WIDTH: u16 = 84;
pub(super) const TWO_COLUMN_MIN_INNER: u16 = 66;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelPointer {
    Row(usize),
    Manual,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderWorkbenchPointer {
    Close,
    SelectProvider(usize),
    AddProvider,
    Field(ProviderFormField),
    CopyApiKey,
    BrowseModels,
    SetActive,
    DeleteSelected,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct WorkbenchLayout {
    pub(super) modal: ModalRects,
    pub(super) two_column: bool,
    pub(super) left_pane: Rect,
    pub(super) divider: Rect,
    pub(super) right_pane: Rect,
    pub(super) footer_divider: Rect,
    pub(super) footer: Rect,
}

pub(super) fn workbench_layout(area: Rect) -> WorkbenchLayout {
    let modal = modal_rects(area, WORKBENCH_WIDTH, WORKBENCH_HEIGHT);
    let inner = modal.inner;
    if inner.width == 0 || inner.height == 0 {
        return WorkbenchLayout {
            modal,
            two_column: false,
            left_pane: Rect::default(),
            divider: Rect::default(),
            right_pane: Rect::default(),
            footer_divider: Rect::default(),
            footer: Rect::default(),
        };
    }

    let has_footer_divider = inner.height >= 8;
    let footer_height: u16 = if inner.height >= 6 { 2 } else { 1 };
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if has_footer_divider {
            vec![
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(footer_height),
            ]
        } else {
            vec![
                Constraint::Min(1),
                Constraint::Length(0),
                Constraint::Length(footer_height),
            ]
        })
        .split(inner);

    let main = v_chunks[0];
    let footer_divider = v_chunks[1];
    let footer = v_chunks[2];

    if main.width >= TWO_COLUMN_MIN_INNER {
        let left_width = (main.width / 3).clamp(22, 30);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(left_width),
                Constraint::Length(3),
                Constraint::Min(1),
            ])
            .split(main);
        let divider = Rect::new(cols[1].x + 1, cols[1].y, 1, cols[1].height);
        WorkbenchLayout {
            modal,
            two_column: true,
            left_pane: cols[0],
            divider,
            right_pane: cols[2],
            footer_divider,
            footer,
        }
    } else {
        WorkbenchLayout {
            modal,
            two_column: false,
            left_pane: main,
            divider: Rect::default(),
            right_pane: main,
            footer_divider,
            footer,
        }
    }
}

pub(super) fn protocol_display_label(
    kind: ConfiguredProviderKind,
    protocol: Option<OpenAiProtocol>,
) -> &'static str {
    match kind {
        ConfiguredProviderKind::OpenAiCompatible => match protocol {
            Some(OpenAiProtocol::Responses) => "Responses",
            Some(OpenAiProtocol::ChatCompletions) => "Chat completions",
            None => "OpenAI-compatible",
        },
        ConfiguredProviderKind::Anthropic => "Anthropic",
    }
}

pub(super) fn form_field_label(field: ProviderFormField) -> &'static str {
    match field {
        ProviderFormField::DisplayName => "Provider name",
        ProviderFormField::Alias => "Config alias",
        ProviderFormField::Kind => "API type",
        ProviderFormField::Protocol => "API protocol",
        ProviderFormField::BaseUrl => "Base URL",
        ProviderFormField::ApiKey => "API key",
        ProviderFormField::Model => "Initial model",
        ProviderFormField::ReasoningEffort => "Thinking mode",
        ProviderFormField::ServiceTier => "Service tier",
        ProviderFormField::Save => "Save provider",
    }
}

pub(super) fn action_hint_line<'a>(
    state: &TuiState,
    actions: &[(&str, &str)],
    surface: Style,
) -> Line<'a> {
    let mut spans = Vec::new();
    for (index, (key, label)) in actions.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                "   ",
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            semantic_style(state, SemanticColor::Focus)
                .add_modifier(Modifier::BOLD)
                .patch(surface),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            semantic_style(state, SemanticColor::Muted).patch(surface),
        ));
    }
    Line::from(spans)
}

pub(super) fn form_field_index(field: ProviderFormField) -> usize {
    ProviderFormField::ALL
        .iter()
        .position(|candidate| *candidate == field)
        .unwrap_or(0)
}
