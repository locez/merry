use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// JSON-RPC request body used by Streamable HTTP MCP.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct JsonRpcRequest {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

impl JsonRpcRequest {
    pub(crate) fn initialize(id: u64) -> Self {
        Self {
            jsonrpc: "2.0",
            id: Some(id),
            method: "initialize",
            params: Some(json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "merry", "version": env!("CARGO_PKG_VERSION") }
            })),
        }
    }

    pub(crate) fn initialized() -> Self {
        Self {
            jsonrpc: "2.0",
            id: None,
            method: "notifications/initialized",
            params: None,
        }
    }

    pub(crate) fn tools_list(id: u64, cursor: Option<&str>) -> Self {
        Self {
            jsonrpc: "2.0",
            id: Some(id),
            method: "tools/list",
            params: cursor.map(|cursor| json!({ "cursor": cursor })),
        }
    }

    pub(crate) fn tools_call(id: u64, name: &str, arguments: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: Some(id),
            method: "tools/call",
            params: Some(json!({ "name": name, "arguments": arguments })),
        }
    }
}

/// JSON-RPC response envelope.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsonRpcResponse<T> {
    pub(crate) result: Option<T>,
    pub(crate) error: Option<JsonRpcError>,
}

impl<T> JsonRpcResponse<T> {
    pub(crate) fn into_result(self) -> Result<T, JsonRpcError> {
        match (self.result, self.error) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => Err(error),
            (Some(_), Some(_)) | (None, None) => Err(JsonRpcError { code: -32603 }),
        }
    }
}

/// JSON-RPC error object.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsonRpcError {
    pub(crate) code: i64,
}

/// `tools/list` response payload.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ToolsListResult {
    pub(crate) tools: Vec<McpTool>,
    #[serde(default, rename = "nextCursor")]
    pub(crate) next_cursor: Option<String>,
}

/// MCP tool descriptor.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct McpTool {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub(crate) input_schema: Value,
}

/// `tools/call` response payload.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct ToolsCallResult {
    pub(crate) content: Vec<McpContent>,
    #[serde(default, rename = "isError")]
    pub(crate) is_error: Option<bool>,
    #[serde(default, rename = "structuredContent")]
    pub(crate) structured_content: Option<Value>,
}

/// One MCP content item.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct McpContent {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(flatten)]
    pub(crate) extra: serde_json::Map<String, Value>,
}

/// Extracts a JSON-RPC response from a Streamable HTTP SSE response body.
pub(crate) fn parse_sse_json_rpc_response<T>(
    body: &str,
) -> Result<JsonRpcResponse<T>, serde_json::Error>
where
    T: for<'de> Deserialize<'de>,
{
    let mut event_data = String::new();
    for line in body.lines() {
        if line.is_empty() {
            if !event_data.is_empty() {
                if let Ok(response) = serde_json::from_str(&event_data) {
                    return Ok(response);
                }
                event_data.clear();
            }
            continue;
        }
        if let Some(fragment) = line.strip_prefix("data:") {
            if !event_data.is_empty() {
                event_data.push('\n');
            }
            event_data.push_str(fragment.trim_start());
        }
    }

    serde_json::from_str(&event_data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_initialized_notification_without_id() {
        let value = serde_json::to_value(JsonRpcRequest::initialized())
            .expect("initialized notification should serialize");

        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["method"], "notifications/initialized");
        assert!(value.get("id").is_none());
    }

    #[test]
    fn parses_tools_list_response() {
        let response: JsonRpcResponse<ToolsListResult> = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [{
                    "name": "library.resolve",
                    "description": "Resolve a library id",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" }
                        },
                        "required": ["name"]
                    }
                }]
            }
        }))
        .expect("valid tools/list response");

        let result = response.into_result().expect("result");
        assert_eq!(result.tools[0].name, "library.resolve");
        assert_eq!(
            result.tools[0].description.as_deref(),
            Some("Resolve a library id")
        );
    }

    #[test]
    fn parses_tool_call_text_result() {
        let response: JsonRpcResponse<ToolsCallResult> = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "content": [{ "type": "text", "text": "hello" }],
                "isError": false
            }
        }))
        .expect("valid tools/call response");

        let result = response.into_result().expect("result");
        assert_eq!(result.content[0].text.as_deref(), Some("hello"));
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn extracts_json_rpc_response_from_sse_data_line() {
        let response: JsonRpcResponse<ToolsCallResult> = parse_sse_json_rpc_response(
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}\n\n",
        )
        .expect("valid sse response");
        let result = response.into_result().expect("result");
        assert_eq!(result.content[0].text.as_deref(), Some("hello"));
    }
}
