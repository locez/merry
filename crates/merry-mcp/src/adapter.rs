use crate::{
    connection::{McpConnection, ToolUnavailable},
    diagnostics::{McpFailureKind, McpServerIssue},
    protocol::{McpContent, ToolsCallResult},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use merry_core::{ErrorInfo, PendingToolCall, SessionToolCatalogEntry};
use merry_runtime::{
    MAX_USER_IMAGE_DIMENSION, MAX_USER_IMAGE_PNG_BYTES, MAX_USER_IMAGES, RegisteredTool,
    ToolActionKind, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture, UserImageInput, user_image_label,
};
use serde_json::{Value, json};
use std::{io::Cursor, sync::Arc};

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
    let images = extract_mcp_images(&result.content);
    let is_error = result.is_error.unwrap_or(false);
    if is_error {
        return failed_outcome(server_id, raw_tool_name, result).with_images(images);
    }

    if let Some(structured) = result.structured_content {
        return ToolExecutionOutcome::succeeded_json(
            json!({
                "structuredContent": structured,
                "content": sanitized_content_summary(&result.content),
            })
            .to_string(),
        )
        .with_images(images);
    }

    let text = joined_mcp_text(&result.content);
    let body = if text.trim().is_empty() && !images.is_empty() {
        format!("[MCP tool returned {} image(s)]", images.len())
    } else if text.trim().is_empty() {
        json!({
            "server": server_id,
            "tool": raw_tool_name,
            "content": sanitized_content_summary(&result.content),
        })
        .to_string()
    } else {
        text
    };
    ToolExecutionOutcome::succeeded_text(body).with_images(images)
}

