//! Multi-plan Markdown UI state and fullscreen Plan Review Modal with native
//! `Alt+C` mouse-selection inline commenting.

mod render;
#[cfg(test)]
mod tests;

pub(crate) use render::{
    handle_mouse_scroll, plan_view_pointer, plan_view_timeline_rect, prepare_viewport,
    render_plan_view,
};

use super::state::{TimelineItem, TuiState};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{
    PlanDocument, PlanDocumentStatus, PlanId, PlanModeState, PlanReviewDecision,
    PlanSessionSnapshot,
};

/// Session-scoped UI projection of Merry's runtime-owned Plan Mode and plans.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PlanUiState {
    mode: PlanModeState,
    plans: Vec<PlanDocument>,
    active_plan_id: Option<PlanId>,
    awaiting_approval_plan_id: Option<PlanId>,
}

impl PlanUiState {
    pub(crate) fn mode(&self) -> PlanModeState {
        self.mode
    }

    pub(crate) fn is_plan_mode_on(&self) -> bool {
        matches!(self.mode, PlanModeState::Pending | PlanModeState::Active)
    }

    pub(crate) fn plans(&self) -> &[PlanDocument] {
        &self.plans
    }

    pub(crate) fn has_plans(&self) -> bool {
        !self.plans.is_empty()
    }

    pub(crate) fn is_awaiting_approval(&self) -> bool {
        self.awaiting_approval_plan_id.is_some()
    }

    pub(crate) fn plan_by_id(&self, plan_id: &PlanId) -> Option<&PlanDocument> {
        self.plans.iter().find(|doc| &doc.summary.id == plan_id)
    }

    pub(crate) fn preferred_plan_id(&self) -> Option<PlanId> {
        self.awaiting_approval_plan_id
            .clone()
            .or_else(|| self.active_plan_id.clone())
            .or_else(|| self.plans.last().map(|doc| doc.summary.id.clone()))
    }

    pub(crate) fn hydrate(&mut self, snapshot: PlanSessionSnapshot, plans: Vec<PlanDocument>) {
        self.mode = snapshot.mode;
        self.active_plan_id = snapshot.active_plan_id;
        self.awaiting_approval_plan_id = plans
            .iter()
            .rev()
            .find(|doc| doc.summary.status == PlanDocumentStatus::AwaitingApproval)
            .map(|doc| doc.summary.id.clone());
        self.plans = plans;
    }

    pub(crate) fn set_mode(&mut self, mode: PlanModeState) {
        self.mode = mode;
        if mode == PlanModeState::Inactive {
            self.active_plan_id = None;
            self.awaiting_approval_plan_id = None;
        }
    }

    pub(crate) fn upsert_plan(&mut self, plan: PlanDocument) {
        self.active_plan_id = Some(plan.summary.id.clone());
        if plan.summary.status == PlanDocumentStatus::AwaitingApproval {
            self.awaiting_approval_plan_id = Some(plan.summary.id.clone());
        } else if self.awaiting_approval_plan_id.as_ref() == Some(&plan.summary.id) {
            self.awaiting_approval_plan_id = None;
        }
        if let Some(existing) = self
            .plans
            .iter_mut()
            .find(|doc| doc.summary.id == plan.summary.id)
        {
            *existing = plan;
        } else {
            self.plans.push(plan);
        }
    }

    pub(crate) fn mark_approval_requested(&mut self, plan: PlanDocument) {
        let id = plan.summary.id.clone();
        self.upsert_plan(plan);
        self.awaiting_approval_plan_id = Some(id);
    }

    pub(crate) fn apply_review(&mut self, plan_id: &PlanId, decision: &PlanReviewDecision) {
        if let Some(doc) = self.plans.iter_mut().find(|d| &d.summary.id == plan_id) {
            doc.summary.status = match decision {
                PlanReviewDecision::Approved { .. } => PlanDocumentStatus::Approved,
                PlanReviewDecision::ChangesRequested { .. } => PlanDocumentStatus::Draft,
                PlanReviewDecision::Abandoned => PlanDocumentStatus::Abandoned,
            };
        }
        if self.awaiting_approval_plan_id.as_ref() == Some(plan_id) {
            self.awaiting_approval_plan_id = None;
        }
        if matches!(
            decision,
            PlanReviewDecision::Approved { .. } | PlanReviewDecision::Abandoned
        ) {
            self.active_plan_id = None;
            self.mode = PlanModeState::Inactive;
        }
    }

