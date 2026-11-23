use crate::{
    adapter::{mcp_call_result_to_outcome, unavailable_outcome},
    catalog::DiscoveredMcpTool,
    connection::{CallLookupError, McpConnection, ServerToolsState},
    diagnostics::McpFailureKind,
};
use futures_util::{StreamExt, stream};
use merry_core::{ErrorInfo, ToolSourceId};
use merry_runtime::{ToolExecutionError, ToolExecutionOutcome};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(crate) const DEFAULT_SEARCH_LIMIT: usize = 10;
pub(crate) const MAX_SEARCH_LIMIT: usize = 50;
const MAX_INLINE_SERVER_TOOL_NAMES: usize = 50;
const MAX_CONCURRENT_REFRESHES: usize = 4;

/// Dynamic runtime registry of configured MCP servers shared by `mcp_search`
/// and `mcp_call`.
#[derive(Debug)]
pub(crate) struct McpRegistry {
    connections: BTreeMap<ToolSourceId, Arc<McpConnection>>,
}

impl McpRegistry {
    pub(crate) fn new(connections: BTreeMap<ToolSourceId, Arc<McpConnection>>) -> Self {
        Self { connections }
    }

    /// Returns configured server IDs in deterministic sorted order.
    pub(crate) fn server_ids(&self) -> Vec<&str> {
        self.connections.keys().map(ToolSourceId::as_str).collect()
    }

    fn connection(&self, server_id: &str) -> Option<Arc<McpConnection>> {
        self.connections
            .iter()
            .find(|(id, _)| id.as_str() == server_id)
            .map(|(_, connection)| Arc::clone(connection))
    }

    /// Searches or lists live MCP tools across configured servers.
    pub(crate) async fn search(
        &self,
        query: Option<&str>,
        server: Option<&str>,
        limit: usize,
        refresh: bool,
    ) -> Result<Value, String> {
        let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
        let targets: Vec<Arc<McpConnection>> = if let Some(server_id) = server {
            let Some(connection) = self.connection(server_id) else {
                return Err(format!(
                    "unknown MCP server `{server_id}`. Configured servers: {}",
                    self.server_ids().join(", ")
                ));
            };
            vec![connection]
        } else {
            self.connections.values().cloned().collect()
        };

        let snapshots = stream::iter(targets)
            .map(|connection| async move {
                let id = connection.source().as_str().to_owned();
                let state = connection.tools_snapshot(refresh).await;
                (id, state)
            })
            .buffer_unordered(MAX_CONCURRENT_REFRESHES)
            .collect::<BTreeMap<_, _>>()
            .await;

        let mut server_summaries = Vec::with_capacity(snapshots.len());
        let mut all_tools: Vec<(&str, &DiscoveredMcpTool)> = Vec::new();
        for (server_id, state) in &snapshots {
            match state {
                ServerToolsState::Ready { tools } => {
                    let listed_names = tools
                        .iter()
                        .take(MAX_INLINE_SERVER_TOOL_NAMES)
                        .map(|tool| tool.name.as_str())
                        .collect::<Vec<_>>();
                    let mut summary = json!({
                        "server": server_id,
                        "status": "ready",
                        "tool_count": tools.len(),
                        "tools": listed_names,
                    });
                    if tools.len() > MAX_INLINE_SERVER_TOOL_NAMES
                        && let Some(object) = summary.as_object_mut()
                    {
                        object.insert("tools_truncated".to_owned(), Value::Bool(true));
                    }
                    server_summaries.push(summary);
                    for tool in tools {
                        all_tools.push((server_id.as_str(), tool));
                    }
                }
                ServerToolsState::Unavailable { failure } => {
                    server_summaries.push(json!({
                        "server": server_id,
                        "status": "unavailable",
                        "stage": format!("{:?}", failure.stage),
                        "error": failure.kind.to_string(),
                        "tool_count": 0,
                        "tools": [],
                    }));
                }
            }
        }

        let trimmed_query = query.map(str::trim).filter(|query| !query.is_empty());
        let (matched_tools, total_matches, hint) = if let Some(query) = trimmed_query {
            let parsed_query = ParsedQuery::new(query);
            let mut scored = all_tools
                .iter()
                .filter_map(|&(server_id, tool)| {
                    let score = score_tool_match(&parsed_query, server_id, tool);
                    (score > 0).then_some((score, server_id, tool))
                })
                .collect::<Vec<_>>();
            scored.sort_by(|left, right| {
                right
                    .0
                    .cmp(&left.0)
                    .then_with(|| left.1.cmp(right.1))
                    .then_with(|| left.2.name.cmp(&right.2.name))
            });
            let total = scored.len();
            let items = scored
                .into_iter()
                .take(limit)
                .map(|(_, server_id, tool)| tool_json(server_id, tool))
                .collect::<Vec<_>>();
            (items, total, None)
        } else if server.is_some() || all_tools.len() <= limit {
            let total = all_tools.len();
            let items = all_tools
                .into_iter()
                .take(limit)
                .map(|(server_id, tool)| tool_json(server_id, tool))
                .collect::<Vec<_>>();
            (items, total, None)
        } else {
            (
                Vec::new(),
                all_tools.len(),
                Some(
                    "Pass `query` or `server` to retrieve full tool descriptions and input schemas.",
                ),
            )
        };

        let mut response = json!({
            "servers": server_summaries,
            "tools": matched_tools,
            "total_matches": total_matches,
        });
        if let Some(hint) = hint
            && let Some(object) = response.as_object_mut()
        {
            object.insert("hint".to_owned(), Value::String(hint.to_owned()));
        }
        Ok(response)
    }

