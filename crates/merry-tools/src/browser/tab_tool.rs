//! Interactive headless browser tab tool over Chrome DevTools Protocol.

use super::session::BrowserSessionManager;
use crate::ssrf::{check_ssrf_url, validate_browser_data_url};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use merry_core::{ErrorInfo, PendingToolCall};
use merry_runtime::{
    MAX_USER_IMAGE_DIMENSION, MAX_USER_IMAGE_PNG_BYTES, RegisteredTool, ToolActionKind,
    ToolBuildError, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture, UserImageInput,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io::Cursor, sync::Arc, time::Duration};
use url::Url;

/// Registered tool name for interactive CDP browser inspection and actions.
pub const BROWSER_TAB_TOOL: &str = "browser_tab";

const SNAPSHOT_JS: &str = r##"(() => {
  const title = document.title || "";
  const url = window.location.href || "";
  const text = ((document.body && (document.body.innerText || document.body.textContent)) || "")
    .replace(/\s+\n/g, "\n")
    .trim()
    .slice(0, 6000);
  const items = Array.from(document.querySelectorAll("a[href],button,input,select,textarea,[role='button']"))
    .slice(0, 60)
    .map((el, i) => {
      const tag = el.tagName.toLowerCase();
      const id = el.id ? "#" + el.id : "";
      const name = el.getAttribute("name") ? `[name="${el.getAttribute("name")}"]` : "";
      const role = el.getAttribute("role") || "";
      const label = (el.innerText || el.value || el.getAttribute("aria-label") || el.getAttribute("placeholder") || "")
        .replace(/\s+/g, " ")
        .trim()
        .slice(0, 80);
      const selector = id || (tag + name) || tag;
      return { index: i, tag, selector, role, label, disabled: Boolean(el.disabled) };
    });
  return { url, title, text, interactive_elements: items };
})()"##;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BrowserTabAction {
    Open,
    Navigate,
    Click,
    Fill,
    Press,
    Evaluate,
    Snapshot,
    Console,
    Resize,
    Close,
}

#[merry_tools_macros::tool(
    crate = "crate",
    name = "browser_tab",
    description = "Control one session-scoped headless Chromium tab for live UI verification. open and navigate take an http(s) URL or a self-contained data:text/html or data:text/plain document, then click, fill, press, evaluate, snapshot (DOM plus PNG), console, resize, or close. A failed open does not start a session; click, snapshot, console, and network inspection require a successful open first. Click and fill take a CSS selector from the latest snapshot. Do not trigger alert, confirm, or prompt."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserTabInput {
    #[schemars(description = "Action to perform on the session browser tab.")]
    action: BrowserTabAction,
    #[serde(default)]
    #[schemars(
        description = "http(s) URL, or a data:text/html / data:text/plain document, for open and navigate. Open only the URL the user named."
    )]
    url: Option<String>,
    #[serde(default)]
    #[schemars(description = "CSS selector for click, fill, or optional press focus.")]
    selector: Option<String>,
    #[serde(default)]
    #[schemars(description = "Text value to fill into the selected input/textarea.")]
    text: Option<String>,
    #[serde(default)]
    #[schemars(description = "Keyboard key name for press (e.g. Enter, Escape, Tab).")]
    key: Option<String>,
    #[serde(default)]
    #[schemars(description = "JavaScript expression to evaluate for evaluate.")]
    expression: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Viewport width in pixels for resize.",
        range(min = 100, max = 4096)
    )]
    width: Option<u32>,
    #[serde(default)]
    #[schemars(
        description = "Viewport height in pixels for resize.",
        range(min = 100, max = 4096)
    )]
    height: Option<u32>,
    #[serde(default)]
    #[schemars(description = "Whether to clear buffered console entries when reading console.")]
    clear: Option<bool>,
    #[serde(default)]
    #[schemars(description = "Optional console level filter (log, warning, error, exception).")]
    level: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "For snapshot, whether to attach a visual PNG screenshot (default true)."
    )]
    include_screenshot: Option<bool>,
}

