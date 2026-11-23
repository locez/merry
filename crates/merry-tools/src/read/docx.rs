//! Word OOXML (`.docx`) text, heading, list, table, and footnote extraction using `zip` + `quick-xml`.

use std::{fs, io::Cursor};

use merry_runtime::ToolExecutionOutcome;
use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
};
use zip::ZipArchive;

use crate::{
    config::WorkspaceToolLimits,
    errors::{BlockingToolError, DomainError, ERROR_INVALID_DOCUMENT},
    path::ValidatedToolPath,
    read::{
        ooxml::{
            MAX_EXTRACTED_TEXT_BYTES, append_markdown_table, push_xml_general_ref,
            read_bounded_office_file, read_zip_entry,
        },
        slice_text_outcome,
    },
};

pub(super) fn read_resolved_docx(
    relative: &ValidatedToolPath,
    file: &mut fs::File,
    start_line: usize,
    max_lines: usize,
    limits: &WorkspaceToolLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    let bytes = read_bounded_office_file(file, "DOCX", is_cancelled)?;
    let text = extract_docx_text(&bytes, is_cancelled)?;
    Ok(slice_text_outcome(
        relative, &text, start_line, max_lines, limits,
    ))
}

pub(super) fn extract_docx_text(
    bytes: &[u8],
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|error| {
        DomainError::new(
            ERROR_INVALID_DOCUMENT,
            format!("could not open DOCX archive: {error}"),
        )
    })?;

    let document_xml =
        read_zip_entry(&mut archive, "word/document.xml", "DOCX")?.ok_or_else(|| {
            DomainError::new(
                ERROR_INVALID_DOCUMENT,
                "DOCX archive is missing word/document.xml",
            )
        })?;
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let mut output = extract_wordprocessing_xml(&document_xml, false).map_err(|error| {
        DomainError::new(
            ERROR_INVALID_DOCUMENT,
            format!("could not parse DOCX document.xml: {error}"),
        )
    })?;

    for (entry, heading) in [
        ("word/footnotes.xml", "--- Footnotes ---"),
        ("word/endnotes.xml", "--- Endnotes ---"),
    ] {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        if let Some(xml) = read_zip_entry(&mut archive, entry, "DOCX")? {
            let notes = extract_wordprocessing_xml(&xml, true).unwrap_or_default();
            if !notes.is_empty() && output.len() < MAX_EXTRACTED_TEXT_BYTES {
                if !output.is_empty() {
                    output.push_str("\n\n");
                }
                output.push_str(heading);
                output.push('\n');
                output.push_str(&notes);
            }
        }
    }

    Ok(output.trim().to_owned())
}

#[derive(Default)]
struct ParagraphState {
    heading_level: Option<usize>,
    is_list_item: bool,
    text: String,
}

impl ParagraphState {
    fn reset(&mut self) {
        self.heading_level = None;
        self.is_list_item = false;
        self.text.clear();
    }

    fn render_line(&self) -> Option<String> {
        let trimmed = self.text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let mut line = String::with_capacity(trimmed.len() + 8);
        if let Some(level) = self.heading_level {
            for _ in 0..level.clamp(1, 6) {
                line.push('#');
            }
            line.push(' ');
        } else if self.is_list_item {
            line.push_str("- ");
        }
        line.push_str(trimmed);
        Some(line)
    }
}

