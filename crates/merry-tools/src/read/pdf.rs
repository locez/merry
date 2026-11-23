//! Pure-Rust PDF page rendering and text extraction backed by `hayro`.

use std::{fmt::Write as _, fs, path::Path, sync::Arc};

use hayro::{
    PixmapSettings, RenderCache, RenderSettings,
    hayro_interpret::{
        BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
        InterpreterCache, InterpreterSettings, SoftMask, font::GlyphRun, hayro_cmap::BfString,
        hayro_syntax::Pdf, interpret_page, util::TransformExt,
    },
    kurbo::{BezPath, Rect},
    vello_cpu::color::palette::css::WHITE,
};
use merry_runtime::{
    MAX_USER_IMAGE_TOTAL_PNG_BYTES, MAX_USER_IMAGES, ToolExecutionOutcome, UserImageInput,
};
use serde::{Deserialize, Serialize};

use crate::{
    READ_FILE_TOOL,
    config::WorkspaceToolLimits,
    errors::{
        BlockingToolError, DomainError, ERROR_FILE_TOO_LARGE, ERROR_INVALID_ARGUMENTS,
        ERROR_INVALID_DOCUMENT,
    },
    file::read_bounded,
    path::ValidatedToolPath,
    read::{
        image::{
            MAX_EMBEDDED_IMAGE_DIMENSION, MAX_EMBEDDED_IMAGE_PIXELS,
            encode_dynamic_image_under_budget,
        },
        input::PdfReadFormat,
        ooxml::MAX_EXTRACTED_TEXT_BYTES,
        slice_text_outcome,
    },
};

const PDF_MAGIC: &[u8; 5] = b"%PDF-";

/// Maximum PDF file size accepted by `read_file` (50 MiB).
const MAX_PDF_BYTES: usize = 50 * 1024 * 1024;
/// Maximum page count automatically read when `pages` is omitted.
const PDF_AUTO_READ_THRESHOLD: usize = 10;
/// Maximum number of PDF pages returned by one `read_file` call.
const PDF_MAX_PAGES_PER_READ: usize = MAX_USER_IMAGES;
/// Target rendering DPI for PDF pages (72 pt/in -> ~2.08x scale at 150 DPI).
const PDF_RENDER_DPI: f32 = 150.0;
/// Maximum PNG byte budget per rendered PDF page (1 MiB so 20 pages <= 20 MiB total).
const MAX_PDF_PAGE_PNG_BYTES: usize = MAX_USER_IMAGE_TOTAL_PNG_BYTES / PDF_MAX_PAGES_PER_READ;
/// Maximum pixel area per rendered PDF page (~1.5 Mpx for sharp text + bounded vision tokens).
const MAX_PDF_PAGE_PIXELS: u64 = 1_572_864;

/// Structured success envelope produced by `read_file` when rendering PDF pages as images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceReadPdfPagesSuccess {
    /// Always `true` for a successful PDF page render.
    pub ok: bool,
    /// Stable tool name (`read_file`).
    pub tool: String,
    /// Content kind discriminant (`pdf_pages`).
    pub kind: String,
    /// Normalized display path.
    pub path: String,
    /// Total number of pages in the PDF document.
    pub total_pages: usize,
    /// One-based page numbers rendered and attached in order.
    pub rendered_pages: Vec<usize>,
    /// Source PDF file size in bytes.
    pub source_bytes: usize,
    /// Combined PNG byte size of all attached page images.
    pub total_png_bytes: usize,
}

impl WorkspaceReadPdfPagesSuccess {
    fn model_text(&self) -> String {
        let pages_label = format_page_list(&self.rendered_pages);
        format!(
            "{}: PDF rendered {} of {} pages ({pages_label}, {} bytes PNG attached)",
            self.path,
            self.rendered_pages.len(),
            self.total_pages,
            self.total_png_bytes
        )
    }
}

/// Detects whether a file should be handled as a PDF document.
pub(super) fn is_pdf_file(path: &Path, sniff_bytes: &[u8]) -> bool {
    sniff_bytes.starts_with(PDF_MAGIC) || super::has_extension(path, "pdf")
}

pub(super) struct PdfReadRequest<'a> {
    pub(super) pages: Option<&'a str>,
    pub(super) format: PdfReadFormat,
    pub(super) start_line: usize,
    pub(super) max_lines: usize,
}

