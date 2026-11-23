use crate::{McpError, McpServerConfig};
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::{
    ClientHandler, RoleClient,
    model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, ErrorCode,
        Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    },
    service::{
        ClientInitializeError, ClientLifecycleMode, RunningService, ServiceError,
        serve_client_with_lifecycle_and_ct,
    },
    transport::{
        DynamicTransportError,
        streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig, StreamableHttpError,
        },
    },
};
use std::{collections::HashMap, time::Duration};
use tokio_util::sync::{CancellationToken, DropGuard};

const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ERROR_RESPONSE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Legacy `initialize` protocol versions tried in order when a version-strict
/// server rejects `2025-11-25` outright instead of counter-offering its own
/// supported version.
const LEGACY_FALLBACK_VERSIONS: &[ProtocolVersion] = &[
    ProtocolVersion::V_2025_06_18,
    ProtocolVersion::V_2025_03_26,
    ProtocolVersion::V_2024_11_05,
];

#[derive(Clone)]
struct MerryClientHandler {
    protocol_version: ProtocolVersion,
}

impl ClientHandler for MerryClientHandler {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("merry", env!("CARGO_PKG_VERSION")),
        )
        .with_protocol_version(self.protocol_version.clone())
    }
}

/// Streamable HTTP MCP client backed by the official `rmcp` SDK with dynamic
/// protocol-version negotiation (`2026-07-28` `server/discover`, `2025-11-25`
/// `initialize` counter-offer, and automatic version-strict legacy fallback).
pub(crate) struct McpHttpClient {
    server: McpServerConfig,
    service: RunningService<RoleClient, MerryClientHandler>,
    // Cancels `rmcp`'s background transport worker when this client is dropped.
    _cancel_on_drop: DropGuard,
}

impl McpHttpClient {
    pub(crate) async fn connect(server: McpServerConfig) -> Result<Self, McpError> {
        server.validated_identity()?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(DEFAULT_TOOL_TIMEOUT)
            .build()
            .map_err(|source| McpError::Http {
                server_id: server.id().to_owned(),
                source,
            })?;
        let custom_headers = build_custom_headers(&server)?;

        // Phase 1: `Auto` probes `server/discover` for `2026-07-28` and falls back
        // to `initialize` naming `2025-11-25` (accepting any server counter-offer).
        let auto_lifecycle = ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::V_2025_11_25),
        };
        let initial_error = match try_connect(
            &http,
            &server,
            &custom_headers,
            ProtocolVersion::V_2025_11_25,
            auto_lifecycle,
        )
        .await
        {
            Ok(client) => return Ok(client),
            Err(error) => error,
        };

        // Phase 2: if a version-strict legacy server rejected `initialize(2025-11-25)`
        // instead of counter-offering, retry older initialize-era revisions in order.
        if should_retry_older_legacy_version(&initial_error) {
            for version in LEGACY_FALLBACK_VERSIONS {
                tracing::debug!(
                    server = %server.id(),
                    protocol_version = %version,
                    "retrying MCP initialize with older protocol version"
                );
                match try_connect(
                    &http,
                    &server,
                    &custom_headers,
                    version.clone(),
                    ClientLifecycleMode::Initialize,
                )
                .await
                {
                    Ok(client) => return Ok(client),
                    Err(error) if should_retry_older_legacy_version(&error) => continue,
                    Err(error) => return Err(map_initialize_error(server.id(), *error)),
                }
            }
        }

        Err(map_initialize_error(server.id(), *initial_error))
    }

    pub(crate) async fn list_tools(
        &self,
        cursor: Option<String>,
    ) -> Result<ListToolsResult, McpError> {
        let params = cursor.map(|cursor| {
            let mut params = PaginatedRequestParams::default();
            params.cursor = Some(cursor);
            params
        });
        self.service
            .peer()
            .list_tools(params)
            .await
            .map_err(|error| map_service_error(self.server.id(), error))
    }

    pub(crate) async fn call_tool(
        &self,
        name: &str,
        arguments: JsonObject,
    ) -> Result<CallToolResult, McpError> {
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        match tokio::time::timeout(DEFAULT_TOOL_TIMEOUT, self.service.peer().call_tool(params))
            .await
        {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(map_service_error(self.server.id(), error)),
            Err(_) => Err(McpError::Timeout {
                server_id: self.server.id().to_owned(),
            }),
        }
    }
}

