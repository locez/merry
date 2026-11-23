use crate::{
    McpResult,
    registry::{DEFAULT_SEARCH_LIMIT, McpRegistry},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, GenericImageView, ImageReader, imageops::FilterType};
use merry_core::{ErrorInfo, PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use merry_runtime::{
    MAX_USER_IMAGE_DIMENSION, MAX_USER_IMAGE_PIXELS, MAX_USER_IMAGE_PNG_BYTES,
    MAX_USER_IMAGE_TOTAL_PNG_BYTES, MAX_USER_IMAGES, RegisteredTool, ToolActionKind,
    ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture, UserImageInput, user_image_label,
};
use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{borrow::Cow, io::Cursor, sync::Arc};

/// Provider-visible tool name for searching and refreshing MCP tools.
pub const MCP_SEARCH_TOOL: &str = "mcp_search";

/// Provider-visible tool name for invoking a discovered MCP tool.
pub const MCP_CALL_TOOL: &str = "mcp_call";

const MIN_MCP_IMAGE_DIMENSION: u32 = 128;
const MAX_MCP_IMAGE_DECODE_PIXELS: u64 = 64_000_000;

/// A registered MCP meta-tool exposed to the runtime.
#[derive(Debug)]
pub struct McpToolRegistration {
    pub tool: RegisteredTool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct McpSearchInput {
    /// Keyword query to search across tool names, descriptions, server IDs, and parameters. Omit to list servers and available tools.
    query: Option<String>,
    /// Optional configured MCP server ID to filter search or inspect all tools on that server.
    server: Option<String>,
    /// When true, re-fetch tool lists from connected servers and immediately retry disconnected servers.
    #[serde(default)]
    refresh: Option<bool>,
    /// Maximum number of matching tool definitions to return (default 10, max 50).
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct McpCallInput {
    /// Configured MCP server ID.
    server: String,
    /// Exact raw MCP tool name on the target server (from mcp_search).
    tool: String,
    /// JSON object of arguments matching the tool's inputSchema.
    #[serde(default)]
    arguments: Option<Map<String, Value>>,
}

pub(crate) fn meta_tool_registrations(
    registry: Arc<McpRegistry>,
) -> McpResult<Vec<McpToolRegistration>> {
    let search_spec = mcp_search_spec(&registry)?;
    let call_spec = mcp_call_spec()?;

    let search = McpToolRegistration {
        tool: RegisteredTool::read_only(
            search_spec,
            Arc::new(McpSearchExecutor {
                registry: Arc::clone(&registry),
            }),
        )
        .with_parallel_safe_execution(),
    };
    let call = McpToolRegistration {
        tool: RegisteredTool::new(
            call_spec,
            Arc::new(McpCallExecutor { registry }),
            ToolActionKind::TrustedExternal,
        ),
    };
    Ok(vec![search, call])
}

const MAX_INLINE_DESCRIPTION_SERVER_CHARS: usize = 240;

fn mcp_search_spec(registry: &McpRegistry) -> McpResult<ToolSpec> {
    let servers = format_server_list_preview(&registry.server_ids());
    let description = format!(
        "Search or list tools exposed by configured MCP servers and return their parameter schemas for mcp_call. Set refresh=true to reload updated tool lists or reconnect offline servers. Configured servers: {servers}."
    );
    let input_schema = ToolInputSchema::new(schema_for!(McpSearchInput))?;
    Ok(ToolSpec::new(
        ToolName::new(MCP_SEARCH_TOOL)?,
        &description,
        input_schema,
    )?)
}

fn format_server_list_preview(ids: &[&str]) -> String {
    let mut preview = String::new();
    for (index, id) in ids.iter().enumerate() {
        let separator = if index == 0 { "" } else { ", " };
        if index > 0
            && preview.len() + separator.len() + id.len() > MAX_INLINE_DESCRIPTION_SERVER_CHARS
        {
            let remaining = ids.len() - index;
            preview.push_str(&format!(", ... (+{remaining} more)"));
            break;
        }
        preview.push_str(separator);
        preview.push_str(id);
    }
    preview
}

fn mcp_call_spec() -> McpResult<ToolSpec> {
    let input_schema = ToolInputSchema::new(schema_for!(McpCallInput))?;
    Ok(ToolSpec::new(
        ToolName::new(MCP_CALL_TOOL)?,
        "Invoke a tool on a configured MCP server by server ID and raw tool name. Use mcp_search first to discover tool names and input schemas.",
        input_schema,
    )?)
}

struct McpSearchExecutor {
    registry: Arc<McpRegistry>,
}

impl ToolExecutor for McpSearchExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }
            let input: McpSearchInput = call.arguments().deserialize_as().map_err(|error| {
                ToolExecutionError::infrastructure(format!("invalid mcp_search arguments: {error}"))
            })?;
            let result = tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => return Err(ToolExecutionError::Cancelled),
                result = self.registry.search(
                    input.query.as_deref(),
                    input.server.as_deref(),
                    input.limit.unwrap_or(DEFAULT_SEARCH_LIMIT),
                    input.refresh.unwrap_or(false),
                ) => result,
            };
            match result {
                Ok(payload) => Ok(ToolExecutionOutcome::succeeded_json(payload.to_string())),
                Err(message) => unavailable_outcome("mcp_server_not_found", &message),
            }
        })
    }
}

