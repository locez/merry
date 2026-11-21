use crate::config::HeaderItemKind;
use crate::tui::theme::SemanticColor;
use merry_core::{ContextWindowSource, SessionUsage};
use std::path::Path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) const HEADER_BRAND_MARK: &str = crate::tui::glyphs::BRAND_PAW;
const BRAND_AND_SEPARATORS_WIDTH: usize = 11;
const MIN_WORKSPACE_WIDTH: usize = 8;

/// A styled segment produced for the configurable top header bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderSegment {
    pub(crate) kind: HeaderItemKind,
    pub(crate) text: String,
    pub(crate) color: SemanticColor,
    pub(crate) bold: bool,
    pub(crate) underlined: bool,
    pub(crate) dim: bool,
}

/// Resolved header bar layout shared by both rendering and click hit-testing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderLayout {
    pub(crate) left_pad: u16,
    pub(crate) segments: Vec<HeaderSegment>,
    pub(crate) model_range: Option<(u16, u16)>,
}

/// Builds the configurable header segments with semantic coloring and graceful
/// narrow-width degradation while tracking the exact clickable model range.
pub(crate) fn build_header_layout(
    workspace: &Path,
    model: &str,
    usage: Option<&SessionUsage>,
    rate: &str,
    active_run: bool,
    configured_items: &[HeaderItemKind],
    width: u16,
) -> HeaderLayout {
    let total_width = usize::from(width);
    let outer_pad = if width >= 80 {
        2usize
    } else if width >= 24 {
        1usize
    } else {
        0usize
    };
    let inner_width = total_width.saturating_sub(outer_pad.saturating_mul(2));
    let brand_width = display_width(HEADER_BRAND_MARK);

    let items = if configured_items.is_empty() {
        &HeaderItemKind::DEFAULT_ITEMS[..]
    } else {
        configured_items
    };

    let want_workspace = items.contains(&HeaderItemKind::Workspace);
    let want_model = items.contains(&HeaderItemKind::Model);
    let telemetry_kinds: Vec<HeaderItemKind> = items
        .iter()
        .copied()
        .filter(|kind| {
            matches!(
                kind,
                HeaderItemKind::Context
                    | HeaderItemKind::Cache
                    | HeaderItemKind::Rate
                    | HeaderItemKind::Tokens
            )
        })
        .collect();

    let workspace_raw = workspace.display().to_string();
    let min_ws = if want_workspace {
        display_width(&workspace_raw).min(MIN_WORKSPACE_WIDTH)
    } else {
        0
    };
    let model_width = if want_model { display_width(model) } else { 0 };

    // Build candidate telemetry segments in configured order, then drop lower-priority
    // segments on narrow widths so context pressure is always preserved first.
    let mut telemetry_segments =
        build_telemetry_segments(usage, rate, active_run, &telemetry_kinds);
    let fixed_left = brand_width
        + if want_workspace { 2 + min_ws } else { 0 }
        + if want_model { 2 + model_width } else { 0 };

    while telemetry_segments.len() > 1
        && fixed_left + 2 + telemetry_display_width(&telemetry_segments) > inner_width
    {
        // Drop lowest priority telemetry first: Tokens -> Rate -> Cache -> Context.
        if let Some(pos) = telemetry_segments
            .iter()
            .rposition(|seg| seg.kind == HeaderItemKind::Tokens)
            .or_else(|| {
                telemetry_segments
                    .iter()
                    .rposition(|seg| seg.kind == HeaderItemKind::Rate)
            })
            .or_else(|| {
                telemetry_segments
                    .iter()
                    .rposition(|seg| seg.kind == HeaderItemKind::Cache)
            })
        {
            telemetry_segments.remove(pos);
        } else {
            telemetry_segments.pop();
        }
    }

    let telemetry_width = if telemetry_segments.is_empty() {
        0
    } else {
        2 + telemetry_display_width(&telemetry_segments)
    };
    let remaining_for_ws_model = inner_width
        .saturating_sub(brand_width)
        .saturating_sub(telemetry_width);

    let mut segments = Vec::new();
    let mut cursor_x = outer_pad + brand_width;
    let mut model_range = None;

    let (ws_text, model_text) = match (want_workspace, want_model) {
        (true, true) => {
            let budget = remaining_for_ws_model.saturating_sub(4);
            let reserved_ws = budget.min(min_ws);
            let model_budget = budget.saturating_sub(reserved_ws).min(model_width);
            let fitted_model = compact_middle(model, model_budget);
            let ws_budget = budget.saturating_sub(display_width(&fitted_model));
            let fitted_ws = compact_middle(&workspace_raw, ws_budget);
            (Some(fitted_ws), Some(fitted_model))
        }
        (true, false) => {
            let budget = remaining_for_ws_model.saturating_sub(2);
            (Some(compact_middle(&workspace_raw, budget)), None)
        }
        (false, true) => {
            let budget = remaining_for_ws_model.saturating_sub(2);
            (None, Some(compact_middle(model, budget)))
        }
        (false, false) => (None, None),
    };

    if let Some(ws) = ws_text
        && !ws.is_empty()
    {
        cursor_x += 2;
        cursor_x += display_width(&ws);
        segments.push(HeaderSegment {
            kind: HeaderItemKind::Workspace,
            text: ws,
            color: SemanticColor::Command,
            bold: false,
            underlined: false,
            dim: false,
        });
    }

    if let Some(model_str) = model_text
        && !model_str.is_empty()
    {
        cursor_x += 2;
        let start = u16::try_from(cursor_x).unwrap_or(u16::MAX);
        let w = u16::try_from(display_width(&model_str)).unwrap_or(0);
        if w > 0 {
            model_range = Some((start, w));
        }
        segments.push(HeaderSegment {
            kind: HeaderItemKind::Model,
            text: model_str,
            color: SemanticColor::ToolKeyword,
            bold: true,
            underlined: true,
            dim: false,
        });
    }

    segments.extend(telemetry_segments);

    HeaderLayout {
        left_pad: u16::try_from(outer_pad).unwrap_or(0),
        segments,
        model_range,
    }
}

