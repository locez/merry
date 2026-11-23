use super::*;
use crate::read::{
    MAX_EMBEDDED_IMAGE_DIMENSION, MAX_EMBEDDED_IMAGE_PIXELS, MAX_IMAGE_SOURCE_BYTES, PdfReadFormat,
    parse_page_range, png_crc32,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{Delay, DynamicImage, Frame, ImageFormat, Rgba, RgbaImage};
use std::io::Cursor;

fn encoded_image(format: ImageFormat, width: u32, height: u32, color: Rgba<u8>) -> Vec<u8> {
    let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, color));
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, format)
        .expect("image fixture should encode");
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
        .expect("animated gif fixture should encode");
    bytes
}

fn png_with_forged_ihdr_dimensions(width: u32, height: u32) -> Vec<u8> {
    let mut png = encoded_image(ImageFormat::Png, 1, 1, Rgba([1, 2, 3, 255]));
    png[16..20].copy_from_slice(&width.to_be_bytes());
    png[20..24].copy_from_slice(&height.to_be_bytes());
    let crc = png_crc32(&png[12..29]);
    png[29..33].copy_from_slice(&crc.to_be_bytes());
    png
}

fn make_test_pdf(page_texts: &[&str]) -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();

    offsets.push(pdf.len());
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let page_count = page_texts.len();
    let kids: Vec<String> = (0..page_count)
        .map(|i| format!("{} 0 R", 3 + i * 3))
        .collect();
    offsets.push(pdf.len());
    pdf.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n",
            kids.join(" "),
            page_count
        )
        .as_bytes(),
    );

    for (i, text) in page_texts.iter().enumerate() {
        let page_obj = 3 + i * 3;
        let content_obj = 4 + i * 3;
        let font_obj = 5 + i * 3;
        let stream_content = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");

        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{page_obj} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {content_obj} 0 R /Resources << /Font << /F1 {font_obj} 0 R >> >> >>\nendobj\n"
            )
            .as_bytes(),
        );

        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{content_obj} 0 obj\n<< /Length {} >>\nstream\n{stream_content}\nendstream\nendobj\n",
                stream_content.len()
            )
            .as_bytes(),
        );

        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{font_obj} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n"
            )
            .as_bytes(),
        );
    }

    let xref_offset = pdf.len();
    let total_objects = 2 + page_count * 3 + 1;
    pdf.extend_from_slice(format!("xref\n0 {total_objects}\n").as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {total_objects} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn read_file_returns_requested_line_window_without_host_root() {
    let temp = TempWorkspace::new("read-window");
    temp.write_text("dir/note.txt", "one\ntwo\nthree\nfour\nfive\n");
    let tools = tools_for(temp.path());
    let outcome = read_text_range_outcome(&tools, "dir/note.txt", 3, 2);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        json_content(&outcome),
        json!({
            "ok": true,
            "tool": READ_FILE_TOOL,
            "path": "dir/note.txt",
            "start_line": 3,
            "end_line": 4,
            "lines": 2,
            "bytes": 11,
            "truncated": true,
            "content": "three\nfour\n"
        })
    );
    assert!(
        !outcome
            .content()
            .as_str()
            .contains(temp.path().to_str().expect("temp path utf8"))
    );
}

#[test]
fn read_file_defaults_to_complete_file_when_it_fits_line_budget() {
    let temp = TempWorkspace::new("read-complete-file");
    let tools = tools_for(temp.path());

    for line_count in [559, 2_000] {
        let content = (1..=line_count)
            .map(|line| format!("line-{line}\n"))
            .collect::<String>();
        temp.write_text("note.txt", &content);

        let outcome = read_outcome(&tools, "note.txt");
        let payload = json_content(&outcome);

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert_eq!(payload["start_line"], 1);
        assert_eq!(payload["end_line"], line_count);
        assert_eq!(payload["lines"], line_count);
        assert_eq!(payload["bytes"], content.len());
        assert_eq!(payload["content"], content);
        assert_eq!(payload["truncated"], false);
    }
}