struct McpCallExecutor {
    registry: Arc<McpRegistry>,
}

impl ToolExecutor for McpCallExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }
            let input: McpCallInput = call.arguments().deserialize_as().map_err(|error| {
                ToolExecutionError::infrastructure(format!("invalid mcp_call arguments: {error}"))
            })?;
            self.registry
                .call(
                    &input.server,
                    &input.tool,
                    input.arguments.unwrap_or_default(),
                    context.cancellation_token().clone(),
                )
                .await
        })
    }
}

pub(crate) fn unavailable_outcome(
    code: &str,
    message: &str,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    let diagnostic = ErrorInfo::new(code, message)
        .map_err(|_| ToolExecutionError::infrastructure("invalid MCP diagnostic"))?;
    Ok(ToolExecutionOutcome::failed_text(message, diagnostic))
}

pub(crate) fn mcp_call_result_to_outcome(
    server_id: &str,
    raw_tool_name: &str,
    result: CallToolResult,
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
    result: CallToolResult,
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

fn joined_mcp_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentBlock::Text(text) => Some(strip_inline_data_image_uris(&text.text)),
            ContentBlock::Resource(embedded) => match &embedded.resource {
                ResourceContents::TextResourceContents { text, .. } => {
                    Some(strip_inline_data_image_uris(text))
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn sanitized_content_summary(content: &[ContentBlock]) -> Vec<Value> {
    content
        .iter()
        .map(|item| match item {
            ContentBlock::Text(text) => json!({
                "type": "text",
                "text": strip_inline_data_image_uris(&text.text),
            }),
            ContentBlock::Image(image) => json!({
                "type": "image",
                "mimeType": image.mime_type,
                "omittedBase64": true,
            }),
            ContentBlock::Audio(audio) => json!({
                "type": "audio",
                "mimeType": audio.mime_type,
                "omittedBase64": true,
            }),
            ContentBlock::Resource(_) => json!({
                "type": "resource",
            }),
            ContentBlock::ResourceLink(link) => json!({
                "type": "resource_link",
                "uri": link.uri,
            }),
            _ => json!({
                "type": "other",
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

fn extract_mcp_images(content: &[ContentBlock]) -> Vec<UserImageInput> {
    let mut images = Vec::new();
    let mut total_png_bytes = 0usize;
    for item in content {
        if images.len() >= MAX_USER_IMAGES || total_png_bytes >= MAX_USER_IMAGE_TOTAL_PNG_BYTES {
            break;
        }
        let raw_b64 = match item {
            ContentBlock::Image(image) => Some(image.data.as_str()),
            ContentBlock::Resource(embedded) => match &embedded.resource {
                ResourceContents::BlobResourceContents {
                    blob,
                    mime_type: Some(mime),
                    ..
                } if mime.starts_with("image/") => Some(blob.as_str()),
                _ => None,
            },
            _ => None,
        };
        let Some(raw_b64) = raw_b64 else {
            continue;
        };
        let max_png_bytes = MAX_USER_IMAGE_PNG_BYTES
            .min(MAX_USER_IMAGE_TOTAL_PNG_BYTES.saturating_sub(total_png_bytes));
        let label = user_image_label(images.len() + 1).expect("bounded 1-based image index");
        if let Some(image) = decode_and_normalize_mcp_image(&label, raw_b64, max_png_bytes) {
            total_png_bytes = total_png_bytes.saturating_add(image.png_bytes().len());
            images.push(image);
        }
    }
    images
}

fn decode_and_normalize_mcp_image(
    label: &str,
    raw_b64: &str,
    max_png_bytes: usize,
) -> Option<UserImageInput> {
    let trimmed = raw_b64.trim();
    let payload = trimmed
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
        .map_or(trimmed, |(_, data)| data);
    let cleaned: String = payload.chars().filter(|ch| !ch.is_whitespace()).collect();
    let decoded = STANDARD.decode(cleaned).ok()?;
    let (header_w, header_h) = ImageReader::new(Cursor::new(&decoded))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    let header_pixels = u64::from(header_w).checked_mul(u64::from(header_h))?;
    if header_w == 0 || header_h == 0 || header_pixels > MAX_MCP_IMAGE_DECODE_PIXELS {
        return None;
    }

    let decoded_image = image::load_from_memory(&decoded).ok()?;
    let (orig_w, orig_h) = decoded_image.dimensions();
    if orig_w == 0 || orig_h == 0 {
        return None;
    }

    let fits_caps = orig_w <= MAX_USER_IMAGE_DIMENSION
        && orig_h <= MAX_USER_IMAGE_DIMENSION
        && u64::from(orig_w) * u64::from(orig_h) <= MAX_USER_IMAGE_PIXELS;

    if fits_caps
        && decoded.starts_with(b"\x89PNG\r\n\x1a\n")
        && decoded.len() <= max_png_bytes
        && let Ok(input) = UserImageInput::png(label, decoded, orig_w, orig_h)
    {
        return Some(input);
    }

    let original_max_side = orig_w.max(orig_h);
    let mut max_side = MAX_USER_IMAGE_DIMENSION.min(original_max_side);
    if u64::from(orig_w) * u64::from(orig_h) > MAX_USER_IMAGE_PIXELS {
        let scale =
            (MAX_USER_IMAGE_PIXELS as f64 / (u64::from(orig_w) * u64::from(orig_h)) as f64).sqrt();
        max_side = max_side.min(((f64::from(original_max_side) * scale).floor() as u32).max(1));
    }

    loop {
        let scaled: Cow<'_, DynamicImage> = if max_side < original_max_side {
            Cow::Owned(decoded_image.resize(max_side, max_side, FilterType::Lanczos3))
        } else {
            Cow::Borrowed(&decoded_image)
        };
        let (width, height) = scaled.dimensions();
        let mut cursor = Cursor::new(Vec::new());
        scaled.write_to(&mut cursor, image::ImageFormat::Png).ok()?;
        let png_bytes = cursor.into_inner();
        if png_bytes.len() <= max_png_bytes
            && width <= MAX_USER_IMAGE_DIMENSION
            && height <= MAX_USER_IMAGE_DIMENSION
            && u64::from(width) * u64::from(height) <= MAX_USER_IMAGE_PIXELS
        {
            return UserImageInput::png(label, png_bytes, width, height).ok();
        }
        if max_side <= MIN_MCP_IMAGE_DIMENSION {
            return None;
        }
        max_side = (max_side * 3 / 4).max(MIN_MCP_IMAGE_DIMENSION);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{ToolCallResultStatus, ToolOutput};

    fn png_base64(width: u32, height: u32, rgba: [u8; 4]) -> String {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("png encodes");
        STANDARD.encode(&png)
    }

    #[test]
    fn converts_text_result_to_successful_outcome() {
        let outcome = mcp_call_result_to_outcome(
            "docs",
            "read.file",
            CallToolResult::success(vec![ContentBlock::text("hello")]),
        );

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert_eq!(outcome.content(), &ToolOutput::text("hello"));
    }

    #[test]
    fn extracts_mcp_image_blocks_and_sanitizes_inline_data_uris() {
        let b64 = png_base64(2, 2, [255, 0, 0, 255]);

        let outcome = mcp_call_result_to_outcome(
            "playwright",
            "screenshot",
            CallToolResult::success(vec![
                ContentBlock::text(format!("captured data:image/png;base64,{b64} done")),
                ContentBlock::image(b64, "image/png"),
            ]),
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
        let b64 = png_base64(1, 1, [0, 255, 0, 255]);

        let outcome = mcp_call_result_to_outcome(
            "playwright",
            "click",
            CallToolResult::error(vec![ContentBlock::image(b64, "image/png")]),
        );

        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_eq!(outcome.images().len(), 1);
    }

    #[test]
    fn oversized_mcp_image_is_downscaled_instead_of_dropped() {
        let b64 = png_base64(3_000, 1_500, [20, 40, 60, 255]);

        let outcome = mcp_call_result_to_outcome(
            "playwright",
            "screenshot",
            CallToolResult::success(vec![ContentBlock::image(b64, "image/png")]),
        );

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert_eq!(outcome.images().len(), 1);
        let attached = &outcome.images()[0];
        assert!(attached.width() <= MAX_USER_IMAGE_DIMENSION);
        assert!(attached.height() <= MAX_USER_IMAGE_DIMENSION);
        assert!(
            u64::from(attached.width()) * u64::from(attached.height()) <= MAX_USER_IMAGE_PIXELS
        );
    }
}
