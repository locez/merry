use super::input::DraftImage;
use image::{
    AnimationDecoder, DynamicImage, GenericImageView, ImageFormat, ImageReader, RgbaImage,
};
use merry_runtime::{
    MAX_USER_IMAGE_DIMENSION, MAX_USER_IMAGE_PIXELS, MAX_USER_IMAGE_PNG_BYTES, RuntimeError,
};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
    sync::Arc,
};
use thiserror::Error;

pub(crate) const MAX_CLIPBOARD_IMAGE_SOURCE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClipboardImage {
    png_bytes: Arc<[u8]>,
    width: u32,
    height: u32,
}

impl ClipboardImage {
    #[cfg(test)]
    #[must_use]
    pub(crate) fn png_bytes(&self) -> &[u8] {
        &self.png_bytes
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    pub(crate) fn into_draft_image(self) -> Result<DraftImage, RuntimeError> {
        DraftImage::new(self.png_bytes, self.width, self.height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClipboardRgba {
    width: usize,
    height: usize,
    bytes: Vec<u8>,
}

#[derive(Debug, Error)]
pub(crate) enum ClipboardImageError {
    #[error("clipboard unavailable: {0}")]
    ClipboardUnavailable(String),
    #[error("no usable image on clipboard: {0}")]
    NoImage(String),
    #[error("clipboard image source is invalid: {0}")]
    InvalidSource(String),
    #[error("clipboard image format is unsupported")]
    UnsupportedFormat,
    #[error("clipboard image decode failed: {0}")]
    Decode(String),
    #[error("clipboard image encode failed: {0}")]
    Encode(String),
    #[error("clipboard image exceeds limit: {0}")]
    Limit(&'static str),
}

pub(crate) fn read_clipboard_image() -> Result<ClipboardImage, ClipboardImageError> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|error| ClipboardImageError::ClipboardUnavailable(error.to_string()))?;
    let files = clipboard.get().file_list().unwrap_or_default();
    read_clipboard_image_with_fallback(
        files.iter().map(std::path::PathBuf::as_path),
        || {
            let image = clipboard
                .get_image()
                .map_err(|error| ClipboardImageError::NoImage(error.to_string()))?;
            Ok(ClipboardRgba {
                width: image.width,
                height: image.height,
                bytes: image.bytes.into_owned(),
            })
        },
        read_platform_encoded_fallback,
    )
}

fn read_clipboard_image_with_fallback<'a, I, F, G>(
    paths: I,
    rgba_reader: F,
    encoded_fallback: G,
) -> Result<ClipboardImage, ClipboardImageError>
where
    I: IntoIterator<Item = &'a Path>,
    F: FnOnce() -> Result<ClipboardRgba, ClipboardImageError>,
    G: FnOnce() -> Option<Result<ClipboardImage, ClipboardImageError>>,
{
    match normalize_file_candidates_or_else(paths, rgba_reader) {
        Ok(image) => Ok(image),
        Err(error @ (ClipboardImageError::Limit(_) | ClipboardImageError::Encode(_))) => Err(error),
        Err(primary_error) => match encoded_fallback() {
            Some(Ok(image)) => Ok(image),
            Some(Err(
                fallback_error @ (ClipboardImageError::Limit(_)
                | ClipboardImageError::Encode(_)
                | ClipboardImageError::Decode(_)
                | ClipboardImageError::UnsupportedFormat),
            )) => Err(fallback_error),
            Some(Err(_)) | None => Err(primary_error),
        },
    }
}

#[cfg(not(target_os = "macos"))]
fn read_platform_encoded_fallback() -> Option<Result<ClipboardImage, ClipboardImageError>> {
    None
}

/// Fallback macOS pasteboard image reader for non-TIFF pasteboard payloads.
///
/// `arboard` on macOS reads only `NSPasteboardTypeTIFF` and decodes it via
/// `image`'s TIFF decoder, which misses PNG/JPEG/GIF/WebP-only pasteboards and
/// some wide-gamut or compressed macOS TIFF variants. Using `/usr/bin/osascript`
/// with JXA lets AppKit read direct encoded image types or render any `NSImage`
/// pasteboard payload to PNG without temp files or `unsafe` FFI.
#[cfg(target_os = "macos")]
fn read_platform_encoded_fallback() -> Option<Result<ClipboardImage, ClipboardImageError>> {
    Some(read_macos_pasteboard_encoded_image())
}

#[cfg(target_os = "macos")]
const MACOS_PASTEBOARD_IMAGE_SCRIPT: &str = r#"
ObjC.import("AppKit");
ObjC.import("Foundation");
ObjC.import("stdlib");

function run() {
  var pb = $.NSPasteboard.generalPasteboard;
  if (!pb || pb.isNil()) {
    $.exit(2);
  }
  var maxBytes = 20971520;
  var directTypes = [
    $.NSPasteboardTypePNG,
    $("public.jpeg"),
    $("com.compuserve.gif"),
    $("org.webmproject.webp"),
  ];
  var data = null;
  for (var i = 0; i < directTypes.length; i++) {
    var candidate = pb.dataForType(directTypes[i]);
    if (candidate && !candidate.isNil() && candidate.length > 0) {
      data = candidate;
      break;
    }
  }
  if (!data) {
    var image = $.NSImage.alloc.initWithPasteboard(pb);
    if (image && !image.isNil()) {
      var tiff = image.TIFFRepresentation;
      if (tiff && !tiff.isNil() && tiff.length > 0) {
        var rep = $.NSBitmapImageRep.imageRepWithData(tiff);
        if (rep && !rep.isNil()) {
          data = rep.representationUsingTypeProperties(
            $.NSBitmapImageFileTypePNG,
            $({}),
          );
        }
      }
    }
  }
  if (!data || data.isNil() || data.length === 0) {
    $.exit(3);
  }
  if (data.length > maxBytes) {
    $.exit(4);
  }
  $.NSFileHandle.fileHandleWithStandardOutput.writeData(data);
  $.exit(0);
}
"#;

#[cfg(target_os = "macos")]
fn read_macos_pasteboard_encoded_image() -> Result<ClipboardImage, ClipboardImageError> {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    const TIMEOUT: Duration = Duration::from_secs(5);
    const MAX_STDERR_BYTES: u64 = 4096;

    let mut child = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", MACOS_PASTEBOARD_IMAGE_SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ClipboardImageError::ClipboardUnavailable(error.to_string()))?;

    let stdout = child.stdout.take().ok_or_else(|| {
        ClipboardImageError::ClipboardUnavailable("osascript stdout pipe unavailable".to_owned())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ClipboardImageError::ClipboardUnavailable("osascript stderr pipe unavailable".to_owned())
    })?;

    let stdout_handle = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let limit =
            u64::try_from(MAX_CLIPBOARD_IMAGE_SOURCE_BYTES + 1).expect("source limit fits u64");
        stdout.take(limit).read_to_end(&mut bytes)?;
        Ok::<Vec<u8>, std::io::Error>(bytes)
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.take(MAX_STDERR_BYTES).read_to_end(&mut bytes)?;
        Ok::<Vec<u8>, std::io::Error>(bytes)
    });

    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(ClipboardImageError::ClipboardUnavailable(
                        "macOS clipboard image read timed out".to_owned(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(ClipboardImageError::ClipboardUnavailable(error.to_string()));
            }
        }
    };

