use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
