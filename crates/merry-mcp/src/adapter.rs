use crate::{
    McpError, McpResult, McpServerConfig,
    client::McpHttpClient,
    map_mcp_tool_names,
    protocol::{McpTool, ToolsCallResult},
};
use merry_core::{ErrorInfo, PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use merry_runtime::{
    RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome,
    ToolExecutor, ToolExecutorFuture,
};
use schemars::Schema;
use serde_json::{Map, Value, json};
use std::{collections::BTreeSet, sync::Arc};

/// A registered MCP tool plus its provider-visible Merry name mapping.
#[derive(Debug)]
pub struct McpToolRegistration {
    pub tool: RegisteredTool,
    pub server_id: String,
    pub raw_tool_name: String,
}

/// Discovers tools exposed by configured MCP servers.
pub async fn discover_mcp_tools(
    servers: &[McpServerConfig],
) -> McpResult<Vec<McpToolRegistration>> {
    let mut registrations = Vec::new();
    for server in servers {
        let client = Arc::new(McpHttpClient::new(server.clone())?);
        client.initialize().await?;
        let tools = filter_allowed_tools(client.list_tools().await?.tools, server.tools());
        let raw_names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        let mappings = map_mcp_tool_names(server.id(), &raw_names)?;

        for (tool, mapping) in tools.into_iter().zip(mappings) {
            let spec = mcp_tool_to_spec(server.id(), &tool, mapping.merry_name.clone())?;
            let executor = Arc::new(McpToolExecutor {
                client: Arc::clone(&client),
                server_id: server.id().to_owned(),
                raw_tool_name: mapping.raw_name.clone(),
            });
            registrations.push(McpToolRegistration {
                tool: RegisteredTool::new(spec, executor, ToolActionKind::TrustedExternal),
                server_id: server.id().to_owned(),
                raw_tool_name: mapping.raw_name,
            });
        }
    }
    Ok(registrations)
}

fn filter_allowed_tools(tools: Vec<McpTool>, allowlist: Option<&[String]>) -> Vec<McpTool> {
    let Some(allowlist) = allowlist else {
        return tools;
    };
    let allowed = allowlist
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    tools
        .into_iter()
        .filter(|tool| allowed.contains(tool.name.as_str()))
        .collect()
}

fn mcp_tool_to_spec(server_id: &str, tool: &McpTool, merry_name: ToolName) -> McpResult<ToolSpec> {
    let normalized_schema = normalize_mcp_input_schema(tool.input_schema.clone());
    let schema =
        Schema::try_from(normalized_schema).map_err(|error| McpError::InvalidToolSchema {
            server_id: server_id.to_owned(),
            tool_name: tool.name.clone(),
            message: error.to_string(),
        })?;
    let input_schema =
        ToolInputSchema::new(schema).map_err(|error| McpError::InvalidToolSchema {
            server_id: server_id.to_owned(),
            tool_name: tool.name.clone(),
            message: error.to_string(),
        })?;
    ToolSpec::new(
        merry_name,
        tool.description
            .as_deref()
            .unwrap_or("MCP tool exposed by a configured server"),
        input_schema,
    )
    .map_err(|error| McpError::InvalidToolSchema {
        server_id: server_id.to_owned(),
        tool_name: tool.name.clone(),
        message: error.to_string(),
    })
}

fn normalize_mcp_input_schema(schema: Value) -> Value {
    let mut object = match schema {
        Value::Object(object) => object,
        _ => Map::new(),
    };

    object
        .entry("type")
        .or_insert_with(|| Value::String("object".to_owned()));
    if !matches!(object.get("properties"), Some(Value::Object(_))) {
        object.insert("properties".to_owned(), Value::Object(Map::new()));
    }

    Value::Object(object)
}

struct McpToolExecutor {
    client: Arc<McpHttpClient>,
    server_id: String,
    raw_tool_name: String,
}

impl ToolExecutor for McpToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let arguments = Value::Object(call.arguments().as_object().clone());
            let result = self
                .client
                .call_tool(&self.raw_tool_name, arguments)
                .await
                .map_err(|source| ToolExecutionError::Infrastructure {
                    message: format!(
                        "MCP tool `{}` on server `{}` failed: {source}",
                        self.raw_tool_name, self.server_id
                    ),
                })?;
            Ok(mcp_call_result_to_outcome(
                &self.server_id,
                &self.raw_tool_name,
                result,
            ))
        })
    }
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
    use serde_json::json;

    #[test]
    fn normalizes_object_schema_without_properties() {
        let schema = normalize_mcp_input_schema(json!({ "type": "object" }));

        assert_eq!(schema["properties"], json!({}));
    }

    #[test]
    fn normalizes_null_properties_to_empty_object() {
        let schema = normalize_mcp_input_schema(json!({
            "type": "object",
            "properties": null
        }));

        assert_eq!(schema["properties"], json!({}));
    }

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
