//! Local raster image decoding, validation, SVG safeguards, and automatic
//! downscaling shared across `read_file` image, PDF, and notebook paths.

use std::{borrow::Cow, fs, io::Cursor, path::Path};

use image::{
    AnimationDecoder, DynamicImage, GenericImageView, ImageFormat, ImageReader,
    imageops::FilterType,
};
use merry_runtime::{
    MAX_USER_IMAGE_DIMENSION, MAX_USER_IMAGE_PIXELS, MAX_USER_IMAGE_PNG_BYTES,
    ToolExecutionOutcome, UserImageInput,
};
use serde::{Deserialize, Serialize};

use crate::{
    READ_FILE_TOOL,
    errors::{
        BlockingToolError, DomainError, ERROR_FILE_TOO_LARGE, ERROR_IMAGE_TOO_LARGE,
        ERROR_INVALID_IMAGE,
    },
    file::read_bounded,
    path::ValidatedToolPath,
};

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Maximum encoded source file size accepted for image reads (20 MiB).
pub(crate) const MAX_IMAGE_SOURCE_BYTES: usize = 20 * 1024 * 1024;
/// Maximum decoded pixel count accepted before refusing to decode (64 Mpx).
const MAX_IMAGE_DECODE_PIXELS: u64 = 64_000_000;
/// Maximum width or height for images embedded by `read_file` after downscaling.
pub(crate) const MAX_EMBEDDED_IMAGE_DIMENSION: u32 = 2_000;
/// Maximum total pixel area (1920x1080) for images embedded by `read_file` after downscaling.
pub(crate) const MAX_EMBEDDED_IMAGE_PIXELS: u64 = 2_073_600;
/// Maximum raw PNG byte size for images embedded by `read_file` (3 MiB raw ~= 4 MiB base64).
pub(super) const MAX_EMBEDDED_IMAGE_PNG_BYTES: usize = 3 * 1024 * 1024;
/// Floor side length when shrinking oversized PNGs to fit the byte budget.
const MIN_EMBEDDED_IMAGE_DIMENSION: u32 = 128;

/// Raster image formats supported by `read_file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SupportedRasterFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
}

impl SupportedRasterFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::WebP => "webp",
        }
    }

    fn image_format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Gif => ImageFormat::Gif,
            Self::WebP => ImageFormat::WebP,
        }
    }
}

impl TryFrom<ImageFormat> for SupportedRasterFormat {
    type Error = ();

    fn try_from(value: ImageFormat) -> Result<Self, Self::Error> {
        match value {
            ImageFormat::Png => Ok(Self::Png),
            ImageFormat::Jpeg => Ok(Self::Jpeg),
            ImageFormat::Gif => Ok(Self::Gif),
            ImageFormat::WebP => Ok(Self::WebP),
            _ => Err(()),
        }
    }
}

/// Structured success envelope produced by `read_file` when reading an image.
///
/// The normalized PNG bytes themselves are attached to the tool outcome rather
/// than embedded in this JSON envelope, keeping stored artifacts compact and
/// avoiding duplicate base64 payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceReadImageSuccess {
    /// Always `true` for a successful image read.
    pub ok: bool,
    /// Stable tool name (`read_file`).
    pub tool: String,
    /// Content kind discriminant (`image`).
    pub kind: String,
    /// Normalized display path (workspace-relative when inside the workspace root).
    pub path: String,
    /// Detected source format (`png`, `jpeg`, `gif`, or `webp`).
    pub source_format: String,
    /// Original decoded image width in pixels.
    pub source_width: u32,
    /// Original decoded image height in pixels.
    pub source_height: u32,
    /// Attached image width in pixels (equal to `source_width` when not resized).
    pub width: u32,
    /// Attached image height in pixels (equal to `source_height` when not resized).
    pub height: u32,
    /// Whether the image was downscaled to fit vision dimension or byte budgets.
    pub resized: bool,
    /// Byte length of the source file on disk.
    pub source_bytes: usize,
    /// Byte length of the normalized PNG attached to the model result.
    pub png_bytes: usize,
}