pub(super) fn read_resolved_pdf(
    relative: &ValidatedToolPath,
    file: &mut fs::File,
    request: PdfReadRequest<'_>,
    limits: &WorkspaceToolLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let bytes = read_bounded(file, MAX_PDF_BYTES, is_cancelled).map_err(|error| match error {
        BlockingToolError::Domain(domain) if domain.code == ERROR_FILE_TOO_LARGE => {
            BlockingToolError::Domain(DomainError::new(
                ERROR_FILE_TOO_LARGE,
                "PDF file exceeds the 50 MiB limit",
            ))
        }
        other => other,
    })?;
    let source_bytes = bytes.len();

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let pdf = Pdf::new(Arc::new(bytes)).map_err(|error| {
        DomainError::new(
            ERROR_INVALID_DOCUMENT,
            format!("could not open PDF document: {error:?}"),
        )
    })?;
    let pages = pdf.pages();
    let total_pages = pages.len();
    if total_pages == 0 {
        return Err(DomainError::new(ERROR_INVALID_DOCUMENT, "PDF document has no pages").into());
    }

    let page_indices = resolve_page_indices(request.pages, total_pages)?;

    match request.format {
        PdfReadFormat::Image => render_pdf_pages_outcome(
            relative,
            pages,
            total_pages,
            &page_indices,
            source_bytes,
            is_cancelled,
        ),
        PdfReadFormat::Text => {
            let text = extract_pdf_pages_text(pages, &page_indices, is_cancelled)?;
            Ok(slice_text_outcome(
                relative,
                &text,
                request.start_line,
                request.max_lines,
                limits,
            ))
        }
    }
}

fn resolve_page_indices(
    pages_spec: Option<&str>,
    total_pages: usize,
) -> Result<Vec<usize>, DomainError> {
    match pages_spec {
        Some(spec) => parse_page_range(spec, total_pages),
        None => {
            if total_pages > PDF_AUTO_READ_THRESHOLD {
                return Err(DomainError::new(
                    ERROR_INVALID_ARGUMENTS,
                    format!(
                        "PDF has {total_pages} pages, exceeding the {PDF_AUTO_READ_THRESHOLD}-page auto-read limit; specify `pages` (e.g. `\"1-5\"`, max {PDF_MAX_PAGES_PER_READ} pages per call)"
                    ),
                ));
            }
            Ok((0..total_pages).collect())
        }
    }
}

/// Parses a one-based page range specification (e.g. `"1-5"`, `"3"`, `"10-"`, `"1,3-5"`)
/// into sorted, deduplicated zero-based page indices.
pub(crate) fn parse_page_range(spec: &str, page_count: usize) -> Result<Vec<usize>, DomainError> {
    let mut pages = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start_raw, end_raw)) = part.split_once('-') {
            let start: usize = start_raw.trim().parse().map_err(|_| {
                DomainError::new(
                    ERROR_INVALID_ARGUMENTS,
                    format!("invalid PDF page number: '{}'", start_raw.trim()),
                )
            })?;
            let end: usize = if end_raw.trim().is_empty() {
                page_count
            } else {
                end_raw.trim().parse().map_err(|_| {
                    DomainError::new(
                        ERROR_INVALID_ARGUMENTS,
                        format!("invalid PDF page number: '{}'", end_raw.trim()),
                    )
                })?
            };
            if start < 1 || start > page_count {
                return Err(DomainError::new(
                    ERROR_INVALID_ARGUMENTS,
                    format!("PDF page {start} is out of range (document has {page_count} pages)"),
                ));
            }
            if start > end {
                return Err(DomainError::new(
                    ERROR_INVALID_ARGUMENTS,
                    format!("invalid PDF page range: {start}-{end} (start must be <= end)"),
                ));
            }
            for page in start..=end.min(page_count) {
                pages.push(page - 1);
            }
        } else {
            let page: usize = part.parse().map_err(|_| {
                DomainError::new(
                    ERROR_INVALID_ARGUMENTS,
                    format!("invalid PDF page number: '{part}'"),
                )
            })?;
            if page < 1 || page > page_count {
                return Err(DomainError::new(
                    ERROR_INVALID_ARGUMENTS,
                    format!("PDF page {page} is out of range (document has {page_count} pages)"),
                ));
            }
            pages.push(page - 1);
        }
    }

    pages.sort_unstable();
    pages.dedup();
    if pages.is_empty() {
        return Err(DomainError::new(
            ERROR_INVALID_ARGUMENTS,
            "PDF `pages` parameter did not specify any pages",
        ));
    }
    if pages.len() > PDF_MAX_PAGES_PER_READ {
        return Err(DomainError::new(
            ERROR_INVALID_ARGUMENTS,
            format!(
                "requested {} PDF pages, maximum is {PDF_MAX_PAGES_PER_READ} pages per call",
                pages.len()
            ),
        ));
    }
    Ok(pages)
}

