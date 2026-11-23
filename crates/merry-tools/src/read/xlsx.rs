//! Excel OOXML (`.xlsx`) workbook and worksheet extraction using `zip` + `quick-xml`.

use std::{collections::HashMap, fs, io::Cursor};

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

/// Maximum column span preserved per row to guard against crafted sparse cell refs (e.g. `XFD1`).
const MAX_XLSX_COLUMNS: usize = 1_024;

pub(super) fn read_resolved_xlsx(
    relative: &ValidatedToolPath,
    file: &mut fs::File,
    start_line: usize,
    max_lines: usize,
    limits: &WorkspaceToolLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    let bytes = read_bounded_office_file(file, "XLSX", is_cancelled)?;
    let text = extract_xlsx_text(&bytes, is_cancelled)?;
    Ok(slice_text_outcome(
        relative, &text, start_line, max_lines, limits,
    ))
}

fn extract_xlsx_text(
    bytes: &[u8],
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|error| {
        DomainError::new(
            ERROR_INVALID_DOCUMENT,
            format!("could not open XLSX archive: {error}"),
        )
    })?;

    let (sheets, date_1904) = resolve_workbook_sheets(&mut archive)?;
    if sheets.is_empty() {
        return Err(DomainError::new(ERROR_INVALID_DOCUMENT, "no worksheets found in XLSX").into());
    }

    let shared_strings = match read_zip_entry(&mut archive, "xl/sharedStrings.xml", "XLSX")? {
        Some(xml) => parse_shared_strings(&xml).map_err(|error| {
            DomainError::new(
                ERROR_INVALID_DOCUMENT,
                format!("could not parse XLSX sharedStrings.xml: {error}"),
            )
        })?,
        None => Vec::new(),
    };

    let date_styles = match read_zip_entry(&mut archive, "xl/styles.xml", "XLSX")? {
        Some(xml) => parse_date_styles(&xml).unwrap_or_default(),
        None => Vec::new(),
    };

    let mut output = String::new();
    for (sheet_name, entry_path) in sheets {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        let Some(sheet_xml) = read_zip_entry(&mut archive, &entry_path, "XLSX")? else {
            continue;
        };
        let rows = parse_worksheet_rows(&sheet_xml, &shared_strings, &date_styles, date_1904)
            .map_err(|error| {
                DomainError::new(
                    ERROR_INVALID_DOCUMENT,
                    format!("could not parse XLSX worksheet '{sheet_name}': {error}"),
                )
            })?;

        if !output.is_empty() && !output.ends_with("\n\n") {
            if !output.ends_with('\n') {
                output.push('\n');
            }
            output.push('\n');
        }
        output.push_str(&format!("--- Sheet: {sheet_name} ---\n"));
        if rows.is_empty() {
            output.push_str("(empty sheet)\n");
        } else {
            append_markdown_table(&mut output, &rows);
        }
        if output.len() >= MAX_EXTRACTED_TEXT_BYTES {
            break;
        }
    }

    if output.is_empty() {
        return Err(DomainError::new(
            ERROR_INVALID_DOCUMENT,
            "no readable worksheets found in XLSX",
        )
        .into());
    }

    Ok(output.trim_end().to_owned())
}

