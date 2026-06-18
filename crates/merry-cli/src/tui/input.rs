use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_INPUT_HISTORY: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TextInputViewport {
    pub(crate) text: String,
    pub(crate) cursor_column: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TextInput {
    text: String,
    cursor: usize,
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
        if max_width == 0 {
            return TextInputViewport {
                text: String::new(),
                cursor_column: 0,
            };
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

    pub(crate) fn replace_text(&mut self, value: String) {
        self.cursor = value.len();
        self.text = value;
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
        self.cursor = previous_char_boundary(&self.text, self.cursor);
    }

    pub(crate) fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
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
    }

    pub(crate) fn take_trimmed(&mut self) -> Option<String> {
        let trimmed = self.text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let value = trimmed.to_owned();
        self.clear();
        Some(value)
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
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct InputHistory {
    entries: Vec<String>,
    navigation: Option<usize>,
    draft: String,
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
                self.draft = input.text().to_owned();
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
        input.replace_text(std::mem::take(&mut self.draft));
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

fn char_width(value: char) -> usize {
    UnicodeWidthChar::width(value).unwrap_or(0)
}
