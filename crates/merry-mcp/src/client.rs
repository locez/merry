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
pub(crate) const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Minimal Streamable HTTP MCP client.
pub(crate) struct McpHttpClient {
    server: McpServerConfig,
    http: reqwest::Client,
    next_id: AtomicU64,
    session_id: Mutex<Option<String>>,
}

impl McpHttpClient {
    pub(crate) fn new(server: McpServerConfig) -> Result<Self, McpError> {
        server.validated_identity()?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
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

    pub(crate) async fn list_tools(
        &self,
        cursor: Option<&str>,
    ) -> Result<ToolsListResult, McpError> {
        let request = JsonRpcRequest::tools_list(self.next_request_id(), cursor);
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
        let request_had_session = self.has_session_id()?;
        self.base_request()?
            .json(&request)
            .send()
            .await
            .map_err(|source| self.map_http_error(source, request_had_session))?
            .error_for_status()
            .map_err(|source| self.map_http_error(source, request_had_session))?;
        Ok(())
    }

    async fn send_json_rpc<T>(&self, request: JsonRpcRequest) -> Result<T, McpError>
    where
        T: DeserializeOwned,
    {
        let request_had_session = self.has_session_id()?;
        let response = self
            .base_request()?
            .json(&request)
            .send()
            .await
            .map_err(|source| self.map_http_error(source, request_had_session))?
            .error_for_status()
            .map_err(|source| self.map_http_error(source, request_had_session))?;

        if let Some(session_id) = response.headers().get("Mcp-Session-Id")
            && let Ok(session_id) = session_id.to_str()
        {
            *self
                .session_id
                .lock()
                .map_err(|_| McpError::TransportState {
                    server_id: self.server.id().to_owned(),
                })? = Some(session_id.to_owned());
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

        if !is_sse && !is_json {
            return Err(McpError::UnsupportedContentType {
                server_id: self.server.id().to_owned(),
                content_type: None,
            });
        }
        let body = self.read_bounded_body(response).await?;
        let response = if is_sse {
            let body = String::from_utf8(body).map_err(|_| McpError::InvalidJson {
                server_id: self.server.id().to_owned(),
                message: "response is not UTF-8".to_owned(),
            })?;
            parse_sse_json_rpc_response::<T>(&body).map_err(|source| McpError::InvalidJson {
                server_id: self.server.id().to_owned(),
                message: source.to_string(),
            })?
        } else {
            serde_json::from_slice::<JsonRpcResponse<T>>(&body).map_err(|_| {
                McpError::InvalidJson {
                    server_id: self.server.id().to_owned(),
                    message: "invalid JSON-RPC response".to_owned(),
                }
            })?
        };

        response.into_result().map_err(|error| McpError::JsonRpc {
            server_id: self.server.id().to_owned(),
            code: error.code,
            message: "server returned a JSON-RPC error".to_owned(),
            data: None,
        })
    }

    async fn read_bounded_body(
        &self,
        mut response: reqwest::Response,
    ) -> Result<Vec<u8>, McpError> {
        let oversized = || McpError::ResponseTooLarge {
            server_id: self.server.id().to_owned(),
        };
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(oversized());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|source| McpError::Http {
            server_id: self.server.id().to_owned(),
            source: source.without_url(),
        })? {
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
                return Err(oversized());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn base_request(&self) -> Result<reqwest::RequestBuilder, McpError> {
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
            .map_err(|_| McpError::TransportState {
                server_id: self.server.id().to_owned(),
            })?
            .clone()
        {
            builder = builder.header("Mcp-Session-Id", session_id);
        }

        Ok(builder)
    }

    fn has_session_id(&self) -> Result<bool, McpError> {
        Ok(self
            .session_id
            .lock()
            .map_err(|_| McpError::TransportState {
                server_id: self.server.id().to_owned(),
            })?
            .is_some())
    }

    fn map_http_error(&self, source: reqwest::Error, request_had_session: bool) -> McpError {
        if request_had_session
            && source.status().is_some_and(|status| status.as_u16() == 404)
            && let Ok(mut session_id) = self.session_id.lock()
        {
            *session_id = None;
            return McpError::SessionExpired {
                server_id: self.server.id().to_owned(),
            };
        }
        McpError::Http {
            server_id: self.server.id().to_owned(),
            source: source.without_url(),
        }
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

impl std::fmt::Debug for McpHttpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpClient")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}