#[test]
fn read_file_defaults_to_a_bounded_first_window() {
    let temp = TempWorkspace::new("read-default-window");
    let content = (1..=2_005)
        .map(|line| format!("line-{line}\n"))
        .collect::<String>();
    temp.write_text("large.txt", &content);
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "large.txt");
    let payload = json_content(&outcome);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(payload["start_line"], 1);
    assert_eq!(payload["lines"], 2_000);
    assert_eq!(payload["end_line"], 2_000);
    assert_eq!(payload["truncated"], true);
    assert!(
        payload["content"]
            .as_str()
            .expect("content text")
            .contains("line-2000\n")
    );
    assert!(
        !payload["content"]
            .as_str()
            .expect("content text")
            .contains("line-2001\n")
    );
}

#[test]
fn read_file_reads_a_range_from_a_file_larger_than_the_read_limit() {
    let temp = TempWorkspace::new("read-large-file-range");
    temp.write_text("large.txt", &"0123456789\n".repeat(20));
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(temp.path().to_path_buf()).with_limits(WorkspaceToolLimits {
            max_read_bytes: 32,
            max_read_lines: 2,
            ..WorkspaceToolLimits::default()
        }),
    )
    .expect("workspace tools should construct");

    let outcome = read_text_range_outcome(&tools, "large.txt", 1, 2);
    let payload = json_content(&outcome);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(payload["lines"], 2);
    assert_eq!(payload["content"], "0123456789\n0123456789\n");
}

#[test]
fn read_file_rejects_ranges_outside_configured_limits() {
    let temp = TempWorkspace::new("read-range-validation");
    temp.write_text("note.txt", "one\ntwo\n");
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(temp.path().to_path_buf()).with_limits(WorkspaceToolLimits {
            max_read_lines: 2,
            ..WorkspaceToolLimits::default()
        }),
    )
    .expect("workspace tools should construct");
    let executor = ReadFileExecutor {
        state: Arc::clone(&tools.state),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime should build");

    for arguments in [
        json!({ "path": "note.txt", "start_line": 0, "max_lines": 1 }),
        json!({ "path": "note.txt", "start_line": 1, "max_lines": 3 }),
    ] {
        let outcome = runtime
            .block_on(executor.execute(pending_call(arguments), ToolExecutionContext::default()))
            .expect("invalid range should resolve as a failed outcome");
        assert_failed_json(
            &outcome,
            ERROR_INVALID_ARGUMENTS,
            Some("note.txt"),
            temp.path(),
        );
    }
}

#[test]
fn read_file_reports_missing_and_non_utf8_failures() {
    let temp = TempWorkspace::new("read-failures");
    temp.write_bytes("binary.bin", &[0xff, 0xfe, 0xfd]);
    temp.write_text("visible.txt", "ok\n");
    let tools = tools_for(temp.path());

    assert_failed_json(
        &read_outcome(&tools, "missing.txt"),
        ERROR_FILE_NOT_FOUND,
        Some("missing.txt"),
        temp.path(),
    );
    assert_failed_json(
        &read_outcome(&tools, "binary.bin"),
        ERROR_NOT_UTF8,
        Some("binary.bin"),
        temp.path(),
    );
    assert_failed_json(
        &read_outcome(&tools, ".secret"),
        ERROR_FILE_NOT_FOUND,
        Some(".secret"),
        temp.path(),
    );
}

#[cfg(unix)]
#[test]
fn read_file_rejects_symlink_without_following_it() {
    let temp = TempWorkspace::new("read-symlink");
    temp.write_text("target.txt", "secret\n");
    symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "link.txt");
    assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        outcome.diagnostic().expect("diagnostic").code(),
        ERROR_PATH_DENIED
    );
}

#[test]
fn read_file_executor_returns_cancelled_when_token_is_cancelled() {
    let temp = TempWorkspace::new("read-cancelled");
    temp.write_text("note.txt", "ok\n");
    let tools = tools_for(temp.path());
    let executor = ReadFileExecutor {
        state: Arc::clone(&tools.state),
    };
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime should build");

    let err = runtime
        .block_on(executor.execute(
            pending_call(json!({ "path": "note.txt" })),
            ToolExecutionContext::new(token),
        ))
        .expect_err("cancelled execution should return cancellation error");

    assert!(matches!(err, ToolExecutionError::Cancelled));
}