/// Interactive headless browser tab tool backed by [`BrowserSessionManager`].
#[derive(Debug, Clone)]
pub struct BrowserTabTool {
    manager: Arc<BrowserSessionManager>,
}

impl BrowserTabTool {
    /// Creates a `browser_tab` tool backed by the shared session manager.
    #[must_use]
    pub fn new(manager: Arc<BrowserSessionManager>) -> Self {
        Self { manager }
    }

    /// Converts this tool into a registered runtime tool.
    pub fn into_registered_tool(self) -> Result<RegisteredTool, ToolBuildError> {
        let spec = BrowserTabInput::tool_spec()?;
        Ok(RegisteredTool::new(
            spec,
            Arc::new(self),
            ToolActionKind::TrustedExternal,
        ))
    }
}

impl ToolExecutor for BrowserTabTool {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }
            let input: BrowserTabInput = match call.arguments().deserialize_as() {
                Ok(input) => input,
                Err(error) => {
                    return Ok(browser_failed(
                        "browser_tab_invalid_arguments",
                        format!("invalid browser_tab arguments: {error}"),
                    ));
                }
            };
            tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => Err(ToolExecutionError::Cancelled),
                outcome = execute_tab_action(&self.manager, input) => Ok(outcome),
            }
        })
    }
}

