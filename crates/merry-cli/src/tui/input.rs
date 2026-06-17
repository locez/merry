use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
            _ => {}
        }
    }
}