fn build_telemetry_segments(
    usage: Option<&SessionUsage>,
    rate: &str,
    active_run: bool,
    kinds: &[HeaderItemKind],
) -> Vec<HeaderSegment> {
    let mut segments = Vec::new();
    let Some(usage) = usage else {
        if kinds.contains(&HeaderItemKind::Context) {
            segments.push(HeaderSegment {
                kind: HeaderItemKind::Context,
                text: "usage -".to_owned(),
                color: SemanticColor::TextSecondary,
                bold: false,
                underlined: false,
                dim: true,
            });
        }
        if kinds.contains(&HeaderItemKind::Rate) {
            segments.push(HeaderSegment {
                kind: HeaderItemKind::Rate,
                text: rate.to_owned(),
                color: if active_run {
                    SemanticColor::ToolKeyword
                } else {
                    SemanticColor::TextSecondary
                },
                bold: false,
                underlined: false,
                dim: !active_run,
            });
        }
        return segments;
    };

    for &kind in kinds {
        match kind {
            HeaderItemKind::Context => {
                let color = context_pressure_color(usage);
                segments.push(HeaderSegment {
                    kind,
                    text: format_context_pressure(usage),
                    color,
                    bold: matches!(color, SemanticColor::Warning | SemanticColor::Error),
                    underlined: false,
                    dim: false,
                });
            }
            HeaderItemKind::Cache => {
                if let Some((text, percent)) = format_cache_ratio_with_percent(
                    usage.last.input_tokens(),
                    usage.last.cached_input_tokens(),
                ) {
                    let color = if percent >= 60 {
                        SemanticColor::Success
                    } else if percent >= 25 {
                        SemanticColor::TextSecondary
                    } else {
                        SemanticColor::Muted
                    };
                    segments.push(HeaderSegment {
                        kind,
                        text,
                        color,
                        bold: false,
                        underlined: false,
                        dim: false,
                    });
                }
            }
            HeaderItemKind::Rate => {
                segments.push(HeaderSegment {
                    kind,
                    text: rate.to_owned(),
                    color: if active_run {
                        SemanticColor::ToolKeyword
                    } else {
                        SemanticColor::TextSecondary
                    },
                    bold: false,
                    underlined: false,
                    dim: false,
                });
            }
            HeaderItemKind::Tokens => {
                let text = format!(
                    "in {} out {} · tot {}",
                    format_token_count(usage.last.input_tokens()),
                    format_token_count(usage.last.output_tokens()),
                    format_token_count(usage.total.total_tokens()),
                );
                segments.push(HeaderSegment {
                    kind,
                    text,
                    color: SemanticColor::Muted,
                    bold: false,
                    underlined: false,
                    dim: false,
                });
            }
            HeaderItemKind::Workspace | HeaderItemKind::Model => {}
        }
    }
    segments
}

