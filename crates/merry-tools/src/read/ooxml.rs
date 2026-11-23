//! Shared safe OOXML (ZIP + XML) archive and entity-decoding helpers used by
//! `.docx`, `.pptx`, and `.xlsx` readers.

use std::{
    fs,
    io::{Cursor, Read},
};

use quick_xml::events::BytesRef;
use zip::ZipArchive;

use crate::{
    errors::{BlockingToolError, DomainError, ERROR_FILE_TOO_LARGE, ERROR_INVALID_DOCUMENT},
    file::read_bounded,
};

/// Maximum Office document size accepted by `read_file` (50 MiB).
const MAX_OFFICE_BYTES: usize = 50 * 1024 * 1024;
/// Maximum decompressed size of any single XML entry inside an OOXML archive (16 MiB).
pub(super) const MAX_XML_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum extracted text size produced from a single Office document (16 MiB).
pub(super) const MAX_EXTRACTED_TEXT_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn read_bounded_office_file(
    file: &mut fs::File,
    label: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }
    read_bounded(file, MAX_OFFICE_BYTES, is_cancelled).map_err(|error| match error {
        BlockingToolError::Domain(domain) if domain.code == ERROR_FILE_TOO_LARGE => {
            BlockingToolError::Domain(DomainError::new(
                ERROR_FILE_TOO_LARGE,
                format!("{label} file exceeds the 50 MiB limit"),
            ))
        }
        other => other,
    })
}

pub(super) fn read_zip_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
    label: &str,
) -> Result<Option<String>, DomainError> {
    let file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => {
            return Err(DomainError::new(
                ERROR_INVALID_DOCUMENT,
                format!("could not open {label} entry {name}: {error}"),
            ));
        }
    };
    let mut content = String::new();
    file.take(MAX_XML_ENTRY_BYTES.saturating_add(1))
        .read_to_string(&mut content)
        .map_err(|error| {
            DomainError::new(
                ERROR_INVALID_DOCUMENT,
                format!("could not read {label} entry {name}: {error}"),
            )
        })?;
    if content.len() as u64 > MAX_XML_ENTRY_BYTES {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            format!("{label} entry {name} exceeds the decompressed size limit"),
        ));
    }
    Ok(Some(content))
}

pub(super) fn push_xml_general_ref(entity: &BytesRef<'_>, out: &mut String) -> Result<(), String> {
    if let Some(ch) = entity
        .resolve_char_ref()
        .map_err(|error| error.to_string())?
    {
        out.push(ch);
    } else {
        let name: &str = entity;
        match quick_xml::escape::resolve_predefined_entity(name) {
            Some(resolved) => out.push_str(resolved),
            None => {
                out.push('&');
                out.push_str(name);
                out.push(';');
            }
        }
    }
    Ok(())
}

fn escape_markdown_table_cell(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for ch in raw.trim().chars() {
        match ch {
            '|' => escaped.push_str("\\|"),
            '\r' => {}
            '\n' => escaped.push(' '),
            other => escaped.push(other),
        }
    }
    escaped
}

pub(super) fn append_markdown_table(out: &mut String, rows: &[Vec<String>]) {
    let max_cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if max_cols == 0 {
        return;
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    for (row_idx, row) in rows.iter().enumerate() {
        out.push('|');
        for col_idx in 0..max_cols {
            out.push(' ');
            if let Some(cell) = row.get(col_idx) {
                out.push_str(&escape_markdown_table_cell(cell));
            }
            out.push_str(" |");
        }
        out.push('\n');
        if row_idx == 0 {
            out.push('|');
            for _ in 0..max_cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    out.push('\n');
}