    let stdout_bytes = stdout_handle
        .join()
        .map_err(|_| {
            ClipboardImageError::ClipboardUnavailable("stdout reader panicked".to_owned())
        })?
        .map_err(|error| ClipboardImageError::InvalidSource(error.to_string()))?;
    let stderr_bytes = stderr_handle
        .join()
        .map_err(|_| {
            ClipboardImageError::ClipboardUnavailable("stderr reader panicked".to_owned())
        })?
        .map_err(|error| ClipboardImageError::InvalidSource(error.to_string()))?;

    let status = status?;
    match status.code() {
        Some(0) => {
            if stdout_bytes.len() > MAX_CLIPBOARD_IMAGE_SOURCE_BYTES {
                return Err(ClipboardImageError::Limit(
                    "encoded source must be at most 20 MiB",
                ));
            }
            if stdout_bytes.is_empty() {
                return Err(ClipboardImageError::NoImage(
                    "clipboard does not contain an image".to_owned(),
                ));
            }
            normalize_encoded_image(&stdout_bytes)
        }
        Some(2) => Err(ClipboardImageError::ClipboardUnavailable(
            "macOS general pasteboard is unavailable".to_owned(),
        )),
        Some(3) => Err(ClipboardImageError::NoImage(
            "clipboard does not contain an image".to_owned(),
        )),
        Some(4) => Err(ClipboardImageError::Limit(
            "encoded source must be at most 20 MiB",
        )),
        _ => {
            let detail = String::from_utf8_lossy(&stderr_bytes).trim().to_owned();
            Err(ClipboardImageError::NoImage(if detail.is_empty() {
                "macOS pasteboard image read failed".to_owned()
            } else {
                detail
            }))
        }
    }
}

