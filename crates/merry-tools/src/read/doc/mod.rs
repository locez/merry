//! Legacy Word (`.doc`) text extractor with pure-Rust OLE2 CFBF + Word 97–2003
//! FIB/Clx piece-table decoding, plus transparent fallbacks for renamed `.docx`
//! and RTF files saved with a `.doc` extension.

use std::fs;

use merry_runtime::ToolExecutionOutcome;

use crate::{
    config::WorkspaceToolLimits,
    errors::{BlockingToolError, DomainError, ERROR_INVALID_DOCUMENT},
    path::ValidatedToolPath,
    read::{
        docx,
        ooxml::{MAX_EXTRACTED_TEXT_BYTES, read_bounded_office_file},
        slice_text_outcome,
    },
};

mod cfb;
mod rtf;

use cfb::{CfbArchive, OLE2_SIGNATURE, read_u16_le, read_u32_le};

const ZIP_SIGNATURE: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];
const WORD_FIB_IDENT: u16 = 0xA5EC;
const FIB_FLAG_ENCRYPTED: u16 = 0x0100;
const FIB_FLAG_WHICH_TBL_STM: u16 = 0x0200;

/// Windows-1252 code points for bytes `0x80..=0x9F`.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

pub(super) fn read_resolved_doc(
    relative: &ValidatedToolPath,
    file: &mut fs::File,
    start_line: usize,
    max_lines: usize,
    limits: &WorkspaceToolLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    let bytes = read_bounded_office_file(file, "DOC", is_cancelled)?;
    let text = extract_doc_text(&bytes, is_cancelled)?;
    Ok(slice_text_outcome(
        relative, &text, start_line, max_lines, limits,
    ))
}

fn extract_doc_text(
    bytes: &[u8],
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }
    if bytes.starts_with(&ZIP_SIGNATURE) {
        return docx::extract_docx_text(bytes, is_cancelled);
    }
    if bytes.starts_with(b"{\\rtf") {
        return Ok(rtf::extract_rtf_text(bytes));
    }
    if !bytes.starts_with(&OLE2_SIGNATURE) {
        return Err(DomainError::new(
            ERROR_INVALID_DOCUMENT,
            "file is not a valid Word .doc (OLE2), .docx, or RTF document",
        )
        .into());
    }

    let cfb = CfbArchive::parse(bytes)?;
    let word_doc = cfb.read_stream("WordDocument")?.ok_or_else(|| {
        DomainError::new(
            ERROR_INVALID_DOCUMENT,
            "OLE2 .doc file is missing the WordDocument stream",
        )
    })?;
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let fib = parse_word_fib(&word_doc)?;
    let table_name = if fib.use_table_1 { "1Table" } else { "0Table" };
    let table_stream = cfb.read_stream(table_name)?.ok_or_else(|| {
        DomainError::new(
            ERROR_INVALID_DOCUMENT,
            format!("OLE2 .doc file is missing the {table_name} stream"),
        )
    })?;

    let raw_text = decode_clx_pieces(&word_doc, &table_stream, &fib)?;
    Ok(clean_word_control_chars(&raw_text))
}

struct WordFib {
    use_table_1: bool,
    ccp_text: u32,
    fc_clx: u32,
    lcb_clx: u32,
}