fn context_pressure_color(usage: &SessionUsage) -> SemanticColor {
    let Some(context) = usage.context else {
        return SemanticColor::TextSecondary;
    };
    if context.effective_window_tokens == 0 {
        return SemanticColor::TextSecondary;
    }
    let percent =
        u128::from(usage.last.input_tokens()) * 100 / u128::from(context.effective_window_tokens);
    if percent >= 90 {
        SemanticColor::Error
    } else if percent >= 70 {
        SemanticColor::Warning
    } else {
        SemanticColor::TextSecondary
    }
}

fn telemetry_display_width(segments: &[HeaderSegment]) -> usize {
    if segments.is_empty() {
        return 0;
    }
    let text_sum: usize = segments.iter().map(|seg| display_width(&seg.text)).sum();
    text_sum + segments.len().saturating_sub(1) * 3 // " · "
}

pub(crate) fn format_session_usage_full(usage: Option<&SessionUsage>, rate: &str) -> String {
    usage
        .map(|usage| format_session_usage(usage, rate))
        .unwrap_or_else(|| SessionUsageDisplay::unavailable(rate))
        .full
}

#[allow(dead_code)]
pub(crate) fn format_header_status_parts(
    workspace: &Path,
    model: &str,
    usage: Option<&SessionUsage>,
    rate: &str,
    width: u16,
) -> [String; 3] {
    let workspace = workspace.display().to_string();
    let usage = usage
        .map(|usage| format_session_usage(usage, rate))
        .unwrap_or_else(|| SessionUsageDisplay::unavailable(rate));
    let width = usize::from(width);
    let minimum_workspace_width = display_width(&workspace).min(MIN_WORKSPACE_WIDTH);
    let model_width = display_width(model);
    let usage = usage
        .variants()
        .find(|candidate| {
            BRAND_AND_SEPARATORS_WIDTH
                + minimum_workspace_width
                + model_width
                + display_width(candidate)
                <= width
        })
        .unwrap_or(usage.minimal.as_str())
        .to_owned();

    let remaining = width.saturating_sub(BRAND_AND_SEPARATORS_WIDTH + display_width(&usage));
    let reserved_workspace = remaining.min(minimum_workspace_width);
    let model_budget = remaining
        .saturating_sub(reserved_workspace)
        .min(model_width);
    let model = compact_middle(model, model_budget);
    let workspace_budget = remaining.saturating_sub(display_width(&model));
    let workspace = compact_middle(&workspace, workspace_budget);

    [workspace, model, usage]
}

struct SessionUsageDisplay {
    full: String,
    medium: String,
    compact: String,
    minimal: String,
}

impl SessionUsageDisplay {
    fn unavailable(rate: &str) -> Self {
        Self {
            full: format!("usage - · {rate}"),
            medium: format!("usage - · {rate}"),
            compact: format!("usage - · {rate}"),
            minimal: "usage -".to_owned(),
        }
    }

    fn variants(&self) -> impl Iterator<Item = &str> {
        [
            self.full.as_str(),
            self.medium.as_str(),
            self.compact.as_str(),
            self.minimal.as_str(),
        ]
        .into_iter()
    }
}