fn normalize_file_candidates_or_else<'a, I, F>(
    paths: I,
    fallback: F,
) -> Result<ClipboardImage, ClipboardImageError>
where
    I: IntoIterator<Item = &'a Path>,
    F: FnOnce() -> Result<ClipboardRgba, ClipboardImageError>,
{
    for path in paths {
        if let Ok(image) = normalize_file_candidate(path) {
            return Ok(image);
        }
    }
    let rgba = fallback()?;
    normalize_rgba(rgba.width, rgba.height, &rgba.bytes)
}

fn normalize_file_candidate(path: &Path) -> Result<ClipboardImage, ClipboardImageError> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| ClipboardImageError::InvalidSource(error.to_string()))?;
    if !metadata.is_file() {
        return Err(ClipboardImageError::InvalidSource(
            "clipboard path is not a regular file".to_owned(),
        ));
    }
    if metadata.len()
        > u64::try_from(MAX_CLIPBOARD_IMAGE_SOURCE_BYTES).expect("source limit fits u64")
    {
        return Err(ClipboardImageError::Limit(
            "source file must be at most 20 MiB",
        ));
    }

    let file =
        File::open(path).map_err(|error| ClipboardImageError::InvalidSource(error.to_string()))?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len()).unwrap_or(MAX_CLIPBOARD_IMAGE_SOURCE_BYTES),
    );
    file.take(u64::try_from(MAX_CLIPBOARD_IMAGE_SOURCE_BYTES + 1).expect("bounded read fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|error| ClipboardImageError::InvalidSource(error.to_string()))?;
    if bytes.len() > MAX_CLIPBOARD_IMAGE_SOURCE_BYTES {
        return Err(ClipboardImageError::Limit(
            "source file must be at most 20 MiB",
        ));
    }
    normalize_encoded_image(&bytes)
}

fn normalize_encoded_image(bytes: &[u8]) -> Result<ClipboardImage, ClipboardImageError> {
    if bytes.len() > MAX_CLIPBOARD_IMAGE_SOURCE_BYTES {
        return Err(ClipboardImageError::Limit(
            "encoded source must be at most 20 MiB",
        ));
    }
    let format = image::guess_format(bytes)
        .map_err(|error| ClipboardImageError::Decode(error.to_string()))?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return Err(ClipboardImageError::UnsupportedFormat);
    }
    let dimensions = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|error| ClipboardImageError::Decode(error.to_string()))?;
    validate_dimensions(dimensions.0, dimensions.1)?;

    let image = if format == ImageFormat::Gif {
        let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
            .map_err(|error| ClipboardImageError::Decode(error.to_string()))?;
        let frame = decoder
            .into_frames()
            .next()
            .transpose()
            .map_err(|error| ClipboardImageError::Decode(error.to_string()))?
            .ok_or_else(|| ClipboardImageError::Decode("GIF contains no frames".to_owned()))?;
        DynamicImage::ImageRgba8(frame.into_buffer())
    } else {
        image::load_from_memory_with_format(bytes, format)
            .map_err(|error| ClipboardImageError::Decode(error.to_string()))?
    };
    normalize_dynamic_image(image)
}

