//! Presentation formatting for MCP tool calls (`mcp_search`, `mcp_call`, and
//! historical `mcp_<server>_<tool>` transcript entries).

use crate::tui::{projector::StartedToolView, state::TimelineItem};
use serde::Deserialize;
use serde_json::Value;

const MAX_STORED_PREVIEW_LINES: usize = 6;
const MAX_STORED_LINE_CHARS: usize = 200;
const MAX_INLINE_SERVER_TOOLS: usize = 8;

pub(super) fn parse_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp_")?;
    let (server, tool) = rest.split_once('_')?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct McpSearchDisplay {
    query: Option<String>,
    server: Option<String>,
    refresh: bool,
}

pub(super) fn parse_mcp_search_display(
    arguments: &serde_json::Map<String, Value>,
) -> McpSearchDisplay {
    McpSearchDisplay {
        query: arguments
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(crate::text::without_control_chars),
        server: arguments
            .get("server")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(crate::text::without_control_chars),
        refresh: arguments
            .get("refresh")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[derive(Debug, Deserialize)]
struct McpSearchServerView {
    server: String,
    status: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    tool_count: usize,
    #[serde(default)]
    tools: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct McpSearchToolView {
    server: String,
    tool: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct McpSearchOutputView {
    #[serde(default)]
    servers: Vec<McpSearchServerView>,
    #[serde(default)]
    tools: Vec<McpSearchToolView>,
    #[serde(default)]
    total_matches: usize,
}

/// Formats completed MCP tool calls (`mcp_search`, `mcp_call`, and historical
/// `mcp_<server>_<tool>` transcript items) as clean single-line collapsible
/// rows without raw JSON dumps or noisy `-> succeeded` suffixes.
pub(super) fn completed_mcp_view(tool: &StartedToolView, output: &str) -> Option<TimelineItem> {
    let name = tool.name.as_str();
    let elapsed_suffix = format_elapsed_suffix(tool.started_at.map(|started| started.elapsed()));
    if name == merry_mcp::MCP_SEARCH_TOOL {
        let (title, body) = format_completed_mcp_search(tool, output, &elapsed_suffix);
        return Some(TimelineItem::Expanded {
            title,
            body,
            expanded: false,
        });
    }
    if name == merry_mcp::MCP_CALL_TOOL || parse_mcp_tool_name(name).is_some() {
        let title = format!(
            "{}{elapsed_suffix}",
            super::tool_output::expanded_tool_title(tool)
        );
        let body = format_mcp_call_body(output);
        return Some(TimelineItem::Expanded {
            title,
            body,
            expanded: false,
        });
    }
    None
}

fn format_completed_mcp_search(
    tool: &StartedToolView,
    output: &str,
    elapsed_suffix: &str,
) -> (String, String) {
    let Ok(parsed) = serde_json::from_str::<McpSearchOutputView>(output) else {
        return (
            format!(
                "{}{elapsed_suffix}",
                super::tool_output::expanded_tool_title(tool)
            ),
            format_mcp_call_body(output),
        );
    };
    let search = tool.mcp_search.as_ref();
    let query = search.and_then(|item| item.query.as_deref());
    let server = search.and_then(|item| item.server.as_deref());
    let refresh = search.is_some_and(|item| item.refresh);

    let verb = if refresh {
        "refreshed"
    } else if query.is_some() {
        "searched"
    } else {
        "listed"
    };
    let subject = match (server, query) {
        (Some(server), Some(query)) => {
            format!("{verb} {server} · \"{}\"", compact_inline(query, 48))
        }
        (None, Some(query)) => format!("{verb} \"{}\"", compact_inline(query, 48)),
        (Some(server), None) => format!("{verb} {server}"),
        (None, None) => match parsed.servers.as_slice() {
            [only] => format!(
                "{verb} {}",
                crate::text::without_control_chars(&only.server)
            ),
            servers => format!("{verb} {} servers", servers.len()),
        },
    };

    let tool_count = if query.is_some() {
        parsed.total_matches
    } else {
        parsed.servers.iter().map(|entry| entry.tool_count).sum()
    };
    let offline_count = parsed
        .servers
        .iter()
        .filter(|entry| entry.status != "ready")
        .count();
    let tool_noun = if tool_count == 1 { "tool" } else { "tools" };
    let summary = if offline_count > 0 {
        format!(" ({tool_count} {tool_noun}, {offline_count} offline)")
    } else {
        format!(" ({tool_count} {tool_noun})")
    };

    let mut body_lines = Vec::new();
    for entry in parsed
        .servers
        .iter()
        .filter(|entry| entry.status != "ready")
    {
        let reason = entry
            .error
            .as_deref()
            .map(crate::text::without_control_chars)
            .unwrap_or_else(|| entry.status.clone());
        body_lines.push(format!(
            "{} · unavailable ({reason})",
            crate::text::without_control_chars(&entry.server)
        ));
    }
    if !parsed.tools.is_empty() {
        for item in &parsed.tools {
            let server_id = crate::text::without_control_chars(&item.server);
            let tool_id = crate::text::without_control_chars(&item.tool);
            let description = crate::text::without_control_chars(&item.description);
            let description = description.trim();
            if description.is_empty() {
                body_lines.push(format!("{server_id}/{tool_id}"));
            } else {
                body_lines.push(format!(
                    "{server_id}/{tool_id} — {}",
                    compact_inline(description, 110)
                ));
            }
        }
    } else {
        for entry in parsed
            .servers
            .iter()
            .filter(|entry| entry.status == "ready")
        {
            let server_id = crate::text::without_control_chars(&entry.server);
            let noun = if entry.tool_count == 1 {
                "tool"
            } else {
                "tools"
            };
            if entry.tools.is_empty() {
                body_lines.push(format!("{server_id} · ready (0 tools)"));
            } else {
                let shown = entry.tools.len().min(MAX_INLINE_SERVER_TOOLS);
                let preview = entry
                    .tools
                    .iter()
                    .take(shown)
                    .map(|name| crate::text::without_control_chars(name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let remaining = entry.tool_count.saturating_sub(shown);
                let suffix = if remaining > 0 {
                    format!(", +{remaining} more")
                } else {
                    String::new()
                };
                body_lines.push(format!(
                    "{server_id} · {} {noun}: {preview}{suffix}",
                    entry.tool_count
                ));
            }
        }
    }

    (
        format!("MCP {subject}{summary}{elapsed_suffix}"),
        bounded_preview_body(body_lines.iter().map(String::as_str)),
    )
}

fn format_mcp_call_body(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(lines) = format_mcp_structured_envelope(&value) {
            return bounded_preview_body(lines.iter().map(String::as_str));
        }
        if matches!(value, Value::Object(_) | Value::Array(_))
            && let Ok(pretty) = serde_json::to_string_pretty(&value)
        {
            return bounded_preview_body(pretty.lines());
        }
    }
    let cleaned = crate::text::without_control_chars_keeping_newlines(trimmed);
    bounded_preview_body(cleaned.lines())
}

fn format_mcp_structured_envelope(value: &Value) -> Option<Vec<String>> {
    let object = value.as_object()?;
    let structured = object.get("structuredContent");
    let content = object.get("content").and_then(Value::as_array);
    if structured.is_none() && content.is_none() {
        return None;
    }
    let mut lines = Vec::new();
    if let Some(items) = content {
        for item in items {
            if item.get("type").and_then(Value::as_str) == Some("text")
                && let Some(text) = item.get("text").and_then(Value::as_str)
            {
                let cleaned = crate::text::without_control_chars_keeping_newlines(text);
                lines.extend(
                    cleaned
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned),
                );
            }
        }
    }
    if let Some(structured) = structured.filter(|value| !value.is_null())
        && let Ok(pretty) = serde_json::to_string_pretty(structured)
    {
        lines.extend(pretty.lines().map(str::to_owned));
    }
    (!lines.is_empty()).then_some(lines)
}

/// Retains up to 6 non-empty lines so `ToolOutputPreview` (which displays 5 lines)
/// preserves exact truncation detection (`...`) without storing giant bodies.
fn bounded_preview_body<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    lines
        .filter(|line| !line.trim().is_empty())
        .take(MAX_STORED_PREVIEW_LINES)
        .map(|line| compact_inline(line, MAX_STORED_LINE_CHARS))
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact_inline(text: &str, max_chars: usize) -> String {
    let clean = crate::text::without_control_chars(text);
    let mut chars = clean.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn format_elapsed_suffix(elapsed: Option<std::time::Duration>) -> String {
    match elapsed.filter(|elapsed| elapsed.as_secs() >= 1) {
        Some(elapsed) => format!("  {:.1}s", elapsed.as_secs_f64()),
        None => String::new(),
    }
}
