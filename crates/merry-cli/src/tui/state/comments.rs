//! Comment mode quotes timeline text and sends the notes as one user message.

use super::{TimelineItem, TuiState};
use crate::tui::input::TuiSubmission;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentEntry {
    pub(crate) item_index: usize,
    pub(crate) occurrence: usize,
    pub(crate) quote: String,
    pub(crate) note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingQuote {
    item_index: usize,
    occurrence: usize,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CommentSession {
    active: bool,
    pending: Option<PendingQuote>,
    /// Saved comment whose note is loaded in the composer.
    editing: Option<usize>,
    comments: Vec<CommentEntry>,
}

impl CommentSession {
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn toggle(&mut self) {
        self.active = !self.active;
        if !self.active {
            self.pending = None;
            self.editing = None;
        }
    }

    pub(crate) fn pending_quote(&self) -> Option<&str> {
        self.pending.as_ref().map(|pending| pending.text.as_str())
    }

    pub(crate) fn len(&self) -> usize {
        self.comments.len()
    }

    pub(crate) fn editing(&self) -> Option<usize> {
        self.editing
    }

    fn set_pending(&mut self, quote: String, item_index: usize, occurrence: usize) {
        let quote = quote.trim();
        if quote.is_empty() {
            return;
        }
        self.pending = Some(PendingQuote {
            item_index,
            occurrence,
            text: quote.to_owned(),
        });
        self.editing = None;
        self.active = true;
    }

    pub(crate) fn clear_pending(&mut self) -> bool {
        let cleared = self.pending.take().is_some();
        self.editing = None;
        cleared
    }

    pub(crate) fn pop(&mut self) -> Option<CommentEntry> {
        let comment = self.comments.pop()?;
        self.pending = Some(PendingQuote {
            item_index: comment.item_index,
            occurrence: comment.occurrence,
            text: comment.quote.clone(),
        });
        self.editing = None;
        self.active = true;
        Some(comment)
    }
}

/// One highlighted quote. `comment_index` is absent for a quote that is not saved yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentHighlight {
    pub(crate) comment_index: Option<usize>,
    pub(crate) item_index: usize,
    pub(crate) occurrence: usize,
    pub(crate) quote: String,
    pub(crate) pending: bool,
    pub(crate) number: Option<usize>,
}

/// Renders saved quotes and an optional closing note as one user message.
pub(crate) fn format_comments(comments: &[CommentEntry], trailing: &str) -> String {
    let mut body = String::from("Review comments:");
    for comment in comments {
        body.push_str("\n\n");
        for line in comment.quote.split('\n') {
            body.push_str("> ");
            body.push_str(line);
            body.push('\n');
        }
        body.push('\n');
        body.push_str(&comment.note);
    }
    let trailing = trailing.trim();
    if !trailing.is_empty() {
        body.push_str("\n\n");
        body.push_str(trailing);
    }
    body
}

impl TuiState {
    pub(crate) fn is_commenting(&self) -> bool {
        self.comments.is_active()
    }

    pub(crate) fn toggle_comment_mode(&mut self) {
        self.comments.toggle();
        self.pending_empty_input_quit = false;
    }

    pub(crate) fn comment_count(&self) -> usize {
        self.comments.len()
    }

    pub(crate) fn pending_comment_quote(&self) -> Option<&str> {
        self.comments.pending_quote()
    }

    pub(crate) fn comment_highlights(&self) -> Vec<CommentHighlight> {
        let editing = self.comments.editing();
        let mut marks = Vec::new();
        for (index, comment) in self.comments.comments.iter().enumerate() {
            if editing == Some(index) {
                continue;
            }
            marks.push(CommentHighlight {
                comment_index: Some(index),
                item_index: comment.item_index,
                occurrence: comment.occurrence,
                quote: comment.quote.clone(),
                pending: false,
                number: Some(index.saturating_add(1)),
            });
        }
        if let Some(pending) = &self.comments.pending {
            marks.push(CommentHighlight {
                comment_index: editing,
                item_index: pending.item_index,
                occurrence: pending.occurrence,
                quote: pending.text.clone(),
                pending: true,
                number: editing.map(|index| index.saturating_add(1)),
            });
        }
        marks
    }

    #[cfg(test)]
    pub(crate) fn capture_comment_quote(&mut self, quote: String) {
        self.open_pending(quote, 0, 0);
    }

    pub(crate) fn capture_comment_selection(
        &mut self,
        quote: String,
        item_index: usize,
        occurrence: usize,
    ) {
        self.open_pending(quote, item_index, occurrence);
    }

    /// Drops the current quote, or leaves comment mode when no quote is pending.
    pub(crate) fn escape_comment_mode(&mut self) -> bool {
        if !self.comments.is_active() {
            return false;
        }
        if self.comments.clear_pending() {
            self.clear_input();
            return true;
        }
        self.comments.toggle();
        true
    }

    /// Clears saved comments when the composer itself is empty.
    pub(crate) fn discard_comment_draft(&mut self) -> bool {
        let has_draft = self.comments.pending_quote().is_some() || self.comments.len() > 0;
        if !has_draft || !self.input.text().is_empty() {
            return false;
        }
        self.comments = CommentSession::default();
        true
    }

    pub(crate) fn can_send_comments(&self) -> bool {
        if !self.comments.is_active() {
            return false;
        }
        if self.comments.pending_quote().is_some() {
            return !self.input.note_text().is_empty();
        }
        self.comments.len() > 0
    }

    pub(crate) fn comment_status_text(&self) -> Option<String> {
        if !self.comments.is_active() {
            return None;
        }
        let ready = self.comments.len();
        let text = if self.comments.editing().is_some() {
            "Comment · Enter saves · Esc keeps the old note".to_owned()
        } else if self.comments.pending_quote().is_some() {
            if ready == 0 {
                "Comment · type the note, then Enter".to_owned()
            } else {
                format!("Comment · type the note, then Enter · {ready} saved")
            }
        } else if ready == 0 {
            "Comment · select text to comment on it".to_owned()
        } else if ready == 1 {
            "Comment · 1 saved · click the highlight to edit · Enter sends".to_owned()
        } else {
            format!("Comment · {ready} saved · click a highlight to edit · Enter sends")
        };
        Some(text)
    }

    pub(crate) fn composer_title(&self, width: u16) -> String {
        if let Some(title) = self.history_search_title(width) {
            return title;
        }
        if !self.comments.is_active() {
            return " M ".to_owned();
        }
        if let Some(index) = self.comments.editing() {
            let quote = self.comments.pending_quote().unwrap_or("");
            return self.quote_title(width, &format!("Edit {}", index.saturating_add(1)), quote);
        }
        let Some(quote) = self.comments.pending_quote() else {
            return if self.comments.len() == 0 {
                " Comment · select text ".to_owned()
            } else if self.comments.len() == 1 {
                " 1 comment ".to_owned()
            } else {
                format!(" {} comments ", self.comments.len())
            };
        };
        self.quote_title(width, "Comment", quote)
    }

    /// Saves the pending quote with the composer text and clears the composer.
    pub(crate) fn pin_pending_comment(&mut self) -> bool {
        if self.composer_has_image() {
            self.comment_image_diagnostic();
            return false;
        }
        let Some(pending) = self.comments.pending.clone() else {
            return false;
        };
        let note = self.input.note_text();
        if note.is_empty() {
            return false;
        }
        if let Some(index) = self.comments.editing {
            if let Some(entry) = self.comments.comments.get_mut(index) {
                entry.note = note;
            }
        } else {
            self.comments.comments.push(CommentEntry {
                item_index: pending.item_index,
                occurrence: pending.occurrence,
                quote: pending.text,
                note,
            });
        }
        self.comments.pending = None;
        self.comments.editing = None;
        self.clear_input();
        true
    }

    /// Puts the newest saved comment back into the composer.
    pub(crate) fn restore_last_comment(&mut self) -> bool {
        if self.comments.editing().is_some() || !self.input.text().is_empty() {
            return false;
        }
        let Some(comment) = self.comments.pop() else {
            return false;
        };
        self.input.replace_text(comment.note);
        self.pending_empty_input_quit = false;
        self.refresh_completion_menu();
        true
    }

    /// Loads a saved comment into the composer without removing its highlight.
    pub(crate) fn edit_comment(&mut self, index: usize) -> bool {
        if self.comments.editing == Some(index) && self.comments.pending.is_some() {
            self.comments.active = true;
            return true;
        }
        if self.comments.comments.get(index).is_none() {
            return false;
        }
        if self.comments.pending.is_some() {
            if self.input.note_text().is_empty() {
                self.comments.clear_pending();
                self.clear_input();
            } else if !self.pin_pending_comment() {
                return false;
            }
        }
        let Some(comment) = self.comments.comments.get(index).cloned() else {
            return false;
        };
        self.comments.pending = Some(PendingQuote {
            item_index: comment.item_index,
            occurrence: comment.occurrence,
            text: comment.quote,
        });
        self.comments.editing = Some(index);
        self.comments.active = true;
        self.input.replace_text(comment.note);
        self.pending_empty_input_quit = false;
        self.refresh_completion_menu();
        true
    }

    /// Builds the review message, including a typed note on the current quote.
    pub(crate) fn take_comment_submission(&mut self) -> Option<TuiSubmission> {
        if self.composer_has_image() {
            self.comment_image_diagnostic();
            return None;
        }
        if self.comments.pending.is_some() {
            if self.input.note_text().is_empty() {
                return None;
            }
            if !self.pin_pending_comment() {
                return None;
            }
        }
        let note = self.input.note_text();
        let comments = self.comments.comments.clone();
        if comments.is_empty() {
            return None;
        }
        let text = format_comments(&comments, &note);
        if merry_runtime::UserMessageInput::text_only(&text).is_err() {
            self.push_timeline_item(TimelineItem::Diagnostic {
                title: "Comment".to_owned(),
                body: "The review text could not be sent. Remove control characters other than newline or tab and try again.".to_owned(),
            });
            return None;
        }
        self.comments = CommentSession::default();
        self.clear_input();
        Some(TuiSubmission {
            history_text: text.clone(),
            text,
            images: Vec::new(),
        })
    }

    fn open_pending(&mut self, quote: String, item_index: usize, occurrence: usize) {
        let quote = quote.trim();
        if quote.is_empty() {
            return;
        }
        if self.comments.pending.is_some() {
            if self.input.note_text().is_empty() {
                self.comments.clear_pending();
                self.clear_input();
            } else if !self.pin_pending_comment() {
                return;
            }
        }
        self.comments
            .set_pending(quote.to_owned(), item_index, occurrence);
        self.pending_empty_input_quit = false;
    }

    fn quote_title(&self, width: u16, label: &str, quote: &str) -> String {
        let flat = quote.replace('\n', " ");
        let prefix = format!(" {label} · ");
        let prefix_width = unicode_width::UnicodeWidthStr::width(prefix.as_str());
        let room = usize::from(width)
            .saturating_sub(prefix_width.saturating_add(2))
            .max(1);
        let shown = crate::tui::click_target::truncate_columns(&flat, room);
        format!("{prefix}{shown} ")
    }

    fn composer_has_image(&self) -> bool {
        self.input.contains_image()
    }

    fn comment_image_diagnostic(&mut self) {
        self.push_timeline_item(TimelineItem::Diagnostic {
            title: "Comment".to_owned(),
            body: "A comment is text. Remove the image from the composer before saving it."
                .to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{CommentEntry, format_comments};

    #[test]
    fn format_comments_quotes_each_line_and_keeps_every_note() {
        let comments = vec![
            CommentEntry {
                item_index: 0,
                occurrence: 0,
                quote: "alpha\n  beta".to_owned(),
                note: "rename this".to_owned(),
            },
            CommentEntry {
                item_index: 0,
                occurrence: 1,
                quote: "omega".to_owned(),
                note: "check the test".to_owned(),
            },
        ];

        assert_eq!(
            format_comments(&comments, " and the docs"),
            "Review comments:\n\n> alpha\n>   beta\n\nrename this\n\n> omega\n\ncheck the test\n\nand the docs"
        );
    }
}
