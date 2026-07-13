use super::*;
use std::sync::Arc;

fn draft_image(marker: u8) -> DraftImage {
    DraftImage::new(
        Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, marker]),
        2,
        3,
    )
    .expect("valid draft image")
}

#[test]
fn image_placeholder_is_atomic_for_cursor_backspace_and_delete() {
    let mut input = TextInput::default();
    input.insert_str("before ");
    let image_start = input.cursor_byte_index();
    input
        .insert_image(draft_image(1))
        .expect("image should insert");
    let image_end = input.cursor_byte_index();
    input.insert_str(" after");

    input.cursor = image_end;
    input.move_left();
    assert_eq!(input.cursor_byte_index(), image_start);
    input.move_right();
    assert_eq!(input.cursor_byte_index(), image_end);

    input.backspace();
    assert_eq!(input.text(), "before  after");
    assert!(input.image_elements().is_empty());

    input.cursor = "before ".len();
    input
        .insert_image(draft_image(2))
        .expect("replacement image should insert");
    input.cursor = "before ".len();
    input.delete();
    assert_eq!(input.text(), "before  after");
    assert!(input.image_elements().is_empty());
}

#[test]
fn replacing_inside_an_image_removes_the_whole_element_and_tracks_utf8_shifts() {
    let mut input = TextInput::default();
    input.insert_str("你");
    input
        .insert_image(draft_image(1))
        .expect("image should insert");
    input.insert_str("好");
    assert_eq!(input.text(), "你[Image #1]好");

    input.replace_range("你[Im".len().."你[Image".len(), "X");

    assert_eq!(input.text(), "你X好");
    assert_eq!(input.cursor_byte_index(), "你X".len());
    assert!(input.image_elements().is_empty());
}

#[test]
fn deleting_an_earlier_image_renumbers_remaining_labels() {
    let mut input = TextInput::default();
    input
        .insert_image(draft_image(1))
        .expect("first image should insert");
    input.insert_str(" ");
    input
        .insert_image(draft_image(2))
        .expect("second image should insert");
    assert_eq!(input.text(), "[Image #1] [Image #2]");

    input.cursor = 0;
    input.delete();

    assert_eq!(input.text(), " [Image #1]");
    assert_eq!(input.image_elements().len(), 1);
    assert_eq!(input.image_elements()[0].placeholder, "[Image #1]");
}

#[test]
fn submission_expands_large_paste_keeps_images_and_builds_text_only_history() {
    let mut input = TextInput::default();
    let pasted = "p".repeat(PASTE_PLACEHOLDER_THRESHOLD_CHARS);
    input.insert_str("start ");
    input.insert_paste(&pasted);
    input.insert_str(" ");
    input
        .insert_image(draft_image(7))
        .expect("image should insert");
    input.insert_str(" end");

    let submission = input
        .take_submission()
        .expect("submission should remain valid")
        .expect("submission should be nonblank");

    assert_eq!(submission.text, format!("start {pasted} [Image #1] end"));
    assert_eq!(submission.history_text, format!("start {pasted}  end"));
    assert_eq!(submission.images.len(), 1);
    assert_eq!(submission.images[0].label(), "[Image #1]");
    assert_eq!(submission.images[0].png_bytes()[8], 7);
    assert!(input.text().is_empty());

    let mut history = InputHistory::default();
    history.record(&submission.history_text);
    let mut restored = TextInput::default();
    history.previous(&mut restored);
    assert_eq!(restored.text(), format!("start {pasted}  end"));
    assert!(restored.image_elements().is_empty());
}

#[test]
fn viewport_marks_image_placeholders_and_clear_releases_payloads() {
    let bytes = Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 9]);
    let image = DraftImage::new(Arc::clone(&bytes), 2, 3).expect("valid draft image");
    let mut input = TextInput::default();
    input.insert_image(image).expect("image should insert");
    assert_eq!(Arc::strong_count(&bytes), 2);

    let viewport = input.viewport(80);
    assert_eq!(viewport.image_placeholders, ["[Image #1]"]);

    input.clear();
    assert_eq!(Arc::strong_count(&bytes), 1);
}