    pub(crate) fn status_summary(&self) -> String {
        let mode_label = match self.mode {
            PlanModeState::Inactive => "inactive",
            PlanModeState::Pending => "pending",
            PlanModeState::Active => "active (read-only)",
            PlanModeState::ExitPending => "exiting after turn",
        };
        if self.plans.is_empty() {
            mode_label.to_owned()
        } else if self.is_awaiting_approval() {
            format!(
                "{mode_label} · {} plans (awaiting review)",
                self.plans.len()
            )
        } else {
            format!("{mode_label} · {} plans", self.plans.len())
        }
    }
}

/// Action returned by [`PlanViewOverlay::handle_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanViewAction {
    Consumed,
    Close,
    StepPlan(isize),
    CopyText(String),
    Approve {
        plan_id: PlanId,
        note: Option<String>,
    },
    RequestChanges {
        plan_id: PlanId,
        feedback: String,
    },
    Abandon(PlanId),
}

/// Pointer target inside [`PlanViewOverlay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanViewPointer {
    Close,
    SelectPlan(PlanId),
    Approve,
    RequestChanges,
    Abandon,
    ToggleComment,
    CopyPlan,
    Composer,
    TimelineCopy(String),
    TimelineCopyTooLarge,
    TimelineSelect(super::text_interaction::TextSelection),
}

/// Fullscreen rounded Plan Review & History overlay.
///
/// Embeds a child [`TuiState`] rendered via `render_timeline_pane` so plan
/// Markdown natively supports syntax-highlighted code blocks, wrapping,
/// mouse-drag text selection, and Merry's `Alt+C` inline commenting workflow
/// (`[1]`/`[2]` highlight badges + click-to-edit + `format_comments`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanViewOverlay {
    selected_plan_id: PlanId,
    child_state: Box<TuiState>,
    hint: Option<String>,
    rail_scroll: usize,
}

impl PlanViewOverlay {
    pub(crate) fn new(plan: &PlanDocument, parent: &TuiState) -> Self {
        let mut child_state = fresh_plan_child_state(parent, plan);
        // Enable inline comment mode by default when reviewing a plan so users
        // can immediately drag-select any text in the plan to attach a comment.
        if !child_state.is_commenting() {
            child_state.toggle_comment_mode();
        }
        Self {
            selected_plan_id: plan.summary.id.clone(),
            child_state: Box::new(child_state),
            hint: None,
            rail_scroll: 0,
        }
    }

    pub(crate) fn selected_plan_id(&self) -> &PlanId {
        &self.selected_plan_id
    }

    pub(crate) fn child_state(&self) -> &TuiState {
        &self.child_state
    }

    pub(crate) fn child_state_mut(&mut self) -> &mut TuiState {
        &mut self.child_state
    }

    pub(crate) fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    fn set_hint(&mut self, hint: impl Into<String>) {
        self.hint = Some(hint.into());
    }

    pub(crate) fn effective_rail_scroll(
        &self,
        selected_index: usize,
        total_plans: usize,
        visible_rows: usize,
    ) -> usize {
        let visible = visible_rows.max(1);
        let max_scroll = total_plans.saturating_sub(visible);
        let base = self.rail_scroll.min(max_scroll);
        if selected_index < base {
            selected_index
        } else if selected_index >= base.saturating_add(visible) {
            selected_index
                .saturating_add(1)
                .saturating_sub(visible)
                .min(max_scroll)
        } else {
            base
        }
    }

    pub(crate) fn sync_rail_scroll(
        &mut self,
        selected_index: usize,
        total_plans: usize,
        visible_rows: usize,
    ) {
        self.rail_scroll = self.effective_rail_scroll(selected_index, total_plans, visible_rows);
    }

    pub(crate) fn select_plan(&mut self, plan: &PlanDocument, parent: &TuiState) {
        let was_commenting = self.child_state.is_commenting();
        self.selected_plan_id = plan.summary.id.clone();
        let mut fresh = fresh_plan_child_state(parent, plan);
        if was_commenting && !fresh.is_commenting() {
            fresh.toggle_comment_mode();
        }
        *self.child_state = fresh;
        self.hint = None;
    }

    pub(crate) fn refresh_plan(&mut self, plan: &PlanDocument, parent: &TuiState) {
        if self.selected_plan_id == plan.summary.id {
            self.select_plan(plan, parent);
        }
    }

    pub(crate) fn scroll_up(&mut self, lines: usize) {
        self.child_state.scroll_timeline_up_by(lines.max(1));
    }

