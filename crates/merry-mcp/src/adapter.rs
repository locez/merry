use crate::{
    connection::{McpConnection, ToolUnavailable},
    diagnostics::{McpFailureKind, McpServerIssue},
    protocol::ToolsCallResult,
};
use merry_core::{ErrorInfo, PendingToolCall, SessionToolCatalogEntry};
use merry_runtime::{
    RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome,
    ToolExecutor, ToolExecutorFuture,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// A registered MCP tool plus its provider-visible Merry name mapping.
#[derive(Debug)]
pub struct McpToolRegistration {
    pub tool: RegisteredTool,
}

impl McpToolRegistration {
    pub(crate) fn new(entry: SessionToolCatalogEntry, binding: McpExecutionBinding) -> Self {
        let executor = Arc::new(McpToolExecutor {
            entry: entry.clone(),
            binding,
        });
        Self {
            tool: RegisteredTool::new(
                entry.spec().clone(),
                executor,
                ToolActionKind::TrustedExternal,
            )
            .with_external_binding(entry.binding().clone()),
        }
    }
}

pub(crate) enum McpExecutionBinding {
    Connection(Arc<McpConnection>),
    Disabled(McpServerIssue),
}

struct McpToolExecutor {
    entry: SessionToolCatalogEntry,
    binding: McpExecutionBinding,
}

impl ToolExecutor for McpToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }
            let connection = match &self.binding {
                McpExecutionBinding::Connection(connection) => connection,
                McpExecutionBinding::Disabled(reason) => {
                    return unavailable_outcome(
                        "mcp_tool_unavailable",
                        &format!(
                            "MCP tool {} is disabled: {reason:?}. Start a new session after correcting configuration.",
                            call.name()
                        ),
                    );
                }
            };
            let client = tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => return Err(ToolExecutionError::Cancelled),
                result = connection.client_for(&self.entry) => match result {
                    Ok(client) => client,
                    Err(ToolUnavailable::Discovery(failure)) => return unavailable_outcome("mcp_server_unavailable", &format!("MCP {} is unavailable: {}. This tool was not executed; use another tool or retry after the connection is restored.", self.entry.binding().source(), failure.kind)),
                    Err(ToolUnavailable::DefinitionChanged) => return unavailable_outcome("mcp_tool_definition_changed", "The MCP tool definition no longer matches this session. The tool was not executed. Start a new session to load the new catalog."),
                },
            };
            let arguments = Value::Object(call.arguments().as_object().clone());
            let result = tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => return Err(ToolExecutionError::Cancelled),
                result = client.call_tool(self.entry.binding().operation().as_str(), arguments) => result,
            };
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let failure = McpFailureKind::from_error(&error);
                    connection.record_call_failure(&client, &error).await;
                    if matches!(
                        failure,
                        McpFailureKind::Connection | McpFailureKind::SessionExpired
                    ) || failure == McpFailureKind::Authentication
                    {
                        return unavailable_outcome(
                            "mcp_server_unavailable",
                            &format!(
                                "MCP {}: {failure}. The tool could not be invoked.",
                                self.entry.binding().source()
                            ),
                        );
                    }
                    return unavailable_outcome(
                        "mcp_tool_outcome_unknown",
                        &format!(
                            "MCP {}: {failure}. The request may have executed but no valid result was received. Do not automatically repeat this operation; verify its effects first.",
                            self.entry.binding().source()
                        ),
                    );
                }
            };
            Ok(mcp_call_result_to_outcome(
                self.entry.binding().source().as_str(),
                self.entry.binding().operation().as_str(),
                result,
            ))
        })
    }
}

fn unavailable_outcome(
    code: &str,
    message: &str,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    let diagnostic = ErrorInfo::new(code, message)
        .map_err(|_| ToolExecutionError::infrastructure("invalid MCP diagnostic"))?;
    Ok(ToolExecutionOutcome::failed_text(message, diagnostic))
}

fn mcp_call_result_to_outcome(
    server_id: &str,
    raw_tool_name: &str,
    result: ToolsCallResult,
) -> ToolExecutionOutcome {
    let is_error = result.is_error.unwrap_or(false);
    if is_error {
        return failed_outcome(server_id, raw_tool_name, result);
    }

    if let Some(structured) = result.structured_content {
        return ToolExecutionOutcome::succeeded_json(
            json!({
                "structuredContent": structured,
                "content": result.content,
            })
            .to_string(),
        );
    }

    let text = result
        .content
        .iter()
        .filter(|content| content.kind == "text")
        .filter_map(|content| content.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    ToolExecutionOutcome::succeeded_text(text)
}

fn failed_outcome(
    server_id: &str,
    raw_tool_name: &str,
    result: ToolsCallResult,
) -> ToolExecutionOutcome {
    let text = result
        .content
        .iter()
        .filter(|content| content.kind == "text")
        .filter_map(|content| content.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    let content = if text.is_empty() {
        json!({
            "server": server_id,
            "tool": raw_tool_name,
            "content": result.content,
            "structuredContent": result.structured_content,
        })
        .to_string()
    } else {
        text
    };
    let diagnostic = ErrorInfo::new(
        "mcp_tool_failed",
        &format!("MCP tool `{raw_tool_name}` on server `{server_id}` returned isError=true"),
    )
    .expect("static MCP diagnostic code is valid");
    ToolExecutionOutcome::failed_text(content, diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{McpContent, ToolsCallResult};
    use merry_core::ToolCallResultStatus;
    use merry_runtime::ArtifactContent;

    #[test]
    fn converts_text_result_to_successful_outcome() {
        let outcome = mcp_call_result_to_outcome(
            "docs",
            "read.file",
            ToolsCallResult {
                content: vec![McpContent {
                    kind: "text".to_owned(),
                    text: Some("hello".to_owned()),
                    extra: Default::default(),
                }],
                is_error: Some(false),
                structured_content: None,
            },
        );

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert_eq!(outcome.content(), &ArtifactContent::text("hello"));
    }
}
