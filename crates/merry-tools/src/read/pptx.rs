//! PowerPoint (`.pptx`) slide and speaker-notes text extraction using `zip` + `quick-xml`.

use std::{fs, io::Cursor};

use merry_runtime::ToolExecutionOutcome;
use quick_xml::{Reader, XmlVersion, events::Event};
use zip::ZipArchive;

use crate::{
    config::WorkspaceToolLimits,
    errors::{BlockingToolError, DomainError, ERROR_INVALID_DOCUMENT},
    path::ValidatedToolPath,
    read::{
        ooxml::{push_xml_general_ref, read_bounded_office_file, read_zip_entry},
        slice_text_outcome,
    },
};

pub(super) fn read_resolved_pptx(
    relative: &ValidatedToolPath,
    file: &mut fs::File,
    start_line: usize,
    max_lines: usize,
    limits: &WorkspaceToolLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    let bytes = read_bounded_office_file(file, "PPTX", is_cancelled)?;
    let text = extract_pptx_text(&bytes, is_cancelled)?;
    Ok(slice_text_outcome(
        relative, &text, start_line, max_lines, limits,
    ))
}

fn extract_pptx_text(
    bytes: &[u8],
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, BlockingToolError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|error| {
        DomainError::new(
            ERROR_INVALID_DOCUMENT,
            format!("could not open PPTX archive: {error}"),
        )
    })?;

    let mut slide_numbers: Vec<u32> = archive
        .file_names()
        .filter_map(|name| {
            name.ok()?
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse()
                .ok()
        })
        .collect();
    slide_numbers.sort_unstable();
    slide_numbers.dedup();

    if slide_numbers.is_empty() {
        return Err(DomainError::new(ERROR_INVALID_DOCUMENT, "no slides found in PPTX").into());
    }

    let mut all_text = String::new();
    for number in slide_numbers {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        let slide_xml = read_zip_entry(
            &mut archive,
            &format!("ppt/slides/slide{number}.xml"),
            "PPTX",
        )?
        .ok_or_else(|| {
            DomainError::new(
                ERROR_INVALID_DOCUMENT,
                format!("could not read PPTX slide {number}"),
            )
        })?;
        let slide_text = extract_drawingml_text(&slide_xml).map_err(|error| {
            DomainError::new(
                ERROR_INVALID_DOCUMENT,
                format!("could not parse PPTX slide {number}: {error}"),
            )
        })?;

        let notes_text = read_zip_entry(
            &mut archive,
            &format!("ppt/notesSlides/notesSlide{number}.xml"),
            "PPTX",
        )
        .ok()
        .flatten()
        .and_then(|xml| extract_drawingml_text(&xml).ok())
        .unwrap_or_default();

        if !all_text.is_empty() {
            all_text.push_str("\n\n");
        }
        all_text.push_str(&format!("--- Slide {number} ---\n"));
        all_text.push_str(&slide_text);
        if !notes_text.is_empty() {
            all_text.push_str("\n\nSpeaker Notes:\n");
            all_text.push_str(&notes_text);
        }
    }

    Ok(all_text)
}

/// Extracts DrawingML text from `<a:t>` runs, one line per `<a:p>` paragraph.
fn extract_drawingml_text(xml: &str) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    let mut text = String::new();
    let mut in_text_run = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) if e.local_name().as_ref() == "t" => in_text_run = true,
            Ok(Event::Text(e)) if in_text_run => {
                text.push_str(&e.xml_content(XmlVersion::Implicit1_0));
            }
            Ok(Event::GeneralRef(e)) if in_text_run => {
                push_xml_general_ref(&e, &mut text)?;
            }
            Ok(Event::End(ref e)) => match e.local_name().as_ref() {
                "t" => in_text_run = false,
                "p" if !text.is_empty() && !text.ends_with('\n') => text.push('\n'),
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(error.to_string()),
            _ => {}
        }
    }

    Ok(text.trim().to_owned())
}
