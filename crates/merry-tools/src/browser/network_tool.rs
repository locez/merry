//! Headless browser network inspection tool over Chrome DevTools Protocol.

use super::cdp::deserialize_lenient_opt_string;
use super::session::BrowserSessionManager;
use merry_core::{ErrorInfo, PendingToolCall};
use merry_runtime::{
    RegisteredTool, ToolActionKind, ToolBuildError, ToolExecutionContext, ToolExecutionError,
    ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// Registered tool name for inspecting captured browser network requests and responses.
pub const BROWSER_NETWORK_TOOL: &str = "browser_network";

const MAX_RESPONSE_BODY_CHARS: usize = 20_000;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BrowserNetworkAction {
    List,
    Details,
}

#[merry_tools_macros::tool(
    crate = "crate",
    name = "browser_network",
    description = "Inspect HTTP/XHR/Fetch requests captured by the active headless browser session: list requests (optionally filtered by URL or failure status) or fetch response headers and body details for a specific request_id. Requires a browser_tab open that succeeded; there is no network list when open failed."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserNetworkInput {
    #[schemars(
        description = "Inspection action: `list` recent requests or `details` for a specific request_id."
    )]
    action: BrowserNetworkAction,
    #[serde(default, deserialize_with = "deserialize_lenient_opt_string")]
    #[schemars(
        description = "Request ID returned by `list` for inspecting response headers and body in `details`."
    )]
    request_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional case-insensitive URL substring filter for `list`.")]
    url_filter: Option<String>,
    #[serde(default)]
    #[schemars(description = "When true in `list`, return only failed or HTTP >= 400 requests.")]
    failed_only: Option<bool>,
    #[serde(default)]
    #[schemars(description = "When true in `list`, clear captured requests after reading them.")]
    clear: Option<bool>,
}

/// Network inspection tool backed by [`BrowserSessionManager`].
#[derive(Debug, Clone)]
pub struct BrowserNetworkTool {
    manager: Arc<BrowserSessionManager>,
}

impl BrowserNetworkTool {
    /// Creates a `browser_network` tool backed by the shared session manager.
    #[must_use]
    pub fn new(manager: Arc<BrowserSessionManager>) -> Self {
        Self { manager }
    }

    /// Converts this tool into a registered runtime tool.
    pub fn into_registered_tool(self) -> Result<RegisteredTool, ToolBuildError> {
        let spec = BrowserNetworkInput::tool_spec()?;
        Ok(
            RegisteredTool::new(spec, Arc::new(self), ToolActionKind::TrustedExternal)
                .with_parallel_safe_execution(),
        )
    }
}

impl ToolExecutor for BrowserNetworkTool {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }
            let input: BrowserNetworkInput = match call.arguments().deserialize_as() {
                Ok(input) => input,
                Err(error) => {
                    return Ok(network_failed(
                        "browser_network_invalid_arguments",
                        format!("invalid browser_network arguments: {error}"),
                    ));
                }
            };
            tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => Err(ToolExecutionError::Cancelled),
                outcome = execute_network_action(&self.manager, input) => Ok(outcome),
            }
        })
    }
}

async fn execute_network_action(
    manager: &BrowserSessionManager,
    input: BrowserNetworkInput,
) -> ToolExecutionOutcome {
    let cdp = match manager.session(false).await {
        Ok(cdp) => cdp,
        Err(e) => return network_failed("browser_network_no_session", e),
    };
    let timeout = manager.config().default_timeout;

    match input.action {
        BrowserNetworkAction::List => {
            let filter_lower = input.url_filter.as_deref().map(str::to_ascii_lowercase);
            let failed_only = input.failed_only.unwrap_or(false);
            let clear = input.clear.unwrap_or(false);
            let entries: Vec<_> = cdp
                .network_entries(clear)
                .into_iter()
                .filter(|entry| {
                    filter_lower
                        .as_deref()
                        .is_none_or(|needle| entry.url.to_ascii_lowercase().contains(needle))
                })
                .filter(|entry| {
                    !failed_only
                        || entry.error_text.is_some()
                        || entry.status.is_some_and(|s| s >= 400)
                })
                .collect();

            let payload = json!({
                "ok": true,
                "action": "list",
                "count": entries.len(),
                "requests": entries,
            });
            let pretty = serde_json::to_string_pretty(&payload).expect("network json serializes");
            ToolExecutionOutcome::succeeded_json(payload.to_string()).with_model_text(pretty)
        }
        BrowserNetworkAction::Details => {
            let Some(request_id) = input
                .request_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return network_failed(
                    "browser_network_invalid_arguments",
                    "`request_id` is required for details".to_owned(),
                );
            };
            let Some(entry) = cdp.network_entry(request_id) else {
                return network_failed(
                    "browser_network_not_found",
                    format!("No captured network request with request_id={request_id:?}"),
                );
            };

            let body_result = cdp
                .call(
                    "Network.getResponseBody",
                    json!({ "requestId": request_id }),
                    timeout,
                )
                .await;

            let (body_text, base64_encoded, body_error) = match body_result {
                Ok(val) => {
                    let b64 = val
                        .get("base64Encoded")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let body = val
                        .get("body")
                        .and_then(serde_json::Value::as_str)
                        .map(|s| {
                            if b64 {
                                format!("[base64 response body omitted ({} chars)]", s.len())
                            } else if s.chars().count() > MAX_RESPONSE_BODY_CHARS {
                                let prefix: String =
                                    s.chars().take(MAX_RESPONSE_BODY_CHARS).collect();
                                format!("{prefix}\n\n[truncated]")
                            } else {
                                s.to_owned()
                            }
                        });
                    (body, b64, None)
                }
                Err(e) => (None, false, Some(e)),
            };

            let payload = json!({
                "ok": true,
                "action": "details",
                "request": entry,
                "base64_encoded": base64_encoded,
                "body": body_text,
                "body_error": body_error,
            });
            let pretty = serde_json::to_string_pretty(&payload).expect("network details serialize");
            ToolExecutionOutcome::succeeded_json(payload.to_string()).with_model_text(pretty)
        }
    }
}

fn network_failed(code: &'static str, message: String) -> ToolExecutionOutcome {
    let payload = json!({
        "ok": false,
        "tool": BROWSER_NETWORK_TOOL,
        "error": {
            "code": code,
            "message": message,
        }
    });
    let safe_msg: String = message
        .chars()
        .filter(|c| !c.is_control())
        .take(512)
        .collect();
    let diagnostic = ErrorInfo::new(
        code,
        if safe_msg.trim().is_empty() {
            "browser_network action failed"
        } else {
            &safe_msg
        },
    )
    .expect("sanitized browser_network diagnostic is valid");
    ToolExecutionOutcome::failed_json(payload.to_string(), diagnostic)
}