fn render_pdf_pages_outcome<'a>(
    relative: &ValidatedToolPath,
    pages: &'a [hayro::hayro_syntax::page::Page<'a>],
    total_pages: usize,
    page_indices: &[usize],
    source_bytes: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    let cache = RenderCache::new();
    let interpreter_settings = InterpreterSettings::default();
    let render_settings = RenderSettings::default();
    let per_page_max_bytes =
        (MAX_USER_IMAGE_TOTAL_PNG_BYTES / page_indices.len().max(1)).min(MAX_PDF_PAGE_PNG_BYTES);

    let mut attached_images = Vec::with_capacity(page_indices.len());
    let mut rendered_pages = Vec::with_capacity(page_indices.len());
    let mut total_png_bytes = 0usize;

    for &page_idx in page_indices {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        let page = &pages[page_idx];
        let scale = safe_render_scale(page.render_dimensions());
        let pixmap = hayro::render(
            page,
            &cache,
            &interpreter_settings,
            &render_settings,
            &PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
            },
        );
        let width = u32::from(pixmap.width());
        let height = u32::from(pixmap.height());
        if width == 0 || height == 0 {
            return Err(DomainError::new(
                ERROR_INVALID_DOCUMENT,
                format!("PDF page {} rendered to empty dimensions", page_idx + 1),
            )
            .into());
        }

        let png_bytes = pixmap.into_png().map_err(|error| {
            DomainError::new(
                ERROR_INVALID_DOCUMENT,
                format!("failed to encode PDF page {} as PNG: {error}", page_idx + 1),
            )
        })?;

        let (png_bytes, final_w, final_h) = if png_bytes.len() <= per_page_max_bytes
            && width <= MAX_EMBEDDED_IMAGE_DIMENSION
            && height <= MAX_EMBEDDED_IMAGE_DIMENSION
            && u64::from(width) * u64::from(height) <= MAX_EMBEDDED_IMAGE_PIXELS
        {
            (png_bytes, width, height)
        } else {
            let decoded = image::load_from_memory_with_format(&png_bytes, image::ImageFormat::Png)
                .map_err(|error| {
                    DomainError::new(
                        ERROR_INVALID_DOCUMENT,
                        format!(
                            "failed to re-decode rendered PDF page {}: {error}",
                            page_idx + 1
                        ),
                    )
                })?;
            encode_dynamic_image_under_budget(
                &decoded,
                per_page_max_bytes,
                MAX_EMBEDDED_IMAGE_DIMENSION,
                MAX_PDF_PAGE_PIXELS,
                is_cancelled,
            )?
        };

        let page_number = page_idx + 1;
        total_png_bytes = total_png_bytes.saturating_add(png_bytes.len());
        let label = format!("{}#page={page_number}", relative.display);
        let image = UserImageInput::png(&label, png_bytes, final_w, final_h).map_err(|error| {
            DomainError::new(
                ERROR_INVALID_DOCUMENT,
                format!("rendered PDF page {page_number} failed image validation: {error}"),
            )
        })?;
        attached_images.push(image);
        rendered_pages.push(page_number);
    }

    let payload = WorkspaceReadPdfPagesSuccess {
        ok: true,
        tool: READ_FILE_TOOL.to_owned(),
        kind: "pdf_pages".to_owned(),
        path: relative.display.clone(),
        total_pages,
        rendered_pages,
        source_bytes,
        total_png_bytes,
    };

    Ok(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("read_file pdf envelope serializes"),
    )
    .with_model_text(payload.model_text())
    .with_images(attached_images))
}

/// Computes a safe uniform scale factor from PDF points (72 DPI) that targets
/// 150 DPI while keeping output dimensions inside `MAX_EMBEDDED_IMAGE_DIMENSION`
/// and `MAX_PDF_PAGE_PIXELS` (and safely below `u16::MAX`).
fn safe_render_scale((width_pt, height_pt): (f32, f32)) -> f32 {
    if !width_pt.is_finite() || !height_pt.is_finite() || width_pt <= 0.0 || height_pt <= 0.0 {
        return 1.0;
    }
    let target_scale = PDF_RENDER_DPI / 72.0;
    let max_side = width_pt.max(height_pt);
    let side_cap = (MAX_EMBEDDED_IMAGE_DIMENSION as f32) / max_side;
    let area_cap =
        ((MAX_PDF_PAGE_PIXELS as f64) / (f64::from(width_pt) * f64::from(height_pt))).sqrt() as f32;
    target_scale
        .min(side_cap)
        .min(area_cap)
        .clamp(0.1, target_scale)
}

