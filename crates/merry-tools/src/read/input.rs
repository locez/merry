use crate::{config::WorkspaceToolLimits, schema::with_property_limit};
use merry_core::{PendingToolCall, ToolSpec};
use merry_runtime::ToolBuildError;
use schemars::JsonSchema;
use serde::Deserialize;

/// Output mode for PDF files read by `read_file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PdfReadFormat {
    /// Render selected pages visually as images attached to the model context.
    #[default]
    Image,
    /// Extract plain text per page with `--- Page N ---` headers.
    Text,
}

#[merry_tools_macros::tool(
    crate = "crate",
    name = "read_file",
    description = "Reads a local file: UTF-8 text, raster images (PNG, JPEG, GIF, WebP; `.svg` as text), PDFs, Office documents (`.docx`, `.doc`, `.xlsx`, `.pptx`), and Jupyter notebooks (`.ipynb`). Accepts workspace-relative paths (also resolving read-only skill/resource roots) or absolute paths (including screenshots or temporary files outside the workspace, subject to sandbox reachability). When the user gives a file path, read it directly instead of probing existence first—a missing file simply returns an error. Images are presented visually (always use `read_file` for screenshots/image paths); PDFs default to visual `format: \"image\"` (or `format: \"text\"`), auto-reading up to 10 pages and requiring `pages` above 10 (max 20/call); notebooks return cells, outputs, and embedded images; text, Office, and PDF-text outputs support `start_line`/`max_lines` (continue at `end_line + 1` when `truncated`). Reads files only, not directories; prefer `read_file` over shell `cat`/`head`/`tail`."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadFileInput {
    #[schemars(
        description = "File path to read, relative to the workspace root or absolute (including dot-prefixed paths such as `.github/workflows`).",
        length(min = 1)
    )]
    pub(crate) path: String,
    #[serde(default)]
    #[schemars(
        description = "1-based start line for text, Office, notebook, and PDF-text outputs (default 1). Ignored for images and PDF image mode.",
        range(min = 1)
    )]
    pub(crate) start_line: Option<usize>,
    #[serde(default)]
    #[schemars(
        description = "Maximum lines to return for text-based outputs (defaults to the configured limit). Ignored for images and PDF image mode.",
        range(min = 1)
    )]
    pub(crate) max_lines: Option<usize>,
    #[serde(default)]
    #[schemars(
        description = "1-based PDF page range (e.g. `\"1-5\"`, `\"3\"`, `\"10-\"`, `\"1,3-5\"`). Required over 10 pages; max 20/call. Ignored for non-PDFs.",
        length(min = 1)
    )]
    pub(crate) pages: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "PDF output mode: `\"image\"` (default, visual pages) or `\"text\"`. Ignored for non-PDFs."
    )]
    pub(crate) format: Option<PdfReadFormat>,
}

pub(crate) fn spec(limits: &WorkspaceToolLimits) -> Result<ToolSpec, ToolBuildError> {
    with_property_limit(
        ReadFileInput::tool_spec()?,
        "max_lines",
        "maximum",
        limits.max_read_lines,
    )
}

pub(super) fn parse(call: &PendingToolCall) -> Result<ReadFileInput, String> {
    call.arguments()
        .deserialize_as()
        .map_err(|error| format!("invalid read_file arguments: {error}"))
}
