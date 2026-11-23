use super::*;
use std::io::Cursor;
use zip::{ZipWriter, write::SimpleFileOptions};

fn make_zip_archive(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    for (name, body) in entries {
        writer
            .start_file(*name, options)
            .expect("zip entry should start");
        writer
            .write_all(body.as_bytes())
            .expect("zip entry should write");
    }
    writer
        .finish()
        .expect("zip archive should finish")
        .into_inner()
}

/// Builds a minimal valid MS-CFB (OLE2 v3, 512-byte sectors) container with
/// `WordDocument` and `1Table` streams (choosing regular FAT vs MiniFAT depending on size).
fn make_word97_doc(compressed_ascii: &str, unicode_text: &str, encrypted: bool) -> Vec<u8> {
    // Build WordDocument stream:
    // - 512-byte FIB header (csw=14, cslw=22, cbRgFcLcb=93)
    // - Compressed CP1252 bytes starting at offset 512
    // - UTF-16LE bytes starting right after compressed bytes (aligned to 2)
    let mut word_doc = vec![0u8; 512];
    word_doc[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes()); // wIdent
    word_doc[2..4].copy_from_slice(&0x00C1u16.to_le_bytes()); // nFib (Word 97)
    let mut flags: u16 = 0x0200; // fWhichTblStm = 1Table
    if encrypted {
        flags |= 0x0100; // fEncrypted
    }
    word_doc[10..12].copy_from_slice(&flags.to_le_bytes());
    word_doc[32..34].copy_from_slice(&14u16.to_le_bytes()); // csw
    word_doc[62..64].copy_from_slice(&22u16.to_le_bytes()); // cslw

    let ascii_bytes = compressed_ascii.as_bytes();
    let utf16_units: Vec<u16> = unicode_text.encode_utf16().collect();
    let total_chars = (ascii_bytes.len() + utf16_units.len()) as u32;
    // FibRgLw97 starts at 64; ccpText is index 3 => offset 76.
    word_doc[76..80].copy_from_slice(&total_chars.to_le_bytes());
    word_doc[152..154].copy_from_slice(&93u16.to_le_bytes()); // cbRgFcLcb

    let ascii_offset = word_doc.len() as u32;
    word_doc.extend_from_slice(ascii_bytes);
    if !word_doc.len().is_multiple_of(2) {
        word_doc.push(0);
    }
    let utf16_offset = word_doc.len() as u32;
    for unit in &utf16_units {
        word_doc.extend_from_slice(&unit.to_le_bytes());
    }
    // Pad WordDocument to >= 4096 bytes so it lives in regular FAT sectors.
    if word_doc.len() < 4096 {
        word_doc.resize(4096, 0);
    }

    // Build Clx in 1Table: 1 Prc dummy + Pcdt with 2 pieces (CP1252 + UTF-16LE).
    let mut table_stream = Vec::new();
    table_stream.push(0x01); // Prc
    table_stream.extend_from_slice(&2u16.to_le_bytes());
    table_stream.extend_from_slice(&[0x00, 0x00]);

    let plc_len: u32 = 4 * 3 + 8 * 2; // 3 CPs + 2 Pcds
    table_stream.push(0x02); // Pcdt
    table_stream.extend_from_slice(&plc_len.to_le_bytes());
    // CPs: 0, ascii_len, total_chars
    table_stream.extend_from_slice(&0u32.to_le_bytes());
    table_stream.extend_from_slice(&(ascii_bytes.len() as u32).to_le_bytes());
    table_stream.extend_from_slice(&total_chars.to_le_bytes());
    // Piece 0: compressed (bit 30 set, fc = byte_offset * 2)
    let fc0 = 0x4000_0000u32 | (ascii_offset * 2);
    table_stream.extend_from_slice(&0u16.to_le_bytes());
    table_stream.extend_from_slice(&fc0.to_le_bytes());
    table_stream.extend_from_slice(&0u16.to_le_bytes());
    // Piece 1: UTF-16LE (bit 30 clear, fc = byte_offset)
    table_stream.extend_from_slice(&0u16.to_le_bytes());
    table_stream.extend_from_slice(&utf16_offset.to_le_bytes());
    table_stream.extend_from_slice(&0u16.to_le_bytes());

    // Write fcClx = 0, lcbClx = table_stream.len() at offset 154 + 33 * 8 = 418.
    word_doc[418..422].copy_from_slice(&0u32.to_le_bytes());
    word_doc[422..426].copy_from_slice(&(table_stream.len() as u32).to_le_bytes());

    // Pack into an OLE2 v3 file where:
    // - Sector 0: FAT
    // - Sector 1: Directory (Root Entry + WordDocument + 1Table)
    // - Sector 2: MiniFAT (for 1Table < 4096 bytes)
    // - Sector 3: Root MiniStream (contains 1Table padded to 512 bytes)
    // - Sectors 4..4+wd_sectors: WordDocument
    let wd_sectors = word_doc.len().div_ceil(512);
    let mut header = vec![0u8; 512];
    header[0..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    header[24..26].copy_from_slice(&0x003Eu16.to_le_bytes()); // minor version
    header[26..28].copy_from_slice(&0x0003u16.to_le_bytes()); // major version 3
    header[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes()); // little endian
    header[30..32].copy_from_slice(&9u16.to_le_bytes()); // 512-byte sectors
    header[32..34].copy_from_slice(&6u16.to_le_bytes()); // 64-byte mini sectors
    header[44..48].copy_from_slice(&1u32.to_le_bytes()); // num FAT sectors
    header[48..52].copy_from_slice(&1u32.to_le_bytes()); // first dir sector = 1
    header[56..60].copy_from_slice(&4096u32.to_le_bytes()); // mini stream cutoff
    header[60..64].copy_from_slice(&2u32.to_le_bytes()); // first minifat sector = 2
    header[64..68].copy_from_slice(&1u32.to_le_bytes()); // num minifat sectors = 1
    header[68..72].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes()); // first difat = ENDOFCHAIN
    header[72..76].copy_from_slice(&0u32.to_le_bytes()); // num difat = 0
    // DIFAT[0] = sector 0, rest FREESECT
    header[76..80].copy_from_slice(&0u32.to_le_bytes());
    for i in 1..109 {
        header[76 + i * 4..80 + i * 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    }

    let mut fat = vec![0xFFFF_FFFFu32; 128];
    fat[0] = 0xFFFF_FFFD; // FATSECT
    fat[1] = 0xFFFF_FFFE; // Dir
    fat[2] = 0xFFFF_FFFE; // MiniFAT
    fat[3] = 0xFFFF_FFFE; // MiniStream
    for i in 0..wd_sectors {
        fat[4 + i] = if i + 1 < wd_sectors {
            (4 + i + 1) as u32
        } else {
            0xFFFF_FFFE
        };
    }
    let mut fat_sector = Vec::with_capacity(512);
    for entry in fat {
        fat_sector.extend_from_slice(&entry.to_le_bytes());
    }

    let mut dir_sector = vec![0u8; 512];
    write_dir_entry(&mut dir_sector[0..128], "Root Entry", 5, 3, 512);
    write_dir_entry(
        &mut dir_sector[128..256],
        "WordDocument",
        2,
        4,
        word_doc.len() as u64,
    );
    write_dir_entry(
        &mut dir_sector[256..384],
        "1Table",
        2,
        0,
        table_stream.len() as u64,
    );

    let mini_sectors = table_stream.len().div_ceil(64);
    let mut minifat = vec![0xFFFF_FFFFu32; 128];
    for (i, slot) in minifat.iter_mut().enumerate().take(mini_sectors) {
        *slot = if i + 1 < mini_sectors {
            (i + 1) as u32
        } else {
            0xFFFF_FFFE
        };
    }
    let mut minifat_sector = Vec::with_capacity(512);
    for entry in minifat {
        minifat_sector.extend_from_slice(&entry.to_le_bytes());
    }

    let mut mini_stream_sector = vec![0u8; 512];
    mini_stream_sector[..table_stream.len()].copy_from_slice(&table_stream);

    let mut padded_wd = word_doc;
    padded_wd.resize(wd_sectors * 512, 0);

    let mut file_bytes = Vec::new();
    file_bytes.extend_from_slice(&header);
    file_bytes.extend_from_slice(&fat_sector);
    file_bytes.extend_from_slice(&dir_sector);
    file_bytes.extend_from_slice(&minifat_sector);
    file_bytes.extend_from_slice(&mini_stream_sector);
    file_bytes.extend_from_slice(&padded_wd);
    file_bytes
}

fn write_dir_entry(buf: &mut [u8], name: &str, obj_type: u8, start_sector: u32, size: u64) {
    let mut name_bytes = Vec::new();
    for unit in name.encode_utf16() {
        name_bytes.extend_from_slice(&unit.to_le_bytes());
    }
    name_bytes.extend_from_slice(&0u16.to_le_bytes());
    buf[..name_bytes.len()].copy_from_slice(&name_bytes);
    buf[64..66].copy_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    buf[66] = obj_type;
    buf[116..120].copy_from_slice(&start_sector.to_le_bytes());
    buf[120..128].copy_from_slice(&size.to_le_bytes());
}

fn pptx_slide_xml(paragraphs: &[&str]) -> String {
    let body: String = paragraphs
        .iter()
        .map(|p| format!("<a:p><a:r><a:t>{p}</a:t></a:r></a:p>"))
        .collect();
    format!(
        r#"<?xml version="1.0"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:sp><p:txBody>{body}</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
    )
}

#[test]
fn read_file_extracts_pptx_slides_and_speaker_notes_in_numeric_order() {
    let temp = TempWorkspace::new("read-file-pptx");
    let pptx = make_zip_archive(&[
        ("[Content_Types].xml", "<Types/>"),
        ("ppt/slides/slide10.xml", &pptx_slide_xml(&["Tenth Slide"])),
        (
            "ppt/slides/slide1.xml",
            &pptx_slide_xml(&["Intro &amp; Overview", "Bullet"]),
        ),
        ("ppt/slides/slide2.xml", &pptx_slide_xml(&["Second Slide"])),
        (
            "ppt/notesSlides/notesSlide2.xml",
            &pptx_slide_xml(&["Remember to demo"]),
        ),
    ]);
    temp.write_bytes("deck.pptx", &pptx);
    temp.write_bytes("broken.pptx", b"not a zip");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "deck.pptx");
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        json_content(&outcome)["content"],
        "--- Slide 1 ---\nIntro & Overview\nBullet\n\n--- Slide 2 ---\nSecond Slide\n\nSpeaker Notes:\nRemember to demo\n\n--- Slide 10 ---\nTenth Slide"
    );

    assert_failed_json(
        &read_outcome(&tools, "broken.pptx"),
        ERROR_INVALID_DOCUMENT,
        Some("broken.pptx"),
        temp.path(),
    );
}