fn extract_pdf_pages_text<'a>(
    pages: &'a [hayro::hayro_syntax::page::Page<'a>],
    page_indices: &[usize],
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, BlockingToolError> {
    let interpreter_cache = InterpreterCache::new();
    let interpreter_settings = InterpreterSettings::default();
    let mut output = String::new();

    for (index, &page_idx) in page_indices.iter().enumerate() {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        if index > 0 {
            output.push('\n');
        }
        let _ = writeln!(&mut output, "--- Page {} ---", page_idx + 1);
        let page = &pages[page_idx];
        let (w, h) = page.render_dimensions();
        let target = Rect::new(0.0, 0.0, f64::from(w.max(1.0)), f64::from(h.max(1.0)));
        let transform = page.initial_transform(true).to_kurbo();
        let mut ctx = Context::new(
            transform,
            target,
            &interpreter_cache,
            page.xref(),
            interpreter_settings.clone(),
        );
        let mut device = TextExtractDevice::default();
        interpret_page(page, &mut ctx, &mut device);
        let page_text = device.finish();
        output.push_str(&page_text);
        if !page_text.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
    }

    Ok(output)
}

#[derive(Default)]
struct TextExtractDevice {
    text: String,
    last_y: Option<f64>,
    last_x: Option<f64>,
}

impl TextExtractDevice {
    fn finish(self) -> String {
        self.text.trim().to_owned()
    }
}

impl<'a> Device<'a> for TextExtractDevice {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph_run(&mut self, glyph_run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        if self.text.len() >= MAX_EXTRACTED_TEXT_BYTES {
            return;
        }
        for glyph in glyph_run.glyphs() {
            if self.text.len() >= MAX_EXTRACTED_TEXT_BYTES {
                break;
            }
            let Some(unicode) = glyph.as_unicode() else {
                continue;
            };
            let full = props.transform * glyph.transform();
            let pos = full.translation();
            // `hayro-interpret` scales `glyph.transform()` by `1.0 / 1000.0` (`UNITS_PER_EM`),
            // so multiplying back by 1000 yields the rendered font size in page points.
            let font_size_pt = (full
                .as_coeffs()
                .iter()
                .take(4)
                .fold(0.0_f64, |acc, v| acc.max(v.abs()))
                * 1_000.0)
                .max(4.0);

            if let Some(prev_y) = self.last_y {
                let dy = (pos.y - prev_y).abs();
                if dy > font_size_pt * 0.55 {
                    if !self.text.is_empty() && !self.text.ends_with('\n') {
                        self.text.push('\n');
                    }
                    self.last_x = None;
                } else if let Some(prev_x) = self.last_x {
                    let dx = pos.x - prev_x;
                    if dx > font_size_pt * 1.05
                        && !self.text.is_empty()
                        && !self.text.ends_with(|ch: char| ch.is_whitespace())
                    {
                        self.text.push(' ');
                    }
                }
            }
            self.last_y = Some(pos.y);
            self.last_x = Some(pos.x);

            match unicode {
                BfString::Char(ch) => {
                    if ch != '\0' {
                        self.text.push(ch);
                    }
                }
                BfString::String(s) => {
                    self.text.push_str(&s.replace('\0', ""));
                }
            }
        }
    }
}

fn format_page_list(pages: &[usize]) -> String {
    if pages.is_empty() {
        return "none".to_owned();
    }
    let mut parts = Vec::new();
    let mut start = pages[0];
    let mut prev = pages[0];
    for &page in &pages[1..] {
        if page == prev + 1 {
            prev = page;
        } else {
            push_page_span(&mut parts, start, prev);
            start = page;
            prev = page;
        }
    }
    push_page_span(&mut parts, start, prev);
    format!("pages {}", parts.join(", "))
}

fn push_page_span(parts: &mut Vec<String>, start: usize, end: usize) {
    if start == end {
        parts.push(start.to_string());
    } else {
        parts.push(format!("{start}-{end}"));
    }
}
