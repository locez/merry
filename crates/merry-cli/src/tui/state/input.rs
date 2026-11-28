//! Composer text input, completion menu, and input history helpers for [`TuiState`].

use super::{TimelineItem, TuiState};
#[cfg(test)]
use crate::tui::input::TextInput;
use crate::tui::{
    completion::{CompletionMenu, CompletionSources},
    input::{DraftImage, TuiSubmission},
};
use merry::events::SkillMetadata;

impl TuiState {
    #[cfg(test)]
    pub(crate) fn input_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    pub(crate) fn set_completion_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.completion_sources = CompletionSources::new(self.workspace_root.clone(), skills);
        self.refresh_completion_menu();
    }

    #[cfg(test)]
    pub(crate) fn input_text(&self) -> &str {
        self.input.text()
    }

    pub(crate) fn plain_input_text(&self) -> Option<&str> {
        self.input.plain_text()
    }

    pub(crate) fn clear_input(&mut self) {
        self.input.clear();
        self.completion_menu = None;
        self.pending_empty_input_quit = false;
    }

    /// Inserts `$<skill-name> ` at the cursor without expanding `SKILL.md`,
    /// preserving any existing draft text and keeping the completion menu closed.
    pub(crate) fn insert_skill_token(&mut self, name: &str) {
        let cursor = self.input.cursor_byte_index();
        let needs_leading_space = self.input.text()[..cursor]
            .chars()
            .last()
            .is_some_and(|ch| !ch.is_whitespace());
        let token = if needs_leading_space {
            format!(" ${name} ")
        } else {
            format!("${name} ")
        };
        self.input.insert_str(&token);
        self.completion_menu = None;
        self.pending_empty_input_quit = false;
    }

    #[cfg(test)]
    pub(crate) fn has_input_selection(&self) -> bool {
        self.input.has_selection()
    }

    pub(crate) fn selected_input_text(&self) -> Option<String> {
        self.input.selected_text()
    }

    pub(crate) fn clear_input_selection(&mut self) -> bool {
        self.input.clear_selection()
    }

    pub(crate) fn cut_input_selection(&mut self) -> Option<String> {
        let cut = self.input.cut_selection()?;
        self.pending_empty_input_quit = false;
        self.refresh_completion_menu();
        Some(cut)
    }

    pub(crate) fn is_composer_mouse_selecting(&self) -> bool {
        self.composer_mouse_selecting
    }

    pub(crate) fn set_composer_mouse_selecting(&mut self, active: bool) {
        self.composer_mouse_selecting = active;
    }

    pub(crate) fn click_input_at(
        &mut self,
        max_width: usize,
        max_rows: usize,
        visual_row: usize,
        visual_column: usize,
        extend_selection: bool,
    ) {
        self.pending_empty_input_quit = false;
        if let Some(search) = self.history_search.as_mut() {
            search.query_input_mut().move_to_viewport_position(
                max_width,
                max_rows,
                visual_row,
                visual_column,
                extend_selection,
            );
            return;
        }
        self.input.move_to_viewport_position(
            max_width,
            max_rows,
            visual_row,
            visual_column,
            extend_selection,
        );
        self.refresh_completion_menu();
    }

    pub(crate) fn input_viewport_rows(
        &self,
        max_width: usize,
        max_rows: usize,
    ) -> crate::tui::input::TextInputViewport {
        self.input.viewport_rows(max_width, max_rows)
    }

    pub(crate) fn take_input_for_submit(&mut self) -> Option<TuiSubmission> {
        self.completion_menu = None;
        self.pending_empty_input_quit = false;
        match self.input.take_submission() {
            Ok(submission) => submission,
            Err(error) => {
                self.push_timeline_item(TimelineItem::Diagnostic {
                    title: "user_input".to_owned(),
                    body: error.to_string(),
                });
                None
            }
        }
    }

    pub(crate) fn previous_input_history(&mut self) {
        self.pending_empty_input_quit = false;
        self.input_history.previous(&mut self.input);
        self.refresh_completion_menu();
    }

    pub(crate) fn next_input_history(&mut self) {
        self.pending_empty_input_quit = false;
        self.input_history.next(&mut self.input);
        self.refresh_completion_menu();
    }

    pub(crate) fn set_input_history(&mut self, entries: Vec<String>) {
        self.input_history.replace_entries(entries);
    }

    pub(crate) fn record_input_history(&mut self, text: &str) {
        self.input_history.record(text);
    }

    #[cfg(test)]
    pub(crate) fn input_history_entries(&self) -> &[String] {
        self.input_history.entries()
    }

    pub(crate) fn handle_input_key(&mut self, key: crossterm::event::KeyEvent) {
        self.pending_empty_input_quit = false;
        self.input.handle_key(key);
        self.refresh_completion_menu();
    }

    pub(crate) fn set_composer_columns(&mut self, columns: usize) {
        self.composer_columns = columns;
    }

    /// Moves within the draft. False means the caret is already on the first or last visual row.
    pub(crate) fn move_input_line(&mut self, forward: bool) -> bool {
        let width = if self.composer_columns == 0 {
            10_000
        } else {
            self.composer_columns
        };
        let moved = self
            .input
            .move_vertical(if forward { 1 } else { -1 }, width);
        if moved {
            self.pending_empty_input_quit = false;
            self.refresh_completion_menu();
        }
        moved
    }

    /// Moves vertically while starting or extending a composer selection.
    pub(crate) fn select_input_line(&mut self, forward: bool) {
        let width = if self.composer_columns == 0 {
            10_000
        } else {
            self.composer_columns
        };
        let _ = self
            .input
            .move_vertical_with_selection(if forward { 1 } else { -1 }, width, true);
        self.pending_empty_input_quit = false;
        self.refresh_completion_menu();
    }

    #[cfg(test)]
    pub(crate) fn insert_input_str(&mut self, text: &str) {
        self.pending_empty_input_quit = false;
        self.input.insert_str(text);
        self.refresh_completion_menu();
    }

    pub(crate) fn insert_input_paste(&mut self, text: &str) {
        self.pending_empty_input_quit = false;
        self.input.insert_paste(text);
        self.refresh_completion_menu();
    }

    pub(crate) fn insert_input_image(
        &mut self,
        image: DraftImage,
    ) -> Result<(), merry::coding::RuntimeError> {
        self.pending_empty_input_quit = false;
        self.input.insert_image(image)?;
        self.refresh_completion_menu();
        Ok(())
    }

    pub(crate) fn insert_input_newline(&mut self) {
        self.pending_empty_input_quit = false;
        self.input.insert_newline();
        self.close_completion_menu();
    }

    pub(crate) fn completion_menu(&self) -> Option<&CompletionMenu> {
        self.completion_menu.as_ref()
    }

    pub(crate) fn close_completion_menu(&mut self) {
        self.completion_menu = None;
    }

    pub(crate) fn select_next_completion(&mut self) -> bool {
        let Some(menu) = self.completion_menu.as_mut() else {
            return false;
        };
        menu.select_next();
        true
    }

    pub(crate) fn select_previous_completion(&mut self) -> bool {
        let Some(menu) = self.completion_menu.as_mut() else {
            return false;
        };
        menu.select_previous();
        true
    }

    pub(crate) fn select_completion(&mut self, index: usize) -> bool {
        self.completion_menu
            .as_mut()
            .is_some_and(|menu| menu.select_index(index))
    }

    pub(crate) fn accept_completion(&mut self) -> bool {
        let Some(menu) = self.completion_menu.take() else {
            return false;
        };
        let Some(replacement) = menu.replacement_text() else {
            return false;
        };
        self.pending_empty_input_quit = false;
        self.input
            .replace_range(menu.replacement_range(), &replacement);
        if !menu.is_slash() {
            self.refresh_completion_menu();
        }
        true
    }

    pub(super) fn refresh_completion_menu(&mut self) {
        let menu = self.completion_sources.menu_for_input(
            self.input.text(),
            self.input.cursor_byte_index(),
            self.completion_menu.as_ref(),
        );
        self.completion_menu =
            menu.filter(|menu| !menu.is_slash() || self.input.plain_text().is_some());
    }
}