    /// Invokes one tool on a configured MCP server after live schema validation.
    pub(crate) async fn call(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: Map<String, Value>,
        cancellation: CancellationToken,
    ) -> Result<ToolExecutionOutcome, ToolExecutionError> {
        if cancellation.is_cancelled() {
            return Err(ToolExecutionError::Cancelled);
        }
        let Some(connection) = self.connection(server_id) else {
            return unavailable_outcome(
                "mcp_server_not_found",
                &format!(
                    "Unknown MCP server `{server_id}`. Configured servers: {}",
                    self.server_ids().join(", ")
                ),
            );
        };

        let mut resolved =
            match resolve_call(&connection, server_id, tool_name, false, &cancellation).await? {
                Ok(resolved) => resolved,
                Err(outcome) => return Ok(outcome),
            };

        // Validate against the cached schema; if validation fails on a non-refreshed
        // entry, re-fetch the server's live catalog once so silent schema updates
        // take effect without restarting the session.
        let arguments = match resolved.tool.validate_arguments(arguments) {
            Ok(valid) => valid,
            Err((arguments, _)) if !resolved.refreshed => {
                resolved =
                    match resolve_call(&connection, server_id, tool_name, true, &cancellation)
                        .await?
                    {
                        Ok(fresh) => fresh,
                        Err(outcome) => return Ok(outcome),
                    };
                match resolved.tool.validate_arguments(arguments) {
                    Ok(valid) => valid,
                    Err((_, validation_error)) => {
                        return invalid_arguments_outcome(
                            server_id,
                            &resolved.tool,
                            &validation_error,
                        );
                    }
                }
            }
            Err((_, validation_error)) => {
                return invalid_arguments_outcome(server_id, &resolved.tool, &validation_error);
            }
        };

        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ToolExecutionError::Cancelled),
            result = resolved.client.call_tool(&resolved.tool.name, arguments) => result,
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let failure = McpFailureKind::from_error(&error);
                connection
                    .record_call_failure(&resolved.client, &error)
                    .await;
                if matches!(
                    failure,
                    McpFailureKind::Connection
                        | McpFailureKind::SessionExpired
                        | McpFailureKind::Authentication
                ) {
                    return unavailable_outcome(
                        "mcp_server_unavailable",
                        &format!("MCP {server_id}: {failure}. The tool could not be invoked."),
                    );
                }
                return unavailable_outcome(
                    "mcp_tool_outcome_unknown",
                    &format!(
                        "MCP {server_id}: {failure}. The request may have executed but no valid result was received. Do not automatically repeat this operation; verify its effects first."
                    ),
                );
            }
        };

        Ok(mcp_call_result_to_outcome(
            server_id,
            &resolved.tool.name,
            result,
        ))
    }
}