async fn execute_tab_action(
    manager: &BrowserSessionManager,
    input: BrowserTabInput,
) -> ToolExecutionOutcome {
    let timeout = manager.config().default_timeout;
    match input.action {
        BrowserTabAction::Close => {
            let was_open = manager.close().await;
            let summary = if was_open {
                "Browser session closed.".to_owned()
            } else {
                "No browser session was active.".to_owned()
            };
            ToolExecutionOutcome::succeeded_json(
                json!({ "ok": true, "closed": was_open }).to_string(),
            )
            .with_model_text(summary)
        }
        BrowserTabAction::Open | BrowserTabAction::Navigate => {
            let Some(raw_url) = input
                .url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return browser_failed(
                    "browser_tab_invalid_arguments",
                    "`url` is required for open/navigate".to_owned(),
                );
            };
            let parsed = match Url::parse(raw_url) {
                Ok(u) => u,
                Err(e) => {
                    return browser_failed(
                        "browser_tab_invalid_url",
                        format!("invalid URL {raw_url:?}: {e}"),
                    );
                }
            };
            if parsed.scheme() == "data" {
                if let Err(reason) = validate_browser_data_url(raw_url) {
                    return browser_failed("browser_tab_invalid_url", reason);
                }
            } else if let Err(reason) = check_ssrf_url(&parsed, manager.config().allow_local).await
            {
                return browser_failed("browser_tab_ssrf_blocked", reason);
            }
            let navigation_url = if parsed.scheme() == "data" {
                raw_url.to_owned()
            } else {
                parsed.as_str().to_owned()
            };
            let cdp = match manager.session(true).await {
                Ok(cdp) => cdp,
                Err(e) => return browser_failed("browser_tab_unavailable", e),
            };
            if let Err(e) = cdp
                .call("Page.navigate", json!({ "url": navigation_url }), timeout)
                .await
            {
                return browser_failed("browser_tab_navigation_failed", e);
            }
            let _ = wait_for_document_ready(&cdp, timeout).await;
            let meta = eval_value(
                &cdp,
                "({ url: window.location.href, title: document.title || '' })",
                timeout,
            )
            .await;
            let url_now = meta
                .as_ref()
                .ok()
                .and_then(|v| v.get("url"))
                .and_then(Value::as_str)
                .unwrap_or(parsed.as_str())
                .to_owned();
            let title_now = meta
                .as_ref()
                .ok()
                .and_then(|v| v.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            ToolExecutionOutcome::succeeded_json(
                json!({ "ok": true, "action": "navigate", "url": url_now, "title": title_now })
                    .to_string(),
            )
            .with_model_text(format!("Navigated to {url_now} (title: {title_now:?})"))
        }
        BrowserTabAction::Click => {
            let Some(selector) = input.selector.as_deref().filter(|s| !s.trim().is_empty()) else {
                return browser_failed(
                    "browser_tab_invalid_arguments",
                    "`selector` is required for click".to_owned(),
                );
            };
            let cdp = match manager.session(false).await {
                Ok(cdp) => cdp,
                Err(e) => return browser_failed("browser_tab_no_session", e),
            };
            let selector_json = serde_json::to_string(selector).expect("string serializes");
            let js = format!(
                r#"(() => {{
                  const el = document.querySelector({selector_json});
                  if (!el) return {{ found: false }};
                  el.scrollIntoView({{ block: 'center', inline: 'center' }});
                  el.click();
                  return {{ found: true, tag: el.tagName.toLowerCase(), text: (el.innerText || '').trim().slice(0, 80) }};
                }})()"#
            );
            match eval_value(&cdp, &js, timeout).await {
                Ok(v) if v.get("found") == Some(&Value::Bool(true)) => {
                    ToolExecutionOutcome::succeeded_json(
                        json!({ "ok": true, "action": "click", "selector": selector, "element": v })
                            .to_string(),
                    )
                    .with_model_text(format!("Clicked element matching `{selector}`."))
                }
                Ok(_) => browser_failed(
                    "browser_tab_selector_not_found",
                    format!("No element matched selector `{selector}`"),
                ),
                Err(e) => browser_failed("browser_tab_click_failed", e),
            }
        }
        BrowserTabAction::Fill => {
            let Some(selector) = input.selector.as_deref().filter(|s| !s.trim().is_empty()) else {
                return browser_failed(
                    "browser_tab_invalid_arguments",
                    "`selector` is required for fill".to_owned(),
                );
            };
            let Some(text) = input.text.as_deref() else {
                return browser_failed(
                    "browser_tab_invalid_arguments",
                    "`text` is required for fill".to_owned(),
                );
            };
            let cdp = match manager.session(false).await {
                Ok(cdp) => cdp,
                Err(e) => return browser_failed("browser_tab_no_session", e),
            };
            let sel_json = serde_json::to_string(selector).expect("string serializes");
            let val_json = serde_json::to_string(text).expect("string serializes");
            let js = format!(
                r#"(() => {{
                  const el = document.querySelector({sel_json});
                  if (!el) return {{ found: false }};
                  el.focus();
                  el.value = {val_json};
                  el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                  el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                  return {{ found: true, tag: el.tagName.toLowerCase() }};
                }})()"#
            );
            match eval_value(&cdp, &js, timeout).await {
                Ok(v) if v.get("found") == Some(&Value::Bool(true)) => {
                    ToolExecutionOutcome::succeeded_json(
                        json!({ "ok": true, "action": "fill", "selector": selector }).to_string(),
                    )
                    .with_model_text(format!("Filled `{selector}`."))
                }
                Ok(_) => browser_failed(
                    "browser_tab_selector_not_found",
                    format!("No element matched selector `{selector}`"),
                ),
                Err(e) => browser_failed("browser_tab_fill_failed", e),
            }
        }
        BrowserTabAction::Press => {
            let Some(key) = input.key.as_deref().filter(|s| !s.trim().is_empty()) else {
                return browser_failed(
                    "browser_tab_invalid_arguments",
                    "`key` is required for press".to_owned(),
                );
            };
            let cdp = match manager.session(false).await {
                Ok(cdp) => cdp,
                Err(e) => return browser_failed("browser_tab_no_session", e),
            };
            if let Some(selector) = input.selector.as_deref().filter(|s| !s.trim().is_empty()) {
                let sel_json = serde_json::to_string(selector).expect("string serializes");
                let focus_js = format!("document.querySelector({sel_json})?.focus()");
                let _ = eval_value(&cdp, &focus_js, timeout).await;
            }
            if let Err(e) = cdp
                .call(
                    "Input.dispatchKeyEvent",
                    json!({ "type": "keyDown", "key": key }),
                    timeout,
                )
                .await
            {
                return browser_failed("browser_tab_press_failed", e);
            }
            if let Err(e) = cdp
                .call(
                    "Input.dispatchKeyEvent",
                    json!({ "type": "keyUp", "key": key }),
                    timeout,
                )
                .await
            {
                return browser_failed("browser_tab_press_failed", e);
            }
            ToolExecutionOutcome::succeeded_json(
                json!({ "ok": true, "action": "press", "key": key }).to_string(),
            )
            .with_model_text(format!("Pressed key `{key}`."))
        }
        BrowserTabAction::Evaluate => {
            let Some(expr) = input.expression.as_deref().filter(|s| !s.trim().is_empty()) else {
                return browser_failed(
                    "browser_tab_invalid_arguments",
                    "`expression` is required for evaluate".to_owned(),
                );
            };
            let cdp = match manager.session(false).await {
                Ok(cdp) => cdp,
                Err(e) => return browser_failed("browser_tab_no_session", e),
            };
            match eval_value(&cdp, expr, timeout).await {
                Ok(value) => {
                    let pretty =
                        serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
                    ToolExecutionOutcome::succeeded_json(
                        json!({ "ok": true, "action": "evaluate", "value": value }).to_string(),
                    )
                    .with_model_text(pretty)
                }
                Err(e) => browser_failed("browser_tab_eval_failed", e),
            }
        }
        BrowserTabAction::Snapshot => {
            let cdp = match manager.session(false).await {
                Ok(cdp) => cdp,
                Err(e) => return browser_failed("browser_tab_no_session", e),
            };
            let dom = eval_value(&cdp, SNAPSHOT_JS, timeout)
                .await
                .unwrap_or_else(|e| json!({ "dom_error": e }));
            let console = cdp.console_entries(false);
            let error_count = console
                .iter()
                .filter(|c| c.level == "error" || c.level == "exception")
                .count();
            let network = cdp.network_entries(false);
            let failed_requests = network
                .iter()
                .filter(|n| n.error_text.is_some() || n.status.is_some_and(|s| s >= 400))
                .count();

            let mut attached_image = None;
            let mut screenshot_note = "screenshot: disabled".to_owned();
            if input.include_screenshot.unwrap_or(true) {
                match cdp
                    .call(
                        "Page.captureScreenshot",
                        json!({ "format": "png" }),
                        timeout,
                    )
                    .await
                {
                    Ok(shot) => {
                        if let Some(b64) = shot.get("data").and_then(Value::as_str) {
                            match decode_png_screenshot("browser-snapshot.png", b64) {
                                Ok(img) => {
                                    screenshot_note = format!(
                                        "screenshot: attached {} ({}x{} PNG)",
                                        img.label(),
                                        img.width(),
                                        img.height()
                                    );
                                    attached_image = Some(img);
                                }
                                Err(err) => {
                                    screenshot_note = format!("screenshot error: {err}");
                                }
                            }
                        } else {
                            screenshot_note = "screenshot error: missing image data".to_owned();
                        }
                    }
                    Err(err) => {
                        screenshot_note = format!("screenshot unavailable: {err}");
                    }
                }
            }

            let title = dom.get("title").and_then(Value::as_str).unwrap_or("");
            let url = dom.get("url").and_then(Value::as_str).unwrap_or("");
            let interactive = dom
                .get("interactive_elements")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let text_preview = dom.get("text").and_then(Value::as_str).unwrap_or("");
            let elements_json = dom
                .get("interactive_elements")
                .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                .unwrap_or_default();

            let model_text = format!(
                "URL: {url}\nTitle: {title}\n{screenshot_note}\nConsole errors/exceptions: {error_count} (total console: {})\nFailed HTTP/network requests: {failed_requests} (total captured: {})\nInteractive elements ({interactive}):\n{elements_json}\n\nVisible text:\n{text_preview}",
                console.len(),
                network.len(),
            );
            let artifact = json!({
                "ok": true,
                "action": "snapshot",
                "dom": dom,
                "console_error_count": error_count,
                "network_failure_count": failed_requests,
                "screenshot": screenshot_note,
            });
            let mut outcome = ToolExecutionOutcome::succeeded_json(artifact.to_string())
                .with_model_text(model_text);
            if let Some(img) = attached_image {
                outcome = outcome.with_image(img);
            }
            outcome
        }
        BrowserTabAction::Console => {
            let cdp = match manager.session(false).await {
                Ok(cdp) => cdp,
                Err(e) => return browser_failed("browser_tab_no_session", e),
            };
            let clear = input.clear.unwrap_or(false);
            let entries: Vec<_> = cdp
                .console_entries(clear)
                .into_iter()
                .filter(|entry| {
                    input
                        .level
                        .as_deref()
                        .is_none_or(|lvl| entry.level.eq_ignore_ascii_case(lvl))
                })
                .collect();
            let payload = json!({
                "ok": true,
                "action": "console",
                "count": entries.len(),
                "entries": entries,
            });
            let pretty = serde_json::to_string_pretty(&payload).expect("console json serializes");
            ToolExecutionOutcome::succeeded_json(payload.to_string()).with_model_text(pretty)
        }
        BrowserTabAction::Resize => {
            let (Some(width), Some(height)) = (input.width, input.height) else {
                return browser_failed(
                    "browser_tab_invalid_arguments",
                    "`width` and `height` are required for resize".to_owned(),
                );
            };
            let cdp = match manager.session(false).await {
                Ok(cdp) => cdp,
                Err(e) => return browser_failed("browser_tab_no_session", e),
            };
            if let Err(e) = cdp
                .call(
                    "Emulation.setDeviceMetricsOverride",
                    json!({
                        "width": width,
                        "height": height,
                        "deviceScaleFactor": 1,
                        "mobile": false,
                    }),
                    timeout,
                )
                .await
            {
                return browser_failed("browser_tab_resize_failed", e);
            }
            ToolExecutionOutcome::succeeded_json(
                json!({ "ok": true, "action": "resize", "width": width, "height": height })
                    .to_string(),
            )
            .with_model_text(format!("Resized viewport to {width}x{height}."))
        }
    }
}