#[test]
fn read_file_extracts_docx_headings_lists_tables_footnotes_and_skips_deletions() {
    let temp = TempWorkspace::new("read-file-docx");
    let docx = make_zip_archive(&[
        ("[Content_Types].xml", "<Types/>"),
        (
            "word/document.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
      <w:r><w:t>Design Spec &amp; Notes</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Keep this</w:t></w:r>
      <w:del><w:r><w:delText> removed draft</w:delText></w:r></w:del>
      <w:r><w:instrText> HYPERLINK "https://example.com" </w:instrText></w:r>
      <w:r><w:t> text.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr><w:numPr><w:ilvl w:val="0"/></w:numPr></w:pPr>
      <w:r><w:t>Bullet item</w:t></w:r>
    </w:p>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Metric</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Latency</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>12ms | p99</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#,
        ),
        (
            "word/footnotes.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:t>---</w:t></w:r></w:p></w:footnote>
  <w:footnote w:id="1"><w:p><w:r><w:t>Footnote reference detail.</w:t></w:r></w:p></w:footnote>
</w:footnotes>"#,
        ),
    ]);
    temp.write_bytes("spec.docx", &docx);
    temp.write_bytes("broken.docx", b"not a zip");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "spec.docx");
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let content = json_content(&outcome)["content"]
        .as_str()
        .expect("docx content")
        .to_owned();
    assert!(content.contains("# Design Spec & Notes"), "{content}");
    assert!(content.contains("Keep this text."), "{content}");
    assert!(!content.contains("removed draft"), "{content}");
    assert!(!content.contains("HYPERLINK"), "{content}");
    assert!(content.contains("- Bullet item"), "{content}");
    assert!(
        content.contains("| Metric | Value |\n| --- | --- |\n| Latency | 12ms \\| p99 |"),
        "{content}"
    );
    assert!(
        content.contains("--- Footnotes ---\nFootnote reference detail."),
        "{content}"
    );

    assert_failed_json(
        &read_outcome(&tools, "broken.docx"),
        ERROR_INVALID_DOCUMENT,
        Some("broken.docx"),
        temp.path(),
    );
}

