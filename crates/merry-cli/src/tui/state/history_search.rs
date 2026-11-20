use super::TuiState;
use crossterm::event::{KeyCode, KeyEvent};

impl TuiState {
    pub(crate) fn history_search_active(&self) -> bool {
        self.history_search.is_some()
    }

    pub(crate) fn history_search(&self) -> Option<&crate::tui::input::HistorySearch> {
        self.history_search.as_ref()
    }

    pub(crate) fn history_search_entries(&self) -> &[String] {
        self.input_history.entries()
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
            "History · ↑↓/Ctrl+R select · Enter uses · Esc cancels".to_owned()
        } else {
            format!("History · {query} · ↑↓ select · Enter uses · Esc cancels")
        })
    }

    pub(crate) fn history_search_title(&self, _width: u16) -> Option<String> {
        let search = self.history_search.as_ref()?;
        let total = search.matches().len();
        if total == 0 {
            Some(" Search history · 0 matches ".to_owned())
        } else {
            Some(format!(
                " Search history · {}/{} ",
                search.selected_pos().saturating_add(1),
                total
            ))
        }
    }

    /// Starts a live history search, or steps to the next older match.
    pub(crate) fn begin_or_older_history_search(&mut self) {
        self.completion_menu = None;
        self.pending_empty_input_quit = false;
        if let Some(search) = self.history_search.as_mut() {
            search.older();
        } else {
            let search = crate::tui::input::HistorySearch::start(
                self.input_history.entries(),
                self.input.history_text(),
            );
            self.history_search = Some(search);
        }
    }

    pub(crate) fn newer_history_search(&mut self) {
        if let Some(search) = self.history_search.as_mut() {
            search.newer();
        }
    }

    /// Routes editing keys directly to the history filter input. Pressing plain
    /// `Backspace` on an empty filter closes history search cleanly.
    pub(crate) fn history_search_handle_key(&mut self, key: KeyEvent) {
        let Some(mut search) = self.history_search.take() else {
            return;
        };
        if key.code == KeyCode::Backspace && key.modifiers.is_empty() && search.query().is_empty() {
            self.refresh_completion_menu();
            return;
        }
        search.handle_key(key, self.input_history.entries());
        self.history_search = Some(search);
    }

    pub(crate) fn history_search_paste(&mut self, text: &str) -> bool {
        let Some(mut search) = self.history_search.take() else {
            return false;
        };
        search.insert_paste(text, self.input_history.entries());
        self.history_search = Some(search);
        true
    }

    pub(crate) fn history_search_selected_text(&self) -> Option<String> {
        self.history_search
            .as_ref()
            .and_then(|search| search.query_input().selected_text())
    }

    pub(crate) fn history_search_clear_selection(&mut self) -> bool {
        self.history_search
            .as_mut()
            .is_some_and(|search| search.query_input_mut().clear_selection())
    }

    pub(crate) fn history_search_cut_selection(&mut self) -> Option<String> {
        let mut search = self.history_search.take()?;
        let cut = search.cut_selection(self.input_history.entries());
        self.history_search = Some(search);
        cut
    }

    pub(crate) fn accept_history_search(&mut self) {
        let Some(search) = self.history_search.take() else {
            return;
        };
        if let Some(index) = search.matched()
            && let Some(matched_text) = self.input_history.entries().get(index).cloned()
        {
            self.input_history.adopt_match(index, search.into_draft());
            self.input.replace_text(matched_text);
        }
        self.refresh_completion_menu();
    }

    pub(crate) fn accept_history_search_visible(
        &mut self,
        visible_offset: usize,
        max_rows: usize,
    ) -> bool {
        let Some(search) = self.history_search.as_mut() else {
            return false;
        };
        if !search.select_visible(visible_offset, max_rows) {
            return false;
        }
        self.accept_history_search();
        true
    }

    pub(crate) fn cancel_history_search(&mut self) {
        if self.history_search.take().is_some() {
            self.refresh_completion_menu();
        }
    }
}
