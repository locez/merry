use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_runtime::{
    MAX_USER_IMAGES, RuntimeError, UserImageInput, UserMessageInput, user_image_label,
};
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

#[path = "input_history.rs"]
mod history;
#[path = "input_types.rs"]
mod types;

use crate::tui::input_layout::{
    next_char_boundary, next_word_boundary, previous_char_boundary, previous_word_boundary,
};
pub(crate) use history::{
    HistorySearch, InputHistory, normalize_input_history, push_input_history_entry,
};
pub(crate) use types::{DraftImage, TuiSubmission};

const PASTE_PLACEHOLDER_THRESHOLD_CHARS: usize = 256;
const MAX_UNDO_HISTORY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TextInputViewport {
    pub(crate) text: String,
    pub(crate) image_placeholders: Vec<String>,
    /// Optional byte range inside each visible row that is currently selected.
    pub(crate) row_selections: Vec<Option<Range<usize>>>,
    pub(crate) cursor_column: usize,
    pub(crate) cursor_row: usize,
    pub(crate) visible_rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    InsertChar,
    Delete,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextInputSnapshot {
    text: String,
    cursor: usize,
    elements: Vec<InputElement>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TextInput {
    text: String,
    cursor: usize,
    /// Anchor byte offset for active selection; when `Some(a)` and `a != cursor`,
    /// `a.min(cursor)..a.max(cursor)` is selected.
    selection_anchor: Option<usize>,
    /// Display column retained while moving across shorter visual rows.
    preferred_column: Option<usize>,
    elements: Vec<InputElement>,
    undo_stack: Vec<TextInputSnapshot>,
    redo_stack: Vec<TextInputSnapshot>,
    last_edit_kind: Option<EditKind>,
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

#[allow(dead_code)]
impl TextInput {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn plain_text(&self) -> Option<&str> {
        self.elements.is_empty().then_some(&self.text)
    }

    pub(crate) fn contains_image(&self) -> bool {
        self.elements
            .iter()
            .any(|element| matches!(element.payload, InputElementPayload::Image { .. }))
    }

    /// Composer text for a comment, with large pastes expanded and images omitted.
    pub(crate) fn note_text(&self) -> String {
        self.history_text().trim().to_owned()
    }

    /// Text stored in input history. Large pastes expand; image placeholders stay out.
    pub(crate) fn history_text(&self) -> String {
        self.rendered_text(false)
    }

    pub(crate) fn cursor_byte_index(&self) -> usize {
        self.cursor
    }

    pub(crate) fn cursor_column(&self) -> usize {
        UnicodeWidthStr::width(&self.text[..self.cursor])
    }

    /// Returns the active selected byte range in `self.text`, if non-empty.
    pub(crate) fn selection_range(&self) -> Option<Range<usize>> {
        let anchor = self.selection_anchor?.min(self.text.len());
        let cursor = self.cursor.min(self.text.len());
        if anchor == cursor {
            None
        } else {
            Some(anchor.min(cursor)..anchor.max(cursor))
        }
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    /// Returns the currently selected text (expanding large-paste placeholders).
    pub(crate) fn selected_text(&self) -> Option<String> {
        let range = self.expanded_edit_range(self.selection_range()?);
        Some(self.rendered_slice(range, true))
    }

    /// Selects the entire composer buffer when non-empty.
    pub(crate) fn select_all(&mut self) {
        self.preferred_column = None;
        self.last_edit_kind = None;
        if self.text.is_empty() {
            self.selection_anchor = None;
            self.cursor = 0;
            return;
        }
        self.selection_anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// Clears any active selection without moving the cursor.
    /// Returns `true` when a non-empty selection was cleared.
    pub(crate) fn clear_selection(&mut self) -> bool {
        let had = self.has_selection();
        self.selection_anchor = None;
        had
    }

    /// Copies and deletes the active selection, returning the cut text.
    pub(crate) fn cut_selection(&mut self) -> Option<String> {
        let range = self.selection_range()?;
        let cut = self.rendered_slice(self.expanded_edit_range(range.clone()), true);
        self.record_undo_checkpoint(EditKind::Other);
        self.selection_anchor = None;
        self.replace_range_atomic(range, "");
        Some(cut)
    }

    pub(crate) fn viewport(&self, max_width: usize) -> TextInputViewport {
        self.viewport_rows(max_width, usize::MAX)
    }

    pub(crate) fn viewport_rows(&self, max_width: usize, max_rows: usize) -> TextInputViewport {
        if max_width == 0 {
            return TextInputViewport {
                text: String::new(),
                image_placeholders: Vec::new(),
                row_selections: vec![None],
                cursor_column: 0,
                cursor_row: 0,
                visible_rows: 1,
            };
        }

        let max_rows = max_rows.max(1);
        let atoms = self.atom_ranges();
        let rows = crate::tui::input_layout::visual_rows(&self.text, max_width, &atoms);
        let (row, column) = crate::tui::input_layout::locate_cursor(&self.text, self.cursor, &rows);
        let column = crate::tui::input_layout::display_column(column, max_width);
        let start = row.saturating_add(1).saturating_sub(max_rows);
        let end = start.saturating_add(max_rows).min(rows.len());
        let selection = self.selection_range();
        let mut lines = Vec::with_capacity(end.saturating_sub(start));
        let mut row_selections = Vec::with_capacity(end.saturating_sub(start));
        for visual in &rows[start..end] {
            lines.push(self.text[visual.start..visual.end].to_owned());
            let row_sel = selection.as_ref().and_then(|sel| {
                let s = sel.start.max(visual.start);
                let e = sel.end.min(visual.end);
                (s < e).then_some((s - visual.start)..(e - visual.start))
            });
            row_selections.push(row_sel);
        }
        let text = lines.join("\n");
        TextInputViewport {
            image_placeholders: self.image_placeholders_in(&text),
            row_selections,
            text,
            cursor_column: column,
            cursor_row: row.saturating_sub(start),
            visible_rows: lines.len().max(1),
        }
    }

    /// Positions the cursor at a clicked/dragged visual row and display column
    /// inside the currently visible composer viewport.
    pub(crate) fn move_to_viewport_position(
        &mut self,
        max_width: usize,
        max_rows: usize,
        visual_row: usize,
        visual_column: usize,
        extend_selection: bool,
    ) {
        if max_width == 0 {
            return;
        }
        let max_rows = max_rows.max(1);
        let atoms = self.atom_ranges();
        let rows = crate::tui::input_layout::visual_rows(&self.text, max_width, &atoms);
        if rows.is_empty() {
            return;
        }
        let (cursor_row, _) =
            crate::tui::input_layout::locate_cursor(&self.text, self.cursor, &rows);
        let start = cursor_row.saturating_add(1).saturating_sub(max_rows);
        let end = start.saturating_add(max_rows).min(rows.len());
        let target_row = start.saturating_add(visual_row).min(end.saturating_sub(1));
        let byte_index = crate::tui::input_layout::byte_at_column(
            &self.text,
            &rows[target_row],
            visual_column,
            &atoms,
        );
        self.prepare_move(extend_selection);
        self.preferred_column = None;
        self.last_edit_kind = None;
        self.cursor = byte_index;
    }

    pub(crate) fn insert_char(&mut self, value: char) {
        let mut encoded = [0_u8; 4];
        self.insert_str(value.encode_utf8(&mut encoded));
    }

    pub(crate) fn insert_str(&mut self, value: &str) {
        if let Some(range) = self.selection_range() {
            self.record_undo_checkpoint(EditKind::Other);
            self.selection_anchor = None;
            self.replace_range_atomic(range, value);
            return;
        }
        let is_single_word_char = {
            let mut chars = value.chars();
            matches!((chars.next(), chars.next()), (Some(ch), None) if !ch.is_whitespace())
        };
        if is_single_word_char {
            self.record_undo_checkpoint(EditKind::InsertChar);
        } else {
            self.record_undo_checkpoint(EditKind::Other);
        }
        self.selection_anchor = None;
        self.replace_range_atomic(self.cursor..self.cursor, value);
    }

    pub(crate) fn insert_paste(&mut self, value: &str) {
        let char_count = value.chars().count();
        if char_count < PASTE_PLACEHOLDER_THRESHOLD_CHARS {
            self.insert_str(value);
            return;
        }

        let target = self.selection_range().unwrap_or(self.cursor..self.cursor);
        self.record_undo_checkpoint(EditKind::Other);
        self.selection_anchor = None;
        let placeholder = format!("[pasted {char_count} chars]");
        self.replace_range_atomic(target, &placeholder);
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
        let target = self.selection_range().unwrap_or(self.cursor..self.cursor);
        self.record_undo_checkpoint(EditKind::Other);
        self.selection_anchor = None;
        let placeholder = user_image_label(image_count + 1)?;
        self.replace_range_atomic(target, &placeholder);
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
        self.selection_anchor = None;
        self.preferred_column = None;
        self.text = value;
        self.elements.clear();
        self.last_edit_kind = None;
    }

    pub(crate) fn replace_range(&mut self, range: std::ops::Range<usize>, value: &str) {
        if range.start > range.end
            || range.end > self.text.len()
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
        {
            return;
        }
        self.record_undo_checkpoint(EditKind::Other);
        self.selection_anchor = None;
        self.replace_range_atomic(range, value);
    }

    pub(crate) fn backspace(&mut self) {
        if self.delete_active_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        self.record_undo_checkpoint(EditKind::Delete);
        let previous = previous_char_boundary(&self.text, self.cursor);
        self.replace_range_atomic(previous..self.cursor, "");
    }

    pub(crate) fn delete(&mut self) {
        if self.delete_active_selection() {
            return;
        }
        if self.cursor >= self.text.len() {
            return;
        }
        self.record_undo_checkpoint(EditKind::Delete);
        let next = next_char_boundary(&self.text, self.cursor);
        self.replace_range_atomic(self.cursor..next, "");
    }

    pub(crate) fn delete_before_cursor(&mut self) {
        if self.delete_active_selection() {
            return;
        }
        let start = self.logical_line_start();
        if start == self.cursor {
            return;
        }
        self.record_undo_checkpoint(EditKind::Other);
        self.replace_range_atomic(start..self.cursor, "");
    }

    pub(crate) fn delete_after_cursor(&mut self) {
        if self.delete_active_selection() {
            return;
        }
        let end = self.logical_line_end();
        if end == self.cursor {
            return;
        }
        self.record_undo_checkpoint(EditKind::Other);
        self.replace_range_atomic(self.cursor..end, "");
    }

    pub(crate) fn delete_previous_word(&mut self) {
        if self.delete_active_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let atoms = self.atom_ranges();
        let start = previous_word_boundary(&self.text, self.cursor, &atoms);
        if start == self.cursor {
            return;
        }
        self.record_undo_checkpoint(EditKind::Other);
        self.replace_range_atomic(start..self.cursor, "");
    }

    pub(crate) fn delete_next_word(&mut self) {
        if self.delete_active_selection() {
            return;
        }
        if self.cursor >= self.text.len() {
            return;
        }
        let atoms = self.atom_ranges();
        let end = next_word_boundary(&self.text, self.cursor, &atoms);
        if end == self.cursor {
            return;
        }
        self.record_undo_checkpoint(EditKind::Other);
        self.replace_range_atomic(self.cursor..end, "");
    }

    fn delete_active_selection(&mut self) -> bool {
        let Some(range) = self.selection_range() else {
            return false;
        };
        self.record_undo_checkpoint(EditKind::Other);
        self.selection_anchor = None;
        self.replace_range_atomic(range, "");
        true
    }

    /// Undoes the last composer edit, restoring text, cursor, and atomic elements.
    pub(crate) fn undo(&mut self) -> bool {
        let Some(previous) = self.undo_stack.pop() else {
            return false;
        };
        let current = self.snapshot();
        push_bounded(&mut self.redo_stack, current, MAX_UNDO_HISTORY);
        self.restore_snapshot(previous);
        true
    }

    /// Redoes the most recently undone composer edit.
    pub(crate) fn redo(&mut self) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        let current = self.snapshot();
        push_bounded(&mut self.undo_stack, current, MAX_UNDO_HISTORY);
        self.restore_snapshot(next);
        true
    }

    /// Moves to the previous or next visual row, keeping the preferred column.
    /// Returns false on the first or last row so the caller can recall history.
    pub(crate) fn move_vertical(&mut self, delta: isize, max_width: usize) -> bool {
        self.move_vertical_with_selection(delta, max_width, false)
    }

    /// Moves vertically by visual row, optionally starting or extending a text selection.
    pub(crate) fn move_vertical_with_selection(
        &mut self,
        delta: isize,
        max_width: usize,
        extend_selection: bool,
    ) -> bool {
        let max_width = if max_width == 0 { 10_000 } else { max_width };
        let atoms = self.atom_ranges();
        let rows = crate::tui::input_layout::visual_rows(&self.text, max_width, &atoms);
        let (row, column) = crate::tui::input_layout::locate_cursor(&self.text, self.cursor, &rows);
        let target = match row.checked_add_signed(delta) {
            Some(target) if target < rows.len() => Some(target),
            _ => None,
        };
        let Some(target) = target else {
            if extend_selection {
                self.prepare_move(true);
                self.preferred_column = None;
                self.last_edit_kind = None;
                self.cursor = if delta < 0 { 0 } else { self.text.len() };
                return true;
            }
            return false;
        };
        self.prepare_move(extend_selection);
        self.last_edit_kind = None;
        let preferred = *self.preferred_column.get_or_insert(column);
        self.cursor =
            crate::tui::input_layout::byte_at_column(&self.text, &rows[target], preferred, &atoms);
        true
    }

    pub(crate) fn move_left(&mut self) {
        self.move_left_with_selection(false);
    }

    pub(crate) fn move_left_with_selection(&mut self, extend_selection: bool) {
        self.preferred_column = None;
        self.last_edit_kind = None;
        if !extend_selection && let Some(range) = self.selection_range() {
            self.cursor = range.start;
            self.selection_anchor = None;
            return;
        }
        self.prepare_move(extend_selection);
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
        self.move_right_with_selection(false);
    }

    pub(crate) fn move_right_with_selection(&mut self, extend_selection: bool) {
        self.preferred_column = None;
        self.last_edit_kind = None;
        if !extend_selection && let Some(range) = self.selection_range() {
            self.cursor = range.end;
            self.selection_anchor = None;
            return;
        }
        self.prepare_move(extend_selection);
        if self.cursor >= self.text.len() {
            return;
        }
        if let Some(range) = self.atomic_range_for_right() {
            self.cursor = range.end;
            return;
        }
        self.cursor = next_char_boundary(&self.text, self.cursor);
    }

    pub(crate) fn move_word_left(&mut self, extend_selection: bool) {
        self.preferred_column = None;
        self.last_edit_kind = None;
        self.prepare_move(extend_selection);
        if self.cursor == 0 {
            return;
        }
        let atoms = self.atom_ranges();
        self.cursor = previous_word_boundary(&self.text, self.cursor, &atoms);
    }

    pub(crate) fn move_word_right(&mut self, extend_selection: bool) {
        self.preferred_column = None;
        self.last_edit_kind = None;
        self.prepare_move(extend_selection);
        if self.cursor >= self.text.len() {
            return;
        }
        let atoms = self.atom_ranges();
        self.cursor = next_word_boundary(&self.text, self.cursor, &atoms);
    }

    pub(crate) fn move_home(&mut self) {
        self.move_home_with_selection(false);
    }

    pub(crate) fn move_home_with_selection(&mut self, extend_selection: bool) {
        self.preferred_column = None;
        self.last_edit_kind = None;
        self.prepare_move(extend_selection);
        self.cursor = self.logical_line_start();
    }

    pub(crate) fn move_end(&mut self) {
        self.move_end_with_selection(false);
    }

    pub(crate) fn move_end_with_selection(&mut self, extend_selection: bool) {
        self.preferred_column = None;
        self.last_edit_kind = None;
        self.prepare_move(extend_selection);
        self.cursor = self.logical_line_end();
    }

    pub(crate) fn move_buffer_start(&mut self, extend_selection: bool) {
        self.preferred_column = None;
        self.last_edit_kind = None;
        self.prepare_move(extend_selection);
        self.cursor = 0;
    }

    pub(crate) fn move_buffer_end(&mut self, extend_selection: bool) {
        self.preferred_column = None;
        self.last_edit_kind = None;
        self.prepare_move(extend_selection);
        self.cursor = self.text.len();
    }

    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.selection_anchor = None;
        self.preferred_column = None;
        self.elements.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
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

    fn prepare_move(&mut self, extend_selection: bool) {
        if extend_selection {
            self.selection_anchor.get_or_insert(self.cursor);
        } else {
            self.selection_anchor = None;
        }
    }

    fn snapshot(&self) -> TextInputSnapshot {
        TextInputSnapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            elements: self.elements.clone(),
        }
    }

    fn restore_snapshot(&mut self, snapshot: TextInputSnapshot) {
        self.text = snapshot.text;
        self.cursor = snapshot.cursor.min(self.text.len());
        self.elements = snapshot.elements;
        self.selection_anchor = None;
        self.preferred_column = None;
        self.last_edit_kind = None;
    }

    fn record_undo_checkpoint(&mut self, kind: EditKind) {
        let coalesce = kind == EditKind::InsertChar
            && self.last_edit_kind == Some(EditKind::InsertChar)
            && !self.undo_stack.is_empty();
        if !coalesce {
            let snapshot = self.snapshot();
            push_bounded(&mut self.undo_stack, snapshot, MAX_UNDO_HISTORY);
        }
        self.redo_stack.clear();
        self.last_edit_kind = Some(kind);
    }

    fn expanded_text(&self) -> String {
        self.rendered_text(true)
    }

    fn rendered_text(&self, include_images: bool) -> String {
        self.rendered_slice(0..self.text.len(), include_images)
    }

    fn rendered_slice(&self, range: Range<usize>, include_images: bool) -> String {
        if self.elements.is_empty() {
            return self.text[range].to_owned();
        }

        let mut elements = self
            .elements
            .iter()
            .filter(|element| ranges_overlap(&range, &element.range))
            .collect::<Vec<_>>();
        elements.sort_by_key(|element| element.range.start);
        let mut rendered = String::with_capacity(range.len());
        let mut cursor = range.start;
        for element in elements {
            let elem_start = element.range.start.max(range.start);
            let elem_end = element.range.end.min(range.end);
            if elem_start > cursor {
                rendered.push_str(&self.text[cursor..elem_start]);
            }
            match &element.payload {
                InputElementPayload::LargePaste { content } => rendered.push_str(content),
                InputElementPayload::Image { .. } if include_images => {
                    rendered.push_str(&element.placeholder);
                }
                InputElementPayload::Image { .. } => {}
            }
            cursor = elem_end;
        }
        if cursor < range.end {
            rendered.push_str(&self.text[cursor..range.end]);
        }
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
        self.preferred_column = None;
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let super_key = key.modifiers.contains(KeyModifiers::SUPER);
        let cmd_or_ctrl = ctrl || super_key;

        match key.code {
            KeyCode::Char('a' | 'A') if cmd_or_ctrl && !alt => {
                self.select_all();
            }
            KeyCode::Char('z') if cmd_or_ctrl && !alt && !shift => {
                let _ = self.undo();
            }
            KeyCode::Char('z' | 'Z') if cmd_or_ctrl && !alt && shift => {
                let _ = self.redo();
            }
            KeyCode::Char('y' | 'Y') if cmd_or_ctrl && !alt => {
                let _ = self.redo();
            }
            KeyCode::Char('e' | 'E') if ctrl && !alt && !super_key => {
                self.move_end_with_selection(shift);
            }
            KeyCode::Char('b' | 'B') if alt && !ctrl && !super_key => {
                self.move_word_left(shift);
            }
            KeyCode::Char('f' | 'F') if alt && !ctrl && !super_key => {
                self.move_word_right(shift);
            }
            KeyCode::Char('b') if ctrl && !alt && !super_key => {
                self.move_left_with_selection(shift);
            }
            KeyCode::Char('f') if ctrl && !alt && !super_key => {
                self.move_right_with_selection(shift);
            }
            KeyCode::Char('d' | 'D') if alt && !ctrl && !super_key => {
                self.delete_next_word();
            }
            KeyCode::Char('d') if ctrl && !alt && !super_key => {
                self.delete();
            }
            KeyCode::Char('u') if ctrl && !alt && !super_key => {
                self.delete_before_cursor();
            }
            KeyCode::Char('k') if ctrl && !alt && !super_key => {
                self.delete_after_cursor();
            }
            KeyCode::Char('w') if ctrl && !alt && !super_key => {
                self.delete_previous_word();
            }
            KeyCode::Char(value)
                if !ctrl
                    && !super_key
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::HYPER | KeyModifiers::META)
                    && !(alt && value.is_ascii()) =>
            {
                self.insert_char(value);
            }
            KeyCode::Backspace if super_key => self.delete_before_cursor(),
            KeyCode::Delete if super_key => self.delete_after_cursor(),
            KeyCode::Backspace if ctrl || alt => self.delete_previous_word(),
            KeyCode::Delete if ctrl || alt => self.delete_next_word(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left if super_key && !ctrl && !alt => self.move_home_with_selection(shift),
            KeyCode::Right if super_key && !ctrl && !alt => self.move_end_with_selection(shift),
            KeyCode::Up if super_key && !ctrl && !alt => self.move_buffer_start(shift),
            KeyCode::Down if super_key && !ctrl && !alt => self.move_buffer_end(shift),
            KeyCode::Left if ctrl || alt => self.move_word_left(shift),
            KeyCode::Right if ctrl || alt => self.move_word_right(shift),
            KeyCode::Left => self.move_left_with_selection(shift),
            KeyCode::Right => self.move_right_with_selection(shift),
            KeyCode::Home if ctrl && !alt => self.move_buffer_start(shift),
            KeyCode::End if ctrl && !alt => self.move_buffer_end(shift),
            KeyCode::Home => self.move_home_with_selection(shift),
            KeyCode::End => self.move_end_with_selection(shift),
            _ => {}
        }
    }

    fn logical_line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    fn logical_line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map(|offset| self.cursor + offset)
            .unwrap_or(self.text.len())
    }

    fn atom_ranges(&self) -> Vec<Range<usize>> {
        self.elements
            .iter()
            .map(|element| element.range.clone())
            .collect()
    }
}

fn push_bounded<T>(stack: &mut Vec<T>, item: T, limit: usize) {
    if stack.len() >= limit {
        stack.remove(0);
    }
    stack.push(item);
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

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
