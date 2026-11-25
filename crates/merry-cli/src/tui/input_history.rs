use crate::tui::input::TextInput;

pub(crate) const MAX_INPUT_HISTORY: usize = 200;

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
                self.draft.replace_text(input.history_text());
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

    /// Remembers where a history search landed so Down can restore the draft.
    pub(crate) fn adopt_match(&mut self, index: usize, draft: String) {
        self.draft.replace_text(draft);
        self.navigation = Some(index);
    }
}

/// Interactive live-filtered history search (`Ctrl+R`).
///
/// The composer temporarily edits `query` while a picker directly above the
/// composer previews all matching history entries newest-first. Accepting a
/// match copies it into the main composer; cancelling leaves the original draft
/// untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistorySearch {
    query: TextInput,
    draft: String,
    /// Indices into `InputHistory::entries()` ordered newest-first.
    matches: Vec<usize>,
    /// Selected position inside `matches` (`0` = newest match).
    selected: usize,
}

impl HistorySearch {
    pub(crate) fn query(&self) -> &str {
        self.query.text()
    }

    pub(crate) fn query_input(&self) -> &TextInput {
        &self.query
    }

    pub(crate) fn query_input_mut(&mut self) -> &mut TextInput {
        &mut self.query
    }

    pub(crate) fn matches(&self) -> &[usize] {
        &self.matches
    }

    pub(crate) fn selected_pos(&self) -> usize {
        self.selected
    }

    pub(crate) fn failed(&self) -> bool {
        self.matches.is_empty()
    }

    pub(crate) fn matched(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    pub(crate) fn into_draft(self) -> String {
        self.draft
    }

    pub(crate) fn start(entries: &[String], draft: String) -> Self {
        let mut search = Self {
            query: TextInput::default(),
            draft,
            matches: Vec::new(),
            selected: 0,
        };
        search.recompute_matches(entries, None);
        search
    }

    pub(crate) fn older(&mut self) {
        if self.selected + 1 < self.matches.len() {
            self.selected += 1;
        }
    }

    pub(crate) fn newer(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(crate) fn handle_key(&mut self, key: crossterm::event::KeyEvent, entries: &[String]) {
        let previous = self.matched();
        self.query.handle_key(key);
        self.recompute_matches(entries, previous);
    }

    pub(crate) fn insert_paste(&mut self, text: &str, entries: &[String]) {
        let previous = self.matched();
        let single_line = crate::text::single_line_paste_collapsed(text);
        self.query.insert_str(&single_line);
        self.recompute_matches(entries, previous);
    }

    pub(crate) fn cut_selection(&mut self, entries: &[String]) -> Option<String> {
        let previous = self.matched();
        let cut = self.query.cut_selection()?;
        self.recompute_matches(entries, previous);
        Some(cut)
    }

    /// Computes the `(start, end)` slice of `self.matches` visible in `max_rows`.
    pub(crate) fn visible_window(&self, max_rows: usize) -> (usize, usize) {
        let max_rows = max_rows.max(1);
        let len = self.matches.len();
        if len <= max_rows {
            return (0, len);
        }
        let half = max_rows / 2;
        let start = self.selected.saturating_sub(half).min(len - max_rows);
        (start, start + max_rows)
    }

    pub(crate) fn select_visible(&mut self, visible_offset: usize, max_rows: usize) -> bool {
        let (start, end) = self.visible_window(max_rows);
        let target = start.saturating_add(visible_offset);
        if target < end {
            self.selected = target;
            true
        } else {
            false
        }
    }

    fn recompute_matches(&mut self, entries: &[String], previous_entry: Option<usize>) {
        let needle = self.query.text().to_lowercase();
        self.matches = entries
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, entry)| needle.is_empty() || entry.to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect();
        self.selected = previous_entry
            .and_then(|idx| self.matches.iter().position(|candidate| *candidate == idx))
            .unwrap_or(0);
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
