//! Pure Plan Mode state machine, multi-plan registry, and dynamic reminders.

use merry_core::{
    PlanDocument, PlanDocumentStatus, PlanDocumentSummary, PlanId, PlanModeState,
    PlanReviewDecision, PlanSessionSnapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

const MAX_PLAN_TITLE_BYTES: usize = 200;
const MAX_PLAN_CONTENT_BYTES: usize = 256 * 1024;
const MAX_PLANS_PER_SESSION: usize = 64;
const MAX_SLUG_LEN: usize = 48;

/// Typed errors returned by Plan Mode state transitions and document authoring.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    /// Plan title is blank, too long, or contains control characters.
    #[error("plan title is invalid: {reason}")]
    InvalidTitle { reason: &'static str },
    /// Plan Markdown body exceeds the bounded size limit.
    #[error("plan content exceeds maximum size of {maximum} bytes (got {actual})")]
    ContentTooLarge { actual: usize, maximum: usize },
    /// Plan feedback is blank when requesting revisions.
    #[error("plan revision feedback must not be blank")]
    EmptyFeedback,
    /// Referenced plan id does not exist in this session.
    #[error("unknown plan id: {plan_id}")]
    UnknownPlan { plan_id: PlanId },
    /// Attempted to overwrite a plan that is already approved or abandoned.
    #[error("plan {plan_id} is {status:?} and cannot be edited; create a new plan instead")]
    PlanNotEditable {
        plan_id: PlanId,
        status: PlanDocumentStatus,
    },
    /// Session reached the bounded maximum number of stored plans.
    #[error("session already has the maximum of {maximum} plans")]
    TooManyPlans { maximum: usize },
    /// Sanitized plan file name failed safety validation.
    #[error("plan file name {file_name:?} is invalid")]
    InvalidFileName { file_name: String },
}

/// Durable Plan Mode state persisted in `journal.jsonl`.
///
/// Markdown bodies live in `<session_dir>/plans/<file_name>` on disk when a
/// `FileSessionStore` is attached, and are cached in memory on
/// [`PlanSessionState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedPlanModeState {
    pub(crate) mode: PlanModeState,
    #[serde(default)]
    pub(crate) was_previously_active: bool,
    #[serde(default)]
    pub(crate) reminder_count: u32,
    #[serde(default)]
    pub(crate) pending_exit_reminder: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_plan_id: Option<PlanId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) awaiting_approval_plan_id: Option<PlanId>,
    #[serde(default = "default_next_plan_sequence")]
    pub(crate) next_plan_sequence: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) plans: Vec<PlanDocumentSummary>,
}

const fn default_next_plan_sequence() -> u32 {
    1
}

impl Default for PersistedPlanModeState {
    fn default() -> Self {
        Self {
            mode: PlanModeState::Inactive,
            was_previously_active: false,
            reminder_count: 0,
            pending_exit_reminder: false,
            active_plan_id: None,
            awaiting_approval_plan_id: None,
            next_plan_sequence: 1,
            plans: Vec::new(),
        }
    }
}

impl PersistedPlanModeState {
    /// Returns whether this state carries any non-default Plan Mode data.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.mode == PlanModeState::Inactive
            && !self.was_previously_active
            && self.reminder_count == 0
            && !self.pending_exit_reminder
            && self.active_plan_id.is_none()
            && self.awaiting_approval_plan_id.is_none()
            && self.plans.is_empty()
    }
}

/// Runtime-owned Plan Mode state machine and multi-plan document store for one session.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlanSessionState {
    persisted: PersistedPlanModeState,
    contents: BTreeMap<PlanId, String>,
}

impl PlanSessionState {
    /// Reconstructs Plan Mode state from persisted journal data and on-disk plan bodies.
    ///
    /// Transient states (`Pending`, `ExitPending`) collapse cleanly on restart:
    /// `Pending -> Inactive`, `ExitPending -> Inactive` with a one-shot exit reminder.
    #[must_use]
    pub(crate) fn from_persisted(
        mut persisted: PersistedPlanModeState,
        contents: BTreeMap<PlanId, String>,
    ) -> Self {
        match persisted.mode {
            PlanModeState::Pending => {
                persisted.mode = PlanModeState::Inactive;
            }
            PlanModeState::ExitPending => {
                persisted.mode = PlanModeState::Inactive;
                persisted.pending_exit_reminder = true;
            }
            PlanModeState::Inactive | PlanModeState::Active => {}
        }
        if persisted.next_plan_sequence == 0 {
            persisted.next_plan_sequence =
                u32::try_from(persisted.plans.len()).unwrap_or(u32::MAX - 1) + 1;
        }
        Self {
            persisted,
            contents,
        }
    }