fn parse_word_fib(word_doc: &[u8]) -> Result<WordFib, DomainError> {
    if word_doc.len() < 34 {
        return Err(DomainError::new(
            ERROR_INVALID_DOCUMENT,
            "WordDocument stream is truncated before FIB header",
        ));
    }
    let w_ident = read_u16_le(word_doc, 0)?;
    if w_ident != WORD_FIB_IDENT {
        return Err(DomainError::new(
            ERROR_INVALID_DOCUMENT,
            format!("unexpected Word FIB signature 0x{w_ident:04X}"),
        ));
    }
    let n_fib = read_u16_le(word_doc, 2)?;
    if n_fib < 0x00C1 {
        return Err(DomainError::new(
            ERROR_INVALID_DOCUMENT,
            "Word documents older than Word 97 are not supported",
        ));
    }
    let flags = read_u16_le(word_doc, 10)?;
    if (flags & FIB_FLAG_ENCRYPTED) != 0 {
        return Err(DomainError::new(
            ERROR_INVALID_DOCUMENT,
            "encrypted .doc files are not supported",
        ));
    }
    let use_table_1 = (flags & FIB_FLAG_WHICH_TBL_STM) != 0;

    let csw = usize::from(read_u16_le(word_doc, 32)?);
    let cslw_offset = 34usize
        .checked_add(csw.checked_mul(2).ok_or_else(invalid_fib)?)
        .ok_or_else(invalid_fib)?;
    let cslw = usize::from(read_u16_le(word_doc, cslw_offset)?);
    let rg_lw_offset = cslw_offset + 2;
    if cslw < 4 {
        return Err(invalid_fib());
    }
    let ccp_text = read_u32_le(word_doc, rg_lw_offset + 3 * 4)?;

    let cb_rg_fc_lcb_offset = rg_lw_offset
        .checked_add(cslw.checked_mul(4).ok_or_else(invalid_fib)?)
        .ok_or_else(invalid_fib)?;
    let cb_rg_fc_lcb = usize::from(read_u16_le(word_doc, cb_rg_fc_lcb_offset)?);
    // Clx is entry 33 (fcClx at +264, lcbClx at +268) in FibRgFcLcb97 (each entry is 8 bytes).
    if cb_rg_fc_lcb <= 33 {
        return Err(invalid_fib());
    }
    let clx_entry_offset = cb_rg_fc_lcb_offset + 2 + 33 * 8;
    let fc_clx = read_u32_le(word_doc, clx_entry_offset)?;
    let lcb_clx = read_u32_le(word_doc, clx_entry_offset + 4)?;

    Ok(WordFib {
        use_table_1,
        ccp_text,
        fc_clx,
        lcb_clx,
    })
}

fn invalid_fib() -> DomainError {
    DomainError::new(
        ERROR_INVALID_DOCUMENT,
        "invalid or truncated Word FIB structure",
    )
}

fn decode_clx_pieces(
    word_doc: &[u8],
    table_stream: &[u8],
    fib: &WordFib,
) -> Result<String, DomainError> {
    let clx_start = usize::try_from(fib.fc_clx).map_err(|_| invalid_fib())?;
    let clx_len = usize::try_from(fib.lcb_clx).map_err(|_| invalid_fib())?;
    let clx_end = clx_start.checked_add(clx_len).ok_or_else(invalid_fib)?;
    let clx = table_stream.get(clx_start..clx_end).ok_or_else(|| {
        DomainError::new(
            ERROR_INVALID_DOCUMENT,
            "Word Clx piece table lies outside the Table stream",
        )
    })?;

    let mut cursor = 0usize;
    while cursor < clx.len() {
        match clx[cursor] {
            0x01 => {
                // Prc record: tag (1) + cbGrpprl (2) + grpprl (cbGrpprl)
                let cb = usize::from(read_u16_le(clx, cursor + 1)?);
                cursor = cursor
                    .checked_add(3 + cb)
                    .ok_or_else(|| DomainError::new(ERROR_INVALID_DOCUMENT, "corrupt Word Prc"))?;
            }
            0x02 => {
                // Pcdt record: tag (1) + lcb (4) + PlcPcd (lcb)
                let lcb = usize::try_from(read_u32_le(clx, cursor + 1)?)
                    .map_err(|_| DomainError::new(ERROR_INVALID_DOCUMENT, "corrupt Word Pcdt"))?;
                let plc_start = cursor + 5;
                let plc_end = plc_start
                    .checked_add(lcb)
                    .ok_or_else(|| DomainError::new(ERROR_INVALID_DOCUMENT, "corrupt Word Pcdt"))?;
                let plc_pcd = clx.get(plc_start..plc_end).ok_or_else(|| {
                    DomainError::new(ERROR_INVALID_DOCUMENT, "truncated Word PlcPcd")
                })?;
                return decode_plc_pcd(word_doc, plc_pcd, fib.ccp_text);
            }
            other => {
                return Err(DomainError::new(
                    ERROR_INVALID_DOCUMENT,
                    format!("unexpected Word Clx record tag 0x{other:02X}"),
                ));
            }
        }
    }

    Err(DomainError::new(
        ERROR_INVALID_DOCUMENT,
        "Word Clx stream did not contain a piece table",
    ))
}

