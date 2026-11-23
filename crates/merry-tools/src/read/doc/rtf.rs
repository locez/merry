//! Lightweight RTF text extractor for `.doc` files saved in RTF format.

use crate::read::{doc::decode_cp1252, ooxml::MAX_EXTRACTED_TEXT_BYTES};

pub(super) fn extract_rtf_text(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    let mut depth = 0usize;
    let mut skip_depth: Option<usize> = None;

    while i < bytes.len() && out.len() < MAX_EXTRACTED_TEXT_BYTES {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                if skip_depth == Some(depth) {
                    skip_depth = None;
                }
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'\r' | b'\n' => i += 1,
            b'\\' => {
                i += 1;
                if i >= bytes.len() {
                    break;
                }
                match bytes[i] {
                    b'\\' | b'{' | b'}' => {
                        if skip_depth.is_none() {
                            out.push(char::from(bytes[i]));
                        }
                        i += 1;
                    }
                    b'*' => {
                        if skip_depth.is_none() {
                            skip_depth = Some(depth);
                        }
                        i += 1;
                    }
                    b'\'' if i + 2 < bytes.len() => {
                        let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                        if skip_depth.is_none()
                            && let Ok(val) = u8::from_str_radix(hex, 16)
                        {
                            out.push(decode_cp1252(val));
                        }
                        i += 3;
                    }
                    b'~' => {
                        if skip_depth.is_none() {
                            out.push(' ');
                        }
                        i += 1;
                    }
                    c if c.is_ascii_alphabetic() => {
                        let start = i;
                        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                            i += 1;
                        }
                        let word = &bytes[start..i];
                        let param_start = i;
                        if i < bytes.len() && bytes[i] == b'-' {
                            i += 1;
                        }
                        while i < bytes.len() && bytes[i].is_ascii_digit() {
                            i += 1;
                        }
                        let param = std::str::from_utf8(&bytes[param_start..i]).ok();
                        if i < bytes.len() && bytes[i] == b' ' {
                            i += 1;
                        }
                        if skip_depth.is_none() {
                            match word {
                                b"fonttbl" | b"colortbl" | b"stylesheet" | b"info" | b"pict"
                                | b"object" | b"header" | b"footer" => {
                                    skip_depth = Some(depth);
                                }
                                b"par" | b"line" => out.push('\n'),
                                b"tab" => out.push('\t'),
                                b"u" => {
                                    if let Some(num) = param.and_then(|s| s.parse::<i32>().ok()) {
                                        let code = if num < 0 {
                                            (num + 65_536) as u32
                                        } else {
                                            num as u32
                                        };
                                        if let Some(ch) = char::from_u32(code) {
                                            out.push(ch);
                                        }
                                        // Skip the single fallback replacement character (usually `?`).
                                        if i < bytes.len() && bytes[i] != b'{' && bytes[i] != b'}' {
                                            i += 1;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => i += 1,
                }
            }
            byte => {
                if skip_depth.is_none() {
                    out.push(decode_cp1252(byte));
                }
                i += 1;
            }
        }
    }
    out.trim().to_owned()
}