fn normalize_rgba(
    width: usize,
    height: usize,
    bytes: &[u8],
) -> Result<ClipboardImage, ClipboardImageError> {
    let width = u32::try_from(width)
        .map_err(|_| ClipboardImageError::Limit("image width exceeds supported integer range"))?;
    let height = u32::try_from(height)
        .map_err(|_| ClipboardImageError::Limit("image height exceeds supported integer range"))?;
    validate_dimensions(width, height)?;
    let expected_bytes = usize::try_from(
        u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(ClipboardImageError::Limit("RGBA byte count overflowed"))?,
    )
    .map_err(|_| ClipboardImageError::Limit("RGBA byte count exceeds platform limits"))?;
    if bytes.len() != expected_bytes {
        return Err(ClipboardImageError::InvalidSource(
            "RGBA buffer length does not match width and height".to_owned(),
        ));
    }
    let rgba = RgbaImage::from_raw(width, height, bytes.to_vec()).ok_or_else(|| {
        ClipboardImageError::InvalidSource("RGBA buffer layout is invalid".to_owned())
    })?;
    normalize_dynamic_image(DynamicImage::ImageRgba8(rgba))
}

fn normalize_dynamic_image(image: DynamicImage) -> Result<ClipboardImage, ClipboardImageError> {
    let (width, height) = image.dimensions();
    validate_dimensions(width, height)?;
    let mut png_bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut png_bytes, ImageFormat::Png)
        .map_err(|error| ClipboardImageError::Encode(error.to_string()))?;
    let png_bytes = png_bytes.into_inner();
    validate_encoded_png_size(png_bytes.len())?;
    Ok(ClipboardImage {
        png_bytes: png_bytes.into(),
        width,
        height,
    })
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), ClipboardImageError> {
    if width == 0 || height == 0 {
        return Err(ClipboardImageError::Limit(
            "image dimensions must be greater than zero",
        ));
    }
    if width > MAX_USER_IMAGE_DIMENSION || height > MAX_USER_IMAGE_DIMENSION {
        return Err(ClipboardImageError::Limit(
            "image dimensions must not exceed 8000 pixels",
        ));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(ClipboardImageError::Limit("image pixel count overflowed"))?;
    if pixels > MAX_USER_IMAGE_PIXELS {
        return Err(ClipboardImageError::Limit(
            "image must not exceed 32 million pixels",
        ));
    }
    Ok(())
}

fn validate_encoded_png_size(size: usize) -> Result<(), ClipboardImageError> {
    if size > MAX_USER_IMAGE_PNG_BYTES {
        return Err(ClipboardImageError::Limit(
            "normalized PNG must be at most 10 MiB",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ClipboardRgba, MAX_CLIPBOARD_IMAGE_SOURCE_BYTES, normalize_encoded_image,
        normalize_file_candidate, normalize_file_candidates_or_else, normalize_rgba,
        validate_dimensions, validate_encoded_png_size,
    };
    use image::{Delay, DynamicImage, Frame, GenericImageView, ImageFormat, Rgba, RgbaImage};
    use std::{
        io::Cursor,
        sync::atomic::{AtomicBool, Ordering},
    };

    fn encoded_fixture(format: ImageFormat, color: Rgba<u8>) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 3, color));
        let mut bytes = Cursor::new(Vec::new());
        image
            .write_to(&mut bytes, format)
            .expect("fixture should encode");
        bytes.into_inner()
    }

    fn animated_gif_fixture() -> Vec<u8> {
        let first = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
        let second = RgbaImage::from_pixel(2, 2, Rgba([0, 0, 255, 255]));
        let mut bytes = Vec::new();
        image::codecs::gif::GifEncoder::new(&mut bytes)
            .encode_frames([
                Frame::from_parts(first, 0, 0, Delay::from_numer_denom_ms(1, 1)),
                Frame::from_parts(second, 0, 0, Delay::from_numer_denom_ms(1, 1)),
            ])
            .expect("animated GIF fixture should encode");
        bytes
    }

    #[test]
    fn normalizes_png_jpeg_gif_and_webp_to_png() {
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::WebP,
        ] {
            let normalized =
                normalize_encoded_image(&encoded_fixture(format, Rgba([10, 20, 30, 255])))
                    .expect("supported fixture should normalize");

            assert_eq!((normalized.width(), normalized.height()), (2, 3));
            assert!(normalized.png_bytes().starts_with(b"\x89PNG\r\n\x1a\n"));
            assert_eq!(
                image::load_from_memory(normalized.png_bytes())
                    .expect("normalized PNG should decode")
                    .dimensions(),
                (2, 3)
            );
        }
    }

    #[test]
    fn animated_gif_normalization_uses_the_first_frame() {
        let normalized =
            normalize_encoded_image(&animated_gif_fixture()).expect("GIF should normalize");
        let decoded = image::load_from_memory(normalized.png_bytes())
            .expect("normalized PNG should decode")
            .to_rgba8();

        assert_eq!(decoded.get_pixel(0, 0), &Rgba([255, 0, 0, 255]));
    }

    #[test]
    fn raw_rgba_normalization_validates_layout_and_preserves_pixels() {
        let rgba = [255, 0, 0, 255, 0, 255, 0, 128];
        let normalized = normalize_rgba(2, 1, &rgba).expect("valid RGBA should normalize");
        let decoded = image::load_from_memory(normalized.png_bytes())
            .expect("normalized PNG should decode")
            .to_rgba8();

        assert_eq!(decoded.as_raw(), &rgba);
        assert!(normalize_rgba(2, 1, &rgba[..7]).is_err());
    }

    #[test]
    fn file_candidates_are_preferred_and_invalid_files_fall_back_to_rgba() {
        let temp = tempfile::tempdir().expect("tempdir");
        let valid_path = temp.path().join("clipboard.webp");
        std::fs::write(
            &valid_path,
            encoded_fixture(ImageFormat::WebP, Rgba([1, 2, 3, 255])),
        )
        .expect("valid fixture should write");
        let fallback_called = AtomicBool::new(false);
        let normalized = normalize_file_candidates_or_else([valid_path.as_path()], || {
            fallback_called.store(true, Ordering::SeqCst);
            Err(super::ClipboardImageError::NoImage(
                "fallback must not run".to_owned(),
            ))
        })
        .expect("valid file candidate should win");
        assert_eq!((normalized.width(), normalized.height()), (2, 3));
        assert!(!fallback_called.load(Ordering::SeqCst));

        let invalid_path = temp.path().join("not-an-image.bin");
        std::fs::write(&invalid_path, b"not an image").expect("invalid fixture should write");
        let normalized = normalize_file_candidates_or_else([invalid_path.as_path()], || {
            Ok(ClipboardRgba {
                width: 1,
                height: 1,
                bytes: vec![9, 8, 7, 255],
            })
        })
        .expect("RGBA should be used after invalid file");
        assert_eq!((normalized.width(), normalized.height()), (1, 1));
    }

    #[test]
    fn source_encoded_and_dimension_limits_reject_before_unbounded_work() {
        let temp = tempfile::tempdir().expect("tempdir");
        let oversized = temp.path().join("oversized.png");
        let file = std::fs::File::create(&oversized).expect("fixture should create");
        file.set_len(u64::try_from(MAX_CLIPBOARD_IMAGE_SOURCE_BYTES + 1).expect("limit fits u64"))
            .expect("sparse fixture should resize");

        assert!(normalize_file_candidate(&oversized).is_err());
        assert!(validate_dimensions(8_001, 1).is_err());
        assert!(validate_dimensions(8_000, 4_001).is_err());
        assert!(validate_encoded_png_size(10 * 1024 * 1024 + 1).is_err());
    }

    #[test]
    fn encoded_fallback_recovers_when_rgba_read_misses_clipboard_image() {
        let fallback_image =
            normalize_encoded_image(&encoded_fixture(ImageFormat::Png, Rgba([4, 5, 6, 255])))
                .expect("png fixture");
        let recovered = super::read_clipboard_image_with_fallback(
            std::iter::empty::<&std::path::Path>(),
            || {
                Err(super::ClipboardImageError::NoImage(
                    "no TIFF payload".to_owned(),
                ))
            },
            || Some(Ok(fallback_image.clone())),
        )
        .expect("encoded fallback should recover PNG-only pasteboard");
        assert_eq!(recovered, fallback_image);

        let fallback_called = AtomicBool::new(false);
        let limit_error = super::read_clipboard_image_with_fallback(
            std::iter::empty::<&std::path::Path>(),
            || Err(super::ClipboardImageError::Limit("too big")),
            || {
                fallback_called.store(true, Ordering::SeqCst);
                None
            },
        )
        .expect_err("limit errors must not invoke fallback");
        assert!(matches!(limit_error, super::ClipboardImageError::Limit(_)));
        assert!(!fallback_called.load(Ordering::SeqCst));
    }
}
