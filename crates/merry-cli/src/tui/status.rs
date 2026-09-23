use merry_core::{ContextWindowSource, SessionUsage};
use std::path::Path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const BRAND_AND_SEPARATORS_WIDTH: usize = 11;
const MIN_WORKSPACE_WIDTH: usize = 8;

pub(crate) fn format_session_usage_full(usage: Option<&SessionUsage>, rate: &str) -> String {
    usage
        .map(|usage| format_session_usage(usage, rate))
        .unwrap_or_else(|| SessionUsageDisplay::unavailable(rate))
        .full
}

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
    let cached_input_tokens = cached_input_tokens?;
    if input_tokens == 0 {
        return None;
    }
    let percent = ((u128::from(cached_input_tokens) * 100 + u128::from(input_tokens / 2))
        / u128::from(input_tokens))
    .min(100);
    Some(format!("cache {percent}%"))
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
