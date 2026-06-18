use crate::{
    McpError, McpServerConfig,
    protocol::{
        JsonRpcRequest, JsonRpcResponse, ToolsCallResult, ToolsListResult,
        parse_sse_json_rpc_response,
    },
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Minimal Streamable HTTP MCP client.
#[derive(Debug)]
pub(crate) struct McpHttpClient {
    server: McpServerConfig,
    http: reqwest::Client,
    next_id: AtomicU64,
    session_id: Mutex<Option<String>>,
}

impl McpHttpClient {
    pub(crate) fn new(server: McpServerConfig) -> Result<Self, McpError> {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TOOL_TIMEOUT)
            .build()
            .map_err(|source| McpError::Http {
                server_id: server.id().to_owned(),
                source,
            })?;
        Ok(Self {
            server,
            http,
            next_id: AtomicU64::new(1),
            session_id: Mutex::new(None),
        })
    }

    pub(crate) async fn initialize(&self) -> Result<(), McpError> {
        let request = JsonRpcRequest::initialize(self.next_request_id());
        let _: Value = self.send_json_rpc(request).await?;
        self.send_notification(JsonRpcRequest::initialized()).await
    }

    pub(crate) async fn list_tools(&self) -> Result<ToolsListResult, McpError> {
        let request = JsonRpcRequest::tools_list(self.next_request_id());
        self.send_json_rpc(request).await
    }

    pub(crate) async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<ToolsCallResult, McpError> {
        let request = JsonRpcRequest::tools_call(self.next_request_id(), name, arguments);
        self.send_json_rpc(request).await
    }

    async fn send_notification(&self, request: JsonRpcRequest) -> Result<(), McpError> {
        self.base_request()
            .json(&request)
            .send()
            .await
            .map_err(|source| McpError::Http {
                server_id: self.server.id().to_owned(),
                source,
            })?
            .error_for_status()
            .map_err(|source| McpError::Http {
                server_id: self.server.id().to_owned(),
                source,
            })?;
        Ok(())
    }

    async fn send_json_rpc<T>(&self, request: JsonRpcRequest) -> Result<T, McpError>
    where
        T: DeserializeOwned,
    {
        let response = self
            .base_request()
            .json(&request)
            .send()
            .await
            .map_err(|source| McpError::Http {
                server_id: self.server.id().to_owned(),
                source,
            })?
            .error_for_status()
            .map_err(|source| McpError::Http {
                server_id: self.server.id().to_owned(),
                source,
            })?;

        if let Some(session_id) = response.headers().get("Mcp-Session-Id")
            && let Ok(session_id) = session_id.to_str()
        {
            *self
                .session_id
                .lock()
                .expect("MCP session id mutex should not be poisoned") =
                Some(session_id.to_owned());
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let is_sse = content_type
            .as_deref()
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let is_json = content_type
            .as_deref()
            .is_none_or(|value| value.starts_with("application/json"));

        let response = if is_sse {
            let body = response.text().await.map_err(|source| McpError::Http {
                server_id: self.server.id().to_owned(),
                source,
            })?;
            parse_sse_json_rpc_response::<T>(&body).map_err(|source| McpError::InvalidJson {
                server_id: self.server.id().to_owned(),
                message: source.to_string(),
            })?
        } else if is_json {
            response
                .json::<JsonRpcResponse<T>>()
                .await
                .map_err(|source| McpError::Http {
                    server_id: self.server.id().to_owned(),
                    source,
                })?
        } else {
            return Err(McpError::UnsupportedContentType {
                server_id: self.server.id().to_owned(),
                content_type,
            });
        };

        response.into_result().map_err(|error| McpError::JsonRpc {
            server_id: self.server.id().to_owned(),
            code: error.code,
            message: error.message,
            data: error.data.map(|data| data.to_string()),
        })
    }

    fn base_request(&self) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .post(self.server.url())
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION);

        for (name, value) in self.server.headers() {
            builder = builder.header(name, value);
        }

        if let Some(session_id) = self
            .session_id
            .lock()
            .expect("MCP session id mutex should not be poisoned")
            .clone()
        {
            builder = builder.header("Mcp-Session-Id", session_id);
        }

        builder
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}