#[test]
fn read_file_extracts_xlsx_sheets_shared_strings_dates_and_sparse_columns() {
    let temp = TempWorkspace::new("read-file-xlsx");
    let xlsx = make_zip_archive(&[
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Summary" sheetId="1" r:id="rId2"/>
    <sheet name="Empty" sheetId="2" r:id="rId1"/>
  </sheets>
</workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Target="worksheets/sheet2.xml"/>
  <Relationship Id="rId2" Target="/xl/worksheets/sheet1.xml"/>
</Relationships>"#,
        ),
        (
            "xl/sharedStrings.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <si><t>Item</t></si>
  <si><r><t>Q1 </t></r><r><t>Revenue &amp; Profit</t></r><rPh><t>ruby</t></rPh></si>
  <si><t>Widget</t></si>
</sst>"#,
        ),
        (
            "xl/styles.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <numFmts count="1">
    <numFmt numFmtId="164" formatCode="yyyy-mm-dd"/>
  </numFmts>
  <cellXfs count="2">
    <xf numFmtId="0"/>
    <xf numFmtId="164"/>
  </cellXfs>
</styleSheet>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>0</v></c>
      <c r="B1" t="s"><v>1</v></c>
      <c r="C1" t="inlineStr"><is><t>Date</t></is></c>
      <c r="D1" t="inlineStr"><is><t>Active</t></is></c>
    </row>
    <row r="2">
      <c r="A2" t="s"><v>2</v></c>
      <c r="C2" s="1"><v>45658</v></c>
      <c r="D2" t="b"><v>1</v></c>
    </row>
  </sheetData>
</worksheet>"#,
        ),
        (
            "xl/worksheets/sheet2.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData/>
</worksheet>"#,
        ),
    ]);
    temp.write_bytes("report.xlsx", &xlsx);
    temp.write_bytes("broken.xlsx", b"not a zip");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "report.xlsx");
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let content = json_content(&outcome)["content"]
        .as_str()
        .expect("xlsx content")
        .to_owned();
    assert!(content.contains("--- Sheet: Summary ---"), "{content}");
    assert!(
        content.contains("| Item | Q1 Revenue & Profit | Date | Active |"),
        "{content}"
    );
    assert!(
        content.contains("| Widget |  | 2025-01-01 | TRUE |"),
        "{content}"
    );
    assert!(!content.contains("ruby"), "{content}");
    assert!(
        content.contains("--- Sheet: Empty ---\n(empty sheet)"),
        "{content}"
    );

    assert_failed_json(
        &read_outcome(&tools, "broken.xlsx"),
        ERROR_INVALID_DOCUMENT,
        Some("broken.xlsx"),
        temp.path(),
    );
}

