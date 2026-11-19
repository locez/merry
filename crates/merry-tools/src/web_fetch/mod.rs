//! Lightweight HTTP fetcher with dual-gate SSRF protection and HTML-to-Markdown conversion.

use crate::ssrf::{check_ssrf_url, is_explicit_local_host, strip_brackets};
use merry_core::{ErrorInfo, PendingToolCall};
use merry_runtime::{
    RegisteredTool, ToolActionKind, ToolBuildError, ToolExecutionContext, ToolExecutionError,
    ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use url::Url;

mod html;

/// Registered tool name for lightweight HTTP/HTML fetching.
pub const WEB_FETCH_TOOL: &str = "web_fetch";

const DEFAULT_TIMEOUT_SECS: u64 = 20;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_MAX_OUTPUT_CHARS: usize = 32_000;
const MAX_REDIRECTS: usize = 10;

/// Configuration for [`WebFetchTool`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebFetchConfig {
    allow_local: bool,
    timeout: Duration,
    max_response_bytes: usize,
    max_output_chars: usize,
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            allow_local: true,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_output_chars: DEFAULT_MAX_OUTPUT_CHARS,
        }
    }
}

impl WebFetchConfig {
    /// Sets whether explicit loopback targets (`localhost`, `127.0.0.1`, `::1`) are permitted.
    #[must_use]
    pub fn with_allow_local(mut self, allow_local: bool) -> Self {
        self.allow_local = allow_local;
        self
    }

    /// Overrides the per-request timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Overrides the maximum response body size downloaded in bytes.
    #[must_use]
    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    /// Overrides the maximum model-visible output characters.
    #[must_use]
    pub fn with_max_output_chars(mut self, max_output_chars: usize) -> Self {
        self.max_output_chars = max_output_chars;
        self
    }
}

#[merry_tools_macros::tool(
    crate = "crate",
    name = "web_fetch",
    description = "Fetch a static web page, API JSON endpoint, or plain text document over HTTP/HTTPS and return clean extracted Markdown or text. Use this instead of browser tools when reading documentation or non-interactive URLs. Authenticated or private URLs fail. An SSRF refusal is final for that URL. Cross-host redirects stop and return the redirect URL instead of following automatically."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WebFetchInput {
    #[schemars(
        description = "Target http:// or https:// URL to fetch.",
        length(min = 1)
    )]
    url: String,
    #[serde(default)]
    #[schemars(
        description = "Optional maximum character length for returned extracted content.",
        range(min = 100)
    )]
    max_chars: Option<usize>,
}

/// Built-in SSRF-guarded HTTP/HTML fetch tool.
#[derive(Debug, Clone)]
pub struct WebFetchTool {
    config: Arc<WebFetchConfig>,
}

impl WebFetchTool {
    /// Creates a `web_fetch` tool with the supplied configuration.
    #[must_use]
    pub fn new(config: WebFetchConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    /// Converts this tool into a registered runtime tool.
    pub fn into_registered_tool(self) -> Result<RegisteredTool, ToolBuildError> {
        let spec = WebFetchInput::tool_spec()?;
        Ok(
            RegisteredTool::new(spec, Arc::new(self), ToolActionKind::TrustedExternal)
                .with_parallel_safe_execution(),
        )
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new(WebFetchConfig::default())
    }
}

impl ToolExecutor for WebFetchTool {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }
            let input: WebFetchInput = match call.arguments().deserialize_as() {
                Ok(input) => input,
                Err(error) => {
                    return Ok(web_fetch_failed(
                        "web_fetch_invalid_arguments",
                        format!("invalid web_fetch arguments: {error}"),
                        None,
                    ));
                }
            };
            tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => Err(ToolExecutionError::Cancelled),
                outcome = execute_fetch(&self.config, input) => Ok(outcome),
            }
        })
    }
}