    /// Populates in-memory plan bodies loaded from `<session_dir>/plans/`.
    pub(crate) fn set_contents(&mut self, contents: BTreeMap<PlanId, String>) {
        self.contents = contents;
    }

    /// Returns the serializable persistence snapshot for `journal.jsonl`.
    #[must_use]
    pub(crate) fn persisted(&self) -> Option<PersistedPlanModeState> {
        (!self.persisted.is_empty()).then(|| self.persisted.clone())
    }

    /// Returns the current Plan Mode state.
    #[must_use]
    pub(crate) const fn mode(&self) -> PlanModeState {
        self.persisted.mode
    }

    /// Returns whether Plan Mode's read-only edit gate is currently active.
    #[must_use]
    pub(crate) const fn is_active(&self) -> bool {
        self.persisted.mode.is_active()
    }

    /// Returns the plan id currently parked for user review, if any.
    #[must_use]
    pub(crate) fn awaiting_approval_plan_id(&self) -> Option<&PlanId> {
        self.persisted.awaiting_approval_plan_id.as_ref()
    }

    /// Returns a compact session snapshot for API/TUI projection.
    #[must_use]
    pub(crate) fn snapshot(&self) -> PlanSessionSnapshot {
        PlanSessionSnapshot {
            mode: self.persisted.mode,
            active_plan_id: self.persisted.active_plan_id.clone(),
            awaiting_approval_plan_id: self.persisted.awaiting_approval_plan_id.clone(),
            plans: self.persisted.plans.clone(),
        }
    }

    /// Returns all persisted plan summaries in chronological order.
    #[must_use]
    pub(crate) fn summaries(&self) -> &[PlanDocumentSummary] {
        &self.persisted.plans
    }

    /// Returns all plan documents with their cached Markdown bodies.
    #[must_use]
    pub(crate) fn documents(&self) -> Vec<PlanDocument> {
        self.persisted
            .plans
            .iter()
            .map(|summary| PlanDocument {
                summary: summary.clone(),
                content: self.contents.get(&summary.id).cloned().unwrap_or_default(),
            })
            .collect()
    }

    /// Looks up one complete plan document by id.
    #[must_use]
    pub(crate) fn document(&self, plan_id: &PlanId) -> Option<PlanDocument> {
        self.persisted
            .plans
            .iter()
            .find(|plan| &plan.id == plan_id)
            .map(|summary| PlanDocument {
                summary: summary.clone(),
                content: self.contents.get(&summary.id).cloned().unwrap_or_default(),
            })
    }

    /// Returns the active plan or the most recently created plan.
    #[must_use]
    pub(crate) fn active_or_latest_document(&self) -> Option<PlanDocument> {
        if let Some(id) = self.persisted.active_plan_id.as_ref()
            && let Some(doc) = self.document(id)
        {
            return Some(doc);
        }
        self.persisted
            .plans
            .last()
            .and_then(|summary| self.document(&summary.id))
    }

    /// User-initiated toggle of Plan Mode. Returns `Some(new_mode)` when the state changed.
    pub(crate) fn set_mode_from_user(
        &mut self,
        enabled: bool,
        turn_in_flight: bool,
    ) -> Option<PlanModeState> {
        let next = if enabled {
            match self.persisted.mode {
                PlanModeState::Inactive => {
                    self.persisted.pending_exit_reminder = false;
                    if turn_in_flight {
                        self.persisted.was_previously_active = true;
                        self.persisted.reminder_count = 0;
                        PlanModeState::Active
                    } else {
                        PlanModeState::Pending
                    }
                }
                PlanModeState::ExitPending => {
                    self.persisted.pending_exit_reminder = false;
                    PlanModeState::Active
                }
                PlanModeState::Pending | PlanModeState::Active => return None,
            }
        } else {
            self.persisted.awaiting_approval_plan_id = None;
            match self.persisted.mode {
                PlanModeState::Pending => PlanModeState::Inactive,
                PlanModeState::Active => {
                    if turn_in_flight {
                        PlanModeState::ExitPending
                    } else {
                        self.persisted.pending_exit_reminder = true;
                        PlanModeState::Inactive
                    }
                }
                PlanModeState::Inactive | PlanModeState::ExitPending => return None,
            }
        };
        self.persisted.mode = next;
        Some(next)
    }