fn failed_outcome(
    server_id: &str,
    raw_tool_name: &str,
    result: ToolsCallResult,
) -> ToolExecutionOutcome {
    let text = joined_mcp_text(&result.content);
    let content = if text.trim().is_empty() {
        json!({
            "server": server_id,
            "tool": raw_tool_name,
            "content": sanitized_content_summary(&result.content),
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

fn joined_mcp_text(content: &[McpContent]) -> String {
    content
        .iter()
        .filter(|item| item.kind == "text")
        .filter_map(|item| item.text.as_deref())
        .map(strip_inline_data_image_uris)
        .collect::<Vec<_>>()
        .join("\n")
}

fn sanitized_content_summary(content: &[McpContent]) -> Vec<Value> {
    content
        .iter()
        .map(|item| match item.kind.as_str() {
            "text" => json!({
                "type": "text",
                "text": item.text.as_deref().map(strip_inline_data_image_uris),
            }),
            "image" => json!({
                "type": "image",
                "mimeType": item.extra.get("mimeType").or_else(|| item.extra.get("mime_type")),
                "omittedBase64": true,
            }),
            _ => json!({
                "type": item.kind,
            }),
        })
        .collect()
}

fn strip_inline_data_image_uris(input: &str) -> String {
    const PREFIX: &str = "data:image/";
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find(PREFIX) {
        output.push_str(&rest[..start]);
        let after_prefix = &rest[start..];
        let Some(base64_marker) = after_prefix.find(";base64,") else {
            output.push_str(PREFIX);
            rest = &rest[start + PREFIX.len()..];
            continue;
        };
        let header = &after_prefix[..base64_marker];
        if header.chars().any(char::is_whitespace) || header.len() > 64 {
            output.push_str(PREFIX);
            rest = &rest[start + PREFIX.len()..];
            continue;
        }
        let payload_start = base64_marker + ";base64,".len();
        let payload = &after_prefix[payload_start..];
        let payload_len = payload
            .find(|ch: char| {
                !(ch.is_ascii_alphanumeric()
                    || ch == '+'
                    || ch == '/'
                    || ch == '='
                    || ch == '-'
                    || ch == '_')
            })
            .unwrap_or(payload.len());
        output.push_str("<data-image-omitted>");
        rest = &after_prefix[payload_start + payload_len..];
    }
    output.push_str(rest);
    output
}

fn extract_mcp_images(content: &[McpContent]) -> Vec<UserImageInput> {
    let mut images = Vec::new();
    for item in content {
        if images.len() >= MAX_USER_IMAGES {
            break;
        }
        let raw_b64 = match item.kind.as_str() {
            "image" => item.extra.get("data").and_then(Value::as_str),
            "resource" => item.extra.get("resource").and_then(|res| {
                let mime = res
                    .get("mimeType")
                    .or_else(|| res.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if mime.starts_with("image/") {
                    res.get("blob").and_then(Value::as_str)
                } else {
                    None
                }
            }),
            _ => None,
        };
        let Some(raw_b64) = raw_b64 else {
            continue;
        };
        let label = user_image_label(images.len() + 1).expect("bounded 1-based image index");
        if let Some(image) = decode_and_normalize_mcp_image(&label, raw_b64) {
            images.push(image);
        }
    }
    images
}

fn decode_and_normalize_mcp_image(label: &str, raw_b64: &str) -> Option<UserImageInput> {
    let trimmed = raw_b64.trim();
    let payload = trimmed
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
        .map_or(trimmed, |(_, data)| data);
    let cleaned: String = payload.chars().filter(|ch| !ch.is_whitespace()).collect();
    let decoded = STANDARD.decode(cleaned).ok()?;
    let decoded_image = image::load_from_memory(&decoded).ok()?;
    let width = decoded_image.width();
    let height = decoded_image.height();
    if width == 0
        || height == 0
        || width > MAX_USER_IMAGE_DIMENSION
        || height > MAX_USER_IMAGE_DIMENSION
    {
        return None;
    }

    let png_bytes =
        if decoded.starts_with(b"\x89PNG\r\n\x1a\n") && decoded.len() <= MAX_USER_IMAGE_PNG_BYTES {
            decoded
        } else {
            let mut cursor = Cursor::new(Vec::new());
            decoded_image
                .write_to(&mut cursor, image::ImageFormat::Png)
                .ok()?;
            cursor.into_inner()
        };

    UserImageInput::png(label, png_bytes, width, height).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{McpContent, ToolsCallResult};
    use merry_core::{ToolCallResultStatus, ToolOutput};

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
        assert_eq!(outcome.content(), &ToolOutput::text("hello"));
    }

    #[test]
    fn extracts_mcp_image_blocks_and_sanitizes_inline_data_uris() {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("png encodes");
        let b64 = STANDARD.encode(&png);

        let mut extra = serde_json::Map::new();
        extra.insert("data".to_owned(), Value::String(b64.clone()));
        extra.insert("mimeType".to_owned(), Value::String("image/png".to_owned()));

        let outcome = mcp_call_result_to_outcome(
            "playwright",
            "screenshot",
            ToolsCallResult {
                content: vec![
                    McpContent {
                        kind: "text".to_owned(),
                        text: Some(format!("captured data:image/png;base64,{b64} done")),
                        extra: Default::default(),
                    },
                    McpContent {
                        kind: "image".to_owned(),
                        text: None,
                        extra,
                    },
                ],
                is_error: Some(false),
                structured_content: None,
            },
        );

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert_eq!(
            outcome.content(),
            &ToolOutput::text("captured <data-image-omitted> done")
        );
        assert_eq!(outcome.images().len(), 1);
        assert_eq!(outcome.images()[0].label(), "[Image #1]");
        assert_eq!(outcome.images()[0].width(), 2);
        assert_eq!(outcome.images()[0].height(), 2);
    }

    #[test]
    fn image_only_mcp_error_preserves_images_and_non_blank_text() {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 255, 0, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("png encodes");
        let b64 = STANDARD.encode(&png);
        let mut extra = serde_json::Map::new();
        extra.insert("data".to_owned(), Value::String(b64));

        let outcome = mcp_call_result_to_outcome(
            "playwright",
            "click",
            ToolsCallResult {
                content: vec![McpContent {
                    kind: "image".to_owned(),
                    text: None,
                    extra,
                }],
                is_error: Some(true),
                structured_content: None,
            },
        );

        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_eq!(outcome.images().len(), 1);
    }
}