fn format_session_usage(usage: &SessionUsage, rate: &str) -> SessionUsageDisplay {
    let minimal = format_context_pressure(usage);
    let compact = format!("{minimal} · {rate}");
    let cache = format_cache_ratio(usage.last.input_tokens(), usage.last.cached_input_tokens());
    let medium = cache.as_ref().map_or_else(
        || compact.clone(),
        |cache| format!("{minimal} · {cache} · {rate}"),
    );

    let mut context_parts = vec![minimal.clone()];
    if let Some(context) = usage.context {
        context_parts.push(format!(
            "win {} {}",
            format_token_count(context.resolved_model_window_tokens),
            context_window_source_label(context.source)
        ));
    }
    if let Some(cache) = cache {
        context_parts.push(cache);
    }
    let full = format!(
        "{} | last in {} out {} | total {} tok | {rate}",
        context_parts.join(" · "),
        format_token_count(usage.last.input_tokens()),
        format_token_count(usage.last.output_tokens()),
        format_token_count(usage.total.total_tokens())
    );

    SessionUsageDisplay {
        full,
        medium,
        compact,
        minimal,
    }
}

fn format_context_pressure(usage: &SessionUsage) -> String {
    let mut context = usage.context.map_or_else(
        || "ctx -".to_owned(),
        |context| {
            format!(
                "ctx {}/{}",
                format_token_count(usage.last.input_tokens()),
                format_token_count(context.effective_window_tokens)
            )
        },
    );
    if usage
        .compaction
        .is_some_and(|compaction| !compaction.auto_compaction_enabled)
    {
        context.push_str(" · compact off");
    }
    context
}

fn format_cache_ratio(input_tokens: u64, cached_input_tokens: Option<u64>) -> Option<String> {
    format_cache_ratio_with_percent(input_tokens, cached_input_tokens).map(|(text, _)| text)
}

fn format_cache_ratio_with_percent(
    input_tokens: u64,
    cached_input_tokens: Option<u64>,
) -> Option<(String, u128)> {
    let cached_input_tokens = cached_input_tokens?;
    if input_tokens == 0 {
        return None;
    }
    let percent = ((u128::from(cached_input_tokens) * 100 + u128::from(input_tokens / 2))
        / u128::from(input_tokens))
    .min(100);
    Some((format!("cache {percent}%"), percent))
}

fn context_window_source_label(source: ContextWindowSource) -> &'static str {
    match source {
        ContextWindowSource::ExplicitConfig => "config",
        ContextWindowSource::ProviderCapabilities => "provider",
        ContextWindowSource::BundledCatalog => "catalog",
        ContextWindowSource::Fallback => "fallback",
    }
}

fn compact_middle(value: &str, max_width: usize) -> String {
    if display_width(value) <= max_width {
        return value.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_owned();
    }

    let prefix_width = (max_width - 1) / 2;
    let suffix_width = max_width - prefix_width - 1;
    let prefix = take_prefix_to_width(value, prefix_width);
    let suffix = take_suffix_to_width(value, suffix_width);
    format!("{prefix}…{suffix}")
}

fn take_prefix_to_width(value: &str, max_width: usize) -> String {
    let mut width = 0;
    value
        .chars()
        .take_while(|character| {
            let character_width = UnicodeWidthChar::width(*character).unwrap_or(0);
            if width + character_width > max_width {
                return false;
            }
            width += character_width;
            true
        })
        .collect()
}

fn take_suffix_to_width(value: &str, max_width: usize) -> String {
    let mut width = 0;
    let mut suffix = value
        .chars()
        .rev()
        .take_while(|character| {
            let character_width = UnicodeWidthChar::width(*character).unwrap_or(0);
            if width + character_width > max_width {
                return false;
            }
            width += character_width;
            true
        })
        .collect::<Vec<_>>();
    suffix.reverse();
    suffix.into_iter().collect()
}

fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

fn format_token_count(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }

    let rounded_tenths = tokens.saturating_add(50) / 100;
    let whole = rounded_tenths / 10;
    let decimal = rounded_tenths % 10;
    if decimal == 0 {
        format!("{whole}k")
    } else {
        format!("{whole}.{decimal}k")
    }
}