    pub(crate) fn scroll_down(&mut self, lines: usize) {
        self.child_state.scroll_timeline_down_by(lines.max(1));
    }

    pub(crate) fn insert_paste(&mut self, text: &str) {
        self.hint = None;
        self.child_state.insert_input_paste(text);
    }

    pub(crate) fn toggle_comment_mode(&mut self) {
        self.hint = None;
        self.child_state.toggle_comment_mode();
    }

    /// Collects all pinned inline comments + any composer text as a single
    /// formatted feedback / approval note without clearing state if empty.
    pub(crate) fn take_combined_feedback(&mut self) -> Option<String> {
        if self.child_state.pending_comment_quote().is_some()
            && !self
                .child_state
                .plain_input_text()
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            self.child_state.pin_pending_comment();
        }
        if let Some(submission) = self.child_state.take_comment_submission() {
            return Some(submission.text);
        }
        let plain = self
            .child_state
            .plain_input_text()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if plain.is_empty() {
            None
        } else {
            self.child_state.clear_input();
            Some(plain)
        }
    }

    fn has_draft_feedback(&self) -> bool {
        self.child_state.comment_count() > 0
            || !self
                .child_state
                .plain_input_text()
                .unwrap_or_default()
                .trim()
                .is_empty()
    }

    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
        can_review: bool,
        plan_content: Option<&str>,
    ) -> PlanViewAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let super_key = key.modifiers.contains(KeyModifiers::SUPER);
        let input_empty = self
            .child_state
            .plain_input_text()
            .is_none_or(|t| t.is_empty());
        let has_pending_quote = self.child_state.pending_comment_quote().is_some();

        // Alt+C toggles inline comment capture mode anywhere inside PlanView.
        if matches!(key.code, KeyCode::Char('c' | 'C')) && alt && !ctrl && !super_key {
            self.toggle_comment_mode();
            return PlanViewAction::Consumed;
        }

        // Ctrl+A approves immediately even if the user typed a note or comments.
        if matches!(key.code, KeyCode::Char('a' | 'A')) && ctrl && !alt && !super_key && can_review
        {
            let note = self.take_combined_feedback();
            return PlanViewAction::Approve {
                plan_id: self.selected_plan_id.clone(),
                note,
            };
        }

        // Ctrl+R requests changes immediately with comments / composer text.
        if matches!(key.code, KeyCode::Char('r' | 'R')) && ctrl && !alt && !super_key && can_review
        {
            return self.try_request_changes();
        }

        match key.code {
            KeyCode::Esc => {
                if self.child_state.clear_text_selection()
                    || self.child_state.clear_input_selection()
                {
                    PlanViewAction::Consumed
                } else if has_pending_quote {
                    self.child_state.escape_comment_mode();
                    PlanViewAction::Consumed
                } else {
                    PlanViewAction::Close
                }
            }
            KeyCode::Char('c' | 'C') if (ctrl || super_key) && !alt => {
                if let Some(selected) = self.child_state.selected_input_text() {
                    PlanViewAction::CopyText(selected)
                } else if super_key {
                    PlanViewAction::Consumed
                } else if !input_empty {
                    self.child_state.clear_input();
                    PlanViewAction::Consumed
                } else {
                    PlanViewAction::Close
                }
            }
            KeyCode::Char('x' | 'X') if (ctrl || super_key) && !alt => {
                if let Some(cut) = self.child_state.cut_input_selection() {
                    PlanViewAction::CopyText(cut)
                } else {
                    PlanViewAction::Consumed
                }
            }
            KeyCode::Enter if shift || alt => {
                self.child_state.insert_input_newline();
                PlanViewAction::Consumed
            }
            KeyCode::Enter => {
                if has_pending_quote {
                    if self.child_state.pin_pending_comment() {
                        self.hint = None;
                    }
                    PlanViewAction::Consumed
                } else if can_review && self.has_draft_feedback() {
                    self.try_request_changes()
                } else {
                    PlanViewAction::Consumed
                }
            }
            KeyCode::Backspace if key.modifiers.is_empty() && input_empty => {
                if self.child_state.restore_last_comment() {
                    self.hint = None;
                }
                PlanViewAction::Consumed
            }
            // Quick review & navigation keys when composer is empty and no quote is pending
            KeyCode::Char('a')
                if input_empty
                    && !has_pending_quote
                    && !ctrl
                    && !alt
                    && !super_key
                    && can_review =>
            {
                let note = self.take_combined_feedback();
                PlanViewAction::Approve {
                    plan_id: self.selected_plan_id.clone(),
                    note,
                }
            }
            KeyCode::Char('r')
                if input_empty
                    && !has_pending_quote
                    && !ctrl
                    && !alt
                    && !super_key
                    && can_review =>
            {
                self.try_request_changes()
            }
            KeyCode::Char('q')
                if input_empty
                    && !has_pending_quote
                    && !ctrl
                    && !alt
                    && !super_key
                    && can_review =>
            {
                PlanViewAction::Abandon(self.selected_plan_id.clone())
            }
            KeyCode::Char('y')
                if input_empty && !has_pending_quote && !ctrl && !alt && !super_key =>
            {
                plan_content
                    .map(|text| PlanViewAction::CopyText(text.to_owned()))
                    .unwrap_or(PlanViewAction::Consumed)
            }
            KeyCode::Char('[')
                if input_empty && !has_pending_quote && !ctrl && !alt && !super_key =>
            {
                PlanViewAction::StepPlan(-1)
            }
            KeyCode::Char(']')
                if input_empty && !has_pending_quote && !ctrl && !alt && !super_key =>
            {
                PlanViewAction::StepPlan(1)
            }
            KeyCode::Left if input_empty && !shift && !ctrl && !alt && !super_key => {
                PlanViewAction::StepPlan(-1)
            }
            KeyCode::Right if input_empty && !shift && !ctrl && !alt && !super_key => {
                PlanViewAction::StepPlan(1)
            }
            KeyCode::Up if input_empty && !shift && !ctrl && !alt && !super_key => {
                self.scroll_up(2);
                PlanViewAction::Consumed
            }
            KeyCode::Down if input_empty && !shift && !ctrl && !alt && !super_key => {
                self.scroll_down(2);
                PlanViewAction::Consumed
            }
            KeyCode::Char('k')
                if input_empty && !has_pending_quote && !ctrl && !alt && !super_key =>
            {
                self.scroll_up(2);
                PlanViewAction::Consumed
            }
            KeyCode::Char('j')
                if input_empty && !has_pending_quote && !ctrl && !alt && !super_key =>
            {
                self.scroll_down(2);
                PlanViewAction::Consumed
            }
            KeyCode::PageUp => {
                self.scroll_up(10);
                PlanViewAction::Consumed
            }
            KeyCode::PageDown => {
                self.scroll_down(10);
                PlanViewAction::Consumed
            }
            _ => {
                self.hint = None;
                self.child_state.handle_input_key(key);
                PlanViewAction::Consumed
            }
        }
    }

    pub(crate) fn try_request_changes(&mut self) -> PlanViewAction {
        if let Some(feedback) = self.take_combined_feedback() {
            PlanViewAction::RequestChanges {
                plan_id: self.selected_plan_id.clone(),
                feedback,
            }
        } else {
            self.set_hint(
                "Drag-select text in the plan to comment, or type feedback below and press Enter.",
            );
            PlanViewAction::Consumed
        }
    }
}