async fn try_connect(
    http: &reqwest::Client,
    server: &McpServerConfig,
    custom_headers: &HashMap<HeaderName, HeaderValue>,
    client_protocol_version: ProtocolVersion,
    lifecycle: ClientLifecycleMode,
) -> Result<McpHttpClient, Box<ClientInitializeError>> {
    let transport_config = StreamableHttpClientTransportConfig::with_uri(server.url())
        .custom_headers(custom_headers.clone())
        .max_json_response_size(MAX_RESPONSE_BYTES)
        .max_sse_event_size(MAX_RESPONSE_BYTES)
        .max_error_response_size(MAX_ERROR_RESPONSE_BYTES)
        .reinit_on_expired_session(false);
    let transport = StreamableHttpClientTransport::with_client(http.clone(), transport_config);
    let ct = CancellationToken::new();
    let cancel_on_drop = ct.clone().drop_guard();
    let handler = MerryClientHandler {
        protocol_version: client_protocol_version,
    };
    let service = serve_client_with_lifecycle_and_ct(handler, transport, lifecycle, ct)
        .await
        .map_err(Box::new)?;
    Ok(McpHttpClient {
        server: server.clone(),
        service,
        _cancel_on_drop: cancel_on_drop,
    })
}

fn build_custom_headers(
    server: &McpServerConfig,
) -> Result<HashMap<HeaderName, HeaderValue>, McpError> {
    let mut headers = HashMap::with_capacity(server.headers().len());
    for (name, value) in server.headers() {
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            McpError::InvalidConfiguration {
                reason: "invalid MCP HTTP header name",
            }
        })?;
        let header_value =
            HeaderValue::from_str(value).map_err(|_| McpError::InvalidConfiguration {
                reason: "invalid MCP HTTP header value",
            })?;
        headers.insert(header_name, header_value);
    }
    Ok(headers)
}

fn should_retry_older_legacy_version(error: &ClientInitializeError) -> bool {
    match error {
        ClientInitializeError::LegacyFallbackFailed { fallback, .. } => {
            should_retry_older_legacy_version(fallback)
        }
        ClientInitializeError::JsonRpcError(data) => matches!(
            data.code,
            ErrorCode::INVALID_PARAMS | ErrorCode::UNSUPPORTED_PROTOCOL_VERSION
        ),
        ClientInitializeError::TransportError { error, .. } => matches!(
            map_dynamic_transport_error("", error),
            McpError::HttpStatus { status: 400, .. }
        ),
        _ => false,
    }
}

fn map_initialize_error(server_id: &str, error: ClientInitializeError) -> McpError {
    match error {
        ClientInitializeError::LegacyFallbackFailed { fallback, .. } => {
            map_initialize_error(server_id, *fallback)
        }
        ClientInitializeError::TransportError { error, .. } => {
            map_dynamic_transport_error(server_id, &error)
        }
        ClientInitializeError::JsonRpcError(data) => McpError::JsonRpc {
            server_id: server_id.to_owned(),
            code: i64::from(data.code.0),
            message: "server returned a JSON-RPC error".to_owned(),
            data: None,
        },
        ClientInitializeError::Cancelled => McpError::TransportState {
            server_id: server_id.to_owned(),
        },
        ClientInitializeError::ConnectionClosed(_) => McpError::Connection {
            server_id: server_id.to_owned(),
        },
        ClientInitializeError::ExpectedInitResponse(_)
        | ClientInitializeError::ExpectedInitResult(_)
        | ClientInitializeError::ConflictInitResponseId(_, _)
        | ClientInitializeError::UncorrelatedErrorResponse { .. }
        | ClientInitializeError::NoCompatibleProtocolVersion { .. }
        | ClientInitializeError::NoPreferredProtocolVersion
        | _ => McpError::InvalidJson {
            server_id: server_id.to_owned(),
            message: "invalid or unsupported MCP handshake response".to_owned(),
        },
    }
}

fn map_service_error(server_id: &str, error: ServiceError) -> McpError {
    match error {
        ServiceError::McpError(data) => McpError::JsonRpc {
            server_id: server_id.to_owned(),
            code: i64::from(data.code.0),
            message: "server returned a JSON-RPC error".to_owned(),
            data: None,
        },
        ServiceError::TransportSend(transport_error) => {
            map_dynamic_transport_error(server_id, &transport_error)
        }
        ServiceError::TransportClosed => McpError::Connection {
            server_id: server_id.to_owned(),
        },
        ServiceError::Timeout { .. } => McpError::Timeout {
            server_id: server_id.to_owned(),
        },
        ServiceError::Cancelled { .. } => McpError::TransportState {
            server_id: server_id.to_owned(),
        },
        ServiceError::UnexpectedResponse
        | ServiceError::SubscriptionLagged { .. }
        | ServiceError::InputRequiredRoundsExceeded { .. }
        | _ => McpError::InvalidJson {
            server_id: server_id.to_owned(),
            message: "unexpected MCP response".to_owned(),
        },
    }
}