#[test]
fn read_file_enforces_read_scope_and_forbidden_paths_while_allowing_resource_roots() {
    let temp = TempWorkspace::new("read-scope");
    let skills = TempWorkspace::new("read-scope-skills");
    temp.write_text("allowed/ok.txt", "allowed content\n");
    temp.write_text("allowed/secret.txt", "secret content\n");
    temp.write_text("other/denied.txt", "denied content\n");
    skills.write_text("SKILL.md", "skill doc\n");

    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(temp.path())
            .with_readonly_resource_roots(vec![skills.path().to_path_buf()])
            .with_read_scope(Some(vec![std::path::PathBuf::from("allowed")]))
            .with_forbidden_paths(vec![std::path::PathBuf::from("allowed/secret.txt")]),
    )
    .expect("scoped tools should build");

    assert_eq!(
        read_outcome(&tools, "allowed/ok.txt").status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(
        read_outcome(&tools, "allowed/secret.txt")
            .diagnostic()
            .expect("diagnostic")
            .code(),
        ERROR_PATH_DENIED
    );
    assert_eq!(
        read_outcome(&tools, "other/denied.txt")
            .diagnostic()
            .expect("diagnostic")
            .code(),
        ERROR_PATH_DENIED
    );
    assert_eq!(
        read_outcome(&tools, "SKILL.md").status(),
        ToolCallResultStatus::Succeeded
    );
}

#[test]
fn read_file_reads_png_jpeg_gif_webp_and_animated_gif_first_frame() {
    let temp = TempWorkspace::new("read-file-images");
    let tools = tools_for(temp.path());

    for (name, format, expected_format) in [
        ("assets/sample.png", ImageFormat::Png, "png"),
        ("assets/sample.jpg", ImageFormat::Jpeg, "jpeg"),
        ("assets/sample.gif", ImageFormat::Gif, "gif"),
        ("assets/sample.webp", ImageFormat::WebP, "webp"),
    ] {
        let source = encoded_image(format, 3, 2, Rgba([10, 20, 30, 255]));
        temp.write_bytes(name, &source);

        let outcome = read_outcome(&tools, name);
        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert_eq!(outcome.images().len(), 1);
        let attached = &outcome.images()[0];
        assert_eq!(attached.label(), name);
        assert_eq!((attached.width(), attached.height()), (3, 2));
        assert!(attached.png_bytes().starts_with(b"\x89PNG\r\n\x1a\n"));

        let payload = json_content(&outcome);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["tool"], READ_FILE_TOOL);
        assert_eq!(payload["kind"], "image");
        assert_eq!(payload["source_format"], expected_format);
        assert_eq!(payload["resized"], false);
    }

    temp.write_bytes("assets/anim.gif", &animated_gif_fixture());
    let anim = read_outcome(&tools, "assets/anim.gif");
    assert_eq!(anim.status(), ToolCallResultStatus::Succeeded);
    let decoded = image::load_from_memory(anim.images()[0].png_bytes())
        .expect("normalized PNG should decode")
        .to_rgba8();
    assert_eq!(decoded.get_pixel(0, 0), &Rgba([255, 0, 0, 255]));
}

#[test]
fn read_file_auto_downscales_oversized_screenshots_preserving_aspect_ratio() {
    let temp = TempWorkspace::new("read-file-downscale");
    let large = encoded_image(ImageFormat::Png, 3_000, 1_500, Rgba([12, 34, 56, 255]));
    temp.write_bytes("wide.png", &large);
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "wide.png");
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(outcome.images().len(), 1);
    let attached = &outcome.images()[0];
    assert!(attached.width() <= MAX_EMBEDDED_IMAGE_DIMENSION);
    assert!(attached.height() <= MAX_EMBEDDED_IMAGE_DIMENSION);
    assert!(
        u64::from(attached.width()) * u64::from(attached.height()) <= MAX_EMBEDDED_IMAGE_PIXELS
    );
    let ratio_in = 3_000.0 / 1_500.0;
    let ratio_out = f64::from(attached.width()) / f64::from(attached.height());
    assert!((ratio_in - ratio_out).abs() < 0.05);

    let payload = json_content(&outcome);
    assert_eq!(payload["resized"], true);
    assert_eq!(payload["source_width"], 3_000);
    assert_eq!(payload["source_height"], 1_500);
    assert_eq!(payload["width"], attached.width());
    assert_eq!(payload["height"], attached.height());
    assert!(
        outcome
            .model_content()
            .expect("model text")
            .as_str()
            .contains("resized to")
    );
}