    /// Activates Plan Mode when the first user prompt arrives in `Pending` state.
    pub(crate) fn activate_on_user_prompt(&mut self) -> Option<PlanModeState> {
        if self.persisted.mode != PlanModeState::Pending {
            return None;
        }
        self.persisted.mode = PlanModeState::Active;
        self.persisted.was_previously_active = true;
        self.persisted.reminder_count = 0;
        self.persisted.pending_exit_reminder = false;
        Some(PlanModeState::Active)
    }

    /// Completes a deferred `ExitPending -> Inactive` transition when the turn settles.
    pub(crate) fn complete_deferred_exit(&mut self) -> Option<PlanModeState> {
        if self.persisted.mode != PlanModeState::ExitPending {
            return None;
        }
        self.persisted.mode = PlanModeState::Inactive;
        self.persisted.pending_exit_reminder = true;
        Some(PlanModeState::Inactive)
    }

    /// Agent-initiated entry via `enter_plan_mode`. Returns `Some(Active)` if state changed.
    pub(crate) fn activate_from_tool(&mut self) -> Option<PlanModeState> {
        if self.persisted.mode == PlanModeState::Active {
            return None;
        }
        self.persisted.mode = PlanModeState::Active;
        self.persisted.was_previously_active = true;
        self.persisted.reminder_count = 0;
        self.persisted.pending_exit_reminder = false;
        Some(PlanModeState::Active)
    }

    /// Resets reminder alternation after compaction so the model receives the full reminder.
    pub(crate) fn reset_after_compaction(&mut self) {
        if self.persisted.mode == PlanModeState::Active {
            self.persisted.reminder_count = 0;
        }
    }

    /// Advances reminder tracking once a model turn consumes the dynamic reminder.
    pub(crate) fn record_prompt_delivered(&mut self) {
        if self.persisted.mode == PlanModeState::Active {
            self.persisted.reminder_count = self.persisted.reminder_count.saturating_add(1);
        } else if self.persisted.mode == PlanModeState::Inactive
            && self.persisted.pending_exit_reminder
        {
            self.persisted.pending_exit_reminder = false;
        }
    }