async fn resolve_call(
    connection: &McpConnection,
    server_id: &str,
    tool_name: &str,
    force_refresh: bool,
    cancellation: &CancellationToken,
) -> Result<Result<crate::connection::ResolvedToolCall, ToolExecutionOutcome>, ToolExecutionError> {
    let lookup = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(ToolExecutionError::Cancelled),
        result = connection.resolve_for_call(tool_name, force_refresh) => result,
    };
    match lookup {
        Ok(resolved) => Ok(Ok(resolved)),
        Err(CallLookupError::ToolDisallowed) => Ok(Err(unavailable_outcome(
            "mcp_tool_disallowed",
            &format!(
                "MCP tool `{tool_name}` on server `{server_id}` is not allowed by configuration."
            ),
        )?)),
        Err(CallLookupError::Unavailable(failure)) => Ok(Err(unavailable_outcome(
            "mcp_server_unavailable",
            &format!(
                "MCP {server_id} is unavailable: {}. This tool was not executed; use mcp_search with refresh=true or retry once the server is restored.",
                failure.kind
            ),
        )?)),
        Err(CallLookupError::ToolNotFound { available_tools }) => {
            let preview = if available_tools.is_empty() {
                "none".to_owned()
            } else {
                available_tools
                    .iter()
                    .take(20)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            Ok(Err(unavailable_outcome(
                "mcp_tool_not_found",
                &format!(
                    "MCP tool `{tool_name}` was not found on server `{server_id}` after refreshing its catalog. Available tools: {preview}."
                ),
            )?))
        }
    }
}

fn invalid_arguments_outcome(
    server_id: &str,
    tool: &DiscoveredMcpTool,
    validation_error: &str,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    let payload = json!({
        "error": format!(
            "Arguments for `{server_id}/{}` failed schema validation: {validation_error}",
            tool.name
        ),
        "server": server_id,
        "tool": tool.name,
        "inputSchema": tool.input_schema,
    })
    .to_string();
    let diagnostic = ErrorInfo::new(
        "mcp_invalid_arguments",
        &format!(
            "MCP arguments failed validation for `{server_id}/{}`",
            tool.name
        ),
    )
    .map_err(|_| ToolExecutionError::infrastructure("invalid MCP diagnostic"))?;
    Ok(ToolExecutionOutcome::failed_text(payload, diagnostic))
}

struct ParsedQuery {
    lower: String,
    tokens: Vec<String>,
}

impl ParsedQuery {
    fn new(query: &str) -> Self {
        let lower = query.to_ascii_lowercase();
        let tokens = tokenize(&lower);
        Self { lower, tokens }
    }
}

fn tool_json(server_id: &str, tool: &DiscoveredMcpTool) -> Value {
    json!({
        "server": server_id,
        "tool": tool.name,
        "description": tool.description,
        "inputSchema": tool.input_schema,
    })
}

fn score_tool_match(query: &ParsedQuery, server_id: &str, tool: &DiscoveredMcpTool) -> u32 {
    let name_lower = tool.name.to_ascii_lowercase();
    let server_lower = server_id.to_ascii_lowercase();
    let qualified = format!("{server_lower}/{name_lower}");

    let mut score = 0u32;
    if query.lower == name_lower || query.lower == qualified {
        score += 100;
    } else if name_lower.contains(&query.lower) || qualified.contains(&query.lower) {
        score += 30;
    }

    if query.tokens.is_empty() {
        return score;
    }

    let name_tokens = tokenize(&tool.name);
    let server_tokens = tokenize(server_id);
    let description_lower = tool.description.to_ascii_lowercase();
    let description_tokens = tokenize(&tool.description);
    let param_tokens = tool
        .parameter_names()
        .into_iter()
        .flat_map(tokenize)
        .collect::<Vec<_>>();

    let mut matched_tokens = 0usize;
    for token in &query.tokens {
        let mut token_hit = false;
        if name_tokens.iter().any(|candidate| candidate == token) {
            score += 16;
            token_hit = true;
        } else if name_lower.contains(token.as_str()) {
            score += 10;
            token_hit = true;
        }
        if server_tokens.iter().any(|candidate| candidate == token)
            || server_lower.contains(token.as_str())
        {
            score += 8;
            token_hit = true;
        }
        if param_tokens.iter().any(|candidate| candidate == token) {
            score += 6;
            token_hit = true;
        }
        if description_tokens
            .iter()
            .any(|candidate| candidate == token)
            || description_lower.contains(token.as_str())
        {
            score += 4;
            token_hit = true;
        }
        if token_hit {
            matched_tokens += 1;
        }
    }

    if matched_tokens == query.tokens.len() {
        score += 20;
    }

    score
}

/// Splits identifiers on non-alphanumeric separators and camelCase boundaries.
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut prev_was_lower = false;

    for ch in input.chars() {
        if !ch.is_alphanumeric() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            prev_was_lower = false;
            continue;
        }
        if prev_was_lower && ch.is_uppercase() && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
        for lower in ch.to_lowercase() {
            current.push(lower);
        }
        prev_was_lower = ch.is_lowercase();
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}