#[test]
fn read_file_extracts_legacy_doc_ole2_rtf_and_renamed_docx_and_rejects_encrypted() {
    let temp = TempWorkspace::new("read-file-doc");

    // 1. Genuine Word 97-2003 OLE2 binary with CP1252 + UTF-16LE CJK + field code + table row.
    let ole2_doc = make_word97_doc(
        "Hello \x13HYPERLINK \"https://x\"\x14Visible Link\x15\rColA\x07ColB\x07\x07",
        "中文段落\r",
        false,
    );
    temp.write_bytes("legacy.doc", &ole2_doc);

    // 2. Encrypted Word 97-2003 OLE2 binary is rejected cleanly.
    let encrypted_doc = make_word97_doc("secret", "", true);
    temp.write_bytes("encrypted.doc", &encrypted_doc);

    // 3. RTF file saved with `.doc` extension.
    temp.write_bytes(
        "notes.doc",
        br"{\rtf1\ansi{\fonttbl{\f0 Arial;}}Hello\par\u20013? \u25991?\tab OK}",
    );

    // 4. Renamed `.docx` saved with `.doc` extension.
    let renamed_docx = make_zip_archive(&[(
        "word/document.xml",
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Renamed DOCX body</w:t></w:r></w:p></w:body></w:document>"#,
    )]);
    temp.write_bytes("renamed.doc", &renamed_docx);
    temp.write_bytes("broken.doc", b"plain garbage bytes");

    let tools = tools_for(temp.path());

    let ole2_outcome = read_outcome(&tools, "legacy.doc");
    assert_eq!(ole2_outcome.status(), ToolCallResultStatus::Succeeded);
    let ole2_content = json_content(&ole2_outcome)["content"]
        .as_str()
        .expect("ole2 doc content")
        .to_owned();
    assert!(
        ole2_content.contains("Hello Visible Link"),
        "{ole2_content}"
    );
    assert!(!ole2_content.contains("HYPERLINK"), "{ole2_content}");
    assert!(ole2_content.contains("ColA | ColB"), "{ole2_content}");
    assert!(ole2_content.contains("中文段落"), "{ole2_content}");

    assert_failed_json(
        &read_outcome(&tools, "encrypted.doc"),
        ERROR_INVALID_DOCUMENT,
        Some("encrypted.doc"),
        temp.path(),
    );

    let rtf_outcome = read_outcome(&tools, "notes.doc");
    assert_eq!(rtf_outcome.status(), ToolCallResultStatus::Succeeded);
    let rtf_content = json_content(&rtf_outcome)["content"]
        .as_str()
        .expect("rtf doc content")
        .to_owned();
    assert!(rtf_content.contains("Hello\n中 文\tOK"), "{rtf_content}");
    assert!(!rtf_content.contains("Arial"), "{rtf_content}");

    let renamed_outcome = read_outcome(&tools, "renamed.doc");
    assert_eq!(renamed_outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        json_content(&renamed_outcome)["content"],
        "Renamed DOCX body"
    );

    assert_failed_json(
        &read_outcome(&tools, "broken.doc"),
        ERROR_INVALID_DOCUMENT,
        Some("broken.doc"),
        temp.path(),
    );
}