fn fresh_plan_child_state(parent: &TuiState, plan: &PlanDocument) -> TuiState {
    let mut child = TuiState::new(
        parent.workspace_root().to_path_buf(),
        parent.model_display_label(),
        parent.keymap().clone(),
        parent.theme().clone(),
    )
    .with_successful_command_output(parent.show_successful_command_output())
    .with_copy_button_position(parent.copy_button_position())
    .with_animations(parent.animations_enabled())
    .with_max_reading_width(parent.max_reading_width());
    child.push_timeline_item(TimelineItem::Assistant {
        text: plan.content.clone(),
    });
    // Start reading from the top of the plan document rather than the bottom.
    child.scroll_timeline_up_by(usize::MAX / 2);
    child
}

pub(crate) fn plan_status_glyph(status: PlanDocumentStatus) -> &'static str {
    match status {
        PlanDocumentStatus::Draft => "✎",
        PlanDocumentStatus::AwaitingApproval => "●",
        PlanDocumentStatus::Approved => "✓",
        PlanDocumentStatus::Abandoned => "⊘",
    }
}

pub(crate) fn plan_status_label(status: PlanDocumentStatus) -> &'static str {
    match status {
        PlanDocumentStatus::Draft => "DRAFT",
        PlanDocumentStatus::AwaitingApproval => "REVIEW",
        PlanDocumentStatus::Approved => "APPROVED",
        PlanDocumentStatus::Abandoned => "ABANDONED",
    }
}