impl WorkspaceReadImageSuccess {
    fn model_text(&self) -> String {
        if self.resized {
            format!(
                "{}: {} image ({}x{} resized to {}x{}, {} bytes source, {} bytes PNG attached)",
                self.path,
                self.source_format,
                self.source_width,
                self.source_height,
                self.width,
                self.height,
                self.source_bytes,
                self.png_bytes
            )
        } else {
            format!(
                "{}: {} image ({}x{}, {} bytes source, {} bytes PNG attached)",
                self.path,
                self.source_format,
                self.width,
                self.height,
                self.source_bytes,
                self.png_bytes
            )
        }
    }
}

/// Returns the supported raster image format when `sniff_bytes` should be handled
/// as a visual image rather than UTF-8 text.
///
/// `.svg` files always stay on the text path even if an Adobe Illustrator SVG
/// begins with a PNG thumbnail prefix.
pub(super) fn detect_raster_image_format(
    path: &Path,
    sniff_bytes: &[u8],
) -> Option<SupportedRasterFormat> {
    if super::has_extension(path, "svg") || sniff_bytes.is_empty() {
        return None;
    }
    SupportedRasterFormat::try_from(image::guess_format(sniff_bytes).ok()?).ok()
}

/// Extracts UTF-8 SVG markup from an `.svg` file that begins with an Adobe
/// Illustrator PNG thumbnail prefix.
pub(super) fn extract_svg_after_png_prefix(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let suffix = skip_complete_png_chunks(bytes);
    if let Some(xml) = find_ascii_case_insensitive(suffix, b"<?xml") {
        let text = std::str::from_utf8(suffix.get(xml..)?).ok()?;
        if text.contains("<svg") || text.contains("<SVG") {
            return Some(text.to_owned());
        }
    }
    let svg = find_ascii_case_insensitive(suffix, b"<svg")?;
    std::str::from_utf8(suffix.get(svg..)?)
        .ok()
        .map(str::to_owned)
}

fn skip_complete_png_chunks(bytes: &[u8]) -> &[u8] {
    let mut i = PNG_SIGNATURE.len();
    while let Some(header) = bytes.get(i..i + 8) {
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let is_iend = header[4..8] == *b"IEND";
        let Some(chunk_end) = (i + 8).checked_add(len).and_then(|n| n.checked_add(4)) else {
            break;
        };
        if chunk_end > bytes.len() {
            break;
        }
        i = chunk_end;
        if is_iend {
            break;
        }
    }
    bytes.get(i..).unwrap_or(&[])
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

pub(super) fn read_resolved_image(
    relative: &ValidatedToolPath,
    file: &mut fs::File,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let bytes =
        read_bounded(file, MAX_IMAGE_SOURCE_BYTES, is_cancelled).map_err(|error| match error {
            BlockingToolError::Domain(domain) if domain.code == ERROR_FILE_TOO_LARGE => {
                BlockingToolError::Domain(DomainError::new(
                    ERROR_IMAGE_TOO_LARGE,
                    "image file exceeds the 20 MiB source limit",
                ))
            }
            other => other,
        })?;
    let source_bytes = bytes.len();

    let normalized = normalize_encoded_image(
        bytes,
        MAX_EMBEDDED_IMAGE_PNG_BYTES.min(MAX_USER_IMAGE_PNG_BYTES),
        MAX_EMBEDDED_IMAGE_DIMENSION.min(MAX_USER_IMAGE_DIMENSION),
        MAX_EMBEDDED_IMAGE_PIXELS.min(MAX_USER_IMAGE_PIXELS),
        is_cancelled,
    )?;

    let png_byte_len = normalized.png_bytes.len();
    let image_input = UserImageInput::png(
        &relative.display,
        normalized.png_bytes,
        normalized.width,
        normalized.height,
    )
    .map_err(|error| DomainError::new(ERROR_INVALID_IMAGE, error.to_string()))?;

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let resized = normalized.width != normalized.source_width
        || normalized.height != normalized.source_height;
    let payload = WorkspaceReadImageSuccess {
        ok: true,
        tool: READ_FILE_TOOL.to_owned(),
        kind: "image".to_owned(),
        path: relative.display.clone(),
        source_format: normalized.source_format.to_owned(),
        source_width: normalized.source_width,
        source_height: normalized.source_height,
        width: normalized.width,
        height: normalized.height,
        resized,
        source_bytes,
        png_bytes: png_byte_len,
    };

    Ok(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("read_file image envelope serializes"),
    )
    .with_model_text(payload.model_text())
    .with_image(image_input))
}