fn resolve_workbook_sheets(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
) -> Result<(Vec<(String, String)>, bool), DomainError> {
    let rels = match read_zip_entry(archive, "xl/_rels/workbook.xml.rels", "XLSX")? {
        Some(xml) => parse_workbook_rels(&xml).unwrap_or_default(),
        None => HashMap::new(),
    };

    if let Some(workbook_xml) = read_zip_entry(archive, "xl/workbook.xml", "XLSX")? {
        let (raw_sheets, date_1904) = parse_workbook_xml(&workbook_xml).map_err(|error| {
            DomainError::new(
                ERROR_INVALID_DOCUMENT,
                format!("could not parse XLSX workbook.xml: {error}"),
            )
        })?;
        let mut resolved = Vec::with_capacity(raw_sheets.len());
        for (index, (name, rel_id)) in raw_sheets.into_iter().enumerate() {
            let target = rel_id
                .and_then(|id| rels.get(&id))
                .and_then(|target| normalize_xl_target(target))
                .unwrap_or_else(|| format!("xl/worksheets/sheet{}.xml", index + 1));
            resolved.push((name, target));
        }
        if !resolved.is_empty() {
            return Ok((resolved, date_1904));
        }
    }

    let mut sheet_numbers: Vec<u32> = archive
        .file_names()
        .filter_map(|name| {
            name.ok()?
                .strip_prefix("xl/worksheets/sheet")?
                .strip_suffix(".xml")?
                .parse()
                .ok()
        })
        .collect();
    sheet_numbers.sort_unstable();
    sheet_numbers.dedup();
    let fallback = sheet_numbers
        .into_iter()
        .map(|num| {
            (
                format!("Sheet{num}"),
                format!("xl/worksheets/sheet{num}.xml"),
            )
        })
        .collect();
    Ok((fallback, false))
}

fn normalize_xl_target(target: &str) -> Option<String> {
    let trimmed = target.trim().replace('\\', "/");
    if trimmed.is_empty() || trimmed.contains("..") {
        return None;
    }
    let without_leading = trimmed.trim_start_matches('/');
    if without_leading.starts_with("xl/") {
        Some(without_leading.to_owned())
    } else {
        Some(format!("xl/{without_leading}"))
    }
}