async fn wait_for_document_ready(
    cdp: &super::cdp::CdpSession,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout.min(Duration::from_secs(5));
    while tokio::time::Instant::now() < deadline {
        if let Ok(Value::String(state)) =
            eval_value(cdp, "document.readyState", Duration::from_secs(2)).await
            && (state == "interactive" || state == "complete")
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

async fn eval_value(
    cdp: &super::cdp::CdpSession,
    expression: &str,
    timeout: Duration,
) -> Result<Value, String> {
    let raw = cdp
        .call(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
            timeout,
        )
        .await?;
    if let Some(exc) = raw.get("exceptionDetails") {
        return Err(format!("JS exception: {exc}"));
    }
    Ok(raw
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null))
}

fn decode_png_screenshot(label: &str, raw_b64: &str) -> Result<UserImageInput, String> {
    let bytes = STANDARD
        .decode(raw_b64.trim())
        .map_err(|e| format!("invalid screenshot base64: {e}"))?;
    let decoded = image::load_from_memory(&bytes)
        .map_err(|e| format!("invalid screenshot image data: {e}"))?;
    let w = decoded.width();
    let h = decoded.height();
    if w == 0 || h == 0 || w > MAX_USER_IMAGE_DIMENSION || h > MAX_USER_IMAGE_DIMENSION {
        return Err(format!(
            "screenshot dimensions {w}x{h} outside allowed bounds"
        ));
    }
    let png_bytes =
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() <= MAX_USER_IMAGE_PNG_BYTES {
            bytes
        } else {
            let mut out = Cursor::new(Vec::new());
            decoded
                .write_to(&mut out, image::ImageFormat::Png)
                .map_err(|e| format!("failed normalizing screenshot PNG: {e}"))?;
            out.into_inner()
        };
    UserImageInput::png(label, png_bytes, w, h).map_err(|e| e.to_string())
}

fn browser_failed(code: &'static str, message: String) -> ToolExecutionOutcome {
    let payload = json!({
        "ok": false,
        "tool": BROWSER_TAB_TOOL,
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
            "browser_tab action failed"
        } else {
            &safe_msg
        },
    )
    .expect("sanitized browser_tab diagnostic is valid");
    ToolExecutionOutcome::failed_json(payload.to_string(), diagnostic)
}