    /// Creates a new numbered plan or updates an existing editable plan.
    pub(crate) fn save_plan(
        &mut self,
        title: &str,
        content: &str,
        requested_plan_id: Option<&PlanId>,
        create_new: bool,
        now_ms: u64,
    ) -> Result<PlanDocument, PlanError> {
        let title = validate_title(title)?;
        if content.len() > MAX_PLAN_CONTENT_BYTES {
            return Err(PlanError::ContentTooLarge {
                actual: content.len(),
                maximum: MAX_PLAN_CONTENT_BYTES,
            });
        }

        let target_index = if create_new {
            None
        } else if let Some(plan_id) = requested_plan_id {
            let (idx, existing) = self
                .persisted
                .plans
                .iter()
                .enumerate()
                .find(|(_, plan)| &plan.id == plan_id)
                .ok_or_else(|| PlanError::UnknownPlan {
                    plan_id: plan_id.clone(),
                })?;
            if !matches!(
                existing.status,
                PlanDocumentStatus::Draft | PlanDocumentStatus::AwaitingApproval
            ) {
                return Err(PlanError::PlanNotEditable {
                    plan_id: existing.id.clone(),
                    status: existing.status,
                });
            }
            Some(idx)
        } else {
            self.persisted
                .active_plan_id
                .as_ref()
                .and_then(|active_id| {
                    self.persisted.plans.iter().position(|plan| {
                        &plan.id == active_id
                            && matches!(
                                plan.status,
                                PlanDocumentStatus::Draft | PlanDocumentStatus::AwaitingApproval
                            )
                    })
                })
        };

        if let Some(summary) = target_index.and_then(|index| self.persisted.plans.get_mut(index)) {
            summary.title = title;
            summary.status = PlanDocumentStatus::Draft;
            summary.revision = summary.revision.saturating_add(1);
            summary.updated_at_ms = now_ms;
            let plan_id = summary.id.clone();
            let summary_clone = summary.clone();
            self.persisted.active_plan_id = Some(plan_id.clone());
            self.contents.insert(plan_id, content.to_owned());
            return Ok(PlanDocument {
                summary: summary_clone,
                content: content.to_owned(),
            });
        }

        if self.persisted.plans.len() >= MAX_PLANS_PER_SESSION {
            return Err(PlanError::TooManyPlans {
                maximum: MAX_PLANS_PER_SESSION,
            });
        }
        let seq = self.persisted.next_plan_sequence;
        self.persisted.next_plan_sequence = seq.saturating_add(1);
        // `plan-{u32}` is guaranteed non-blank, ASCII, and within identifier length bounds.
        let plan_id = PlanId::new(&format!("plan-{seq}")).expect("generated plan id is valid");
        let file_name = format!("{seq:03}-{}.md", slugify_title(&title));
        validate_plan_file_name(&file_name)?;
        let summary = PlanDocumentSummary {
            id: plan_id.clone(),
            title,
            file_name,
            status: PlanDocumentStatus::Draft,
            revision: 1,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        self.persisted.plans.push(summary.clone());
        self.persisted.active_plan_id = Some(plan_id.clone());
        self.contents.insert(plan_id, content.to_owned());
        Ok(PlanDocument {
            summary,
            content: content.to_owned(),
        })
    }

    /// Marks the active or requested plan as awaiting user approval.
    ///
    /// If no plan has been authored yet in this cycle, seeds an empty untitled
    /// plan document so the review UI still opens cleanly with an empty-state notice.
    pub(crate) fn request_approval(
        &mut self,
        requested_plan_id: Option<&PlanId>,
        now_ms: u64,
    ) -> Result<PlanDocument, PlanError> {
        let target_id = if let Some(plan_id) = requested_plan_id {
            plan_id.clone()
        } else if let Some(active_id) = self.persisted.active_plan_id.clone() {
            active_id
        } else {
            let seeded = self.save_plan("Untitled Plan", "", None, true, now_ms)?;
            seeded.summary.id
        };

        let summary = self
            .persisted
            .plans
            .iter_mut()
            .find(|plan| plan.id == target_id)
            .ok_or_else(|| PlanError::UnknownPlan {
                plan_id: target_id.clone(),
            })?;
        summary.status = PlanDocumentStatus::AwaitingApproval;
        summary.updated_at_ms = now_ms;
        let summary_clone = summary.clone();
        self.persisted.active_plan_id = Some(target_id.clone());
        self.persisted.awaiting_approval_plan_id = Some(target_id.clone());
        let content = self.contents.get(&target_id).cloned().unwrap_or_default();
        Ok(PlanDocument {
            summary: summary_clone,
            content,
        })
    }

    /// Approves a plan, exits Plan Mode, and clears `active_plan_id` so the next
    /// planning session starts a fresh numbered plan.
    pub(crate) fn approve_plan(
        &mut self,
        plan_id: &PlanId,
        note: Option<String>,
        now_ms: u64,
    ) -> Result<(PlanDocument, PlanReviewDecision), PlanError> {
        let summary = self
            .persisted
            .plans
            .iter_mut()
            .find(|plan| &plan.id == plan_id)
            .ok_or_else(|| PlanError::UnknownPlan {
                plan_id: plan_id.clone(),
            })?;
        summary.status = PlanDocumentStatus::Approved;
        summary.updated_at_ms = now_ms;
        let summary_clone = summary.clone();
        self.persisted.mode = PlanModeState::Inactive;
        self.persisted.reminder_count = 0;
        self.persisted.pending_exit_reminder = false;
        self.persisted.awaiting_approval_plan_id = None;
        self.persisted.active_plan_id = None;
        let content = self.contents.get(plan_id).cloned().unwrap_or_default();
        let note = note.and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        });
        Ok((
            PlanDocument {
                summary: summary_clone,
                content,
            },
            PlanReviewDecision::Approved { note },
        ))
    }

    /// Returns a plan to `Draft`, keeps Plan Mode active, and clears the awaiting gate.
    pub(crate) fn request_changes(
        &mut self,
        plan_id: &PlanId,
        feedback: String,
        now_ms: u64,
    ) -> Result<(PlanDocument, PlanReviewDecision), PlanError> {
        let feedback = feedback.trim().to_owned();
        if feedback.is_empty() {
            return Err(PlanError::EmptyFeedback);
        }
        let summary = self
            .persisted
            .plans
            .iter_mut()
            .find(|plan| &plan.id == plan_id)
            .ok_or_else(|| PlanError::UnknownPlan {
                plan_id: plan_id.clone(),
            })?;
        summary.status = PlanDocumentStatus::Draft;
        summary.updated_at_ms = now_ms;
        let summary_clone = summary.clone();
        self.persisted.mode = PlanModeState::Active;
        self.persisted.was_previously_active = true;
        self.persisted.reminder_count = 0;
        self.persisted.pending_exit_reminder = false;
        self.persisted.active_plan_id = Some(plan_id.clone());
        self.persisted.awaiting_approval_plan_id = None;
        let content = self.contents.get(plan_id).cloned().unwrap_or_default();
        Ok((
            PlanDocument {
                summary: summary_clone,
                content,
            },
            PlanReviewDecision::ChangesRequested { feedback },
        ))
    }

    /// Abandons a plan and exits Plan Mode without starting implementation.
    pub(crate) fn abandon_plan(
        &mut self,
        plan_id: &PlanId,
        now_ms: u64,
    ) -> Result<(PlanDocument, PlanReviewDecision), PlanError> {
        let summary = self
            .persisted
            .plans
            .iter_mut()
            .find(|plan| &plan.id == plan_id)
            .ok_or_else(|| PlanError::UnknownPlan {
                plan_id: plan_id.clone(),
            })?;
        summary.status = PlanDocumentStatus::Abandoned;
        summary.updated_at_ms = now_ms;
        let summary_clone = summary.clone();
        self.persisted.mode = PlanModeState::Inactive;
        self.persisted.reminder_count = 0;
        self.persisted.pending_exit_reminder = false;
        self.persisted.awaiting_approval_plan_id = None;
        self.persisted.active_plan_id = None;
        let content = self.contents.get(plan_id).cloned().unwrap_or_default();
        Ok((
            PlanDocument {
                summary: summary_clone,
                content,
            },
            PlanReviewDecision::Abandoned,
        ))
    }

    /// Renders a compact dynamic reminder for provider requests.
    ///
    /// Returns `None` during normal inactive operation so non-plan turns incur
    /// zero extra tokens and preserve prompt/KV cache stability.
    #[must_use]
    pub(crate) fn dynamic_prompt_reminder(&self) -> Option<String> {
        match self.persisted.mode {
            PlanModeState::Active => {
                let full = self.persisted.reminder_count.is_multiple_of(2);
                if !full {
                    return Some(
                        "<plan_mode>\nPlan Mode is still active. Workspace file edits remain blocked; update the plan with `write_plan` and call `exit_plan_mode` when ready for user review.\n</plan_mode>"
                            .to_owned(),
                    );
                }
                let plan_status = match self.persisted.active_plan_id.as_ref().and_then(|id| {
                    self.persisted.plans.iter().find(|plan| &plan.id == id)
                }) {
                    Some(summary) => format!(
                        "Active draft: `{}` (\"{}\", file `{}`, r{}). Update it in place with `write_plan`.",
                        summary.id, summary.title, summary.file_name, summary.revision
                    ),
                    None => {
                        "No draft written for this cycle yet. Author your Markdown plan with `write_plan`."
                            .to_owned()
                    }
                };
                Some(format!(
                    "<plan_mode>\nPlan Mode is active (read-only). Do not modify workspace source files; `WorkspaceWrite` tools are blocked until the user approves your plan.\n\
                     {plan_status}\n\
                     Explore the repository read-only, write a clear Markdown plan (Context, Recommended Approach, Key Files, Verification) with `write_plan`, and finish your planning turn by calling `exit_plan_mode` (or asking the user clarifying questions).\n\
                     </plan_mode>"
                ))
            }
            PlanModeState::Inactive if self.persisted.pending_exit_reminder => Some(
                "<plan_mode_exit>\nYou have exited Plan Mode. Workspace file edits are now enabled.\n</plan_mode_exit>"
                    .to_owned(),
            ),
            PlanModeState::Inactive | PlanModeState::Pending | PlanModeState::ExitPending => None,
        }
    }
}