pub(super) struct NormalizedImage {
    pub(super) source_format: &'static str,
    pub(super) source_width: u32,
    pub(super) source_height: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) png_bytes: Vec<u8>,
}

/// Validates, decodes, and normalizes encoded PNG/JPEG/GIF/WebP bytes into a
/// structurally complete PNG within the supplied dimension, pixel, and byte budgets.
pub(super) fn normalize_encoded_image(
    bytes: Vec<u8>,
    max_bytes: usize,
    max_side_px: u32,
    max_pixels: u64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<NormalizedImage, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }
    if bytes.len() > MAX_IMAGE_SOURCE_BYTES {
        return Err(DomainError::new(
            ERROR_IMAGE_TOO_LARGE,
            "image exceeds the 20 MiB source limit",
        )
        .into());
    }

    let format = image::guess_format(&bytes)
        .ok()
        .and_then(|fmt| SupportedRasterFormat::try_from(fmt).ok())
        .ok_or_else(|| {
            DomainError::new(
                ERROR_INVALID_IMAGE,
                "image header could not be recognized as PNG, JPEG, GIF, or WebP",
            )
        })?;
    let source_format = format.as_str();

    let (header_width, header_height) =
        ImageReader::with_format(Cursor::new(&bytes), format.image_format())
            .into_dimensions()
            .map_err(|error| {
                DomainError::new(
                    ERROR_INVALID_IMAGE,
                    format!("could not read {source_format} image dimensions: {error}"),
                )
            })?;
    validate_decode_dimensions(header_width, header_height)?;

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let decoded = decode_image(&bytes, format)?;
    let (source_width, source_height) = decoded.dimensions();
    validate_decode_dimensions(source_width, source_height)?;

    let (png_bytes, width, height) = encode_under_budget(
        &decoded,
        Some((bytes, format)),
        max_bytes,
        max_side_px,
        max_pixels,
        is_cancelled,
    )?;

    Ok(NormalizedImage {
        source_format,
        source_width,
        source_height,
        width,
        height,
        png_bytes,
    })
}

/// Encodes an in-memory `DynamicImage` into a structurally valid PNG within
/// `max_bytes`, `max_side_px`, and `max_pixels`, downscaling proportionally if needed.
pub(super) fn encode_dynamic_image_under_budget(
    decoded: &DynamicImage,
    max_bytes: usize,
    max_side_px: u32,
    max_pixels: u64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<u8>, u32, u32), BlockingToolError> {
    encode_under_budget(
        decoded,
        None,
        max_bytes,
        max_side_px,
        max_pixels,
        is_cancelled,
    )
}

fn validate_decode_dimensions(width: u32, height: u32) -> Result<(), DomainError> {
    if width == 0 || height == 0 {
        return Err(DomainError::new(
            ERROR_INVALID_IMAGE,
            "image dimensions must be greater than zero",
        ));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| DomainError::new(ERROR_IMAGE_TOO_LARGE, "image pixel count overflowed"))?;
    if pixels > MAX_IMAGE_DECODE_PIXELS {
        return Err(DomainError::new(
            ERROR_IMAGE_TOO_LARGE,
            format!(
                "image pixel count ({width}x{height}) exceeds the {MAX_IMAGE_DECODE_PIXELS} pixel decode limit"
            ),
        ));
    }
    Ok(())
}

fn decode_image(bytes: &[u8], format: SupportedRasterFormat) -> Result<DynamicImage, DomainError> {
    if format == SupportedRasterFormat::Gif {
        let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|error| {
            DomainError::new(
                ERROR_INVALID_IMAGE,
                format!("could not decode gif image: {error}"),
            )
        })?;
        let frame = decoder
            .into_frames()
            .next()
            .transpose()
            .map_err(|error| {
                DomainError::new(
                    ERROR_INVALID_IMAGE,
                    format!("could not decode gif frame: {error}"),
                )
            })?
            .ok_or_else(|| DomainError::new(ERROR_INVALID_IMAGE, "gif image contains no frames"))?;
        Ok(DynamicImage::ImageRgba8(frame.into_buffer()))
    } else {
        let label = format.as_str();
        image::load_from_memory_with_format(bytes, format.image_format()).map_err(|error| {
            DomainError::new(
                ERROR_INVALID_IMAGE,
                format!("could not decode {label} image: {error}"),
            )
        })
    }
}

