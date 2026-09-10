use crate::tui::state::TuiState;

/// A short-lived request/error notice; a terminal write is not a clipboard acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClipboardFeedback {
    pub(crate) message: String,
    pub(crate) failed: bool,
    expires_at: tokio::time::Instant,
}

impl ClipboardFeedback {
    fn new(message: String, failed: bool) -> Self {
        Self {
            message,
            failed,
            expires_at: tokio::time::Instant::now() + std::time::Duration::from_secs(4),
        }
    }

    fn has_expired(&self) -> bool {
        tokio::time::Instant::now() >= self.expires_at
    }
}

impl TuiState {
    pub(crate) fn clipboard_feedback(&self) -> Option<&ClipboardFeedback> {
        self.clipboard_feedback.as_ref()
    }

    pub(crate) fn show_clipboard_feedback(&mut self, message: String, failed: bool) {
        self.clipboard_feedback = Some(ClipboardFeedback::new(message, failed));
    }

    pub(crate) fn expire_clipboard_feedback(&mut self) {
        if self
            .clipboard_feedback
            .as_ref()
            .is_some_and(ClipboardFeedback::has_expired)
        {
            self.clipboard_feedback = None;
        }
    }
}