fn extract_wordprocessing_xml(xml: &str, notes_mode: bool) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    let mut output = String::new();
    let mut para = ParagraphState::default();
    let mut in_paragraph = false;
    let mut in_text_run = false;
    let mut skip_depth: usize = 0;

    // Table state (handles top-level tables cleanly; nested tables flatten into their parent cell).
    let mut table_depth: usize = 0;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    let mut in_cell = false;

    loop {
        let event = reader.read_event().map_err(|error| error.to_string())?;
        match event {
            Event::Start(ref e) => {
                let name = e.local_name();
                let local = name.as_ref();
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                if should_skip_subtree(local, notes_mode, e) {
                    skip_depth = 1;
                    continue;
                }
                match local {
                    "tbl" => {
                        table_depth += 1;
                        if table_depth == 1 {
                            table_rows.clear();
                        }
                    }
                    "tr" if table_depth == 1 => current_row.clear(),
                    "tc" if table_depth == 1 => {
                        in_cell = true;
                        current_cell.clear();
                    }
                    "p" => {
                        in_paragraph = true;
                        para.reset();
                    }
                    "pStyle" if in_paragraph => {
                        para.heading_level = parse_heading_level(e);
                    }
                    "numPr" if in_paragraph => {
                        para.is_list_item = true;
                    }
                    "t" if in_paragraph => in_text_run = true,
                    "tab" if in_paragraph => para.text.push('\t'),
                    "br" | "cr" if in_paragraph => para.text.push('\n'),
                    _ => {}
                }
            }
            Event::Empty(ref e) => {
                if skip_depth > 0 {
                    continue;
                }
                match e.local_name().as_ref() {
                    "pStyle" if in_paragraph => {
                        para.heading_level = parse_heading_level(e);
                    }
                    "numPr" if in_paragraph => {
                        para.is_list_item = true;
                    }
                    "tab" if in_paragraph => para.text.push('\t'),
                    "br" | "cr" if in_paragraph => para.text.push('\n'),
                    _ => {}
                }
            }
            Event::Text(e) if skip_depth == 0 && in_text_run => {
                if para.text.len() + output.len() < MAX_EXTRACTED_TEXT_BYTES {
                    para.text.push_str(&e.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::GeneralRef(e) if skip_depth == 0 && in_text_run => {
                if para.text.len() + output.len() < MAX_EXTRACTED_TEXT_BYTES {
                    push_xml_general_ref(&e, &mut para.text)?;
                }
            }
            Event::End(ref e) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                match e.local_name().as_ref() {
                    "t" => in_text_run = false,
                    "p" => {
                        in_paragraph = false;
                        in_text_run = false;
                        if in_cell {
                            let trimmed = para.text.trim();
                            if !trimmed.is_empty() {
                                if !current_cell.is_empty() {
                                    current_cell.push(' ');
                                }
                                current_cell.push_str(trimmed);
                            }
                        } else if let Some(line) = para.render_line()
                            && output.len() < MAX_EXTRACTED_TEXT_BYTES
                        {
                            if !output.is_empty() {
                                output.push('\n');
                            }
                            output.push_str(&line);
                        }
                        para.reset();
                    }
                    "tc" if table_depth == 1 => {
                        in_cell = false;
                        current_row.push(std::mem::take(&mut current_cell));
                    }
                    "tr" if table_depth == 1 => {
                        if current_row.iter().any(|cell| !cell.trim().is_empty()) {
                            table_rows.push(std::mem::take(&mut current_row));
                        } else {
                            current_row.clear();
                        }
                    }
                    "tbl" if table_depth > 0 => {
                        table_depth -= 1;
                        if table_depth == 0 && !table_rows.is_empty() {
                            if output.len() < MAX_EXTRACTED_TEXT_BYTES {
                                append_markdown_table(&mut output, &table_rows);
                            }
                            table_rows.clear();
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(output.trim().to_owned())
}

fn should_skip_subtree(local: &str, notes_mode: bool, start: &BytesStart<'_>) -> bool {
    if matches!(local, "del" | "delText" | "instrText" | "Fallback") {
        return true;
    }
    if notes_mode && matches!(local, "footnote" | "endnote") {
        for attr in start.attributes().flatten() {
            if attr.key.local_name().as_ref() == "type"
                && let Ok(value) = attr.normalized_value(XmlVersion::Implicit1_0)
                && matches!(
                    value.as_ref(),
                    "separator" | "continuationSeparator" | "continuationNotice"
                )
            {
                return true;
            }
        }
    }
    false
}

fn parse_heading_level(start: &BytesStart<'_>) -> Option<usize> {
    for attr in start.attributes().flatten() {
        if attr.key.local_name().as_ref() != "val" {
            continue;
        }
        let Ok(value) = attr.normalized_value(XmlVersion::Implicit1_0) else {
            continue;
        };
        let lower = value.to_ascii_lowercase();
        if lower == "title" {
            return Some(1);
        }
        if let Some(rest) = lower.strip_prefix("heading")
            && let Ok(level) = rest.trim().parse::<usize>()
            && (1..=6).contains(&level)
        {
            return Some(level);
        }
    }
    None
}