fn encode_under_budget(
    decoded: &DynamicImage,
    original: Option<(Vec<u8>, SupportedRasterFormat)>,
    max_bytes: usize,
    max_side_px: u32,
    max_pixels: u64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<u8>, u32, u32), BlockingToolError> {
    let (orig_w, orig_h) = decoded.dimensions();
    let exceeds_caps = orig_w > max_side_px
        || orig_h > max_side_px
        || u64::from(orig_w) * u64::from(orig_h) > max_pixels;

    if let Some((original_bytes, SupportedRasterFormat::Png)) = original
        && !exceeds_caps
        && original_bytes.len() <= max_bytes
        && png_has_complete_chunks(&original_bytes)
    {
        return Ok((original_bytes, orig_w, orig_h));
    }

    let original_max_side = orig_w.max(orig_h);
    let mut max_side = max_side_px.min(original_max_side);
    if u64::from(orig_w) * u64::from(orig_h) > max_pixels {
        max_side = max_side.min(area_capped_side(
            original_max_side,
            orig_w.min(orig_h),
            max_pixels,
        ));
    }

    loop {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }

        let scaled: Cow<'_, DynamicImage> = if max_side < original_max_side {
            Cow::Owned(decoded.resize(max_side, max_side, FilterType::Lanczos3))
        } else {
            Cow::Borrowed(decoded)
        };
        let (w, h) = scaled.dimensions();

        let mut buf = Vec::new();
        scaled
            .write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .map_err(|error| {
                DomainError::new(
                    ERROR_INVALID_IMAGE,
                    format!("could not encode normalized PNG image: {error}"),
                )
            })?;

        if buf.len() <= max_bytes {
            return Ok((buf, w, h));
        }

        if max_side <= MIN_EMBEDDED_IMAGE_DIMENSION {
            return Err(DomainError::new(
                ERROR_IMAGE_TOO_LARGE,
                format!(
                    "normalized PNG still exceeds the {max_bytes}-byte limit after downscaling to {w}x{h}"
                ),
            )
            .into());
        }
        max_side = (max_side * 3 / 4).max(MIN_EMBEDDED_IMAGE_DIMENSION);
    }
}

/// Largest target long side whose aspect-preserving resize area stays within `max_pixels`.
fn area_capped_side(long: u32, short: u32, max_pixels: u64) -> u32 {
    let scale = (max_pixels as f64 / (u64::from(long) * u64::from(short)) as f64).sqrt();
    let mut side = ((f64::from(long) * scale).floor() as u32).clamp(1, long);
    while side > 1 && predicted_resize_area(long, short, side) > max_pixels {
        side -= 1;
    }
    side
}

fn predicted_resize_area(long: u32, short: u32, side: u32) -> u64 {
    let ratio = f64::from(side) / f64::from(long);
    let scaled_long = (f64::from(long) * ratio).round().max(1.0) as u64;
    let scaled_short = (f64::from(short) * ratio).round().max(1.0) as u64;
    scaled_long * scaled_short
}

/// Walks PNG chunks to verify `IEND` is present and every chunk's bounds and CRC32 are valid.
fn png_has_complete_chunks(bytes: &[u8]) -> bool {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return false;
    }
    let mut i = PNG_SIGNATURE.len();
    while let Some(header) = bytes.get(i..i + 8) {
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let is_iend = header[4..8] == *b"IEND";
        let Some(data_end) = (i + 8).checked_add(len) else {
            return false;
        };
        let Some(chunk_end) = data_end.checked_add(4) else {
            return false;
        };
        let Some(crc_bytes) = bytes.get(data_end..chunk_end) else {
            return false;
        };
        let expected_crc =
            u32::from_be_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]);
        let Some(crc_span) = bytes.get(i + 4..data_end) else {
            return false;
        };
        if png_crc32(crc_span) != expected_crc {
            return false;
        }
        if is_iend {
            return true;
        }
        i = chunk_end;
    }
    false
}

pub(crate) fn png_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