fn map_dynamic_transport_error(server_id: &str, error: &DynamicTransportError) -> McpError {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error.error.as_ref());
    while let Some(err) = current {
        if let Some(http_err) = err.downcast_ref::<StreamableHttpError<reqwest::Error>>() {
            return map_streamable_http_error(server_id, http_err);
        }
        if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>() {
            return map_reqwest_error(server_id, reqwest_err);
        }
        current = err.source();
    }
    McpError::TransportState {
        server_id: server_id.to_owned(),
    }
}

fn map_streamable_http_error(
    server_id: &str,
    error: &StreamableHttpError<reqwest::Error>,
) -> McpError {
    match error {
        StreamableHttpError::SessionExpired => McpError::SessionExpired {
            server_id: server_id.to_owned(),
        },
        StreamableHttpError::ResponseBodyTooLarge { .. } => McpError::ResponseTooLarge {
            server_id: server_id.to_owned(),
        },
        StreamableHttpError::AuthRequired(_) => McpError::HttpStatus {
            server_id: server_id.to_owned(),
            status: 401,
        },
        StreamableHttpError::InsufficientScope(_) => McpError::HttpStatus {
            server_id: server_id.to_owned(),
            status: 403,
        },
        StreamableHttpError::HttpStatus(status_err) => McpError::HttpStatus {
            server_id: server_id.to_owned(),
            status: status_err.status,
        },
        StreamableHttpError::Client(reqwest_err) => map_reqwest_error(server_id, reqwest_err),
        StreamableHttpError::UnexpectedContentType(_) => McpError::UnsupportedContentType {
            server_id: server_id.to_owned(),
            content_type: None,
        },
        StreamableHttpError::UnexpectedServerResponse(message) => {
            if let Some(status) = parse_http_status_prefix(message) {
                McpError::HttpStatus {
                    server_id: server_id.to_owned(),
                    status,
                }
            } else {
                McpError::InvalidJson {
                    server_id: server_id.to_owned(),
                    message: "invalid JSON-RPC response".to_owned(),
                }
            }
        }
        StreamableHttpError::Deserialize(_) | StreamableHttpError::Sse(_) => {
            McpError::InvalidJson {
                server_id: server_id.to_owned(),
                message: "invalid JSON-RPC response".to_owned(),
            }
        }
        StreamableHttpError::SessionRecoveryTimeout
        | StreamableHttpError::ControlRequestTimeout => McpError::Timeout {
            server_id: server_id.to_owned(),
        },
        StreamableHttpError::ReinitializationFailed(data) => McpError::JsonRpc {
            server_id: server_id.to_owned(),
            code: i64::from(data.code.0),
            message: "server returned a JSON-RPC error".to_owned(),
            data: None,
        },
        _ => McpError::TransportState {
            server_id: server_id.to_owned(),
        },
    }
}

fn map_reqwest_error(server_id: &str, error: &reqwest::Error) -> McpError {
    if error.is_timeout() {
        McpError::Timeout {
            server_id: server_id.to_owned(),
        }
    } else if error.is_connect() {
        McpError::Connection {
            server_id: server_id.to_owned(),
        }
    } else if let Some(status) = error.status() {
        McpError::HttpStatus {
            server_id: server_id.to_owned(),
            status: status.as_u16(),
        }
    } else if error.is_decode() {
        McpError::InvalidJson {
            server_id: server_id.to_owned(),
            message: "invalid JSON-RPC response".to_owned(),
        }
    } else {
        McpError::TransportState {
            server_id: server_id.to_owned(),
        }
    }
}

/// Extracts an HTTP status code from rmcp's `"HTTP <code> ..."` diagnostic prefix
/// without retaining any untrusted response body text.
fn parse_http_status_prefix(message: &str) -> Option<u16> {
    let rest = message.strip_prefix("HTTP ")?;
    let end = rest.bytes().take_while(u8::is_ascii_digit).count();
    (end == 3)
        .then(|| rest[..end].parse::<u16>().ok())
        .flatten()
}

impl std::fmt::Debug for McpHttpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpClient")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}