fn validate_title(title: &str) -> Result<String, PlanError> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(PlanError::InvalidTitle {
            reason: "must not be blank",
        });
    }
    if trimmed.len() > MAX_PLAN_TITLE_BYTES {
        return Err(PlanError::InvalidTitle {
            reason: "exceeds maximum length of 200 UTF-8 bytes",
        });
    }
    if trimmed.chars().any(char::is_control) {
        return Err(PlanError::InvalidTitle {
            reason: "must not contain control characters",
        });
    }
    Ok(trimmed.to_owned())
}

/// Validates a plan file name stored directly under `<session_dir>/plans/`.
pub(crate) fn validate_plan_file_name(file_name: &str) -> Result<(), PlanError> {
    if file_name.is_empty()
        || !file_name.ends_with(".md")
        || file_name.contains('/')
        || file_name.contains('\\')
        || file_name.contains("..")
        || file_name.chars().any(char::is_control)
    {
        return Err(PlanError::InvalidFileName {
            file_name: file_name.to_owned(),
        });
    }
    Ok(())
}

/// Produces a safe ASCII kebab-case slug from a human-readable plan title.
fn slugify_title(title: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !slug.is_empty() && !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
        if slug.len() >= MAX_SLUG_LEN {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "plan".to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_toggle_and_prompt_lifecycle() {
        let mut state = PlanSessionState::default();
        assert_eq!(state.mode(), PlanModeState::Inactive);
        assert_eq!(state.dynamic_prompt_reminder(), None);

        assert_eq!(
            state.set_mode_from_user(true, false),
            Some(PlanModeState::Pending)
        );
        assert!(!state.is_active());
        assert_eq!(state.activate_on_user_prompt(), Some(PlanModeState::Active));
        assert!(state.is_active());
        assert!(
            state
                .dynamic_prompt_reminder()
                .expect("full reminder")
                .contains("write_plan")
        );

        state.record_prompt_delivered();
        assert!(
            state
                .dynamic_prompt_reminder()
                .expect("sparse reminder")
                .contains("still active")
        );

        assert_eq!(
            state.set_mode_from_user(false, true),
            Some(PlanModeState::ExitPending)
        );
        assert_eq!(
            state.complete_deferred_exit(),
            Some(PlanModeState::Inactive)
        );
        assert!(
            state
                .dynamic_prompt_reminder()
                .expect("exit reminder")
                .contains("exited Plan Mode")
        );
        state.record_prompt_delivered();
        assert_eq!(state.dynamic_prompt_reminder(), None);
    }

    #[test]
    fn multiple_named_plans_preserve_history_and_slugs() {
        let mut state = PlanSessionState::default();
        state.activate_from_tool();

        let p1 = state
            .save_plan("Auth Architecture!", "# Plan 1\n", None, false, 100)
            .expect("plan 1");
        assert_eq!(p1.summary.id.as_str(), "plan-1");
        assert_eq!(p1.summary.file_name, "001-auth-architecture.md");
        assert_eq!(p1.summary.revision, 1);

        let p1_v2 = state
            .save_plan("Auth Architecture v2", "# Plan 1 v2\n", None, false, 200)
            .expect("plan 1 update");
        assert_eq!(p1_v2.summary.id, p1.summary.id);
        assert_eq!(p1_v2.summary.revision, 2);
        assert_eq!(p1_v2.summary.file_name, "001-auth-architecture.md");

        let pending = state.request_approval(None, 250).expect("approval");
        assert_eq!(pending.summary.status, PlanDocumentStatus::AwaitingApproval);
        let (approved, decision) = state
            .approve_plan(&p1.summary.id, Some("ship it".to_owned()), 300)
            .expect("approve");
        assert_eq!(approved.summary.status, PlanDocumentStatus::Approved);
        assert!(matches!(
            decision,
            PlanReviewDecision::Approved { note: Some(_) }
        ));
        assert_eq!(state.mode(), PlanModeState::Inactive);

        // Second plan in the same session gets a fresh numbered file name.
        state.activate_from_tool();
        let p2 = state
            .save_plan("重构计划面板", "# Plan 2\n", None, false, 400)
            .expect("plan 2");
        assert_eq!(p2.summary.id.as_str(), "plan-2");
        assert_eq!(p2.summary.file_name, "002-plan.md");
        assert_eq!(state.summaries().len(), 2);
    }

    #[test]
    fn reload_collapses_transient_states() {
        let persisted = PersistedPlanModeState {
            mode: PlanModeState::ExitPending,
            ..Default::default()
        };
        let restored = PlanSessionState::from_persisted(persisted, BTreeMap::new());
        assert_eq!(restored.mode(), PlanModeState::Inactive);
        assert!(restored.dynamic_prompt_reminder().is_some());
    }
}
