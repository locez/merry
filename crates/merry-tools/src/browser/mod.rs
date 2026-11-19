//! Native CDP browser automation and inspection tools (`browser_tab` & `browser_network`).

mod cdp;
mod network_tool;
mod session;
mod tab_tool;
mod ws;

pub use network_tool::{BROWSER_NETWORK_TOOL, BrowserNetworkTool};
pub use session::{BrowserConfig, BrowserSessionManager};
pub use tab_tool::{BROWSER_TAB_TOOL, BrowserTabTool};

#[cfg(test)]
mod tests {
    use super::cdp::CdpSession;
    use super::ws::handshake_ws;
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use merry_core::{
        PendingToolCall, ToolCallArguments, ToolCallId, ToolCallResultStatus, ToolName,
    };
    use merry_runtime::{ToolExecutionContext, ToolExecutor};
    use serde_json::{Value, json};
    use std::{io::Cursor, sync::Arc};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::sync::CancellationToken;

    fn call(tool: &str, args: Value) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("call-browser").expect("valid id"),
            ToolName::new(tool).expect("valid tool"),
            ToolCallArguments::try_from(args).expect("object args"),
        )
    }

    async fn write_server_text_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, text: &str) {
        let bytes = text.as_bytes();
        let mut frame = Vec::with_capacity(10 + bytes.len());
        frame.push(0x81);
        if bytes.len() < 126 {
            frame.push(bytes.len() as u8);
        } else if let Ok(len16) = u16::try_from(bytes.len()) {
            frame.push(126);
            frame.extend_from_slice(&len16.to_be_bytes());
        } else {
            frame.push(127);
            frame.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        }
        frame.extend_from_slice(bytes);
        writer.write_all(&frame).await.expect("write ws text frame");
        writer.flush().await.expect("flush ws text frame");
    }

    async fn read_masked_client_text<R: AsyncReadExt + Unpin>(reader: &mut R) -> Option<String> {
        let mut header = [0u8; 2];
        if reader.read_exact(&mut header).await.is_err() {
            return None;
        }
        let opcode = header[0] & 0x0F;
        if opcode == 0x8 {
            return None;
        }
        let masked = (header[1] & 0x80) != 0;
        let len_code = header[1] & 0x7F;
        let len = match len_code {
            0..=125 => usize::from(len_code),
            126 => {
                let mut b = [0u8; 2];
                reader.read_exact(&mut b).await.ok()?;
                usize::from(u16::from_be_bytes(b))
            }
            127 => {
                let mut b = [0u8; 8];
                reader.read_exact(&mut b).await.ok()?;
                usize::try_from(u64::from_be_bytes(b)).ok()?
            }
            _ => unreachable!(),
        };
        let mask = if masked {
            let mut m = [0u8; 4];
            reader.read_exact(&mut m).await.ok()?;
            Some(m)
        } else {
            None
        };
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload).await.ok()?;
        if let Some(m) = mask {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= m[i % 4];
            }
        }
        String::from_utf8(payload).ok()
    }

    #[tokio::test]
    async fn browser_tab_and_network_work_with_cdp_events_screenshots_and_ssrf() {
        let (client_stream, server_stream) = tokio::io::duplex(16 * 1024);

        let mut png_bytes = Vec::new();
        let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([0, 128, 255, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
            .expect("png encodes");
        let screenshot_b64 = STANDARD.encode(&png_bytes);

        let server_task = tokio::spawn(async move {
            let (mut s_read, mut s_write) = tokio::io::split(server_stream);
            let mut buf = [0u8; 1024];
            let _ = s_read.read(&mut buf).await.expect("upgrade");
            s_write
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
                )
                .await
                .expect("101");

            // Emit a console error and a numeric CDP requestId network event proactively!
            write_server_text_frame(
                &mut s_write,
                &json!({
                    "method": "Runtime.consoleAPICalled",
                    "params": {
                        "type": "error",
                        "args": [{ "value": "Uncaught TypeError: boom" }]
                    }
                })
                .to_string(),
            )
            .await;
            write_server_text_frame(
                &mut s_write,
                &json!({
                    "method": "Network.requestWillBeSent",
                    "params": {
                        "requestId": 62576.34,
                        "type": "Fetch",
                        "request": { "method": "GET", "url": "http://127.0.0.1:3000/api/health" }
                    }
                })
                .to_string(),
            )
            .await;
            write_server_text_frame(
                &mut s_write,
                &json!({
                    "method": "Network.responseReceived",
                    "params": {
                        "requestId": "62576.34",
                        "type": "Fetch",
                        "response": { "status": 500, "statusText": "Internal Server Error", "mimeType": "application/json" }
                    }
                })
                .to_string(),
            )
            .await;

            while let Some(raw) = read_masked_client_text(&mut s_read).await {
                let req: Value = serde_json::from_str(&raw).expect("valid rpc json");
                let id = req["id"].as_u64().expect("id");
                let method = req["method"].as_str().expect("method");
                let result = match method {
                    "Page.navigate" => json!({ "frameId": "main" }),
                    "Runtime.evaluate" => {
                        let expr = req["params"]["expression"].as_str().unwrap_or("");
                        if expr == "document.readyState" {
                            json!({ "result": { "value": "complete" } })
                        } else if expr.contains("interactive_elements") {
                            json!({
                                "result": {
                                    "value": {
                                        "url": "http://127.0.0.1:3000/",
                                        "title": "App Preview",
                                        "text": "Welcome",
                                        "interactive_elements": [{ "index": 0, "tag": "button", "selector": "#save", "label": "Save" }]
                                    }
                                }
                            })
                        } else if expr.contains("click()") {
                            json!({ "result": { "value": { "found": true, "tag": "button", "text": "Save" } } })
                        } else {
                            json!({ "result": { "value": { "url": "http://127.0.0.1:3000/", "title": "App Preview" } } })
                        }
                    }
                    "Page.captureScreenshot" => json!({ "data": screenshot_b64 }),
                    "Network.getResponseBody" => {
                        json!({ "body": r#"{"error":"boom"}"#, "base64Encoded": false })
                    }
                    _ => json!({}),
                };
                write_server_text_frame(
                    &mut s_write,
                    &json!({ "id": id, "result": result }).to_string(),
                )
                .await;
            }
        });

        let (ws_r, ws_w) = handshake_ws(client_stream, "127.0.0.1:9222", "/devtools/page/1")
            .await
            .expect("handshake");
        let cdp = CdpSession::from_ws(ws_r, ws_w);
        let manager = Arc::new(BrowserSessionManager::with_test_cdp(
            BrowserConfig::default(),
            cdp,
        ));
        let tab_tool = BrowserTabTool::new(Arc::clone(&manager));
        let net_tool = BrowserNetworkTool::new(Arc::clone(&manager));

        // 1. SSRF blocks metadata IP before any CDP call runs.
        let ssrf_blocked = tab_tool
            .execute(
                call(
                    BROWSER_TAB_TOOL,
                    json!({ "action": "open", "url": "http://169.254.169.254/latest/meta-data" }),
                ),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("returns outcome");
        assert_eq!(ssrf_blocked.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            ssrf_blocked.diagnostic().expect("diag").code(),
            "browser_tab_ssrf_blocked"
        );

        let data_opened = tab_tool
            .execute(
                call(
                    BROWSER_TAB_TOOL,
                    json!({
                        "action": "open",
                        "url": "data:text/html;charset=utf-8,<html><body><h1>Merry Browser Test</h1><button id=\"btn\">Verify Button</button></body></html>"
                    }),
                ),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("data document open returns");
        assert_eq!(data_opened.status(), ToolCallResultStatus::Succeeded);

        let data_script = tab_tool
            .execute(
                call(
                    BROWSER_TAB_TOOL,
                    json!({ "action": "open", "url": "data:text/javascript,alert(1)" }),
                ),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("script data URL returns");
        assert_eq!(data_script.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            data_script.diagnostic().expect("diag").code(),
            "browser_tab_invalid_url"
        );

        // 2. Navigate to loopback works.
        let opened = tab_tool
            .execute(
                call(
                    BROWSER_TAB_TOOL,
                    json!({ "action": "open", "url": "http://127.0.0.1:3000/" }),
                ),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("open succeeds");
        assert_eq!(opened.status(), ToolCallResultStatus::Succeeded);

        // 3. Click element works.
        let clicked = tab_tool
            .execute(
                call(
                    BROWSER_TAB_TOOL,
                    json!({ "action": "click", "selector": "#save" }),
                ),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("click succeeds");
        assert_eq!(clicked.status(), ToolCallResultStatus::Succeeded);

        // 4. Snapshot captures DOM + attached normalized PNG screenshot!
        let snapshot = tab_tool
            .execute(
                call(BROWSER_TAB_TOOL, json!({ "action": "snapshot" })),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("snapshot succeeds");
        assert_eq!(snapshot.status(), ToolCallResultStatus::Succeeded);
        assert_eq!(snapshot.images().len(), 1);
        assert_eq!(snapshot.images()[0].label(), "browser-snapshot.png");
        assert_eq!(snapshot.images()[0].width(), 3);
        assert_eq!(snapshot.images()[0].height(), 2);

        // 5. Network list & details handle lenient CDP requestId seamlessly!
        let listed = net_tool
            .execute(
                call(
                    BROWSER_NETWORK_TOOL,
                    json!({ "action": "list", "failed_only": true }),
                ),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("network list succeeds");
        assert_eq!(listed.status(), ToolCallResultStatus::Succeeded);
        let listed_json: Value = serde_json::from_str(listed.content().as_str()).expect("json");
        assert_eq!(listed_json["count"], 1);
        assert_eq!(listed_json["requests"][0]["request_id"], "62576.34");

        let details = net_tool
            .execute(
                call(
                    BROWSER_NETWORK_TOOL,
                    json!({ "action": "details", "request_id": 62576.34 }),
                ),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("network details succeeds");
        assert_eq!(details.status(), ToolCallResultStatus::Succeeded);
        let details_json: Value = serde_json::from_str(details.content().as_str()).expect("json");
        assert_eq!(details_json["body"], r#"{"error":"boom"}"#);

        // 6. Close shuts down cleanly.
        let closed = tab_tool
            .execute(
                call(BROWSER_TAB_TOOL, json!({ "action": "close" })),
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
            .expect("close succeeds");
        assert_eq!(closed.status(), ToolCallResultStatus::Succeeded);
        server_task.abort();
    }
}
