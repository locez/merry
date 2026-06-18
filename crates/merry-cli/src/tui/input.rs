use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_INPUT_HISTORY: usize = 200;
const PASTE_PLACEHOLDER_THRESHOLD_CHARS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TextInputViewport {
    pub(crate) text: String,
    pub(crate) cursor_column: usize,
    pub(crate) cursor_row: usize,
    pub(crate) visible_rows: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TextInput {
    text: String,
    cursor: usize,
    paste_blocks: Vec<PasteBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PasteBlock {
    placeholder: String,
    content: String,
}

#[allow(dead_code)]
impl TextInput {
    pub(crate) fn text(&self) -> &str {
        &self.text
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

        TextInputViewport {
            text: self.text[start..end].to_owned(),
            cursor_column,
            cursor_row: 0,
            visible_rows: 1,
        }
    }

    pub(crate) fn insert_char(&mut self, value: char) {
        self.text.insert(self.cursor, value);
        self.cursor += value.len_utf8();
    }

    pub(crate) fn insert_str(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
    }

    pub(crate) fn insert_paste(&mut self, value: &str) {
        let char_count = value.chars().count();
        if char_count < PASTE_PLACEHOLDER_THRESHOLD_CHARS {
            self.insert_str(value);
            return;
        }

        let placeholder = format!("[pasted {char_count} chars]");
        self.insert_str(&placeholder);
        self.paste_blocks.push(PasteBlock {
            placeholder,
            content: value.to_owned(),
        });
    }

    pub(crate) fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub(crate) fn replace_text(&mut self, value: String) {
        self.cursor = value.len();
        self.text = value;
        self.paste_blocks.clear();
    }

    pub(crate) fn replace_range(&mut self, range: std::ops::Range<usize>, value: &str) {
        if range.start > range.end
            || range.end > self.text.len()
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
        {
            return;
        }
        self.text.replace_range(range.clone(), value);
        self.cursor = range.start + value.len();
    }

    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some(range) = self.paste_placeholder_range_for_backspace() {
            self.remove_paste_block_for_range(&range);
            self.text.drain(range.clone());
            self.cursor = range.start;
            return;
        }
        let previous = self.text[..self.cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.text.drain(previous..self.cursor);
        self.cursor = previous;
    }

    pub(crate) fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        if let Some(range) = self.paste_placeholder_range_for_delete() {
            self.remove_paste_block_for_range(&range);
            self.text.drain(range.clone());
            self.cursor = range.start;
            return;
        }
        let next = next_char_boundary(&self.text, self.cursor);
        self.text.drain(self.cursor..next);
    }

    pub(crate) fn delete_before_cursor(&mut self) {
        self.text.drain(..self.cursor);
        self.cursor = 0;
    }

    pub(crate) fn delete_after_cursor(&mut self) {
        self.text.truncate(self.cursor);
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

        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    pub(crate) fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some(range) = self.paste_placeholder_range_for_backspace() {
            self.cursor = range.start;
            return;
        }
        self.cursor = previous_char_boundary(&self.text, self.cursor);
    }

    pub(crate) fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        if let Some(range) = self.paste_placeholder_range_for_delete() {
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
        self.paste_blocks.clear();
    }

    pub(crate) fn take_trimmed(&mut self) -> Option<String> {
        let value = self.expanded_text();
        if value.trim().is_empty() {
            return None;
        }
        self.clear();
        Some(value)
    }

    fn expanded_text(&self) -> String {
        if self.paste_blocks.is_empty() {
            return self.text.clone();
        }

        let mut value = self.text.clone();
        for block in &self.paste_blocks {
            if let Some(start) = value.find(&block.placeholder) {
                let end = start + block.placeholder.len();
                value.replace_range(start..end, &block.content);
            }
        }
        value
    }

    fn paste_placeholder_range_for_backspace(&self) -> Option<std::ops::Range<usize>> {
        self.paste_placeholder_ranges()
            .into_iter()
            .find(|range| self.cursor > range.start && self.cursor <= range.end)
    }

    fn paste_placeholder_range_for_delete(&self) -> Option<std::ops::Range<usize>> {
        self.paste_placeholder_ranges()
            .into_iter()
            .find(|range| self.cursor >= range.start && self.cursor < range.end)
    }

    fn paste_placeholder_ranges(&self) -> Vec<std::ops::Range<usize>> {
        let mut ranges = Vec::new();
        for block in &self.paste_blocks {
            let mut search_start = 0;
            while let Some(offset) = self.text[search_start..].find(&block.placeholder) {
                let start = search_start + offset;
                let end = start + block.placeholder.len();
                ranges.push(start..end);
                search_start = end;
            }
        }
        ranges
    }

    fn remove_paste_block_for_range(&mut self, range: &std::ops::Range<usize>) {
        let Some(placeholder) = self.text.get(range.clone()) else {
            return;
        };
        if let Some(index) = self
            .paste_blocks
            .iter()
            .position(|block| block.placeholder == placeholder)
        {
            self.paste_blocks.remove(index);
        }
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

        TextInputViewport {
            text: visible_lines.join("\n"),
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
        if text.trim().is_empty() {
            return;
        }

        self.entries.push(text.to_owned());
        if self.entries.len() > MAX_INPUT_HISTORY {
            self.entries.remove(0);
        }
        self.navigation = None;
        self.draft.clear();
    }

    pub(crate) fn previous(&mut self, input: &mut TextInput) {
        if self.entries.is_empty() {
            return;
        }

        let index = match self.navigation {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = input.clone();
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