async fn execute_fetch(config: &WebFetchConfig, input: WebFetchInput) -> ToolExecutionOutcome {
    let mut url = match Url::parse(input.url.trim()) {
        Ok(url) => url,
        Err(error) => {
            return web_fetch_failed(
                "web_fetch_invalid_url",
                format!("invalid URL {:?}: {error}", input.url),
                None,
            );
        }
    };

    if url.scheme() == "http"
        && let Some(host) = url.host_str()
        && !is_explicit_local_host(host)
    {
        let _ = url.set_scheme("https");
    }

    let initial_addrs = match check_ssrf_url(&url, config.allow_local).await {
        Ok(addrs) => addrs,
        Err(reason) => {
            return web_fetch_failed("web_fetch_ssrf_blocked", reason, Some(url.as_str()));
        }
    };

    let client = match build_pinned_client(config, &url, &initial_addrs) {
        Ok(client) => client,
        Err(error) => {
            return web_fetch_failed("web_fetch_client_error", error, Some(url.as_str()));
        }
    };

    let mut current_url = url.clone();
    let response = match follow_same_host_redirects(&client, config, &mut current_url).await {
        Ok(RedirectStep::Response(resp)) => resp,
        Ok(RedirectStep::CrossHostRedirect {
            status_code,
            redirect_url,
        }) => {
            let summary = format!(
                "Stopped at cross-host redirect ({status_code}) from {current_url} to {redirect_url}. Inspect the destination URL and fetch it directly if intended."
            );
            return ToolExecutionOutcome::succeeded_text(summary);
        }
        Err(outcome) => return *outcome,
    };

    let status_code = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let (raw_bytes, body_truncated) = match read_bounded_body(response, config.max_response_bytes)
        .await
    {
        Ok(data) => data,
        Err(error) => {
            return web_fetch_failed("web_fetch_read_failed", error, Some(current_url.as_str()));
        }
    };

    let (title, full_extracted) = format_response_content(&raw_bytes, content_type.as_deref());
    let limit = input
        .max_chars
        .unwrap_or(config.max_output_chars)
        .min(config.max_output_chars);
    let (model_body, text_truncated) = truncate_chars(&full_extracted, limit);
    let truncated = body_truncated || text_truncated;

    if status_code >= 400 {
        let detail = format!("HTTP {status_code} fetching {current_url}\n\n{model_body}");
        return web_fetch_failed("web_fetch_http_error", detail, Some(current_url.as_str()));
    }

    let mut model_summary = format!("URL: {current_url}\nStatus: {status_code}\n");
    if let Some(title_text) = title.as_deref() {
        model_summary.push_str(&format!("Title: {title_text}\n"));
    }
    model_summary.push('\n');
    model_summary.push_str(&model_body);
    if truncated {
        model_summary.push_str("\n\n[truncated]");
    }

    ToolExecutionOutcome::succeeded_text(model_summary)
}

enum RedirectStep {
    Response(reqwest::Response),
    CrossHostRedirect {
        status_code: u16,
        redirect_url: String,
    },
}

async fn follow_same_host_redirects(
    client: &reqwest::Client,
    config: &WebFetchConfig,
    current_url: &mut Url,
) -> Result<RedirectStep, Box<ToolExecutionOutcome>> {
    for hop in 0..=MAX_REDIRECTS {
        let response = client
            .get(current_url.clone())
            .send()
            .await
            .map_err(|error| {
                Box::new(web_fetch_failed(
                    "web_fetch_request_failed",
                    format!("HTTP request failed: {error}"),
                    Some(current_url.as_str()),
                ))
            })?;
        let status = response.status();
        if !status.is_redirection() {
            return Ok(RedirectStep::Response(response));
        }
        let Some(location) = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
        else {
            return Ok(RedirectStep::Response(response));
        };
        let next_url = current_url.join(location).map_err(|error| {
            Box::new(web_fetch_failed(
                "web_fetch_invalid_redirect",
                format!("invalid redirect location {location:?}: {error}"),
                Some(current_url.as_str()),
            ))
        })?;
        if !same_host(current_url, &next_url) {
            return Ok(RedirectStep::CrossHostRedirect {
                status_code: status.as_u16(),
                redirect_url: next_url.to_string(),
            });
        }
        if hop == MAX_REDIRECTS {
            return Err(Box::new(web_fetch_failed(
                "web_fetch_too_many_redirects",
                format!("exceeded {MAX_REDIRECTS} same-host redirects"),
                Some(next_url.as_str()),
            )));
        }
        if let Err(reason) = check_ssrf_url(&next_url, config.allow_local).await {
            return Err(Box::new(web_fetch_failed(
                "web_fetch_ssrf_blocked",
                reason,
                Some(next_url.as_str()),
            )));
        }
        *current_url = next_url;
    }
    unreachable!("redirect loop returns within bounded hops")
}

fn build_pinned_client(
    config: &WebFetchConfig,
    url: &Url,
    addrs: &[SocketAddr],
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(config.timeout)
        .user_agent("MerryWebFetch/0.1");

    if let Some(host) = url.host_str()
        && !addrs.is_empty()
    {
        builder = builder.resolve_to_addrs(strip_brackets(host), addrs);
    }

    builder
        .build()
        .map_err(|error| format!("failed to build HTTP client: {error}"))
}

fn same_host(a: &Url, b: &Url) -> bool {
    let normalize = |u: &Url| {
        u.host_str()
            .map(|h| h.trim_start_matches("www.").to_ascii_lowercase())
    };
    normalize(a).is_some() && normalize(a) == normalize(b)
}