fn decode_plc_pcd(word_doc: &[u8], plc_pcd: &[u8], ccp_text: u32) -> Result<String, DomainError> {
    if plc_pcd.len() < 16 || !(plc_pcd.len() - 4).is_multiple_of(12) {
        return Err(DomainError::new(
            ERROR_INVALID_DOCUMENT,
            "Word PlcPcd has an invalid byte length",
        ));
    }
    let piece_count = (plc_pcd.len() - 4) / 12;
    let pcd_base = (piece_count + 1) * 4;
    let mut output = String::new();

    for i in 0..piece_count {
        let cp_start = read_u32_le(plc_pcd, i * 4)?;
        let cp_end = read_u32_le(plc_pcd, (i + 1) * 4)?;
        if cp_end <= cp_start {
            continue;
        }
        if ccp_text > 0 && cp_start >= ccp_text {
            break;
        }
        let clamped_end = if ccp_text > 0 {
            cp_end.min(ccp_text)
        } else {
            cp_end
        };
        let char_count = usize::try_from(clamped_end - cp_start).unwrap_or(0);
        if char_count == 0 {
            continue;
        }

        let fc_compressed = read_u32_le(plc_pcd, pcd_base + i * 8 + 2)?;
        let is_compressed = (fc_compressed & 0x4000_0000) != 0;
        let raw_fc = fc_compressed & 0x3FFF_FFFF;

        if is_compressed {
            let byte_offset = usize::try_from(raw_fc / 2).unwrap_or(usize::MAX);
            let byte_end = byte_offset.saturating_add(char_count);
            if let Some(slice) = word_doc.get(byte_offset..byte_end) {
                for &b in slice {
                    output.push(decode_cp1252(b));
                    if output.len() >= MAX_EXTRACTED_TEXT_BYTES {
                        return Ok(output);
                    }
                }
            }
        } else {
            let byte_offset = usize::try_from(raw_fc).unwrap_or(usize::MAX);
            let byte_len = char_count.saturating_mul(2);
            let byte_end = byte_offset.saturating_add(byte_len);
            if let Some(slice) = word_doc.get(byte_offset..byte_end) {
                let units = slice
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair));
                for decoded in char::decode_utf16(units) {
                    output.push(decoded.unwrap_or(char::REPLACEMENT_CHARACTER));
                    if output.len() >= MAX_EXTRACTED_TEXT_BYTES {
                        return Ok(output);
                    }
                }
            }
        }
    }

    Ok(output)
}

fn decode_cp1252(byte: u8) -> char {
    if (0x80..=0x9F).contains(&byte) {
        CP1252_HIGH[usize::from(byte - 0x80)]
    } else {
        char::from(byte)
    }
}

fn clean_word_control_chars(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    // Track nested Word field states: false = inside field instruction (hide), true = visible result.
    let mut field_stack: Vec<bool> = Vec::new();
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\u{0013}' => {
                field_stack.push(false);
                continue;
            }
            '\u{0014}' => {
                if let Some(top) = field_stack.last_mut() {
                    *top = true;
                }
                continue;
            }
            '\u{0015}' => {
                field_stack.pop();
                continue;
            }
            _ => {}
        }

        if field_stack.iter().any(|&visible| !visible) {
            continue;
        }

        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\u{000B}' => out.push('\n'),
            '\u{000C}' => {
                if !out.ends_with("\n\n") {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push('\n');
                }
            }
            '\u{0007}' => {
                if chars.peek() == Some(&'\u{0007}') {
                    chars.next();
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                } else {
                    out.push_str(" | ");
                }
            }
            '\u{00A0}' => out.push(' '),
            '\t' | '\n' => out.push(ch),
            // Strip Word inline placeholders, object anchors, footnote refs, soft hyphens, and NUL.
            '\u{0000}'..='\u{001F}' => {}
            other => out.push(other),
        }
    }

    out.trim().to_owned()
}
