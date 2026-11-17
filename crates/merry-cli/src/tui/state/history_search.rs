use super::TuiState;

impl TuiState {
    pub(crate) fn history_search_active(&self) -> bool {
        self.history_search.is_some()
    }

    pub(crate) fn history_search_status(&self) -> Option<String> {
        let search = self.history_search.as_ref()?;
        let query = search.query();
        if search.failed() {
            return Some(if query.is_empty() {
                "No history match · Esc cancels".to_owned()
            } else {
                format!("No history match · {query} · Esc cancels")
            });
        }
        Some(if query.is_empty() {
            "History · Ctrl+R older · Enter keeps · Esc cancels".to_owned()
        } else {
            format!("History · {query} · Enter keeps · Esc cancels")
        })
    }

    pub(crate) fn history_search_title(&self, width: u16) -> Option<String> {
        let search = self.history_search.as_ref()?;
        let query = search.query().replace('\n', " ");
        if query.is_empty() {
            return Some(" History ".to_owned());
        }
        let prefix = " History · ";
        let prefix_width = unicode_width::UnicodeWidthStr::width(prefix);
        let room = usize::from(width)
            .saturating_sub(prefix_width.saturating_add(2))
            .max(1);
        let shown = crate::tui::click_target::truncate_columns(&query, room);
        Some(format!("{prefix}{shown} "))
    }

    /// Starts a reverse search, or steps to an older match.
    pub(crate) fn begin_or_older_history_search(&mut self) {
        self.completion_menu = None;
        self.pending_empty_input_quit = false;
        let text = if let Some(search) = self.history_search.take() {
            let mut search = search;
            let text = search.older(self.input_history.entries());
            self.history_search = Some(search);
            text
        } else {
            let (search, text) = crate::tui::input::HistorySearch::start(
                self.input_history.entries(),
                self.input.history_text(),
            );
            self.history_search = Some(search);
            text
        };
        if let Some(text) = text {
            self.input.replace_text(text);
        }
    }

    pub(crate) fn newer_history_search(&mut self) {
        let Some(mut search) = self.history_search.take() else {
            return;
        };
        let text = search.newer(self.input_history.entries());
        self.history_search = Some(search);
        if let Some(text) = text {
            self.input.replace_text(text);
        }
    }

    /// Edits the search query. An empty query backs out of the search.
    pub(crate) fn history_search_backspace(&mut self) {
        let Some(mut search) = self.history_search.take() else {
            return;
        };
        match search.pop_char(self.input_history.entries()) {
            None => self.input.replace_text(search.into_draft()),
            Some(text) => {
                self.history_search = Some(search);
                if let Some(text) = text {
                    self.input.replace_text(text);
                }
            }
        }
    }

    pub(crate) fn history_search_push(&mut self, character: char) {
        let Some(mut search) = self.history_search.take() else {
            return;
        };
        let text = search.push_char(character, self.input_history.entries());
        self.history_search = Some(search);
        if let Some(text) = text {
            self.input.replace_text(text);
        }
    }

    pub(crate) fn accept_history_search(&mut self) {
        let Some(search) = self.history_search.take() else {
            return;
        };
        if let Some(index) = search.matched() {
            self.input_history.adopt_match(index, search.into_draft());
        } else {
            self.input.replace_text(search.into_draft());
        }
        self.refresh_completion_menu();
    }

    pub(crate) fn cancel_history_search(&mut self) {
        if let Some(search) = self.history_search.take() {
            self.input.replace_text(search.into_draft());
            self.refresh_completion_menu();
        }
    }
}