async fn read_bounded_body(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool), String> {
    let mut out = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("error reading response body: {error}"))?
    {
        let remaining = max_bytes.saturating_sub(out.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        if chunk.len() > remaining {
            out.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        out.extend_from_slice(&chunk);
    }
    Ok((out, truncated))
}

fn format_response_content(bytes: &[u8], content_type: Option<&str>) -> (Option<String>, String) {
    let mime = content_type
        .unwrap_or("text/plain")
        .split(';')
        .next()
        .unwrap_or("text/plain")
        .trim()
        .to_ascii_lowercase();

    let text = String::from_utf8_lossy(bytes);
    if mime == "text/html" || mime == "application/xhtml+xml" {
        let title = html::extract_html_title(&text);
        let md = html::html_to_markdown(&text);
        let content = if md.trim().is_empty() {
            "[HTML page had no extractable text content]".to_owned()
        } else {
            md
        };
        return (title, content);
    }
    if mime == "application/json" || mime.ends_with("+json") {
        let formatted = serde_json::from_slice::<serde_json::Value>(bytes)
            .and_then(|v| serde_json::to_string_pretty(&v))
            .unwrap_or_else(|_| text.into_owned());
        return (None, html::strip_base64_data_uris(&formatted));
    }
    if mime.starts_with("text/")
        || mime.contains("xml")
        || mime.contains("javascript")
        || mime.contains("yaml")
    {
        let cleaned = html::strip_base64_data_uris(&text);
        let content = if cleaned.trim().is_empty() {
            "[empty text response]".to_owned()
        } else {
            cleaned
        };
        return (None, content);
    }
    (
        None,
        format!("[non-text content type={mime} bytes={}]", bytes.len()),
    )
}

fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_owned(), false);
    }
    let truncated: String = text.chars().take(max_chars).collect();
    (truncated, true)
}

fn web_fetch_failed(
    code: &'static str,
    message: String,
    url: Option<&str>,
) -> ToolExecutionOutcome {
    let payload = json!({
        "ok": false,
        "tool": WEB_FETCH_TOOL,
        "url": url,
        "error": {
            "code": code,
            "message": message,
        }
    });
    let safe_diag_msg: String = message
        .chars()
        .filter(|c| !c.is_control())
        .take(512)
        .collect();
    let diagnostic = ErrorInfo::new(
        code,
        if safe_diag_msg.trim().is_empty() {
            "web_fetch failed"
        } else {
            &safe_diag_msg
        },
    )
    .expect("sanitized web_fetch diagnostic is valid");
    ToolExecutionOutcome::failed_json(payload.to_string(), diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        http::StatusCode,
        response::{Html, IntoResponse, Redirect},
        routing::get,
    };
    use merry_core::{
        PendingToolCall, ToolCallArguments, ToolCallId, ToolCallResultStatus, ToolName,
    };
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    fn test_call(args: serde_json::Value) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("call-web-fetch").expect("valid id"),
            ToolName::new(WEB_FETCH_TOOL).expect("valid name"),
            ToolCallArguments::try_from(args).expect("object args"),
        )
    }

    #[tokio::test]
    async fn blocks_ssrf_targets_and_fetches_loopback_html_when_allowed() {
        let blocked_tool = WebFetchTool::new(WebFetchConfig::default().with_allow_local(false));
        let blocked = blocked_tool
            .execute(
                test_call(json!({"url": "http://127.0.0.1:8080/"})),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("returns failed outcome");
        assert_eq!(blocked.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            blocked.diagnostic().expect("diag").code(),
            "web_fetch_ssrf_blocked"
        );

        let app = Router::new()
            .route(
                "/docs",
                get(|| async {
                    Html("<html><head><title>Test Page</title></head><body><h1>Hello</h1><p>World</p><script>bad()</script></body></html>")
                }),
            )
            .route(
                "/bounce",
                get(|| async { Redirect::temporary("https://other.example.com/target").into_response() }),
            )
            .route(
                "/local-redirect",
                get(|| async { (StatusCode::FOUND, [("location", "/docs")]).into_response() }),
            );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let local_tool = WebFetchTool::default();
        let ok = local_tool
            .execute(
                test_call(
                    json!({"url": format!("http://127.0.0.1:{}/local-redirect", addr.port())}),
                ),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("fetch succeeds");
        assert_eq!(ok.status(), ToolCallResultStatus::Succeeded);
        assert!(ok.model_content().is_none());
        let text = ok.content().as_str();
        assert!(text.contains("Title: Test Page"));
        assert!(text.contains("# Hello"));
        assert!(!text.contains("bad()"));

        let cross_host = local_tool
            .execute(
                test_call(json!({"url": format!("http://127.0.0.1:{}/bounce", addr.port())})),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("cross-host redirect returns cleanly");
        assert_eq!(cross_host.status(), ToolCallResultStatus::Succeeded);
        assert!(cross_host.model_content().is_none());
        assert!(
            cross_host
                .content()
                .as_str()
                .contains("https://other.example.com/target")
        );

        server.abort();
    }
}
