use crate::tui::state::ToolOutputPreview;

#[test]
fn output_preview_bounds_long_ascii_and_multibyte_lines() {
    for content in ["x".repeat(900_000), "界🚀".repeat(150_000)] {
        let preview = ToolOutputPreview::new(content.lines(), false);

        assert!(preview.lines.iter().map(String::len).sum::<usize>() <= 720);
        assert_eq!(
            preview.lines,
            [content.chars().take(180).collect::<String>()]
        );
        assert!(preview.truncated);
    }
}

#[test]
fn output_preview_preserves_exact_character_boundary_and_source_truncation() {
    let content = "🚀".repeat(180);
    for source_truncated in [false, true] {
        let preview = ToolOutputPreview::new(content.lines(), source_truncated);

        assert_eq!(preview.lines.as_slice(), std::slice::from_ref(&content));
        assert_eq!(preview.truncated, source_truncated);
    }
}

#[test]
fn output_preview_bounds_total_retained_text_and_keeps_short_lines() {
    let long_line = "界🚀".repeat(200);
    let content = format!("\nshort\n{long_line}\n{long_line}\n{long_line}\n{long_line}\nsixth");
    let preview = ToolOutputPreview::new(content.lines(), false);

    assert_eq!(preview.lines.len(), 5);
    assert_eq!(preview.lines[0], "short");
    assert!(preview.lines.iter().all(|line| line.chars().count() <= 180));
    assert!(preview.lines.iter().map(String::len).sum::<usize>() <= 3_600);
    assert!(preview.truncated);
}