#[test]
fn read_file_keeps_svg_and_png_prefixed_svg_on_text_path_and_rejects_corrupt_or_bomb_images() {
    let temp = TempWorkspace::new("read-file-svg-and-guards");
    temp.write_text("icon.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"/>\n");
    let mut adobe_svg = encoded_image(ImageFormat::Png, 2, 2, Rgba([0, 0, 0, 255]));
    adobe_svg.extend_from_slice(b"<?xml version=\"1.0\"?><svg><rect/></svg>\n");
    temp.write_bytes("adobe.svg", &adobe_svg);
    temp.write_bytes("corrupt.png", b"\x89PNG\r\n\x1a\nnot-valid-chunks");
    temp.write_bytes("bomb.png", &png_with_forged_ihdr_dimensions(10_000, 10_000));
    let oversized_path = temp.path().join("huge.png");
    let mut file = File::create(&oversized_path).expect("sparse file");
    file.write_all(b"\x89PNG\r\n\x1a\n")
        .expect("png signature should write");
    file.set_len(u64::try_from(MAX_IMAGE_SOURCE_BYTES + 1).expect("limit fits u64"))
        .expect("sparse file resize");
    let tools = tools_for(temp.path());

    let svg = read_outcome(&tools, "icon.svg");
    assert_eq!(svg.status(), ToolCallResultStatus::Succeeded);
    assert!(svg.images().is_empty());
    assert!(
        json_content(&svg)["content"]
            .as_str()
            .expect("svg text content")
            .contains("<svg")
    );

    let adobe = read_outcome(&tools, "adobe.svg");
    assert_eq!(adobe.status(), ToolCallResultStatus::Succeeded);
    assert!(adobe.images().is_empty());
    assert!(
        json_content(&adobe)["content"]
            .as_str()
            .expect("adobe svg text content")
            .contains("<svg><rect/></svg>")
    );

    assert_failed_json(
        &read_outcome(&tools, "corrupt.png"),
        ERROR_INVALID_IMAGE,
        Some("corrupt.png"),
        temp.path(),
    );
    assert_failed_json(
        &read_outcome(&tools, "bomb.png"),
        ERROR_IMAGE_TOO_LARGE,
        Some("bomb.png"),
        temp.path(),
    );
    assert_failed_json(
        &read_outcome(&tools, "huge.png"),
        ERROR_IMAGE_TOO_LARGE,
        Some("huge.png"),
        temp.path(),
    );
}

#[test]
fn read_file_reads_pdf_as_rendered_page_images_and_text_with_page_ranges() {
    let temp = TempWorkspace::new("read-file-pdf");
    temp.write_bytes(
        "doc.pdf",
        &make_test_pdf(&["Page One Alpha", "Page Two Beta", "Page Three Gamma"]),
    );
    let many_pages: Vec<&str> = (0..12).map(|_| "Page").collect();
    temp.write_bytes("long.pdf", &make_test_pdf(&many_pages));
    temp.write_bytes("broken.pdf", b"%PDF-1.4\nnot valid pdf");
    let tools = tools_for(temp.path());

    // Default format is rendered page images.
    let rendered = read_file_blocking(
        &tools.state,
        ReadFileInput {
            path: "doc.pdf".to_owned(),
            start_line: None,
            max_lines: None,
            pages: Some("1,3".to_owned()),
            format: None,
        },
    );
    assert_eq!(rendered.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(rendered.images().len(), 2);
    assert_eq!(rendered.images()[0].label(), "doc.pdf#page=1");
    assert_eq!(rendered.images()[1].label(), "doc.pdf#page=3");
    for img in rendered.images() {
        assert!(img.png_bytes().starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(img.width() > 0 && img.height() > 0);
    }
    let payload = json_content(&rendered);
    assert_eq!(payload["kind"], "pdf_pages");
    assert_eq!(payload["total_pages"], 3);
    assert_eq!(payload["rendered_pages"], json!([1, 3]));

    // Explicit format="text" extracts text with page headers.
    let text_mode = read_file_blocking(
        &tools.state,
        ReadFileInput {
            path: "doc.pdf".to_owned(),
            start_line: None,
            max_lines: None,
            pages: Some("2".to_owned()),
            format: Some(PdfReadFormat::Text),
        },
    );
    assert_eq!(text_mode.status(), ToolCallResultStatus::Succeeded);
    assert!(text_mode.images().is_empty());
    let text_content = json_content(&text_mode)["content"]
        .as_str()
        .expect("pdf text content")
        .to_owned();
    assert!(text_content.contains("--- Page 2 ---"), "{text_content}");
    assert!(text_content.contains("Page Two Beta"), "{text_content}");
    assert!(!text_content.contains("Page One Alpha"), "{text_content}");

    // PDFs over 10 pages require explicit `pages`.
    assert_failed_json(
        &read_outcome(&tools, "long.pdf"),
        ERROR_INVALID_ARGUMENTS,
        Some("long.pdf"),
        temp.path(),
    );
    assert_failed_json(
        &read_outcome(&tools, "broken.pdf"),
        ERROR_INVALID_DOCUMENT,
        Some("broken.pdf"),
        temp.path(),
    );
    assert_eq!(parse_page_range("1-2,4", 5).expect("range"), vec![0, 1, 3]);
    assert!(parse_page_range("5-2", 5).is_err());
    assert!(parse_page_range("1-25", 30).is_err());
}

#[test]
fn read_file_formats_jupyter_notebook_cells_outputs_and_embedded_images() {
    let temp = TempWorkspace::new("read-file-ipynb");
    let plot_png = encoded_image(ImageFormat::Png, 4, 3, Rgba([100, 150, 200, 255]));
    let notebook = json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "cells": [
            {
                "cell_type": "markdown",
                "source": ["# Analysis\n", "Summary text"]
            },
            {
                "cell_type": "code",
                "execution_count": 2,
                "source": ["print('hello')\n", "plot()"],
                "outputs": [
                    {
                        "output_type": "stream",
                        "name": "stdout",
                        "text": ["hello\n"]
                    },
                    {
                        "output_type": "display_data",
                        "data": {
                            "text/plain": ["<Figure size 4x3>"],
                            "image/png": STANDARD.encode(&plot_png)
                        }
                    }
                ]
            }
        ]
    });
    temp.write_text("analysis.ipynb", &notebook.to_string());
    temp.write_text("broken.ipynb", "{not json");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "analysis.ipynb");
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let content = json_content(&outcome)["content"]
        .as_str()
        .expect("notebook content")
        .to_owned();
    assert!(content.contains("--- Cell 1 [markdown] ---"), "{content}");
    assert!(content.contains("# Analysis"), "{content}");
    assert!(
        content.contains("--- Cell 2 [code, execution_count=2] ---"),
        "{content}"
    );
    assert!(content.contains("[output:stdout]\nhello"), "{content}");
    assert!(
        content.contains("analysis.ipynb#cell=2-img=1 (4x3)"),
        "{content}"
    );
    assert_eq!(outcome.images().len(), 1);
    assert_eq!(outcome.images()[0].label(), "analysis.ipynb#cell=2-img=1");
    assert_eq!(
        (outcome.images()[0].width(), outcome.images()[0].height()),
        (4, 3)
    );

    assert_failed_json(
        &read_outcome(&tools, "broken.ipynb"),
        ERROR_INVALID_DOCUMENT,
        Some("broken.ipynb"),
        temp.path(),
    );
}

pub(super) fn read_text_range_outcome(
    tools: &WorkspaceTools,
    path: &str,
    start_line: usize,
    max_lines: usize,
) -> ToolExecutionOutcome {
    read_file_blocking(
        &tools.state,
        ReadFileInput {
            path: path.to_owned(),
            start_line: Some(start_line),
            max_lines: Some(max_lines),
            pages: None,
            format: None,
        },
    )
}
