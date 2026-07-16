use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_runtime::{
    MAX_USER_IMAGES, RuntimeError, UserImageInput, UserMessageInput, user_image_label,
};
use std::{ops::Range, sync::Arc};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) const MAX_INPUT_HISTORY: usize = 200;
const PASTE_PLACEHOLDER_THRESHOLD_CHARS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TextInputViewport {
    pub(crate) text: String,
    pub(crate) image_placeholders: Vec<String>,
    pub(crate) cursor_column: usize,
    pub(crate) cursor_row: usize,
    pub(crate) visible_rows: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TextInput {
    text: String,
    cursor: usize,
    elements: Vec<InputElement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputElement {
    range: Range<usize>,
    placeholder: String,
    payload: InputElementPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InputElementPayload {
    LargePaste { content: String },
    Image { image: DraftImage },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DraftImage {
    png_bytes: Arc<[u8]>,
    width: u32,
    height: u32,
}

impl DraftImage {
    pub(crate) fn new(
        png_bytes: impl Into<Arc<[u8]>>,
        width: u32,
        height: u32,
    ) -> Result<Self, RuntimeError> {
        let image = UserImageInput::png("[Image #1]", png_bytes, width, height)?;
        Ok(Self {
            png_bytes: image.shared_png_bytes(),
            width,
            height,
        })
    }

    #[must_use]
    pub(crate) fn shared_png_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.png_bytes)
    }

    #[must_use]
    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub(crate) fn height(&self) -> u32 {
        self.height
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TuiSubmission {
    pub(crate) text: String,
    pub(crate) history_text: String,
    pub(crate) images: Vec<UserImageInput>,
}

impl TuiSubmission {
    pub(crate) fn into_user_message_and_history(
        self,
    ) -> Result<(UserMessageInput, String), RuntimeError> {
        let Self {
            text,
            history_text,
            images,
        } = self;
        let message = UserMessageInput::new(&text, images)?;
        Ok((message, history_text))
    }
}

#[allow(dead_code)]
impl TextInput {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn plain_text(&self) -> Option<&str> {
        self.elements.is_empty().then_some(&self.text)
    }

    pub(crate) fn cursor_byte_index(&self) -> usize {
        self.cursor
    }

    pub(crate) fn cursor_column(&self) -> usize {
        UnicodeWidthStr::width(&self.text[..self.cursor])
    }

    pub(crate) fn viewport(&self, max_width: usize) -> TextInputViewport {
        self.viewport_rows(max_width, usize::MAX)
    }

    pub(crate) fn viewport_rows(&self, max_width: usize, max_rows: usize) -> TextInputViewport {
        if max_width == 0 {
            return TextInputViewport {
                text: String::new(),
                image_placeholders: Vec::new(),
                cursor_column: 0,
                cursor_row: 0,
                visible_rows: 1,
            };
        }

        if self.text.contains('\n') {
            return self.multiline_viewport(max_width, max_rows.max(1));
        }

        let full_cursor_column = self.cursor_column();
        let (start, cursor_column) = if full_cursor_column < max_width {
            (0, full_cursor_column)
        } else {
            self.viewport_start_before_cursor(max_width.saturating_sub(1))
        };
        let end = visible_text_end(&self.text, start, max_width);

        let text = self.text[start..end].to_owned();
        TextInputViewport {
            image_placeholders: self.image_placeholders_in(&text),
            text,
            cursor_column,
            cursor_row: 0,
            visible_rows: 1,
        }
    }

    pub(crate) fn insert_char(&mut self, value: char) {
        let mut encoded = [0_u8; 4];
        self.insert_str(value.encode_utf8(&mut encoded));
    }

    pub(crate) fn insert_str(&mut self, value: &str) {
        self.replace_range_atomic(self.cursor..self.cursor, value);
    }

    pub(crate) fn insert_paste(&mut self, value: &str) {
        let char_count = value.chars().count();
        if char_count < PASTE_PLACEHOLDER_THRESHOLD_CHARS {
            self.insert_str(value);
            return;
        }

        let placeholder = format!("[pasted {char_count} chars]");
        self.replace_range_atomic(self.cursor..self.cursor, &placeholder);
        let end = self.cursor;
        self.elements.push(InputElement {
            range: end - placeholder.len()..end,
            placeholder,
            payload: InputElementPayload::LargePaste {
                content: value.to_owned(),
            },
        });
    }

    pub(crate) fn insert_image(&mut self, image: DraftImage) -> Result<(), RuntimeError> {
        let image_count = self
            .elements
            .iter()
            .filter(|element| matches!(&element.payload, InputElementPayload::Image { .. }))
            .count();
        if image_count >= MAX_USER_IMAGES {
            return Err(RuntimeError::InvalidUserImageInput {
                reason: "one user message may contain at most 20 images".to_owned(),
            });
        }
        let placeholder = user_image_label(image_count + 1)?;
        self.replace_range_atomic(self.cursor..self.cursor, &placeholder);
        let end = self.cursor;
        self.elements.push(InputElement {
            range: end - placeholder.len()..end,
            placeholder,
            payload: InputElementPayload::Image { image },
        });
        Ok(())
    }

    pub(crate) fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub(crate) fn replace_text(&mut self, value: String) {
        self.cursor = value.len();
        self.text = value;
        self.elements.clear();
    }

    pub(crate) fn replace_range(&mut self, range: std::ops::Range<usize>, value: &str) {
        if range.start > range.end
            || range.end > self.text.len()
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
        {
            return;
        }
        self.replace_range_atomic(range, value);
    }

    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let previous = self.text[..self.cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.replace_range_atomic(previous..self.cursor, "");
    }

    pub(crate) fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = next_char_boundary(&self.text, self.cursor);
        self.replace_range_atomic(self.cursor..next, "");
    }

    pub(crate) fn delete_before_cursor(&mut self) {
        self.replace_range_atomic(0..self.cursor, "");
    }

    pub(crate) fn delete_after_cursor(&mut self) {
        self.replace_range_atomic(self.cursor..self.text.len(), "");
    }

    pub(crate) fn delete_previous_word(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let mut start = self.cursor;
        let mut seen_word = false;
        for (index, value) in self.text[..self.cursor].char_indices().rev() {
            if value.is_whitespace() {
                if seen_word {
                    break;
                }
            } else {
                seen_word = true;
            }
            start = index;
        }

        self.replace_range_atomic(start..self.cursor, "");
    }

    pub(crate) fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some(range) = self.atomic_range_for_left() {
            self.cursor = range.start;
            return;
        }
        self.cursor = previous_char_boundary(&self.text, self.cursor);
    }

    pub(crate) fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        if let Some(range) = self.atomic_range_for_right() {
            self.cursor = range.end;
            return;
        }
        self.cursor = next_char_boundary(&self.text, self.cursor);
    }

    pub(crate) fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.elements.clear();
    }

    pub(crate) fn take_trimmed(&mut self) -> Option<String> {
        self.take_submission()
            .ok()
            .flatten()
            .map(|value| value.text)
    }

    pub(crate) fn take_submission(&mut self) -> Result<Option<TuiSubmission>, RuntimeError> {
        let text = self.rendered_text(true);
        if text.trim().is_empty() {
            return Ok(None);
        }
        let history_text = self.rendered_text(false);
        let images = self
            .elements
            .iter()
            .filter_map(|element| match &element.payload {
                InputElementPayload::Image { image } => Some(image),
                InputElementPayload::LargePaste { .. } => None,
            })
            .enumerate()
            .map(|(offset, image)| {
                UserImageInput::png(
                    &user_image_label(offset + 1)?,
                    image.shared_png_bytes(),
                    image.width(),
                    image.height(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let submission = TuiSubmission {
            text,
            history_text,
            images,
        };
        UserMessageInput::new(&submission.text, submission.images.clone())?;
        self.clear();
        Ok(Some(submission))
    }

    fn expanded_text(&self) -> String {
        self.rendered_text(true)
    }

    fn rendered_text(&self, include_images: bool) -> String {
        if self.elements.is_empty() {
            return self.text.clone();
        }

        let mut elements = self.elements.iter().collect::<Vec<_>>();
        elements.sort_by_key(|element| element.range.start);
        let mut rendered = String::with_capacity(self.text.len());
        let mut cursor = 0;
        for element in elements {
            rendered.push_str(&self.text[cursor..element.range.start]);
            match &element.payload {
                InputElementPayload::LargePaste { content } => rendered.push_str(content),
                InputElementPayload::Image { .. } if include_images => {
                    rendered.push_str(&element.placeholder);
                }
                InputElementPayload::Image { .. } => {}
            }
            cursor = element.range.end;
        }
        rendered.push_str(&self.text[cursor..]);
        rendered
    }

    fn atomic_range_for_left(&self) -> Option<Range<usize>> {
        self.elements
            .iter()
            .find(|element| self.cursor > element.range.start && self.cursor <= element.range.end)
            .map(|element| element.range.clone())
    }

    fn atomic_range_for_right(&self) -> Option<Range<usize>> {
        self.elements
            .iter()
            .find(|element| self.cursor >= element.range.start && self.cursor < element.range.end)
            .map(|element| element.range.clone())
    }

    fn replace_range_atomic(&mut self, range: Range<usize>, value: &str) {
        let range = self.expanded_edit_range(range);
        let old_end = range.end;
        let old_len = range.end - range.start;
        let removed_image = self.elements.iter().any(|element| {
            ranges_overlap(&range, &element.range)
                && matches!(&element.payload, InputElementPayload::Image { .. })
        });

        self.text.replace_range(range.clone(), value);
        self.cursor = range.start + value.len();

        let mut retained = Vec::with_capacity(self.elements.len());
        for mut element in self.elements.drain(..) {
            if ranges_overlap(&range, &element.range) {
                continue;
            }
            if element.range.start >= old_end {
                element.range.start = shifted_index(element.range.start, old_len, value.len());
                element.range.end = shifted_index(element.range.end, old_len, value.len());
            }
            retained.push(element);
        }
        self.elements = retained;

        if removed_image {
            self.renumber_images();
        }
    }

    fn expanded_edit_range(&self, mut range: Range<usize>) -> Range<usize> {
        loop {
            let previous = range.clone();
            for element in &self.elements {
                if edit_touches_element(&range, &element.range) {
                    range.start = range.start.min(element.range.start);
                    range.end = range.end.max(element.range.end);
                }
            }
            if range == previous {
                return range;
            }
        }
    }

    fn renumber_images(&mut self) {
        let image_indices = self
            .elements
            .iter()
            .enumerate()
            .filter_map(|(index, element)| {
                matches!(&element.payload, InputElementPayload::Image { .. }).then_some(index)
            })
            .collect::<Vec<_>>();

        for (offset, index) in image_indices.into_iter().enumerate() {
            let desired = user_image_label(offset + 1).expect("positive image label index");
            if self.elements[index].placeholder == desired {
                continue;
            }
            let old_range = self.elements[index].range.clone();
            let old_len = old_range.end - old_range.start;
            self.text.replace_range(old_range.clone(), &desired);
            for (other_index, element) in self.elements.iter_mut().enumerate() {
                if other_index != index && element.range.start >= old_range.end {
                    element.range.start =
                        shifted_index(element.range.start, old_len, desired.len());
                    element.range.end = shifted_index(element.range.end, old_len, desired.len());
                }
            }
            self.elements[index].range = old_range.start..old_range.start + desired.len();
            self.elements[index].placeholder = desired;
        }
    }

    fn image_placeholders_in(&self, visible_text: &str) -> Vec<String> {
        self.elements
            .iter()
            .filter(|element| {
                matches!(&element.payload, InputElementPayload::Image { .. })
                    && visible_text.contains(&element.placeholder)
            })
            .map(|element| element.placeholder.clone())
            .collect()
    }

    #[cfg(test)]
    fn image_elements(&self) -> Vec<&InputElement> {
        self.elements
            .iter()
            .filter(|element| matches!(&element.payload, InputElementPayload::Image { .. }))
            .collect()
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_home();
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_end();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_before_cursor();
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_after_cursor();
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_previous_word();
            }
            KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char(value);
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Home => self.move_home(),
            KeyCode::End => self.move_end(),
            _ => {}
        }
    }

    fn viewport_start_before_cursor(&self, max_width_before_cursor: usize) -> (usize, usize) {
        let mut start = self.cursor;
        let mut width = 0;
        for (index, value) in self.text[..self.cursor].char_indices().rev() {
            let value_width = char_width(value);
            if width + value_width > max_width_before_cursor {
                break;
            }
            width += value_width;
            start = index;
        }
        (start, width)
    }

    fn multiline_viewport(&self, max_width: usize, max_rows: usize) -> TextInputViewport {
        let cursor_line_index = self.text[..self.cursor]
            .bytes()
            .filter(|value| *value == b'\n')
            .count();
        let cursor_line_start = self.text[..self.cursor]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let cursor_line_end = self.text[self.cursor..]
            .find('\n')
            .map(|offset| self.cursor + offset)
            .unwrap_or(self.text.len());
        let cursor_line = &self.text[cursor_line_start..cursor_line_end];
        let cursor_byte_index = self.cursor - cursor_line_start;
        let (cursor_line_text, cursor_column) =
            visible_line_around_cursor(cursor_line, cursor_byte_index, max_width);

        let mut visible_lines = self
            .text
            .split('\n')
            .map(|line| visible_line_prefix(line, max_width))
            .collect::<Vec<_>>();
        visible_lines[cursor_line_index] = cursor_line_text;
        let start = cursor_line_index.saturating_add(1).saturating_sub(max_rows);
        let end = start.saturating_add(max_rows).min(visible_lines.len());
        let visible_lines = visible_lines[start..end].to_vec();

        let text = visible_lines.join("\n");
        TextInputViewport {
            image_placeholders: self.image_placeholders_in(&text),
            text,
            cursor_column,
            cursor_row: cursor_line_index.saturating_sub(start),
            visible_rows: visible_lines.len().max(1),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct InputHistory {
    entries: Vec<String>,
    navigation: Option<usize>,
    draft: TextInput,
}

#[allow(dead_code)]
impl InputHistory {
    pub(crate) fn record(&mut self, text: &str) {
        push_input_history_entry(&mut self.entries, text);
        self.navigation = None;
        self.draft.clear();
    }

    pub(crate) fn replace_entries(&mut self, entries: Vec<String>) {
        self.entries = normalize_input_history(entries);
        self.navigation = None;
        self.draft.clear();
    }

    pub(crate) fn entries(&self) -> &[String] {
        &self.entries
    }

    pub(crate) fn previous(&mut self, input: &mut TextInput) {
        if self.entries.is_empty() {
            return;
        }

        let index = match self.navigation {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft.replace_text(input.rendered_text(false));
                self.entries.len() - 1
            }
        };
        self.navigation = Some(index);
        input.replace_text(self.entries[index].clone());
    }

    pub(crate) fn next(&mut self, input: &mut TextInput) {
        let Some(index) = self.navigation else {
            return;
        };

        if index + 1 < self.entries.len() {
            let index = index + 1;
            self.navigation = Some(index);
            input.replace_text(self.entries[index].clone());
            return;
        }

        self.navigation = None;
        *input = std::mem::take(&mut self.draft);
    }
}

pub(crate) fn normalize_input_history(entries: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::with_capacity(entries.len().min(MAX_INPUT_HISTORY));
    for entry in entries {
        push_input_history_entry(&mut normalized, &entry);
    }
    normalized
}

pub(crate) fn push_input_history_entry(entries: &mut Vec<String>, text: &str) {
    if text.trim().is_empty() || entries.last().is_some_and(|entry| entry == text) {
        return;
    }
    entries.push(text.to_owned());
    let excess = entries.len().saturating_sub(MAX_INPUT_HISTORY);
    if excess > 0 {
        entries.drain(..excess);
    }
}

fn edit_touches_element(edit: &Range<usize>, element: &Range<usize>) -> bool {
    if edit.is_empty() {
        return edit.start > element.start && edit.start < element.end;
    }
    ranges_overlap(edit, element)
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn shifted_index(index: usize, old_len: usize, new_len: usize) -> usize {
    if new_len >= old_len {
        index + (new_len - old_len)
    } else {
        index - (old_len - new_len)
    }
}

fn previous_char_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .char_indices()
        .last()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_char_boundary(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .char_indices()
        .nth(1)
        .map(|(index, _)| cursor + index)
        .unwrap_or(text.len())
}

fn visible_text_end(text: &str, start: usize, max_width: usize) -> usize {
    let mut width = 0;
    let mut end = start;
    for (offset, value) in text[start..].char_indices() {
        let value_width = char_width(value);
        if width + value_width > max_width {
            break;
        }
        width += value_width;
        end = start + offset + value.len_utf8();
    }
    end
}

fn visible_line_prefix(text: &str, max_width: usize) -> String {
    text[..visible_text_end(text, 0, max_width)].to_owned()
}

fn visible_line_around_cursor(
    line: &str,
    cursor_byte_index: usize,
    max_width: usize,
) -> (String, usize) {
    let cursor_column = UnicodeWidthStr::width(&line[..cursor_byte_index]);
    if cursor_column < max_width {
        return (visible_line_prefix(line, max_width), cursor_column);
    }

    let (start, visible_cursor_column) =
        viewport_start_before_text_cursor(&line[..cursor_byte_index], max_width.saturating_sub(1));
    let end = visible_text_end(line, start, max_width);
    (line[start..end].to_owned(), visible_cursor_column)
}

fn viewport_start_before_text_cursor(text: &str, max_width_before_cursor: usize) -> (usize, usize) {
    let mut start = text.len();
    let mut width = 0;
    for (index, value) in text.char_indices().rev() {
        let value_width = char_width(value);
        if width + value_width > max_width_before_cursor {
            break;
        }
        width += value_width;
        start = index;
    }
    (start, width)
}

fn char_width(value: char) -> usize {
    UnicodeWidthChar::width(value).unwrap_or(0)
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