fn parse_workbook_rels(xml: &str) -> Result<HashMap<String, String>, String> {
    let mut reader = Reader::from_str(xml);
    let mut rels = HashMap::new();
    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(ref e) | Event::Empty(ref e)
                if e.local_name().as_ref() == "Relationship" =>
            {
                let mut id = None;
                let mut target = None;
                for attr in e.attributes().flatten() {
                    match attr.key.local_name().as_ref() {
                        "Id" => {
                            id = attr
                                .normalized_value(XmlVersion::Implicit1_0)
                                .ok()
                                .map(|v| v.into_owned())
                        }
                        "Target" => {
                            target = attr
                                .normalized_value(XmlVersion::Implicit1_0)
                                .ok()
                                .map(|v| v.into_owned())
                        }
                        _ => {}
                    }
                }
                if let (Some(id), Some(target)) = (id, target) {
                    rels.insert(id, target);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(rels)
}

type WorkbookSheetRef = (String, Option<String>);

fn parse_workbook_xml(xml: &str) -> Result<(Vec<WorkbookSheetRef>, bool), String> {
    let mut reader = Reader::from_str(xml);
    let mut sheets = Vec::new();
    let mut date_1904 = false;
    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(ref e) | Event::Empty(ref e) => match e.local_name().as_ref() {
                "workbookPr" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == "date1904"
                            && let Ok(val) = attr.normalized_value(XmlVersion::Implicit1_0)
                            && matches!(val.as_ref(), "1" | "true" | "TRUE")
                        {
                            date_1904 = true;
                        }
                    }
                }
                "sheet" => {
                    let mut name = None;
                    let mut rel_id = None;
                    for attr in e.attributes().flatten() {
                        match attr.key.local_name().as_ref() {
                            "name" => {
                                name = attr
                                    .normalized_value(XmlVersion::Implicit1_0)
                                    .ok()
                                    .map(|v| v.into_owned())
                            }
                            "id" => {
                                rel_id = attr
                                    .normalized_value(XmlVersion::Implicit1_0)
                                    .ok()
                                    .map(|v| v.into_owned())
                            }
                            _ => {}
                        }
                    }
                    if let Some(name) = name {
                        sheets.push((name, rel_id));
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((sheets, date_1904))
}

fn parse_shared_strings(xml: &str) -> Result<Vec<String>, String> {
    let mut reader = Reader::from_str(xml);
    let mut strings = Vec::new();
    let mut in_si = false;
    let mut in_t = false;
    let mut phonetic_depth: usize = 0;
    let mut current = String::new();

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(ref e) => match e.local_name().as_ref() {
                "si" => {
                    in_si = true;
                    current.clear();
                }
                "rPh" if in_si => phonetic_depth += 1,
                "t" if in_si && phonetic_depth == 0 => in_t = true,
                _ => {}
            },
            Event::Empty(ref e) if e.local_name().as_ref() == "si" => {
                strings.push(String::new());
            }
            Event::Text(e) if in_t && phonetic_depth == 0 => {
                current.push_str(&e.xml_content(XmlVersion::Implicit1_0));
            }
            Event::GeneralRef(e) if in_t && phonetic_depth == 0 => {
                push_xml_general_ref(&e, &mut current)?;
            }
            Event::End(ref e) => match e.local_name().as_ref() {
                "t" => in_t = false,
                "rPh" if phonetic_depth > 0 => phonetic_depth -= 1,
                "si" => {
                    in_si = false;
                    in_t = false;
                    phonetic_depth = 0;
                    strings.push(std::mem::take(&mut current));
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(strings)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateStyleKind {
    None,
    Date,
    Time,
    DateTime,
}

fn parse_date_styles(xml: &str) -> Result<Vec<DateStyleKind>, String> {
    let mut reader = Reader::from_str(xml);
    let mut custom_formats: HashMap<u32, DateStyleKind> = HashMap::new();
    let mut cell_xfs = Vec::new();
    let mut in_cell_xfs = false;

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(ref e) => match e.local_name().as_ref() {
                "cellXfs" => in_cell_xfs = true,
                "numFmt" => record_custom_num_fmt(e, &mut custom_formats),
                "xf" if in_cell_xfs => cell_xfs.push(resolve_xf_date_kind(e, &custom_formats)),
                _ => {}
            },
            Event::Empty(ref e) => match e.local_name().as_ref() {
                "numFmt" => record_custom_num_fmt(e, &mut custom_formats),
                "xf" if in_cell_xfs => cell_xfs.push(resolve_xf_date_kind(e, &custom_formats)),
                _ => {}
            },
            Event::End(ref e) if e.local_name().as_ref() == "cellXfs" => in_cell_xfs = false,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(cell_xfs)
}

fn record_custom_num_fmt(start: &BytesStart<'_>, map: &mut HashMap<u32, DateStyleKind>) {
    let mut id = None;
    let mut code = None;
    for attr in start.attributes().flatten() {
        match attr.key.local_name().as_ref() {
            "numFmtId" => {
                id = attr
                    .normalized_value(XmlVersion::Implicit1_0)
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok());
            }
            "formatCode" => {
                code = attr
                    .normalized_value(XmlVersion::Implicit1_0)
                    .ok()
                    .map(|v| v.into_owned())
            }
            _ => {}
        }
    }
    if let (Some(id), Some(code)) = (id, code) {
        map.insert(id, classify_number_format_code(&code));
    }
}

fn resolve_xf_date_kind(
    start: &BytesStart<'_>,
    custom_formats: &HashMap<u32, DateStyleKind>,
) -> DateStyleKind {
    let mut num_fmt_id = None;
    for attr in start.attributes().flatten() {
        if attr.key.local_name().as_ref() == "numFmtId" {
            num_fmt_id = attr
                .normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .and_then(|v| v.parse::<u32>().ok());
        }
    }
    let Some(id) = num_fmt_id else {
        return DateStyleKind::None;
    };
    if let Some(&kind) = custom_formats.get(&id) {
        return kind;
    }
    match id {
        14..=17 => DateStyleKind::Date,
        18..=21 | 45..=47 => DateStyleKind::Time,
        22 => DateStyleKind::DateTime,
        _ => DateStyleKind::None,
    }
}

fn classify_number_format_code(code: &str) -> DateStyleKind {
    let mut has_date = false;
    let mut has_time = false;
    let mut in_quotes = false;
    let mut in_brackets = false;
    let mut escaped = false;

    for ch in code.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => in_quotes = !in_quotes,
            '[' if !in_quotes => in_brackets = true,
            ']' if !in_quotes => in_brackets = false,
            _ if in_quotes || in_brackets => {}
            'y' | 'Y' | 'd' | 'D' => has_date = true,
            'h' | 'H' | 's' | 'S' => has_time = true,
            _ => {}
        }
    }
    match (has_date, has_time) {
        (true, true) => DateStyleKind::DateTime,
        (true, false) => DateStyleKind::Date,
        (false, true) => DateStyleKind::Time,
        (false, false) => DateStyleKind::None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellType {
    Number,
    SharedString,
    InlineString,
    Bool,
    FormulaString,
    Error,
}

fn parse_worksheet_rows(
    xml: &str,
    shared_strings: &[String],
    date_styles: &[DateStyleKind],
    date_1904: bool,
) -> Result<Vec<Vec<String>>, String> {
    let mut reader = Reader::from_str(xml);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut total_chars = 0usize;

    let mut in_row = false;
    let mut current_row: Vec<String> = Vec::new();
    let mut next_col = 0usize;

    let mut in_cell = false;
    let mut cell_col = 0usize;
    let mut cell_type = CellType::Number;
    let mut cell_style: Option<usize> = None;
    let mut in_value = false;
    let mut in_inline_text = false;
    let mut phonetic_depth = 0usize;
    let mut raw_value = String::new();

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(ref e) => match e.local_name().as_ref() {
                "row" => {
                    in_row = true;
                    current_row.clear();
                    next_col = 0;
                }
                "c" if in_row => {
                    in_cell = true;
                    raw_value.clear();
                    let (parsed_col, parsed_type, parsed_style) = parse_cell_attrs(e, next_col);
                    cell_col = parsed_col;
                    next_col = parsed_col.saturating_add(1);
                    cell_type = parsed_type;
                    cell_style = parsed_style;
                }
                "v" if in_cell => in_value = true,
                "rPh" if in_cell => phonetic_depth += 1,
                "t" if in_cell && cell_type == CellType::InlineString && phonetic_depth == 0 => {
                    in_inline_text = true;
                }
                _ => {}
            },
            Event::Text(e) if in_value || (in_inline_text && phonetic_depth == 0) => {
                raw_value.push_str(&e.xml_content(XmlVersion::Implicit1_0));
            }
            Event::GeneralRef(e) if in_value || (in_inline_text && phonetic_depth == 0) => {
                push_xml_general_ref(&e, &mut raw_value)?;
            }
            Event::End(ref e) => match e.local_name().as_ref() {
                "v" => in_value = false,
                "t" => in_inline_text = false,
                "rPh" if phonetic_depth > 0 => phonetic_depth -= 1,
                "c" if in_cell => {
                    in_cell = false;
                    in_value = false;
                    in_inline_text = false;
                    phonetic_depth = 0;
                    if cell_col < MAX_XLSX_COLUMNS {
                        let formatted = format_cell_value(
                            cell_type,
                            &raw_value,
                            cell_style,
                            shared_strings,
                            date_styles,
                            date_1904,
                        );
                        if !formatted.is_empty() {
                            if current_row.len() <= cell_col {
                                current_row.resize(cell_col + 1, String::new());
                            }
                            total_chars = total_chars.saturating_add(formatted.len());
                            current_row[cell_col] = formatted;
                        }
                    }
                    raw_value.clear();
                }
                "row" if in_row => {
                    in_row = false;
                    while current_row.last().is_some_and(|cell| cell.is_empty()) {
                        current_row.pop();
                    }
                    if !current_row.is_empty() {
                        rows.push(std::mem::take(&mut current_row));
                    }
                    if total_chars >= MAX_EXTRACTED_TEXT_BYTES {
                        break;
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(rows)
}

fn parse_cell_attrs(
    start: &BytesStart<'_>,
    fallback_col: usize,
) -> (usize, CellType, Option<usize>) {
    let mut col = fallback_col;
    let mut cell_type = CellType::Number;
    let mut style = None;
    for attr in start.attributes().flatten() {
        let Ok(value) = attr.normalized_value(XmlVersion::Implicit1_0) else {
            continue;
        };
        match attr.key.local_name().as_ref() {
            "r" => {
                if let Some(parsed) = parse_column_index(&value) {
                    col = parsed;
                }
            }
            "t" => {
                cell_type = match value.as_ref() {
                    "s" => CellType::SharedString,
                    "inlineStr" => CellType::InlineString,
                    "b" => CellType::Bool,
                    "str" => CellType::FormulaString,
                    "e" => CellType::Error,
                    _ => CellType::Number,
                };
            }
            "s" => style = value.parse::<usize>().ok(),
            _ => {}
        }
    }
    (col, cell_type, style)
}

fn parse_column_index(cell_ref: &str) -> Option<usize> {
    let mut col: usize = 0;
    let mut saw_letter = false;
    for byte in cell_ref.bytes() {
        if byte.is_ascii_alphabetic() {
            saw_letter = true;
            let digit = usize::from(byte.to_ascii_uppercase() - b'A') + 1;
            col = col.checked_mul(26)?.checked_add(digit)?;
        } else {
            break;
        }
    }
    saw_letter.then(|| col - 1)
}

fn format_cell_value(
    cell_type: CellType,
    raw: &str,
    style: Option<usize>,
    shared_strings: &[String],
    date_styles: &[DateStyleKind],
    date_1904: bool,
) -> String {
    match cell_type {
        CellType::SharedString => raw
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|idx| shared_strings.get(idx))
            .cloned()
            .unwrap_or_default(),
        CellType::InlineString | CellType::FormulaString | CellType::Error => raw.to_owned(),
        CellType::Bool => match raw.trim() {
            "1" | "true" | "TRUE" => "TRUE".to_owned(),
            "0" | "false" | "FALSE" => "FALSE".to_owned(),
            other => other.to_owned(),
        },
        CellType::Number => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return String::new();
            }
            let style_kind = style
                .and_then(|idx| date_styles.get(idx))
                .copied()
                .unwrap_or(DateStyleKind::None);
            if style_kind != DateStyleKind::None
                && let Ok(serial) = trimmed.parse::<f64>()
                && serial.is_finite()
                && serial >= 0.0
                && let Some(formatted) = format_excel_serial_date(serial, style_kind, date_1904)
            {
                return formatted;
            }
            trimmed.to_owned()
        }
    }
}

fn format_excel_serial_date(serial: f64, kind: DateStyleKind, date_1904: bool) -> Option<String> {
    let whole_days = serial.floor() as i64;
    let frac = serial - serial.floor();
    let total_seconds = (frac * 86_400.0).round() as u32;
    let (carry_days, secs_in_day) = if total_seconds >= 86_400 {
        (1i64, total_seconds - 86_400)
    } else {
        (0i64, total_seconds)
    };
    let hours = secs_in_day / 3_600;
    let minutes = (secs_in_day % 3_600) / 60;
    let seconds = secs_in_day % 60;

    if kind == DateStyleKind::Time && whole_days == 0 {
        return Some(format!("{hours:02}:{minutes:02}:{seconds:02}"));
    }

    // Unix epoch (1970-01-01) is serial 25569 in the 1900 system (accounting for Excel's
    // intentional Lotus 1-2-3 1900-02-29 bug for serials >= 60) and 24107 in the 1904 system.
    let adjusted_days = if date_1904 {
        whole_days + carry_days - 24_107
    } else {
        let lotus_fix = if whole_days < 60 { 1 } else { 0 };
        whole_days + carry_days + lotus_fix - 25_569
    };
    let (year, month, day) = civil_from_unix_days(adjusted_days)?;
    match kind {
        DateStyleKind::Date => Some(format!("{year:04}-{month:02}-{day:02}")),
        DateStyleKind::Time => Some(format!("{hours:02}:{minutes:02}:{seconds:02}")),
        DateStyleKind::DateTime => Some(format!(
            "{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}"
        )),
        DateStyleKind::None => None,
    }
}

/// Howard Hinnant's civil-from-days conversion (days since 1970-01-01 -> `(year, month, day)`).
fn civil_from_unix_days(days_since_epoch: i64) -> Option<(i64, u32, u32)> {
    let z = days_since_epoch.checked_add(719_468)?;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    Some((year, m as u32, d as u32))
}
